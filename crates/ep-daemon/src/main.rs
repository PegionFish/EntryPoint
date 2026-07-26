mod api;
mod state;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tracing_subscriber::EnvFilter;

use ep_core::compute::detect_all_devices;
use ep_core::config::AppConfig;
use ep_core::module::discovery::discover_modules;
use ep_core::port::PortManager;

use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ep_daemon=info,ep_core=info".into()),
        )
        .init();

    tracing::info!("EntryPoint Daemon starting...");

    // 1. Load configuration
    let config_dir = std::path::Path::new("config");
    let config = AppConfig::load_or_create(config_dir)?;
    tracing::info!("Configuration loaded");

    // 2. Detect compute devices
    let devices = detect_all_devices(&config.compute.disabled_backends);
    tracing::info!(count = devices.len(), "Compute devices detected");

    // 3. Discover modules
    let modules_dir = std::path::Path::new("modules");
    let modules = discover_modules(modules_dir);
    tracing::info!(count = modules.len(), "Modules discovered");

    // 4. Create managers
    let (port_start, port_end) = config.port_range();
    let port_manager = PortManager::new(port_start, port_end);

    // 5. Build AppState (creates broadcast channels internally)
    let state = Arc::new(AppState::new(config, devices, modules, port_manager));

    // 6. Build router
    let app = Router::new()
        .merge(api::api_router())
        .merge(ws::ws_router())
        .fallback_service(ServeDir::new("crates/ep-webui/static"))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);

    // 7. Start server
    let addr = SocketAddr::from(([0, 0, 0, 0], 9800));
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Daemon shut down gracefully");
    Ok(())
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
