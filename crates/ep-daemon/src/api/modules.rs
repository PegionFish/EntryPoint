use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    routing::{get, post},
    Json,
};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use ep_core::module::discovery::DiscoveryStatus;
use ep_core::types::DeviceId;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/modules", get(list_modules))
        .route("/modules/{id}/start", post(start_module))
        .route("/modules/{id}/stop", post(stop_module))
        .route("/modules/{id}/status", get(module_status))
        .route("/modules/{id}/logs", get(module_logs))
}

// ─── Response types ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub(crate) struct ModuleResponse {
    id: String,
    name: String,
    version: String,
    description: String,
    category: String,
    path: String,
    status: String,
    service_status: String,
}

// ─── Handlers ───────────────────────────────────────────────────────────────

pub async fn list_modules(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<ModuleResponse>> {
    let modules = state.modules.read().await;
    let pm = state.process_manager.read().await;

    let resp: Vec<ModuleResponse> = modules
        .iter()
        .map(|m| {
            let (id, name, version, description, category) =
                if let Some(ref manifest) = m.manifest {
                    (
                        manifest.module.id.clone(),
                        manifest.module.name.clone(),
                        manifest.module.version.clone(),
                        manifest.module.description.clone(),
                        manifest.module.category.to_string(),
                    )
                } else {
                    let dir_name = m
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    (
                        dir_name.clone(),
                        dir_name.clone(),
                        String::new(),
                        String::new(),
                        String::new(),
                    )
                };

            let discovery_status = match &m.status {
                DiscoveryStatus::Valid => "valid".to_string(),
                DiscoveryStatus::Invalid(reason) => format!("invalid: {reason}"),
            };

            let service_status = pm
                .get_status(&id)
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "stopped".to_string());

            ModuleResponse {
                id,
                name,
                version,
                description,
                category,
                path: m.path.display().to_string(),
                status: discovery_status,
                service_status,
            }
        })
        .collect();

    Json(resp)
}

pub async fn start_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<Value> {
    // Verify module exists
    let modules = state.modules.read().await;
    let module = match modules.iter().find(|m| {
        m.manifest
            .as_ref()
            .map(|mf| mf.module.id == id)
            .unwrap_or(false)
    }) {
        Some(m) => m.clone(),
        None => {
            return Json(json!({
                "error": format!("module '{id}' not found")
            }));
        }
    };
    drop(modules);

    let manifest = match module.manifest {
        Some(mf) => mf,
        None => {
            return Json(json!({
                "error": format!("module '{id}' has no valid manifest")
            }));
        }
    };

    // Allocate a port
    let port = {
        let mut pm = state.port_manager.write().await;
        match pm.allocate(&id) {
            Ok(p) => p,
            Err(e) => {
                return Json(json!({
                    "error": format!("port allocation failed: {e}")
                }));
            }
        }
    };

    // Pick the first device whose backend matches the manifest's compute.backends,
    // otherwise fall back to CPU.
    let device = {
        let devices = state.devices.read().await;
        devices
            .iter()
            .find(|d| manifest.compute.backends.contains(&d.backend))
            .map(|d| d.id.clone())
            .unwrap_or(DeviceId::Cpu)
    };

    // Build environment variables for the module process
    let env_vars = {
        let root = std::env::current_dir().unwrap_or_default();
        let module_dir = &module.path;
        let model_dir = if let Some(model) = manifest.models.iter().find(|m| m.default) {
            root.join("models").join(&model.target_dir)
        } else if let Some(model) = manifest.models.first() {
            root.join("models").join(&model.target_dir)
        } else {
            module_dir.clone()
        };

        let mut vars = HashMap::new();
        vars.insert("ROOT".to_string(), root.to_string_lossy().to_string());
        vars.insert("MODULE_DIR".to_string(), module_dir.to_string_lossy().to_string());
        vars.insert("MODEL_DIR".to_string(), model_dir.to_string_lossy().to_string());
        vars.insert("PORT".to_string(), port.to_string());
        vars.insert("DEVICE".to_string(), device.to_string());
        vars.insert("BACKEND".to_string(), device.backend().to_string());
        vars.insert(
            "DEVICE_INDEX".to_string(),
            device.index().map(|i| i.to_string()).unwrap_or_default(),
        );
        vars.insert("WORKSPACE".to_string(), root.join("workspace").to_string_lossy().to_string());
        vars
    };

    info!(module_id = %id, %port, %device, "starting module");

    // Start the module process
    let mut pm = state.process_manager.write().await;
    match pm
        .start_module(&id, &manifest, device, port, env_vars)
        .await
    {
        Ok(()) => Json(json!({
            "status": "starting",
            "module_id": id,
            "port": port
        })),
        Err(e) => {
            // Release the port on failure
            warn!(module_id = %id, error = %e, "failed to start module");
            state.port_manager.write().await.release(&id);
            Json(json!({
                "error": format!("failed to start module: {e}")
            }))
        }
    }
}

pub async fn stop_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<Value> {
    let mut pm = state.process_manager.write().await;
    match pm.stop_module(&id).await {
        Ok(()) => {
            state.port_manager.write().await.release(&id);
            info!(module_id = %id, "module stopped");
            Json(json!({
                "status": "stopped",
                "module_id": id
            }))
        }
        Err(e) => Json(json!({
            "error": format!("failed to stop module: {e}")
        })),
    }
}

pub async fn module_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<Value> {
    let pm = state.process_manager.read().await;
    match pm.get_instance(&id) {
        Some(inst) => {
            let status_str = match &inst.status {
                ep_core::types::ServiceStatus::Running => "running",
                ep_core::types::ServiceStatus::Stopped => "stopped",
                ep_core::types::ServiceStatus::Starting => "starting",
                ep_core::types::ServiceStatus::Preparing => "preparing",
                ep_core::types::ServiceStatus::NotReady => "not_ready",
                ep_core::types::ServiceStatus::Error(_) => "error",
            };

            let uptime_secs = inst
                .started_at
                .map(|t| {
                    let now_ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    (now_ts - t.timestamp()).max(0)
                })
                .unwrap_or(0);

            Json(json!({
                "module_id": id,
                "status": status_str,
                "port": inst.port,
                "uptime_secs": uptime_secs
            }))
        }
        None => Json(json!({
            "module_id": id,
            "status": "stopped",
            "port": null,
            "uptime_secs": 0
        })),
    }
}

pub async fn module_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<Value> {
    let pm = state.process_manager.read().await;
    match pm.get_instance(&id) {
        Some(inst) => {
            let lines: Vec<&String> = inst.log_buffer.iter().collect();
            Json(json!({
                "module_id": id,
                "lines": lines
            }))
        }
        None => Json(json!({
            "module_id": id,
            "lines": []
        })),
    }
}
