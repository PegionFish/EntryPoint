//! 管线管理 API（列表/读取/保存/删除/状态）
//!
//! ⚠ 文件所有权：Wave 2 代理 W2-B。POST /api/pipelines/execute 在 api/execute.rs。
//!
//! ## 文件布局与命名策略
//! - 管线 TOML 一律存放在 `{root}/config/pipelines/*.toml`。
//! - **读取**（GET list / GET :id / DELETE）：扫描目录并逐个 load_spec，
//!   以 spec 内的 `[pipeline].id` 匹配——文件名与 id 可能不同
//!   （如 `audio_extract.toml` 的 id 是 `audio-extract`），绝不假设文件名。
//! - **写入**（PUT）：统一落盘为 `{id 的下划线形式}.toml`
//!   （连字符 → 下划线，如 `my-pipe` → `my_pipe.toml`）。这与发行版内置文件
//!   命名一致；PUT 前会清理同一 id 的其他旧文件，保证扫描结果唯一。
//!
//! ## pipeline_bridge 模块声明
//! ep-daemon 为纯 bin crate（无 lib.rs），main.rs 非本代理所有，
//! 故在此用 `#[path]` 将 `src/pipeline_bridge.rs` 声明为本模块的子模块。
//! W2-D（execute.rs）请经 `crate::api::pipelines::pipeline_bridge` 访问。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::{delete, get, put},
    Json,
};
use serde_json::{Value, json};

use ep_core::types::TaskStatus;

use crate::api::err_response;
use crate::state::AppState;

#[path = "../pipeline_bridge.rs"]
pub mod pipeline_bridge;

use pipeline_bridge::PipelineSpec;

/// 内置示例管线 id 集合：随发行版预置在 config/pipelines/ 下
/// （audio_extract.toml / video_to_srt.toml）。允许覆盖保存，但不可删除。
const BUILTIN_PIPELINE_IDS: &[&str] = &["audio-extract", "video-to-srt"];

fn is_builtin(id: &str) -> bool {
    BUILTIN_PIPELINE_IDS.contains(&id)
}

/// 管线 id 命名规则：`^[a-z0-9][a-z0-9-]*$`（小写字母/数字/连字符）
fn is_valid_pipeline_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn pipelines_dir(state: &AppState) -> PathBuf {
    state.root.join("config").join("pipelines")
}

/// 扫描目录下所有 *.toml 并加载为 spec；损坏文件跳过并 warn。
/// 按文件名排序，保证列表顺序稳定。
fn scan_specs(dir: &Path) -> Vec<(PathBuf, PipelineSpec)> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return result; // 目录不存在 → 视为空
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .collect();
    paths.sort();

    for path in paths {
        match pipeline_bridge::load_spec(&path) {
            Ok(spec) => result.push((path, spec)),
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "pipeline file corrupted, skipping");
            }
        }
    }
    result
}

/// 按 spec id 查找管线文件（扫描匹配，不假设文件名）
fn find_spec_file(dir: &Path, id: &str) -> Option<(PathBuf, PipelineSpec)> {
    scan_specs(dir)
        .into_iter()
        .find(|(_, spec)| spec.pipeline.id == id)
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/pipelines", get(list_pipelines))
        .route("/pipelines/{id}", get(get_pipeline))
        .route("/pipelines/{id}", put(update_pipeline))
        .route("/pipelines/{id}", delete(delete_pipeline))
        .route("/pipelines/{id}/status", get(pipeline_status))
}

/// GET /api/pipelines — 管线列表（id/name/description/source）
async fn list_pipelines(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let dir = pipelines_dir(&state);
    let list: Vec<Value> = scan_specs(&dir)
        .into_iter()
        .map(|(_, spec)| {
            json!({
                "id": spec.pipeline.id,
                "name": spec.pipeline.name,
                "description": spec.pipeline.description,
                "source": if is_builtin(&spec.pipeline.id) { "builtin" } else { "custom" },
            })
        })
        .collect();
    (StatusCode::OK, Json(Value::Array(list)))
}

/// GET /api/pipelines/:id — 完整 spec JSON
async fn get_pipeline(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let dir = pipelines_dir(&state);
    match find_spec_file(&dir, &id) {
        Some((_, spec)) => (
            StatusCode::OK,
            Json(serde_json::to_value(&spec).unwrap_or_else(|_| json!({}))),
        ),
        None => {
            err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiPipelines.pipelines.notFound",
                &[],
            )
            .await
        }
    }
}

/// PUT /api/pipelines/:id — 保存 spec（body 为完整 spec JSON）
///
/// 以 String 提取 body 自行解析，保证解析失败时返回本地化 JSON 错误。
async fn update_pipeline(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    body: String,
) -> (StatusCode, Json<Value>) {
    if !is_valid_pipeline_id(&id) {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiPipelines.pipelines.invalidId",
            &[],
        )
        .await;
    }

    let raw: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        // serde 解析消息为英文技术细节，经 {{detail}} 透传
        Err(e) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.pipelines.specNotJson",
                &[("detail", e.to_string())],
            )
            .await
        }
    };
    let spec: PipelineSpec = match serde_json::from_value(raw) {
        Ok(s) => s,
        Err(e) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.pipelines.specMalformed",
                &[("detail", e.to_string())],
            )
            .await
        }
    };

    if spec.pipeline.id != id {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiPipelines.pipelines.idMismatch",
            &[
                ("specId", spec.pipeline.id.clone()),
                ("pathId", id.clone()),
            ],
        )
        .await;
    }
    // 用执行层视角做一次完整校验（spec_to_pipeline 内含全部结构校验）；
    // bridge 的 anyhow 消息为英文技术细节，经 {{detail}} 透传
    if let Err(e) = pipeline_bridge::spec_to_pipeline(&spec) {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiPipelines.specInvalid",
            &[("detail", e.to_string())],
        )
        .await;
    }

    let dir = pipelines_dir(&state);
    // 命名策略：统一下划线文件名（连字符 → 下划线）
    let target = dir.join(format!("{}.toml", id.replace('-', "_")));

    // 清理同一 id 的旧文件（文件名可能与规范名不同），避免重复定义
    for (path, existing) in scan_specs(&dir) {
        if existing.pipeline.id == id && path != target {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(file = %path.display(), error = %e, "failed to clean up stale pipeline file");
            }
        }
    }

    match pipeline_bridge::save_spec(&spec, &target) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        // bridge/fs 错误为英文技术细节，经 {{detail}} 透传
        Err(e) => {
            err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiPipelines.pipelines.saveFailed",
                &[("detail", e.to_string())],
            )
            .await
        }
    }
}

/// DELETE /api/pipelines/:id — 删除管线文件；内置管线拒绝（403）
async fn delete_pipeline(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    if is_builtin(&id) {
        return err_response(
            &state,
            StatusCode::FORBIDDEN,
            "apiPipelines.pipelines.builtinReadOnly",
            &[],
        )
        .await;
    }

    let dir = pipelines_dir(&state);
    match find_spec_file(&dir, &id) {
        None => {
            err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiPipelines.pipelines.notFound",
                &[],
            )
            .await
        }
        Some((path, _)) => match std::fs::remove_file(&path) {
            Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
            Err(e) => {
                err_response(
                    &state,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiPipelines.pipelines.deleteFailed",
                    &[("detail", e.to_string())],
                )
                .await
            }
        },
    }
}

/// GET /api/pipelines/:id/status — 兼容端点：查 runner 任务表的最新状态
async fn pipeline_status(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Json<Value> {
    let status = {
        let runner = state.runner.lock().await;
        let mut matched: Vec<_> = runner
            .list_tasks()
            .into_iter()
            .filter(|t| t.id == id || t.pipeline_name == id)
            .collect();
        // started_at 为 ISO 8601 字符串，字典序即时间序；取最新一条
        matched.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        matched.pop().map(|t| task_status_str(&t.status))
    };
    match status {
        Some(s) => Json(json!({ "status": s })),
        None => Json(json!({ "status": "unknown" })),
    }
}

fn task_status_str(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed(_) => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// tempdir root 的测试 AppState
    fn test_state() -> Arc<AppState> {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-pipelines-api-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    fn sample_spec_json(id: &str) -> String {
        format!(
            r#"{{
  "pipeline": {{"id": "{id}", "name": "测试", "description": "desc"}},
  "nodes": [
    {{"id": "input", "label": "输入", "kind": "builtin", "builtin": "file_input", "params": {{}}}},
    {{"id": "out", "label": "输出", "kind": "builtin", "builtin": "file_output", "params": {{"extension": "txt"}}, "position": {{"x": 1.5, "y": 2}}}}
  ],
  "edges": [{{"from": ["input", "output"], "to": ["out", "input"]}}]
}}"#
        )
    }

    // ── id 校验 ─────────────────────────────────────────────────────────────

    #[test]
    fn test_pipeline_id_validation() {
        assert!(is_valid_pipeline_id("a"));
        assert!(is_valid_pipeline_id("abc-123"));
        assert!(is_valid_pipeline_id("0pipe"));
        assert!(!is_valid_pipeline_id(""));
        assert!(!is_valid_pipeline_id("-abc"));
        assert!(!is_valid_pipeline_id("Abc"));
        assert!(!is_valid_pipeline_id("a_b"));
        assert!(!is_valid_pipeline_id("a b"));
        assert!(!is_valid_pipeline_id("中文"));
    }

    #[tokio::test]
    async fn test_put_invalid_id_returns_400() {
        let state = test_state();
        let (status, body) = update_pipeline(
            State(state),
            AxumPath("Bad_ID".to_string()),
            sample_spec_json("Bad_ID"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.0["error"].as_str().unwrap().contains("格式非法"));
    }

    // ── PUT 落盘后 GET 可读 ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_put_then_get_roundtrip() {
        let state = test_state();

        let (status, body) = update_pipeline(
            State(state.clone()),
            AxumPath("my-pipe".to_string()),
            sample_spec_json("my-pipe"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.0["ok"], true);

        // 命名策略：连字符 → 下划线落盘
        let file = state.root.join("config/pipelines/my_pipe.toml");
        assert!(file.exists(), "应落盘为下划线文件名");

        // GET 回读（扫描匹配 spec id，而非文件名）
        let (status, body) =
            get_pipeline(State(state.clone()), AxumPath("my-pipe".to_string())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.0["pipeline"]["id"], "my-pipe");
        assert_eq!(body.0["nodes"][1]["params"]["extension"], "txt");
        assert_eq!(body.0["nodes"][1]["position"]["x"], 1.5);
        assert_eq!(body.0["edges"][0]["from"], json!(["input", "output"]));

        // 列表中 source = custom
        let (_, list) = list_pipelines(State(state)).await;
        let items = list.0.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], "my-pipe");
        assert_eq!(items[0]["source"], "custom");
    }

    #[tokio::test]
    async fn test_put_builtin_overwrite_allowed() {
        let state = test_state();
        // 内置 id 允许覆盖保存（用户可改示例）
        let (status, _) = update_pipeline(
            State(state.clone()),
            AxumPath("audio-extract".to_string()),
            sample_spec_json("audio-extract"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(state
            .root
            .join("config/pipelines/audio_extract.toml")
            .exists());
    }

    #[tokio::test]
    async fn test_put_validation_errors() {
        let state = test_state();

        // body 非 JSON
        let (status, body) = update_pipeline(
            State(state.clone()),
            AxumPath("p1".to_string()),
            "not json".to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.0["error"].as_str().unwrap().contains("不是合法 JSON"));

        // pipeline.id 与路径不一致
        let (status, body) = update_pipeline(
            State(state.clone()),
            AxumPath("p2".to_string()),
            sample_spec_json("other-id"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.0["error"].as_str().unwrap().contains("不一致"));

        // 结构错误：空节点列表 → 本地化前缀 + 英文 bridge 技术细节（{{detail}}）
        let (status, body) = update_pipeline(
            State(state.clone()),
            AxumPath("p3".to_string()),
            r#"{"pipeline": {"id": "p3", "name": "x"}, "nodes": []}"#.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = body.0["error"].as_str().unwrap();
        assert!(msg.contains("管线 spec 结构无效"), "got: {msg}");
        assert!(msg.contains("at least one node"), "got: {msg}");
    }

    // ── DELETE ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_delete_builtin_forbidden() {
        let state = test_state();
        let (status, body) = delete_pipeline(
            State(state),
            AxumPath("audio-extract".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body.0["error"], "内置管线不可删除");
    }

    #[tokio::test]
    async fn test_delete_custom_then_get_404() {
        let state = test_state();
        let _ = update_pipeline(
            State(state.clone()),
            AxumPath("temp-pipe".to_string()),
            sample_spec_json("temp-pipe"),
        )
        .await;

        let (status, body) = delete_pipeline(
            State(state.clone()),
            AxumPath("temp-pipe".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.0["ok"], true);

        let (status, _) =
            get_pipeline(State(state), AxumPath("temp-pipe".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_and_delete_missing_returns_404() {
        let state = test_state();
        let (status, body) =
            get_pipeline(State(state.clone()), AxumPath("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "管线不存在");

        let (status, _) =
            delete_pipeline(State(state), AxumPath("ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ── 列表：损坏文件跳过 + builtin 判定 ───────────────────────────────────

    #[tokio::test]
    async fn test_list_skips_corrupt_and_marks_builtin() {
        let state = test_state();
        let dir = pipelines_dir(&state);
        std::fs::create_dir_all(&dir).unwrap();

        // builtin id 的文件（文件名故意与 id 不同，验证扫描匹配）
        std::fs::write(
            dir.join("weird_name.toml"),
            r#"
[pipeline]
id = "video-to-srt"
name = "视频转字幕"
description = "d"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
"#,
        )
        .unwrap();
        // 损坏文件
        std::fs::write(dir.join("broken.toml"), "this is [[ not toml").unwrap();
        // custom
        let _ = update_pipeline(
            State(state.clone()),
            AxumPath("my-pipe".to_string()),
            sample_spec_json("my-pipe"),
        )
        .await;

        let (status, list) = list_pipelines(State(state)).await;
        assert_eq!(status, StatusCode::OK);
        let items = list.0.as_array().unwrap();
        assert_eq!(items.len(), 2, "损坏文件应被跳过");
        let builtin = items.iter().find(|v| v["id"] == "video-to-srt").unwrap();
        assert_eq!(builtin["source"], "builtin");
        let custom = items.iter().find(|v| v["id"] == "my-pipe").unwrap();
        assert_eq!(custom["source"], "custom");
    }

    // ── status 兼容端点 ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_status_unknown_when_no_tasks() {
        let state = test_state();
        let body =
            pipeline_status(State(state), AxumPath("any-pipe".to_string())).await;
        assert_eq!(body.0["status"], "unknown");
    }
}
