//! 统一事件日志查询 API（PLAN_TRIGGER_UNIFIED_LOG §5.3/§5.7）。
//!
//! `GET /api/events?rule=<id>&type=<事件类型>&limit=<N>` —— 倒序
//! （最新在前，从最新月份文件向前）返回统一事件日志条目。
//!
//! - `rule`：按事件 `rule` 字段精确过滤（watcher 规则 id）
//! - `type`：按事件类型过滤（`watcher_trigger` / `task_terminal`）
//! - `limit`：默认 100，钳制 1..=1000
//!
//! 响应形状：`{"events": [ ... ]}`；事件为单行 JSON 对象（公共字段
//! `ts`/`type`，形状见 [`crate::eventlog`] 文档）。查询无业务错误路径，
//! 参数反序列化失败由 axum `Query` 提取器统一回 400（与既有 api 文件一致）。

use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State},
    routing::get,
    Json,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/events", get(list_events))
}

/// GET /api/events 查询参数（§5.3）
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// watcher 规则 id 过滤（空 = 不过滤）
    #[serde(default)]
    pub rule: Option<String>,
    /// 事件类型过滤（`type` 为 Rust 关键字，字段名改写）
    #[serde(default, rename = "type")]
    pub event_type: Option<String>,
    /// 截断条数（默认 100，钳制 1..=1000）
    #[serde(default)]
    pub limit: Option<usize>,
}

/// 默认返回条数（§5.3：limit 缺省 100）
const DEFAULT_LIMIT: usize = 100;
/// limit 钳制上限
const MAX_LIMIT: usize = 1000;

async fn list_events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsQuery>,
) -> Json<Value> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let events = crate::eventlog::read_events(
        &state.root,
        query.rule.as_deref(),
        query.event_type.as_deref(),
        limit,
    );
    Json(json!({ "events": events }))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> std::path::PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-events-api-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_state(root: std::path::PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    fn get_request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    async fn response_json(
        resp: axum::response::Response,
    ) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("响应不是合法 JSON: {e}; body={bytes:?}"));
        (status, json)
    }

    /// 预置事件：2 条 watcher_trigger（r1/r2）+ 1 条 task_terminal
    fn seed_events(root: &std::path::Path) {
        use chrono::TimeZone;
        let ts = chrono::Local
            .with_ymd_and_hms(2026, 8, 15, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp();
        crate::eventlog::append_event(
            root,
            &json!({
                "ts": ts,
                "type": "watcher_trigger",
                "rule": "r1",
                "file": "/a.mkv",
                "status": "submitted",
            }),
        );
        crate::eventlog::append_event(
            root,
            &json!({
                "ts": ts + 1,
                "type": "watcher_trigger",
                "rule": "r2",
                "file": "/b.mkv",
                "status": "archive_done",
            }),
        );
        crate::eventlog::write_task_terminal(root, "t1", "p1", "completed", None);
    }

    // 无参数：默认 limit=100，倒序返回全部事件
    #[tokio::test]
    async fn list_events_default_returns_all_newest_first() {
        let root = unique_root("default");
        seed_events(&root);
        let state = test_state(root.clone());

        let resp = super::router()
            .with_state(state)
            .oneshot(get_request("/events"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 3);
        // 倒序：最新（task_terminal t1）在前
        assert_eq!(events[0]["type"], "task_terminal");
        assert_eq!(events[0]["task_id"], "t1");
        assert_eq!(events[2]["rule"], "r1");
        // 事件形状：公共字段 ts/type
        assert!(events[0].get("ts").and_then(Value::as_i64).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    // rule 过滤
    #[tokio::test]
    async fn list_events_filters_by_rule() {
        let root = unique_root("rule");
        seed_events(&root);
        let state = test_state(root.clone());

        let resp = super::router()
            .with_state(state)
            .oneshot(get_request("/events?rule=r1"))
            .await
            .unwrap();
        let (_, body) = response_json(resp).await;
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["rule"], "r1");
        let _ = std::fs::remove_dir_all(&root);
    }

    // type 过滤
    #[tokio::test]
    async fn list_events_filters_by_type() {
        let root = unique_root("type");
        seed_events(&root);
        let state = test_state(root.clone());

        let resp = super::router()
            .with_state(state)
            .oneshot(get_request("/events?type=task_terminal"))
            .await
            .unwrap();
        let (_, body) = response_json(resp).await;
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "task_terminal");
        let _ = std::fs::remove_dir_all(&root);
    }

    // rule + type 组合过滤无命中 → 空 events 数组
    #[tokio::test]
    async fn list_events_combined_filter_no_match() {
        let root = unique_root("combo");
        seed_events(&root);
        let state = test_state(root.clone());

        let resp = super::router()
            .with_state(state)
            .oneshot(get_request("/events?rule=r1&type=task_terminal"))
            .await
            .unwrap();
        let (_, body) = response_json(resp).await;
        assert_eq!(body["events"], json!([]));
        let _ = std::fs::remove_dir_all(&root);
    }

    // limit 截断 + 钳制（0 → 1；1000 上限不炸）
    #[tokio::test]
    async fn list_events_limit_clamped_and_truncates() {
        let root = unique_root("limit");
        seed_events(&root);
        let state = test_state(root.clone());

        // limit=2 → 最新 2 条
        let resp = super::router()
            .with_state(state.clone())
            .oneshot(get_request("/events?limit=2"))
            .await
            .unwrap();
        let (_, body) = response_json(resp).await;
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "task_terminal");

        // limit=0 → 钳制为 1
        let resp = super::router()
            .with_state(state.clone())
            .oneshot(get_request("/events?limit=0"))
            .await
            .unwrap();
        let (_, body) = response_json(resp).await;
        assert_eq!(body["events"].as_array().unwrap().len(), 1);

        // limit=99999 → 钳制为 1000，正常返回全部 3 条
        let resp = super::router()
            .with_state(state)
            .oneshot(get_request("/events?limit=99999"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["events"].as_array().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    // 事件目录为空 / 不存在 → 200 + 空数组
    #[tokio::test]
    async fn list_events_empty_logs_returns_empty_array() {
        let root = unique_root("empty");
        let state = test_state(root.clone());
        let resp = super::router()
            .with_state(state)
            .oneshot(get_request("/events"))
            .await
            .unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["events"], json!([]));
        let _ = std::fs::remove_dir_all(&root);
    }

    // 非法 limit（非数字）→ axum Query 提取器统一 400
    #[tokio::test]
    async fn list_events_invalid_limit_rejected() {
        let root = unique_root("badlimit");
        let state = test_state(root.clone());
        let resp = super::router()
            .with_state(state)
            .oneshot(get_request("/events?limit=abc"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&root);
    }

    // 挂载完整 /api 路由树验证路由无冲突（events 与既有模块并存）
    #[tokio::test]
    async fn events_route_mounted_in_full_api_router() {
        let root = unique_root("mount");
        seed_events(&root);
        let state = test_state(root.clone());
        let app = crate::api::api_router(state.clone()).with_state(state);

        let resp = app.oneshot(get_request("/events?limit=1")).await.unwrap();
        let (status, body) = response_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["events"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
