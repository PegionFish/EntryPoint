mod api;
mod state;
mod ws;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::ConnectInfo;
use axum::response::IntoResponse;
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tracing_subscriber::EnvFilter;

use ep_core::compute::detect_all_devices;
use ep_core::config::{self, AppConfig};
use ep_core::deps::DepReport;
use ep_core::module::discovery::discover_modules;
use ep_core::port::PortManager;
use ep_core::process::ProcessManager;

use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ep_daemon=info,ep_core=info".into()),
        )
        .init();

    // Parse CLI args
    let args: Vec<String> = std::env::args().collect();

    if let Some(pos) = args.iter().position(|a| a == "--run-module") {
        let module_id = args
            .get(pos + 1)
            .ok_or_else(|| anyhow::anyhow!("--run-module requires a module ID"))?;
        return run_module_standalone(module_id).await;
    }

    run_server().await
}

/// Standalone module runner — start a single module and keep it running.
async fn run_module_standalone(module_id: &str) -> anyhow::Result<()> {
    tracing::info!("Standalone mode: running module '{}'", module_id);

    // Resolve root directory and load config
    let root = config::resolve_root();
    let config_dir = root.join("config");
    let mut cfg = AppConfig::load_or_create(&config_dir).unwrap_or_default();
    cfg.resolve_paths(&root);

    // Discover modules
    let modules_dir = root.join("modules");
    let modules = discover_modules(&modules_dir);

    // Find the target module
    let discovered = modules
        .iter()
        .find(|m| {
            m.manifest
                .as_ref()
                .map(|mf| mf.module.id == module_id)
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("Module '{}' not found in modules/", module_id))?;

    let manifest = discovered
        .manifest
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Module '{}' has invalid manifest", module_id))?;

    tracing::info!(
        "Found module: {} v{} ({})",
        manifest.module.name,
        manifest.module.version,
        manifest.module.category
    );

    // Detect devices and pick the best one
    let devices = detect_all_devices(&cfg.compute.disabled_backends);
    let device = devices
        .iter()
        .find(|d| {
            manifest
                .compute
                .backends
                .contains(&d.backend)
        })
        .map(|d| d.id.clone())
        .unwrap_or(ep_core::types::DeviceId::Cpu);

    tracing::info!("Using device: {}", device);

    // Allocate port
    let (port_start, port_end) = cfg.port_range();
    let mut port_manager = PortManager::new(port_start, port_end);
    let port = port_manager.allocate(module_id)?;
    tracing::info!("Allocated port: {}", port);

    // Build environment variables (root already resolved above)
    let module_dir = root.join("modules").join(module_id);
    let model_dir = if let Some(model) = manifest.models.iter().find(|m| m.default) {
        root.join("models").join(&model.target_dir)
    } else if let Some(model) = manifest.models.first() {
        root.join("models").join(&model.target_dir)
    } else {
        module_dir.clone()
    };

    let mut env_vars = HashMap::new();
    env_vars.insert("EP_ROOT".to_string(), root.to_string_lossy().to_string());
    env_vars.insert("EP_MODULE_DIR".to_string(), module_dir.to_string_lossy().to_string());
    env_vars.insert("EP_MODULE_ID".to_string(), module_id.to_string());
    env_vars.insert("EP_MODEL_DIR".to_string(), model_dir.to_string_lossy().to_string());
    env_vars.insert("EP_WORKSPACE".to_string(), root.join("workspace").to_string_lossy().to_string());
    env_vars.insert("EP_LOG_LEVEL".to_string(), "info".to_string());

    if let Some(model) = manifest.models.iter().find(|m| m.default).or(manifest.models.first()) {
        env_vars.insert("EP_MODEL_ID".to_string(), model.id.clone());
    }

    // Start the module
    let mut process_manager = ProcessManager::new();
    process_manager
        .start_module(module_id, manifest, device.clone(), port, env_vars)
        .await?;

    tracing::info!(
        "Module '{}' started on port {} (device: {})",
        module_id,
        port,
        device
    );
    tracing::info!("Press Ctrl+C to stop");

    // Wait for shutdown signal
    tokio::signal::ctrl_c().await?;

    tracing::info!("Shutting down module '{}'...", module_id);
    process_manager.stop_module(module_id).await?;
    port_manager.release(module_id);
    tracing::info!("Module '{}' stopped", module_id);

    Ok(())
}

/// Normal HTTP server mode.
async fn run_server() -> anyhow::Result<()> {
    tracing::info!("EntryPoint Daemon starting...");

    // 1. Resolve root directory and load configuration
    let root = config::resolve_root();
    tracing::info!(root = %root.display(), "project root resolved");

    let config_dir = root.join("config");
    let mut cfg = AppConfig::load_or_create(&config_dir)?;
    cfg.resolve_paths(&root);
    tracing::info!("Configuration loaded");

    // 2. Check and auto-install missing system dependencies
    {
        let results = DepReport::check_and_install_missing(&root);
        for (dep, result) in &results {
            match result {
                ep_core::deps_install::InstallResult::Installed => {
                    tracing::info!(dep = ?dep, "dependency auto-installed");
                }
                ep_core::deps_install::InstallResult::AlreadyPresent => {}
                ep_core::deps_install::InstallResult::Failed(msg) => {
                    tracing::warn!(dep = ?dep, msg = %msg, "dependency auto-install failed");
                }
                ep_core::deps_install::InstallResult::ManualRequired(msg) => {
                    tracing::warn!(dep = ?dep, msg = %msg, "manual installation required");
                }
            }
        }
    }

    // 3. Detect compute devices
    let devices = detect_all_devices(&cfg.compute.disabled_backends);
    tracing::info!(count = devices.len(), "Compute devices detected");

    // 4. Discover modules
    let modules_dir = root.join("modules");
    let modules = discover_modules(&modules_dir);
    tracing::info!(count = modules.len(), "Modules discovered");

    // 5. Create managers
    let (port_start, port_end) = cfg.port_range();
    let port_manager = PortManager::new(port_start, port_end);

    // 6. Build AppState (creates broadcast channels internally)
    let state = Arc::new(AppState::new(root.clone(), cfg, devices, modules, port_manager));

    // 7. Log public access setting
    {
        let cfg = state.config.read().await;
        if cfg.server.allow_public {
            tracing::warn!("public access ENABLED — no built-in auth/encryption, use at your own risk");
        } else {
            tracing::info!("public access blocked — only private/loopback IPs allowed (set allow_public=true to change)");
        }
    }

    // 8. Build router with SPA fallback (absolute path)
    // Packaged layout: <root>/webui; dev layout: <root>/crates/ep-webui/static
    let static_dir = if root.join("webui").is_dir() {
        root.join("webui")
    } else {
        root.join("crates").join("ep-webui").join("static")
    };
    let static_dir_str = static_dir.to_string_lossy().to_string();
    let index_path = static_dir.join("index.html");
    let index_path_str = index_path.to_string_lossy().to_string();
    let app = Router::new()
        .nest("/api", api::api_router())
        .merge(ws::ws_router())
        .fallback_service(
            ServeDir::new(&static_dir_str)
                .not_found_service(ServeFile::new(&index_path_str)),
        )
        .layer(axum::middleware::from_fn_with_state(state.clone(), ip_filter))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state.clone());

    // 8. Spawn background monitor loop (H1 log capture + H2 health check polling)
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                interval.tick().await;

                // Collect module IDs that have active process instances
                let module_ids: Vec<String> = {
                    let pm = state.process_manager.read().await;
                    pm.list_running()
                        .iter()
                        .map(|inst| inst.module_id.clone())
                        .collect()
                };

                for mid in &module_ids {
                    let mut pm = state.process_manager.write().await;
                    let _ = pm.monitor_process(mid).await;

                    // Broadcast new log lines to WebSocket subscribers
                    if let Some(inst) = pm.get_instance(mid) {
                        let lines: Vec<String> = inst.log_buffer.iter().cloned().collect();
                        if !lines.is_empty() {
                            if let Some(last) = lines.last() {
                                let _ = state.log_tx.send(crate::state::LogMessage {
                                    module_id: mid.clone(),
                                    line: last.clone(),
                                });
                            }
                        }
                    }
                }
            }
        });
    }

    // 9. Start server
    let cfg = state.config.read().await;
    let addr: SocketAddr = format!("{}:{}", cfg.server.host, cfg.server.port)
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 9800)));
    drop(cfg);
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    tracing::info!("Daemon shut down gracefully");
    Ok(())
}

/// 判断 IP 是否为私有/本地地址（RFC 1918 + loopback + link-local）
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// IP 过滤中间件：allow_public = false 时拒绝非私有 IP
async fn ip_filter(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let allow_public = {
        let config = state.config.read().await;
        config.server.allow_public
    };
    if !allow_public && !is_private_ip(&addr.ip()) {
        tracing::warn!(ip = %addr.ip(), "blocked public access attempt");
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C signal handler");
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::extract::State;
    use serde_json::Value;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId};

    use crate::state::AppState;

    /// Helper: build a test AppState with one CPU device.
    fn test_state() -> Arc<AppState> {
        let devices = vec![ComputeDevice {
            id: DeviceId::Cpu,
            backend: ComputeBackend::Cpu,
            name: "Test CPU".to_string(),
            total_memory_mb: Some(16384),
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        }];

        Arc::new(AppState::new(
            std::path::PathBuf::from("/tmp/ep-test"),
            AppConfig::default(),
            devices,
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    // 1. GET /api/health → 200 + JSON with status "ok"
    #[tokio::test]
    async fn test_health_endpoint() {
        // health_check takes no extractor, call directly
        let resp = crate::api::health::health_check().await;
        let json = resp.0;
        assert_eq!(json["status"], "ok");
        assert!(json["version"].is_string());
    }

    // 2. GET /api/devices → device list with our test CPU
    #[tokio::test]
    async fn test_devices_endpoint() {
        let state = test_state();
        let resp = crate::api::devices::list_devices(State(state)).await;
        // Serialize to JSON to inspect fields (DeviceResponse fields are private)
        let json: Value = serde_json::to_value(&resp.0).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "cpu");
        assert_eq!(arr[0]["name"], "Test CPU");
        assert_eq!(arr[0]["total_memory_mb"], 16384);
    }

    // 3. GET /api/modules → empty module list
    #[tokio::test]
    async fn test_modules_endpoint() {
        let state = test_state();
        let resp = crate::api::modules::list_modules(State(state)).await;
        let modules = resp.0;
        assert!(modules.is_empty());
    }

    // 4. GET /api/config → default config
    #[tokio::test]
    async fn test_config_get() {
        let state = test_state();
        let resp = crate::api::config::get_config(State(state)).await;
        let config = resp.0;
        assert_eq!(config.general.language, "zh-CN");
        assert_eq!(config.ports.range_start, 18000);
        assert_eq!(config.ports.range_end, 19000);
    }

    // 5. POST /api/modules/:id/start + stop — non-existent module returns error
    #[tokio::test]
    async fn test_start_stop_module() {
        let state = test_state();

        // Start non-existent module
        let resp = crate::api::modules::start_module(
            State(state.clone()),
            axum::extract::Path("nonexistent".to_string()),
        )
        .await;
        let json: Value = resp.0;
        assert!(
            json.get("error").is_some(),
            "expected error key in start response"
        );

        // Stop non-existent module
        let resp = crate::api::modules::stop_module(
            State(state.clone()),
            axum::extract::Path("nonexistent".to_string()),
        )
        .await;
        let json: Value = resp.0;
        assert!(
            json.get("error").is_some(),
            "expected error key in stop response"
        );
    }

    // 6. PUT /api/config → update and verify
    #[tokio::test]
    async fn test_config_put() {
        let state = test_state();

        let mut new_config = AppConfig::default();
        new_config.general.language = "en-US".to_string();
        new_config.ports.range_start = 20000;

        let resp = crate::api::config::put_config(
            State(state.clone()),
            axum::Json(new_config),
        )
        .await;
        let updated = resp.0;
        assert_eq!(updated.general.language, "en-US");
        assert_eq!(updated.ports.range_start, 20000);

        // Verify the state was actually updated
        let config = state.config.read().await;
        assert_eq!(config.general.language, "en-US");
    }

    // 7. GET /api/modules/:id/logs — non-existent module returns empty lines
    #[tokio::test]
    async fn test_module_logs_empty() {
        let state = test_state();
        let resp = crate::api::modules::module_logs(
            State(state),
            axum::extract::Path("ghost".to_string()),
        )
        .await;
        let json: Value = resp.0;
        assert_eq!(json["module_id"], "ghost");
        assert!(json["lines"].as_array().unwrap().is_empty());
    }
}
