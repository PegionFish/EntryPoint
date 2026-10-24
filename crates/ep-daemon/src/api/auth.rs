//! 全局访问保护（T-06）：`[server].access_protection = "token"` 时，
//! 对 `/api/*` 与 `/ws*` 校验共享令牌。
//!
//! 设计要点：
//! - 默认 `none`：中间件直接放行，行为与改造前完全一致；
//! - `/api/v1/*` 由 v1 子 Router 自带的 token 中间件负责，这里直通避免双重校验；
//! - 静态资源与页面路由不属于 `/api`、`/ws` 前缀，直接放行，避免每个资源
//!   请求都去读 config 锁；
//! - `Authorization: Bearer <token>` 与 `X-API-Key: <token>` 均可用；
//! - GET/HEAD 额外接受 `?token=`（浏览器 `<img>/<video>/<a download>` 无法
//!   附加请求头）；POST/PUT/DELETE 不接受 query token，避免写操作把令牌
//!   泄漏进访问日志/浏览器历史；
//! - 令牌在启动配置中为空时失败关闭（fail-closed），返回 401 而非静默放行。

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use ep_core::config::AccessProtection;

use super::err_response;
use super::inference::{ct_eq, parse_bearer_token};
use crate::state::AppState;

/// 401 错误体 i18n 键（apiCore 命名空间，zh/en 均已配置）。
const UNAUTHORIZED_KEY: &str = "apiCore.accessProtection.unauthorized";

/// 是否需要本中间件处理该路径。
///
/// 只有 `/api`、`/api/...`、`/ws`、`/ws/...` 进入鉴权；其余（页面、
/// `/assets/*` 静态资源、`/health` 等）直接放行。
fn is_protected_path(path: &str) -> bool {
    path == "/api"
        || path.starts_with("/api/")
        || path == "/ws"
        || path.starts_with("/ws/")
}

/// v1 子 Router 自带 token 校验：此处放行，避免双重校验与语义冲突。
fn is_v1_path(path: &str) -> bool {
    path == "/api/v1" || path.starts_with("/api/v1/")
}

/// 从 query string 中提取 `token` 参数（支持百分号编码）。
///
/// 仅用于 GET/HEAD 的浏览器原生加载场景；写操作不接受 query token。
fn query_token(query: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        if name != "token" {
            return None;
        }
        Some(percent_decode(value))
    })
}

/// URL 百分号解码（`%HH`），非法序列按字面保留。
fn percent_decode(raw: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 全局访问保护中间件（挂在 `build_app_router` 最外层，覆盖 /api 与 /ws）。
pub async fn require_access(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();

    // 非 API/WS 路径（静态资源、页面）与 v1 前缀放行
    if !is_protected_path(&path) || is_v1_path(&path) {
        return next.run(request).await;
    }
    // CORS 预检请求不带凭据；OPTIONS 本身不泄露数据，放行让 CORS 中间件响应
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }

    // C1 修复（同 v1 中间件）：先快照配置并释放读锁，再 await 下游/写响应，
    // 否则写锁到来后会阻塞整个 daemon 的 config 读取。
    let (mode, token) = {
        let cfg = state.config.read().await;
        (cfg.server.access_protection, cfg.server.token.clone())
    };

    if mode == AccessProtection::None {
        return next.run(request).await;
    }

    // token 模式但令牌为空：配置不完整，失败关闭（绝不静默放行）
    if token.is_empty() {
        return unauthorized(&state, &path).await;
    }

    // RFC 6750：scheme 大小写不敏感
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_bearer_token);
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim);

    let mut ok = bearer
        .map(|v| ct_eq(v.as_bytes(), token.as_bytes()))
        .unwrap_or(false)
        || api_key
            .map(|v| ct_eq(v.as_bytes(), token.as_bytes()))
            .unwrap_or(false);

    // GET/HEAD 允许 ?token=（浏览器原生加载图片/视频/下载链接无法带 header）
    if !ok && matches!(request.method(), &Method::GET | &Method::HEAD) {
        if let Some(q) = request.uri().query() {
            if let Some(candidate) = query_token(q) {
                ok = ct_eq(candidate.as_bytes(), token.as_bytes());
            }
        }
    }

    if ok {
        next.run(request).await
    } else {
        unauthorized(&state, &path).await
    }
}

/// 统一的 401 响应：i18n 文案 + RFC 6750 挑战头 + 禁止缓存。
async fn unauthorized(state: &Arc<AppState>, path: &str) -> Response {
    tracing::debug!(path, "access protection: rejected unauthenticated request");
    let (status, body) = err_response(state, StatusCode::UNAUTHORIZED, UNAUTHORIZED_KEY, &[]).await;
    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"entrypoint\""),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;

    const TOKEN: &str = "s3cret-token";

    fn test_state(mode: AccessProtection, token: &str) -> Arc<AppState> {
        let mut config = AppConfig::default();
        config.general.language = "zh-CN".into();
        config.server.access_protection = mode;
        config.server.token = token.into();
        Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-auth-test-{}", std::process::id())),
            config,
            vec![],
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    fn test_app(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/tasks",
                get(|| async { "tasks" })
                    .post(|| async { "created" })
                    .options(|| async { "preflight" }),
            )
            .route("/api/v1/capabilities", get(|| async { "v1" }))
            .route("/ws", get(|| async { "ws" }))
            .route("/", get(|| async { "static" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_access,
            ))
            .with_state(state)
    }

    async fn call(
        app: Router,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = app
            .oneshot(builder.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn none_mode_passes_without_credentials() {
        let app = test_app(test_state(AccessProtection::None, ""));
        let (status, body) = call(app, "GET", "/api/tasks", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "tasks");
    }

    #[tokio::test]
    async fn token_mode_without_credentials_is_401() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, body) = call(app, "GET", "/api/tasks", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("访问令牌"), "body should be localized: {body}");
    }

    #[tokio::test]
    async fn token_mode_with_bearer_passes() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, body) = call(
            app,
            "GET",
            "/api/tasks",
            &[("authorization", "Bearer s3cret-token")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "tasks");
    }

    #[tokio::test]
    async fn token_mode_with_x_api_key_passes() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, _) = call(app, "GET", "/api/tasks", &[("x-api-key", TOKEN)]).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn v1_path_passes_without_credentials() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, body) = call(app, "GET", "/api/v1/capabilities", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "v1");
    }

    #[tokio::test]
    async fn static_path_passes_without_credentials() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, body) = call(app, "GET", "/", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "static");
    }

    #[tokio::test]
    async fn ws_query_token_passes() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, body) = call(app, "GET", "/ws?token=s3cret-token", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ws");
    }

    #[tokio::test]
    async fn get_query_token_is_percent_decoded() {
        let app = test_app(test_state(AccessProtection::Token, "a b"));
        let (status, _) = call(app, "GET", "/api/tasks?token=a%20b", &[]).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn post_query_token_is_still_401() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, _) = call(app, "POST", "/api/tasks?token=s3cret-token", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_mode_with_empty_token_fails_closed() {
        let app = test_app(test_state(AccessProtection::Token, ""));
        let (status, _) = call(app, "GET", "/api/tasks", &[("authorization", "Bearer ")]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_token_is_401() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, _) = call(
            app,
            "GET",
            "/api/tasks",
            &[("authorization", "Bearer nope")],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// handoff T-06 验收：WS 无凭据必须拒绝握手。
    #[tokio::test]
    async fn ws_without_token_is_401() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, body) = call(app, "GET", "/ws", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("访问令牌"), "body should be localized: {body}");
    }

    /// 浏览器原生加载（HEAD 探测）也应接受 ?token=。
    #[tokio::test]
    async fn head_query_token_passes() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, _) = call(app, "HEAD", "/api/tasks?token=s3cret-token", &[]).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// CORS 预检不带凭据：中间件放行，交给 CORS 层响应。
    #[tokio::test]
    async fn options_preflight_passes_without_credentials() {
        let app = test_app(test_state(AccessProtection::Token, TOKEN));
        let (status, body) = call(
            app,
            "OPTIONS",
            "/api/tasks",
            &[
                ("origin", "http://localhost:5173"),
                ("access-control-request-method", "GET"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "preflight");
    }
}
