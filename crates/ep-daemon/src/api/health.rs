use std::sync::Arc;

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/health", get(health_check))
}

pub async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;

    use crate::state::AppState;

    // 经路由树访问 /health（而非直调 handler）：200 + status/version 形状，
    // version 必须等于本 crate 编译期版本（守护探针接线漂移）。
    #[tokio::test]
    async fn health_route_returns_ok_with_crate_version() {
        let state = Arc::new(AppState::new(
            std::env::temp_dir().join(format!("ep-api-health-test-{}", std::process::id())),
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ));
        let app = super::router().with_state(state);
        let req = Request::builder().uri("/health").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    }
}
