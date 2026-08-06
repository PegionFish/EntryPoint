pub mod all;
pub mod logs;
pub mod progress;

use std::sync::Arc;

use axum::Router;

use crate::state::AppState;

pub fn ws_router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(all::router())
        .merge(logs::router())
        .merge(progress::router())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;

    use crate::state::AppState;

    // 三条 WS 路由均已注册：非升级请求会被 WebSocketUpgrade 提取器拒绝，
    // 但绝不应落入 404（防路由树重构时静默丢路由；main.rs 仅冒烟 /ws）。
    #[tokio::test]
    async fn ws_routes_registered_reject_non_upgrade_not_404() {
        let state = Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-ws-route-test-{}", std::process::id())),
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ));
        for uri in ["/ws", "/ws/logs", "/ws/progress"] {
            let app = super::ws_router().with_state(state.clone());
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_ne!(resp.status(), StatusCode::NOT_FOUND, "路由 {uri} 应已注册");
        }
    }
}
