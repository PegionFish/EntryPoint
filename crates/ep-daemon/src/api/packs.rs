//! 整合包（Pack）管理 API — Wave S 骨架（S1 预注册路由，消灭注册点冲突）。
//!
//! # 冻结契约（`docs/PACK_UNIFY_PLAN.md` §8.1，共 7 条路由）
//!
//! | 方法+路径 | 语义 |
//! |---|---|
//! | GET /api/packs | 已装包列表（注册表） |
//! | POST /api/packs/import | `{source:"local",path}` \| `{source:"url",url}` → 202 `{pack_id}`，进度走 WS |
//! | POST /api/packs/upload | multipart `.epzip` → 202 同上 |
//! | GET /api/packs/{id} | 详情（内容清单/适配报告） |
//! | DELETE /api/packs/{id} | `?keep_models=true` 卸载（模型可选保留） |
//! | POST /api/packs/build | 圈选模型+管线 → 202 → 构建完成可下载 |
//! | GET /api/packs/{id}/export | `.epzip` 流式下载 |
//!
//! # 实现所有权
//!
//! 本文件整体由 Wave 2 **B2 (DaemonPacks)** 接管填实现（含注册表与
//! WS `pack_import` 进度事件，见 state.rs）；编排核心在 ep-pack crate
//! （A3/A4/B1 的模块）。当前 handler 一律返回 **501 + i18n
//! `common.tip.comingSoon`** 占位错误，不解析请求体——请求体类型化待
//! A3/B1 的 ep-pack 类型就位后由 B2 引入。路由路径与 handler 命名冻结，
//! 函数签名供 B2 原位替换。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use crate::api::err_response;
use crate::state::AppState;

/// DELETE /api/packs/{id} 查询参数：`?keep_models=true` → 卸载时保留模型文件。
#[derive(Debug, Clone, Deserialize)]
pub struct DeletePackQuery {
    #[serde(default)]
    pub keep_models: bool,
}

/// `/api/packs/*` 路由表（挂载于 [`crate::api::api_router`]）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/packs", get(list_packs))
        .route("/packs/import", post(import_pack))
        .route("/packs/upload", post(upload_pack))
        .route("/packs/build", post(build_pack))
        .route("/packs/{id}", get(get_pack).delete(delete_pack))
        .route("/packs/{id}/export", get(export_pack))
}

/// 501 占位响应（i18n：`common.tip.comingSoon`，zh-CN 默认"功能即将上线"）。
async fn not_implemented(state: &Arc<AppState>) -> (StatusCode, Json<Value>) {
    err_response(
        state,
        StatusCode::NOT_IMPLEMENTED,
        "common.tip.comingSoon",
        &[],
    )
    .await
}

/// GET /api/packs — 已装包列表（注册表）。B2 实现。
pub async fn list_packs(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    not_implemented(&state).await
}

/// POST /api/packs/import — 从本地路径 / URL 导入整合包。B2 实现
/// （请求体类型化待 A3/B1 的 ep-pack 类型就位；当前仅占位接收 JSON）。
pub async fn import_pack(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let _ = req;
    not_implemented(&state).await
}

/// POST /api/packs/upload — multipart `.epzip` 上传导入。B2 实现
/// （届时引入 `Multipart` 提取器；骨架阶段不消费请求体）。
pub async fn upload_pack(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    not_implemented(&state).await
}

/// GET /api/packs/{id} — 包详情（内容清单/适配报告）。B2 实现。
pub async fn get_pack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let _ = id;
    not_implemented(&state).await
}

/// DELETE /api/packs/{id} — 卸载整合包（`keep_models` 控制是否保留模型）。B2 实现。
pub async fn delete_pack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<DeletePackQuery>,
) -> (StatusCode, Json<Value>) {
    let _ = (id, query.keep_models);
    not_implemented(&state).await
}

/// POST /api/packs/build — 按 tag/逐模型圈选 + 管线列表构建整合包。B2 实现
/// （请求体契约 §8.1：models / pipelines / bundle / tags）。
pub async fn build_pack(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let _ = req;
    not_implemented(&state).await
}

/// GET /api/packs/{id}/export — `.epzip` 流式下载。B2 实现
/// （届时返回类型改为流式响应）。
pub async fn export_pack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let _ = id;
    not_implemented(&state).await
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn test_state() -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-packs-api-test-{}-{seq}",
            std::process::id()
        ));
        Arc::new(AppState::new(
            root,
            ep_core::config::AppConfig::default(),
            vec![],
            vec![],
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    /// 挂载完整 /api 路由树（同时验证 packs 与其他模块路由无冲突）
    fn app(state: Arc<AppState>) -> Router {
        crate::api::api_router().with_state(state)
    }

    fn get_request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn json_request(method: Method, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    /// 断言响应为 501 + i18n 占位错误（zh-CN 默认文案）
    async fn assert_501_coming_soon(resp: axum::response::Response) {
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["error"], "功能即将上线",
            "501 stub 应走 i18n common.tip.comingSoon"
        );
    }

    // §8.1 冻结的 7 条路由全部预注册：命中即 501 占位（非 404 漏注册）
    #[tokio::test]
    async fn all_seven_pack_routes_registered_as_501() {
        let app = app(test_state());

        // 1. GET /api/packs
        assert_501_coming_soon(app.clone().oneshot(get_request("/packs")).await.unwrap()).await;

        // 2. POST /api/packs/import（契约请求体形状之一）
        assert_501_coming_soon(
            app.clone()
                .oneshot(json_request(
                    Method::POST,
                    "/packs/import",
                    json!({ "source": "local", "path": "subtitle-kit-1.0.0.epzip" }),
                ))
                .await
                .unwrap(),
        )
        .await;

        // 3. POST /api/packs/upload（骨架不消费 multipart 体）
        assert_501_coming_soon(
            app.clone()
                .oneshot(json_request(Method::POST, "/packs/upload", json!({})))
                .await
                .unwrap(),
        )
        .await;

        // 4. GET /api/packs/{id}
        assert_501_coming_soon(
            app.clone()
                .oneshot(get_request("/packs/pigeonfish.subtitle-kit"))
                .await
                .unwrap(),
        )
        .await;

        // 5. DELETE /api/packs/{id}?keep_models=true（Query 提取器可解析）
        assert_501_coming_soon(
            app.clone()
                .oneshot(get_request_with(
                    Method::DELETE,
                    "/packs/pigeonfish.subtitle-kit?keep_models=true",
                ))
                .await
                .unwrap(),
        )
        .await;

        // 6. POST /api/packs/build（契约请求体形状）
        assert_501_coming_soon(
            app.clone()
                .oneshot(json_request(
                    Method::POST,
                    "/packs/build",
                    json!({
                        "models": ["ep.systran.faster-whisper@large-v3"],
                        "pipelines": ["video-to-srt"],
                        "bundle": ["ep.systran.faster-whisper"],
                        "tags": ["字幕"]
                    }),
                ))
                .await
                .unwrap(),
        )
        .await;

        // 7. GET /api/packs/{id}/export
        assert_501_coming_soon(
            app.clone()
                .oneshot(get_request("/packs/pigeonfish.subtitle-kit/export"))
                .await
                .unwrap(),
        )
        .await;
    }

    fn get_request_with(method: Method, uri: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    // 未注册的 packs 子路径 → 落入 api 统一 404（而非 501）
    #[tokio::test]
    async fn unknown_pack_subroute_falls_to_api_404() {
        let app = app(test_state());
        let resp = app
            .oneshot(get_request("/packs/some-id/no-such-action"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
