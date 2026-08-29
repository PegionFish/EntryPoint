//! daemon 模块代理端点（COMFYUI_BRIDGE_PLAN §3.5，代号 E）。
//!
//! `GET|POST|DELETE /api/modules/{module_id}/extra/{*path}`：把 WebUI 对模块
//! 自有扩展端点（如 ComfyUI 桥接 adapter 的 `/workflows` 工作流管理）的请求
//! 原样转发给**运行中**适配器：
//!
//! 1. 查模块端口注册表（[`ep_core::process::ProcessManager`] 内的运行时实例表，
//!    经 `state.process_manager` 访问：`get_instance(module_id)` →
//!    `ServiceInstance { status, port, .. }`）。模块不存在 / 未处于
//!    [`ep_core::types::ServiceStatus::Running`] / 端口未注册 → `409` +
//!    i18n `apiCore.moduleNotRunning`（键缺失时回退显示键本身，预期行为）。
//!
//! 路由挂载于 `/api` nest 内部（`api_router()`），完整前端路径形如
//! `GET|POST|DELETE /api/modules/{module_id}/extra/{*path}`。
//! 2. 原样转发 method / headers / body（multipart 透传，Content-Type 原样携带
//!    boundary；hop-by-hop 头与 host/content-length 由转发方重建，不透传）到
//!    `http://127.0.0.1:{port}/{path}`（含 query string）。
//! 3. 回传上游响应 status + body（Content-Type 原样）；上游连接失败 →
//!    `502` + `{"error": ..}`。
//! 4. 转发目标仅限 127.0.0.1 适配器（与既有模块端口同一信任域）；路径不含
//!    `../` 穿越（`{*path}` 由 axum 通配段保证）。
//!
//! 前端调用形如 `/api/modules/comfyui-bridge/extra/workflows`。

use std::sync::Arc;
use std::sync::OnceLock;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use super::err_response;
use crate::state::AppState;

/// 请求体转发缓冲上限（防御无界缓冲 OOM；合法工作流 JSON / 图片上传远小于此）。
const MAX_PROXY_BODY_BYTES: usize = 1024 * 1024 * 1024;

/// hop-by-hop 逐跳头（RFC 7230 §6.1）：不得跨代理转发，由转发方按连接重建。
const HOP_BY_HOP_HEADERS: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// 共享转发客户端（连接池复用；显式 `no_proxy`——转发目标恒为本机回环
/// 127.0.0.1，禁止出口代理拦截回环流量，与 ep-core `check_health` 同口径）。
fn proxy_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("failed to build module proxy HTTP client")
    })
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/modules/{module_id}/extra/{*path}",
        get(proxy_extra).post(proxy_extra).delete(proxy_extra),
    )
}

/// 查模块端口注册表：仅运行中（Running）且端口已注册的实例可代理。
async fn running_adapter_port(state: &AppState, module_id: &str) -> Option<u16> {
    let pm = state.process_manager.read().await;
    pm.get_instance(module_id)
        .filter(|inst| inst.status.is_running())
        .and_then(|inst| inst.port)
}

async fn proxy_extra(
    State(state): State<Arc<AppState>>,
    Path((module_id, path)): Path<(String, String)>,
    request: Request,
) -> Response {
    // 1. 运行中适配器端口（不存在 / 未运行 / 无端口 → 409）
    let Some(port) = running_adapter_port(&state, &module_id).await else {
        return err_response(
            &state,
            StatusCode::CONFLICT,
            "apiCore.moduleNotRunning",
            &[("module", module_id)],
        )
        .await
        .into_response();
    };

    // 2. 原样转发（method/headers/body；multipart 透传，Content-Type 原样带 boundary）
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!("http://127.0.0.1:{port}/{path}{query}");

    let method = request.method().clone();
    let mut upstream_req = proxy_client().request(method, &url);
    for (name, value) in request.headers() {
        // host / content-length 按目标连接重建；hop-by-hop 头不透传
        if name != header::HOST
            && name != header::CONTENT_LENGTH
            && !HOP_BY_HOP_HEADERS.contains(&name.as_str())
        {
            upstream_req = upstream_req.header(name, value);
        }
    }
    let body = match axum::body::to_bytes(request.into_body(), MAX_PROXY_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({ "error": format!("module proxy body read failed: {e}") })),
            )
                .into_response();
        }
    };
    if !body.is_empty() {
        upstream_req = upstream_req.body(body);
    }

    // 3. 回传上游响应 status + body（Content-Type 原样）；上游连接失败 → 502
    let upstream = match upstream_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": format!("module adapter unreachable at 127.0.0.1:{port}: {e}")
                })),
            )
                .into_response();
        }
    };

    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let upstream_status = upstream.status();
    let payload = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("module adapter response read failed: {e}") })),
            )
                .into_response();
        }
    };

    let mut builder = Response::builder().status(upstream_status);
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder.body(Body::from(payload)).unwrap_or_else(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("module proxy response build failed: {e}") })),
        )
            .into_response()
    })
}

// ─── 测试 ────────────────────────────────────────────────────────────────────
//
// Router::oneshot + 进程内 mock adapter（tokio TcpListener 极简回显服务器）：
// 非 /health 请求回显「请求行 + content-type 头 + 原始 body」，供透传断言。

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::module::manifest::{
        ComputeConfig, InterfaceConfig, InterfaceType, ModuleInfo, ModuleManifest, RuntimeConfig,
        RuntimeType,
    };
    use ep_core::port::PortManager;
    use ep_core::types::{ComputeBackend, DeviceId};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    const MODULE_ID: &str = "proxy-mod";

    fn unique_root(tag: &str) -> std::path::PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-module-proxy-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState::new(
            unique_root("state"),
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(48400, 49000),
        ))
    }

    /// 挂载完整 /api 路由树（同时验证与既有模块路由无冲突）
    fn app(state: Arc<AppState>) -> axum::Router {
        crate::api::api_router(state.clone()).with_state(state)
    }

    /// native 空转模块（真实子进程占位，端口指向 mock adapter）
    fn fixture_manifest() -> ModuleManifest {
        ModuleManifest {
            module: ModuleInfo {
                id: MODULE_ID.to_string(),
                name: "Proxy Fixture".to_string(),
                version: "0.1.0".to_string(),
                description: "module proxy fixture".to_string(),
                category: ep_core::types::ModuleCategory::Custom,
                genre: "comfyui".to_string(),
                authors: vec![],
                license: None,
                homepage: None,
                tags: vec![],
            },
            runtime: RuntimeConfig {
                runtime_type: RuntimeType::Native,
                python_version: None,
                requirements: None,
                entrypoint: None,
                // 跨平台空转命令：真实子进程占位（对齐 modules.rs 测试做法）
                start_command: Some(if cfg!(windows) {
                    "ping -n 30 127.0.0.1".to_string()
                } else {
                    "sleep 30".to_string()
                }),
                binaries: None,
                requirements_by_backend: Default::default(),
            },
            compute: ComputeConfig {
                backends: vec![ComputeBackend::Cpu],
                default_backend: None,
                vram_estimate_mb: None,
                min_vram_mb: None,
                env: None,
            },
            models: vec![],
            interface: InterfaceConfig {
                interface_type: InterfaceType::Http,
                health_endpoint: None, // 缺省 /health，由 mock adapter 应答
                ready_timeout_secs: None,
                working_dir: None,
                capabilities: vec![],
            },
        }
    }

    /// 在 state 注册表里造一个 Running 实例：真实子进程 + 端口指向 mock adapter，
    /// 经 `monitor_process` 健康检查（mock 回 200）翻转为 Running。
    async fn start_running_module(state: &Arc<AppState>, adapter_port: u16) {
        let manifest = fixture_manifest();
        state
            .process_manager
            .write()
            .await
            .start_module(
                MODULE_ID,
                &manifest,
                DeviceId::Cpu,
                adapter_port,
                std::collections::HashMap::new(),
            )
            .await
            .unwrap();
        state
            .process_manager
            .write()
            .await
            .monitor_process(MODULE_ID)
            .await
            .unwrap();
        assert!(
            state
                .process_manager
                .read()
                .await
                .get_instance(MODULE_ID)
                .map(|inst| inst.status.is_running())
                .unwrap_or(false),
            "monitor_process 后实例应为 Running"
        );
    }

    async fn stop_module(state: &Arc<AppState>) {
        state
            .process_manager
            .write()
            .await
            .stop_module(MODULE_ID)
            .await
            .unwrap();
    }

    /// 极简回显服务器：非 /health 请求回显「请求行 + content-type 头 + 空行 +
    /// 原始 body」；每连接一个请求（Connection: close，reqwest 不复用连接）。
    /// 返回（端口，任务句柄）——句柄 abort 即关闭端口（502 场景用）。
    async fn spawn_mock_adapter() -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf: Vec<u8> = Vec::new();
                    let mut tmp = [0u8; 8192];

                    // 读到头部结束（\r\n\r\n）
                    let head_end = loop {
                        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                            break pos + 4;
                        }
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    };

                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let request_line = head.lines().next().unwrap_or("").to_string();
                    let content_length: usize = head
                        .lines()
                        .find_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            if k.eq_ignore_ascii_case("content-length") {
                                v.trim().parse().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);

                    // 读满 body
                    while buf.len() < head_end + content_length {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let body = buf[head_end.min(buf.len())..].to_vec();

                    let target = request_line.split(' ').nth(1).unwrap_or("").to_string();
                    let (status_line, content_type, payload): (&str, &str, Vec<u8>) =
                        if target == "/health" {
                            ("HTTP/1.1 200 OK", "text/plain", b"ok".to_vec())
                        } else {
                            let ct = head
                                .lines()
                                .skip(1)
                                .find_map(|line| {
                                    let (k, v) = line.split_once(':')?;
                                    if k.eq_ignore_ascii_case("content-type") {
                                        Some(v.trim().to_string())
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or_default();
                            let mut echo =
                                format!("{request_line}\ncontent-type: {ct}\n\n").into_bytes();
                            echo.extend_from_slice(&body);
                            ("HTTP/1.1 200 OK", "application/octet-stream", echo)
                        };

                    let resp = format!(
                        "{status_line}\r\nContent-Type: {content_type}\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        payload.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.write_all(&payload).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (port, handle)
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    async fn response_bytes(resp: Response) -> (StatusCode, Vec<u8>, Option<String>) {
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, bytes, content_type)
    }

    fn get_request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn delete_request(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::DELETE)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    // ── 1. 运行中 GET 转发（含 query string） ────────────────────────────────

    #[tokio::test]
    async fn proxy_get_forwards_when_running() {
        let state = test_state();
        let (port, mock) = spawn_mock_adapter().await;
        start_running_module(&state, port).await;

        let resp = app(state.clone())
            .oneshot(get_request("/modules/proxy-mod/extra/workflows?verbose=1"))
            .await
            .unwrap();
        let (status, bytes, _) = response_bytes(resp).await;
        let body = String::from_utf8_lossy(&bytes);
        assert_eq!(status, StatusCode::OK);
        // 请求行回显：method + 转发路径 + query 原样
        assert!(
            body.starts_with("GET /workflows?verbose=1"),
            "回显应为转发后请求行, got: {body}"
        );

        stop_module(&state).await;
        mock.abort();
    }

    // ── 2. 运行中 POST multipart 原样透传（headers + body 带 boundary） ──────

    #[tokio::test]
    async fn proxy_post_multipart_passthrough() {
        let state = test_state();
        let (port, mock) = spawn_mock_adapter().await;
        start_running_module(&state, port).await;

        const BOUNDARY: &str = "----ep-module-proxy-test-boundary";
        let file_bytes: &[u8] = b"\x89PNG\r\n\x1a\n fake-image-bytes-\xff\xfe";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"wf.api.json\"\r\n\
              Content-Type: application/json\r\n\r\n",
        );
        body.extend_from_slice(b"{\"workflow\": true}");
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        body.extend_from_slice(
            "Content-Disposition: form-data; name=\"image\"; filename=\"in.png\"\r\n\
             Content-Type: image/png\r\n\r\n"
                .as_bytes(),
        );
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

        let req = Request::builder()
            .method(Method::POST)
            .uri("/modules/proxy-mod/extra/upload/image")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header("content-length", body.len().to_string())
            .body(Body::from(body.clone()))
            .unwrap();

        let resp = app(state.clone()).oneshot(req).await.unwrap();
        let (status, bytes, content_type) = response_bytes(resp).await;
        let echo = String::from_utf8_lossy(&bytes);
        assert_eq!(status, StatusCode::OK);
        // method + path 透传
        assert!(
            echo.starts_with("POST /upload/image"),
            "回显应为转发后请求行, got: {echo}"
        );
        // Content-Type 原样透传（含 boundary）
        assert!(
            echo.contains(&format!("content-type: multipart/form-data; boundary={BOUNDARY}")),
            "Content-Type 头应原样透传, got: {echo}"
        );
        // body 原样透传（二进制安全：multipart 原始字节逐字节到达 adapter）
        assert!(
            bytes.ends_with(&body),
            "body 应逐字节透传, echo_len={} body_len={}",
            bytes.len(),
            body.len()
        );
        assert!(bytes.windows(file_bytes.len()).any(|w| w == file_bytes));
        // 响应 Content-Type 回传上游原样
        assert_eq!(content_type.as_deref(), Some("application/octet-stream"));

        stop_module(&state).await;
        mock.abort();
    }

    // ── 3. 运行中 DELETE 透传 ────────────────────────────────────────────────

    #[tokio::test]
    async fn proxy_delete_forwards_when_running() {
        let state = test_state();
        let (port, mock) = spawn_mock_adapter().await;
        start_running_module(&state, port).await;

        let resp = app(state.clone())
            .oneshot(delete_request(
                "/modules/proxy-mod/extra/workflows/tpl.json",
            ))
            .await
            .unwrap();
        let (status, bytes, _) = response_bytes(resp).await;
        let body = String::from_utf8_lossy(&bytes);
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.starts_with("DELETE /workflows/tpl.json"),
            "DELETE 应原样转发, got: {body}"
        );

        stop_module(&state).await;
        mock.abort();
    }

    // ── 4. 未运行 / 不存在 → 409 + i18n（键缺失回退显示键本身） ──────────────

    #[tokio::test]
    async fn proxy_module_not_running_returns_409() {
        let state = test_state();
        let app = app(state.clone());

        // 不存在的模块
        let resp = app
            .clone()
            .oneshot(get_request("/modules/no-such-mod/extra/workflows"))
            .await
            .unwrap();
        let (status, bytes, _) = response_bytes(resp).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // i18n 资源无此键 → 回退显示键本身（预期行为）
        assert_eq!(json["error"], "apiCore.moduleNotRunning");

        // 启动后停止 → 未运行同样 409
        let (port, mock) = spawn_mock_adapter().await;
        start_running_module(&state, port).await;
        stop_module(&state).await;

        let resp = app
            .oneshot(get_request("/modules/proxy-mod/extra/workflows"))
            .await
            .unwrap();
        let (status, bytes, _) = response_bytes(resp).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"], "apiCore.moduleNotRunning");

        mock.abort();
    }

    // ── 5. 上游连接失败 → 502 + {"error":..} ─────────────────────────────────

    #[tokio::test]
    async fn proxy_upstream_unreachable_returns_502() {
        let state = test_state();
        let (port, mock) = spawn_mock_adapter().await;
        start_running_module(&state, port).await;

        // 关停 mock adapter（端口释放）→ 转发目标不可达
        mock.abort();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = app(state.clone())
            .oneshot(get_request("/modules/proxy-mod/extra/workflows"))
            .await
            .unwrap();
        let (status, bytes, _) = response_bytes(resp).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["error"].as_str().is_some());

        stop_module(&state).await;
    }
}
