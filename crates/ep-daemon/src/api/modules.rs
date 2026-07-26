use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    routing::{get, post},
    Json,
};
use serde::Serialize;
use serde_json::{Value, json};

use ep_core::module::discovery::DiscoveryStatus;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/modules", get(list_modules))
        .route("/modules/{id}/start", post(start_module))
        .route("/modules/{id}/stop", post(stop_module))
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

    // Pick the first available device or fall back to CPU
    let device = {
        let devices = state.devices.read().await;
        devices
            .first()
            .map(|d| d.id.clone())
            .unwrap_or(ep_core::types::DeviceId::Cpu)
    };

    // Start the module process
    let mut pm = state.process_manager.write().await;
    match pm
        .start_module(&id, &manifest, device, port, Default::default())
        .await
    {
        Ok(()) => Json(json!({
            "status": "started",
            "module_id": id,
            "port": port
        })),
        Err(e) => {
            // Release the port on failure
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
