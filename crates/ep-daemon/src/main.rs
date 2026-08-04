mod api;
mod state;
mod ws;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
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

    // Build environment variables (A2/P0-3 修复：公共构建函数产出裸占位符键
    // —— ROOT/MODULE_DIR/MODEL_DIR/WORKSPACE/MODULE_ID/MODEL_ID/LOG_LEVEL/DEVICE...
    // EP_ 前缀由 process.rs 统一加一次，{MODULE_DIR}/{venv_python} 占位符替换生效)
    let env_vars = ep_core::process::build_module_env(&root, module_id, manifest, &device);

    // Start the module（§3.1：注入共享 CUDA 库目录，平台分支在 process.rs 内部）
    let cuda_libs_dir =
        ep_core::process::resolve_cuda_libs_dir(&root, &cfg.compute.cuda_libs_dir);
    let mut process_manager = ProcessManager::new().with_cuda_libs_dir(cuda_libs_dir);
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
    let app = build_app_router(state.clone(), &static_dir);

    // 9. Spawn background monitor loop (H1 log capture + H2 health check polling)
    //    tick 1s；日志按快照后缀去重，只广播新增行（策略见 diff_new_lines）。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            // module_id → 上一 tick 的日志快照（广播去重游标）
            let mut last_snapshots: HashMap<String, Vec<String>> = HashMap::new();

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

                // 模块消失（停止/退出）时清理对应游标
                last_snapshots.retain(|mid, _| module_ids.iter().any(|m| m == mid));

                for mid in &module_ids {
                    // 取当前日志快照（monitor_process 会先 poll_logs 填充缓冲）
                    let snapshot: Vec<String> = {
                        let mut pm = state.process_manager.write().await;
                        let _ = pm.monitor_process(mid).await;
                        pm.get_instance(mid)
                            .map(|inst| inst.log_buffer.iter().cloned().collect())
                            .unwrap_or_default()
                    };

                    // 与上一快照比对，只广播新增行（每行一条 LogMessage）
                    let last = last_snapshots.entry(mid.clone()).or_default();
                    for line in diff_new_lines(last, &snapshot) {
                        let _ = state.log_tx.send(crate::state::LogMessage {
                            module_id: mid.clone(),
                            line,
                        });
                    }
                    *last = snapshot;
                }
            }
        });
    }

    // 10. Spawn device refresh task — 周期性重新探测计算设备，更新 state.devices。
    //     detect_all_devices 内部用阻塞的 std::process::Command 调 nvidia-smi 等，
    //     故放入 spawn_blocking 执行，避免阻塞 async 运行时。
    //     间隔取启动时 config.compute.refresh_interval_secs（不热跟随变更）。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let interval_secs = {
                let cfg = state.config.read().await;
                u64::from(cfg.compute.refresh_interval_secs.max(1))
            };
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            interval.tick().await; // 首个 tick 立即返回，跳过以避免与启动探测重复
            loop {
                interval.tick().await;
                let disabled = {
                    let cfg = state.config.read().await;
                    cfg.compute.disabled_backends.clone()
                };
                match tokio::task::spawn_blocking(move || detect_all_devices(&disabled)).await {
                    Ok(devices) => {
                        tracing::debug!(count = devices.len(), "devices refreshed");
                        *state.devices.write().await = devices;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "device refresh task failed");
                    }
                }
            }
        });
    }

    // 11. Start server
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

/// 构建完整路由（API + WebSocket + SPA 静态资源）。
///
/// SPA fallback 选用 `ServeDir::fallback`（而非 `not_found_service`）：
/// tower-http 0.6 中 `not_found_service` 会把 fallback 服务的响应状态码强制改为
/// 404（内部 `SetStatus::new(svc, 404)` 包装），导致深链得到 "404 + index.html"；
/// `fallback` 则原样保留内层服务的状态码，`ServeFile` 返回 200 + text/html，
/// 这正是 SPA 深链（如 /tasks/xxx）需要的语义。
fn build_app_router(state: Arc<AppState>, static_dir: &Path) -> Router {
    let index_path = static_dir.join("index.html");
    Router::new()
        .nest("/api", api::api_router())
        .merge(ws::ws_router())
        .fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(index_path)))
        .layer(axum::middleware::from_fn_with_state(state.clone(), ip_filter))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}

/// 计算两次日志快照之间的新增行（后缀比对去重）。
///
/// 策略：从大到小寻找最长的 k，使 `last` 末尾 k 行与 `new` 开头 k 行完全相同，
/// 则 `new[k..]` 即本轮新增行：
/// - 正常追加：k == last.len()，只广播增量尾部（不丢行、不重复）；
/// - 缓冲回绕导致新快照更短（环形缓冲 500 行上限截断头部）：
///   只要仍有重叠后缀，依然只广播增量；
/// - 完全无重叠（模块重启、日志重置等极端情况）：k == 0，
///   整个 `new` 快照视为新增（宁可整批重播，也不静默丢日志）。
fn diff_new_lines(last: &[String], new: &[String]) -> Vec<String> {
    let max_overlap = last.len().min(new.len());
    for k in (0..=max_overlap).rev() {
        if last[last.len() - k..] == new[..k] {
            return new[k..].to_vec();
        }
    }
    // k == 0 必然命中，此处不可达
    new.to_vec()
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
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, ComputeDevice, DeviceId};

    use crate::diff_new_lines;
    use crate::state::AppState;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// Helper: build a test AppState with one CPU device and a unique tempdir root.
    fn test_state() -> Arc<AppState> {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("ep-daemon-test-{}-{seq}", std::process::id()));
        test_state_with_root(root)
    }

    fn test_state_with_root(root: std::path::PathBuf) -> Arc<AppState> {
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
            root,
            AppConfig::default(),
            devices,
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    /// 构造带 loopback ConnectInfo 的请求（ip_filter 中间件需要）
    fn request(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .extension(axum::extract::ConnectInfo(SocketAddr::from((
                [127, 0, 0, 1],
                54321,
            ))))
            .body(Body::empty())
            .unwrap()
    }

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
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

    // 5. POST /api/modules/:id/start + stop — non-existent module → 404 + 中文错误
    #[tokio::test]
    async fn test_start_stop_module() {
        let state = test_state();

        // Start non-existent module
        let (status, json) = crate::api::modules::start_module(
            State(state.clone()),
            axum::extract::Path("nonexistent".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            json.0.get("error").is_some(),
            "expected error key in start response"
        );

        // Stop non-existent module
        let (status, json) = crate::api::modules::stop_module(
            State(state.clone()),
            axum::extract::Path("nonexistent".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            json.0.get("error").is_some(),
            "expected error key in stop response"
        );
    }

    // 6. PUT /api/config → update, verify state and file persistence
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
        .await
        .expect("put_config should succeed");
        let updated = resp.0;
        assert_eq!(updated.general.language, "en-US");
        assert_eq!(updated.ports.range_start, 20000);

        // Verify the state was actually updated
        let config = state.config.read().await;
        assert_eq!(config.general.language, "en-US");
    }

    // 7. GET /api/modules/:id/logs — non-existent module → 404 + 中文错误
    #[tokio::test]
    async fn test_module_logs_unknown_module() {
        let state = test_state();
        let (status, json) = crate::api::modules::module_logs(
            State(state),
            axum::extract::Path("ghost".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(json.0["error"].as_str().unwrap().contains("模块不存在"));
    }

    // ─── 新增测试 ───────────────────────────────────────────────────────────

    // 8. PUT /api/config 后配置实际落盘（tempdir 场景）
    #[tokio::test]
    async fn test_config_put_persists_to_disk() {
        let state = test_state();

        let mut new_config = AppConfig::default();
        new_config.general.language = "zh-TW".to_string();
        new_config.server.port = 12345;

        let _resp = crate::api::config::put_config(State(state.clone()), axum::Json(new_config))
            .await
            .expect("put_config should succeed");

        // 文件必须存在且可回读
        let file_path = state.root.join("config").join("app.toml");
        assert!(file_path.exists(), "config file should be written to disk");
        let loaded = AppConfig::load(state.root.join("config").as_path())
            .expect("reload persisted config");
        assert_eq!(loaded.general.language, "zh-TW");
        assert_eq!(loaded.server.port, 12345);
    }

    // 9. PUT /api/config 落盘失败 → 500 + 中文错误
    #[tokio::test]
    async fn test_config_put_save_failure_returns_500() {
        // root 指向一个普通文件 → root/config 无法创建 → save 失败
        let file_root = std::env::temp_dir().join(format!(
            "ep-daemon-file-root-{}-{}",
            std::process::id(),
            TEST_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&file_root, "blocker").unwrap();
        let state = test_state_with_root(file_root.clone());

        let result = crate::api::config::put_config(
            State(state),
            axum::Json(AppConfig::default()),
        )
        .await;
        let (status, json) = result.expect_err("save should fail when root is a file");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(json.0["error"].as_str().unwrap().contains("保存配置失败"));

        let _ = std::fs::remove_file(&file_root);
    }

    // 10. SPA 深链 → 200 + text/html（index.html 内容）
    #[tokio::test]
    async fn test_spa_deep_link_serves_index_html() {
        let state = test_state();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(
            static_dir.join("index.html"),
            "<html><body>EP-SPA-MARKER</body></html>",
        )
        .unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/tasks/deep/link")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
        assert!(ct.contains("text/html"), "content-type should be html, got {ct}");
        let body = body_string(resp).await;
        assert!(body.contains("EP-SPA-MARKER"));
    }

    // 11. 静态资源命中时直接返回文件（不走 fallback）
    #[tokio::test]
    async fn test_spa_static_file_served_directly() {
        let state = test_state();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<html>index</html>").unwrap();
        std::fs::write(static_dir.join("app.js"), "console.log(1);").unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/app.js")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert_eq!(body, "console.log(1);");
    }

    // 12. /api/unknown → 404 + JSON（不落入 HTML fallback）
    #[tokio::test]
    async fn test_api_unknown_route_returns_404_json() {
        let state = test_state();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<html>index</html>").unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/api/unknown")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
        assert!(ct.contains("application/json"), "content-type should be json, got {ct}");
        let body = body_string(resp).await;
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"], "接口不存在");
    }

    // 12b. /api/unknown + config.language=en → 404 + 英文错误
    #[tokio::test]
    async fn test_api_unknown_route_returns_404_json_en() {
        let state = test_state();
        state.config.write().await.general.language = "en".to_string();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<html>index</html>").unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/api/unknown")).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_string(resp).await;
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"], "API endpoint not found");
    }

    // 13. /ws 路由已注册：非升级请求被拒绝（而不是落入 SPA fallback）
    #[tokio::test]
    async fn test_ws_route_registered_rejects_non_upgrade() {
        let state = test_state();
        let static_dir = state.root.join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<html>index</html>").unwrap();

        let app = crate::build_app_router(state, &static_dir);
        let resp = app.oneshot(request("/ws")).await.unwrap();

        // WebSocketUpgrade 拒绝普通请求（4xx），绝不能是 200 + html
        assert!(
            resp.status().is_client_error(),
            "expected 4xx for non-upgrade /ws request, got {}",
            resp.status()
        );
    }

    // 14. Wave 2 骨架 stub 已全部实现（下载/删除/更新/上传/管线 CRUD/执行/任务），
    //     原 test_wave2_stubs_return_501 于 Wave 2 门禁退役；各端点真实行为由
    //     models.rs / upload.rs / pipelines.rs / execute.rs / tasks.rs 的测试覆盖。

    // ─── diff_new_lines 单元测试（日志去重策略） ────────────────────────────

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // 15. 正常追加 → 只广播增量
    #[test]
    fn test_diff_append_only_new_lines() {
        let last = s(&["a", "b", "c"]);
        let new = s(&["a", "b", "c", "d", "e"]);
        assert_eq!(diff_new_lines(&last, &new), s(&["d", "e"]));
    }

    // 16. 无变化 → 不重复广播
    #[test]
    fn test_diff_no_change_broadcasts_nothing() {
        let last = s(&["a", "b", "c"]);
        assert_eq!(diff_new_lines(&last, &last), Vec::<String>::new());
    }

    // 17. 缓冲回绕（新快照更短，头部被截断）→ 仍只广播增量
    #[test]
    fn test_diff_wraparound_with_overlap() {
        let last = s(&["a", "b", "c", "d"]);
        // 环形缓冲丢掉头部 "a"，又追加了 "e"
        let new = s(&["b", "c", "d", "e"]);
        assert_eq!(diff_new_lines(&last, &new), s(&["e"]));
    }

    // 18. 完全无重叠（模块重启/日志重置）→ 整个新快照视为新增
    #[test]
    fn test_diff_disjoint_broadcasts_all() {
        let last = s(&["a", "b"]);
        let new = s(&["x", "y", "z"]);
        assert_eq!(diff_new_lines(&last, &new), s(&["x", "y", "z"]));
    }

    // 19. 首次快照（last 为空）→ 全部视为新增
    #[test]
    fn test_diff_first_snapshot_broadcasts_all() {
        let last: Vec<String> = Vec::new();
        let new = s(&["boot line 1", "boot line 2"]);
        assert_eq!(diff_new_lines(&last, &new), new);
    }

    // 20. 重复行不误判（最长后缀匹配）
    #[test]
    fn test_diff_repeated_lines() {
        let last = s(&["a", "a", "a"]);
        let new = s(&["a", "a", "a", "a"]);
        assert_eq!(diff_new_lines(&last, &new), s(&["a"]));
    }
}
