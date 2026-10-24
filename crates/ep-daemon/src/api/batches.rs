//! v1 批量接入端点 — 文件夹批量下发 + 任务组（外部应用接入模式）
//!
//! 面向把本地 AI 能力接入自身工具链的外部应用（docs/API_INTEGRATION_GUIDE.md）：
//! 应用传入一个**服务器侧文件夹**，EP 自动枚举文件、逐文件提交指定
//! 模块能力，并以 `group_id` 聚成一组——
//!
//! - `POST /v1/batches`                      — 文件夹批量提交（202 + group_id + task_ids）
//! - `GET /v1/groups`                        — 组列表（滚动状态 + 计数）
//! - `GET /v1/groups/{group_id}`             — 组详情（成员任务逐条状态）
//! - `POST /v1/groups/{group_id}/cancel`     — 组取消（非终态成员逐一受理）
//!
//! ## 聚合与寻址分离（契约关键）
//!
//! 任务中心（WebUI）把同组任务**折叠为一条**聚合展示（部分失败语义
//! `partial`，见 `ep_core::task_registry::rollup_group`）；但注册表保持
//! 逐条扁平——接入方对每个文件仍持独立 `task_id`，轮询/取消走既有
//! `GET /v1/tasks/{task_id}` 逐条契约，`/v1/tasks` 列表也不做折叠。
//!
//! ## folder 准入策略（与 input_path 同一信任域）
//!
//! 缺省仅接受 `workspace/uploads` 内的目录（canonicalize 后前缀校验，
//! 杜绝越权读取服务器任意目录）；`[api] batches_allow_external_folders =
//! true` 放开为任意可读目录（单机/可信局域网部署的便捷开关）。
//!
//! ## 组元数据
//!
//! 提交时落盘 `{registry 持久化目录}/groups/{group_id}.json`
//! （module/capability/folder 展示信息）；成员记录各自持久化 `group`
//! 字段，重启后聚合照常可用。元数据文件缺失时展示字段从成员记录
//! 降级推导（pipeline_id `direct/{module_id}` 前缀）。

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json,
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use ep_core::task_registry::{GroupRollup, TaskRecord, rollup_group};

use crate::api::autostart;
use crate::api::execute::execution::{self, SubmitOptions};
use crate::api::inference::{
    autostart_error_response, input_error, map_param_error, param_error,
    resolve_capability, submit_error_response, v1_error,
};
use crate::api::upload::input_uploads_dir;
use crate::state::AppState;

/// 单批文件数硬上限（防御性；正常批量应远小于此）
const MAX_BATCH_FILES: usize = 512;

/// 组元数据（`groups/{group_id}.json`；展示信息，非权威数据——
/// 权威成员关系在成员记录的 `group` 字段）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupMeta {
    group_id: String,
    module_id: String,
    capability: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    created_at: String,
    /// 受理提交的文件数（= 初始 submitted）
    requested: usize,
}

/// 批量提交请求（JSON 体）
#[derive(Debug, Deserialize)]
struct BatchRequest {
    module_id: String,
    capability: String,
    /// 服务器侧文件夹（准入范围见模块文档）
    folder: String,
    /// 应用于全部文件的能力参数（经同一 schema 校验与默认值注入）
    #[serde(default)]
    params: Value,
    /// 递归枚举子目录（缺省仅一层）
    #[serde(default)]
    recursive: bool,
    /// 文件名扩展名过滤（如 `[".png", ".jpg"]`；大小写不敏感；缺省不过滤）
    #[serde(default)]
    extensions: Vec<String>,
    /// 每个成员任务完成后的回调（透传给既有 callback 机制）
    #[serde(default)]
    callback_url: Option<String>,
}

/// 批量路由（挂载于 v1 门面 token 中间件**之内**——由 inference::router 合并）
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/batches", post(submit_batch))
        .route("/v1/groups", get(list_groups))
        .route("/v1/groups/{group_id}", get(get_group))
        .route("/v1/groups/{group_id}/cancel", post(cancel_group))
}

// ─── POST /v1/batches ───────────────────────────────────────────────────────

/// 文件夹枚举 → 逐文件提交 → 同组 task_ids
///
/// 契约： folder 不存在/不可读或无匹配文件 → 400；超出队列在途容量 →
/// 429 BATCH_QUEUE_INSUFFICIENT（整批拒绝，调用方等队列排空后重试或
/// 拆分目录）；folder 越权 → 403 FOLDER_NOT_ALLOWED。受理后逐文件独立
/// 提交，单个文件提交失败只计入 `failed_submissions`，不影响其余成员。
async fn submit_batch(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchRequest>,
) -> (StatusCode, Json<Value>) {
    // 1. 模块/capability/参数校验（纯读、无副作用，先于任何落盘）
    let cap = match resolve_capability(&state, &req.module_id, &req.capability).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let params = match crate::api::execute::validate_and_fill_params(&cap, req.params) {
        Ok(p) => p,
        Err(e) => return map_param_error(e),
    };
    if params.is_null() {
        return param_error("`params` must be a JSON object");
    }

    // 2. folder 准入（canonicalize 前缀校验，语义同 submit_json 的 input_path）
    let folder = match resolve_folder(&state, &req.folder).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    // 3. 枚举文件（确定性排序；空/全被过滤 → 400）
    let files = match enumerate_files(&folder, req.recursive, &req.extensions) {
        Ok(f) => f,
        Err(detail) => return input_error(detail),
    };
    if files.is_empty() {
        return input_error(format!(
            "folder contains no matching files: {}",
            req.folder
        ));
    }
    if files.len() > MAX_BATCH_FILES {
        return v1_error(
            StatusCode::BAD_REQUEST,
            "BATCH_TOO_LARGE",
            format!(
                "folder contains {} matching files, batch limit is {MAX_BATCH_FILES} — split the folder",
                files.len()
            ),
        );
    }

    // 4. 队列容量预检：整批须能全部入队（MAX_INFLIGHT = 排队 + 运行中）
    let inflight = execution::inflight_count();
    let available = execution::max_inflight_tasks().saturating_sub(inflight);
    if files.len() > available {
        return v1_error(
            StatusCode::TOO_MANY_REQUESTS,
            "BATCH_QUEUE_INSUFFICIENT",
            format!(
                "batch of {} files exceeds available queue capacity ({available} in-flight slots left of {}); wait for the queue to drain or split the folder",
                files.len(),
                execution::max_inflight_tasks(),
            ),
        );
    }

    // 5. 模块自动拉起（整批一次，成员提交不再重复拉起）
    if let Err(e) = autostart::ensure_module_running(&state, &req.module_id).await {
        return autostart_error_response(e);
    }

    // 6. 建组 + 元数据落盘（展示信息；落盘失败不阻断——成员记录自带 group 字段）
    let group_id = execution::new_group_id();
    let meta = GroupMeta {
        group_id: group_id.clone(),
        module_id: req.module_id.clone(),
        capability: req.capability.clone(),
        folder: Some(req.folder.clone()),
        created_at: chrono::Utc::now().to_rfc3339(),
        requested: files.len(),
    };
    if let Err(e) = store_group_meta(&meta) {
        tracing::warn!(group = %group_id, error = %e, "failed to persist group metadata (non-fatal)");
    }

    // 7. 逐文件提交（同组；单个失败不阻断其余成员）
    let options = SubmitOptions {
        wait: false,
        callback_url: req.callback_url.clone(),
        group: Some(group_id.clone()),
    };
    let mut task_ids = Vec::with_capacity(files.len());
    let mut failed_submissions: Vec<String> = Vec::new();
    for file in &files {
        match execution::submit_direct_full(
            &state,
            &req.module_id,
            &cap.name,
            params.clone(),
            file.clone(),
            None,
            options.clone(),
        )
        .await
        {
            Ok(outcome) => task_ids.push(outcome.task_id),
            Err(e) => {
                tracing::warn!(group = %group_id, file = %file.display(), error = %e, "batch member submission failed");
                failed_submissions.push(file.display().to_string());
            }
        }
    }

    let submitted = task_ids.len();
    if submitted == 0 {
        // 整批未受理（预检后队列被并发占满等竞态）：回滚组元数据，避免空组残留
        remove_group_meta(&group_id);
        return submit_error_response(
            execution::SubmitError::QueueFull(execution::max_inflight_tasks()),
        );
    }

    let mut body = json!({
        "group_id": group_id,
        "submitted": submitted,
        "skipped": failed_submissions.len(),
        "task_ids": task_ids,
    });
    if !failed_submissions.is_empty() {
        body["failed_submissions"] = json!(failed_submissions);
    }
    (StatusCode::ACCEPTED, Json(body))
}

/// folder 准入解析：存在且为目录 + 范围校验（uploads 内 / 配置放开）
async fn resolve_folder(
    state: &Arc<AppState>,
    folder: &str,
) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    let candidate = PathBuf::from(folder);
    if !candidate.is_dir() {
        return Err(input_error(format!(
            "folder does not exist or is not a directory: {folder}"
        )));
    }
    let (canon_folder, canon_uploads) = {
        let uploads = input_uploads_dir(state).await;
        // uploads 目录尚不存在时回退原始路径（canonicalize 必失败）：
        // 前缀校验自然不命中 → 走 FOLDER_NOT_ALLOWED，而非误报 400
        let canon_uploads = uploads
            .canonicalize()
            .unwrap_or_else(|_| uploads.clone());
        match candidate.canonicalize() {
            Ok(f) => (f, canon_uploads),
            _ => {
                return Err(input_error(format!(
                    "failed to resolve folder path: {folder}"
                )))
            }
        }
    };
    let allow_external = {
        let cfg = state.config.read().await;
        cfg.api.batches_allow_external_folders
    };
    if allow_external {
        return Ok(canon_folder);
    }
    if !canon_folder.starts_with(&canon_uploads) {
        return Err(v1_error(
            StatusCode::FORBIDDEN,
            "FOLDER_NOT_ALLOWED",
            format!(
                "folder must be inside workspace/uploads (rejected: {folder}); set [api] batches_allow_external_folders = true to allow any server folder"
            ),
        ));
    }
    Ok(canon_folder)
}

/// 目录枚举：一层或递归（深度上限 16 防符号链接环），扩展名过滤，
/// 按路径排序保证成员顺序确定
fn enumerate_files(
    folder: &std::path::Path,
    recursive: bool,
    extensions: &[String],
) -> Result<Vec<PathBuf>, String> {
    let ext_filter: Option<Vec<String>> = if extensions.is_empty() {
        None
    } else {
        Some(
            extensions
                .iter()
                .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
                .collect(),
        )
    };
    let mut files = Vec::new();
    fn walk(
        dir: &std::path::Path,
        recursive: bool,
        depth: usize,
        ext_filter: &Option<Vec<String>>,
        out: &mut Vec<PathBuf>,
    ) -> Result<(), String> {
        if depth > 16 {
            return Ok(()); // 深度兜底（符号链接环保护），不视为错误
        }
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("failed to read folder {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("failed to read entry: {e}"))?;
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_dir() {
                if recursive {
                    walk(&path, recursive, depth + 1, ext_filter, out)?;
                }
                continue;
            }
            if !file_type.is_file() {
                continue; // 符号链接/特殊文件跳过
            }
            if let Some(filter) = ext_filter {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if !filter.contains(&ext) {
                    continue;
                }
            }
            out.push(path);
        }
        Ok(())
    }
    walk(folder, recursive, 0, &ext_filter, &mut files)?;
    files.sort();
    Ok(files)
}

// ─── 组元数据落盘 ───────────────────────────────────────────────────────────

fn groups_dir() -> Option<PathBuf> {
    execution::persistence_dir().map(|d| d.join("groups"))
}

fn store_group_meta(meta: &GroupMeta) -> Result<(), String> {
    let Some(dir) = groups_dir() else {
        return Ok(()); // 未启用持久化（测试/内存注册表）→ 跳过
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.json", meta.group_id));
    let bytes = serde_json::to_vec(meta).map_err(|e| e.to_string())?;
    // 原子写（与注册表同口径：写 .tmp 后 rename）
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn remove_group_meta(group_id: &str) {
    if let Some(dir) = groups_dir() {
        let _ = std::fs::remove_file(dir.join(format!("{group_id}.json")));
    }
}

fn load_group_meta(group_id: &str) -> Option<GroupMeta> {
    let dir = groups_dir()?;
    let bytes = std::fs::read(dir.join(format!("{group_id}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ─── 组视图组装（列表 / 详情共用） ──────────────────────────────────────────

/// 组展示信息：元数据优先，缺失时从成员记录降级推导
pub(crate) fn group_display(members: &[TaskRecord]) -> (String, Option<String>) {
    // (module_id, capability)
    if let Some(meta) = members
        .first()
        .and_then(|m| m.group.as_deref())
        .and_then(load_group_meta)
    {
        return (meta.module_id, Some(meta.capability));
    }
    // 降级：成员 pipeline_id 形如 `direct/{module_id}`
    let module_id = members
        .first()
        .map(|m| {
            m.pipeline_id
                .strip_prefix("direct/")
                .unwrap_or(&m.pipeline_id)
                .to_string()
        })
        .unwrap_or_default();
    (module_id, None)
}

fn group_summary_view(group_id: &str, members: &[TaskRecord]) -> Value {
    let rollup @ GroupRollup { .. } = rollup_group(&members.iter().collect::<Vec<_>>());
    let (module_id, capability) = group_display(members);
    let mut view = json!({
        "group_id": group_id,
        "module_id": module_id,
        "status": rollup.status.as_str(),
        "counts": {
            "total": rollup.total,
            "queued": rollup.queued,
            "running": rollup.running,
            "completed": rollup.completed,
            "failed": rollup.failed,
            "cancelled": rollup.cancelled,
        },
    });
    if let Some(cap) = capability {
        view["capability"] = json!(cap);
    }
    view
}

// ─── GET /v1/groups ─────────────────────────────────────────────────────────

/// 组列表（新组在前；最小稳定面：滚动状态 + 计数）
async fn list_groups() -> (StatusCode, Json<Value>) {
    let views: Vec<Value> = execution::group_ids()
        .iter()
        .map(|id| {
            let members = execution::snapshot_group(id);
            group_summary_view(id, &members)
        })
        .collect();
    (StatusCode::OK, Json(json!({ "groups": views })))
}

// ─── GET /v1/groups/{group_id} ──────────────────────────────────────────────

/// 组详情：汇总 + 成员任务逐条状态（接入方逐成员核对的权威明细）
async fn get_group(Path(group_id): Path<String>) -> (StatusCode, Json<Value>) {
    let members = execution::snapshot_group(&group_id);
    if members.is_empty() {
        return v1_error(
            StatusCode::NOT_FOUND,
            "GROUP_NOT_FOUND",
            format!("task group `{group_id}` not found"),
        );
    }
    let mut view = group_summary_view(&group_id, &members);
    view["tasks"] = json!(
        members
            .iter()
            .map(|m| json!({
                "task_id": m.id,
                "status": m.status.as_str(),
            }))
            .collect::<Vec<_>>()
    );
    (StatusCode::OK, Json(view))
}

// ─── POST /v1/groups/{group_id}/cancel ──────────────────────────────────────

/// 组取消：非终态成员逐一受理（与单任务取消同一执行语义）
async fn cancel_group(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    match execution::cancel_group(&state, &group_id).await {
        execution::GroupCancelOutcome::NotFound => v1_error(
            StatusCode::NOT_FOUND,
            "GROUP_NOT_FOUND",
            format!("task group `{group_id}` not found"),
        ),
        execution::GroupCancelOutcome::Cancelled {
            cancelled,
            already_terminal,
        } => {
            // 取消后重取滚动状态（成员已在取消路径置 cancelled）
            let members = execution::snapshot_group(&group_id);
            let mut view = group_summary_view(&group_id, &members);
            view["cancelled_tasks"] = json!(cancelled);
            view["already_terminal"] = json!(already_terminal);
            (StatusCode::OK, Json(view))
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::module::manifest::ModuleManifest;
    use ep_core::port::PortManager;

    use crate::api::execute::execution::lock_for_tests;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-v1-batches-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_state(
        root: PathBuf,
        config: AppConfig,
        modules: Vec<ep_core::module::discovery::DiscoveredModule>,
    ) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            config,
            vec![],
            modules,
            PortManager::new(18000, 19000),
        ))
    }

    /// 单能力 mock 模块（校验序列须穿过 manifest/capability/params）
    fn mock_module(root: &std::path::Path) -> ep_core::module::discovery::DiscoveredModule {
        let manifest: ModuleManifest = toml::from_str(
            r#"
[module]
id = "mock-mod"
name = "Mock"
version = "0.1.0"
description = "test"
category = "asr"
genre = "test"
license = "MIT"

[runtime]
type = "python"

[compute]
backends = ["cpu"]

[interface]
type = "http"

[[interface.capabilities]]
name = "run"
description = "test"
input_type = "text"
output_type = "text"
"#,
        )
        .unwrap();
        ep_core::module::discovery::DiscoveredModule {
            manifest: Some(manifest),
            path: root.join("modules/mock-mod"),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        }
    }

    async fn post_json(uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    async fn body_of(resp: axum::response::Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("响应不是合法 JSON: {e}; body={bytes:?}"));
        (status, value)
    }

    /// uploads 内建一批文件（root 为 AppState root）
    fn seed_uploads(root: &std::path::Path, names: &[&str]) -> PathBuf {
        let uploads = root.join("workspace/uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        for name in names {
            std::fs::write(uploads.join(name), "batch-item").unwrap();
        }
        uploads
    }

    fn batch_body(uploads: &std::path::Path) -> Value {
        json!({
            "module_id": "mock-mod",
            "capability": "run",
            "folder": uploads.display().to_string(),
        })
    }

    // ── 1. folder 不存在 → 400 INPUT_INVALID（state 携带模块：
    //     capability 校验先于 folder 校验，须保证走到 folder 分支） ────────

    #[tokio::test]
    async fn missing_folder_is_400() {
        let root = unique_root("missing-folder");
        let state = test_state(root.clone(), AppConfig::default(), vec![mock_module(&root)]);
        let app = super::router().with_state(state);
        let mut body = batch_body(&root);
        body["folder"] = json!(root.join("no-such-dir").display().to_string());
        let resp = app.oneshot(post_json("/v1/batches", body).await).await.unwrap();
        let (status, body) = body_of(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "INPUT_INVALID");
    }

    // ── 2. uploads 之外 → 403 FOLDER_NOT_ALLOWED；配置放开后放行 ────────────

    #[tokio::test]
    async fn folder_outside_uploads_is_403_unless_allowed() {
        let root = unique_root("folder-scope");
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/a.txt"), "x").unwrap();
        let module = mock_module(&root);
        let state = test_state(root.clone(), AppConfig::default(), vec![module]);
        let app = super::router().with_state(state);
        let mut body = batch_body(&root);
        body["folder"] = json!(root.join("outside").display().to_string());
        let resp = app.oneshot(post_json("/v1/batches", body.clone()).await).await.unwrap();
        let (status, rejected) = body_of(resp).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(rejected["error"]["code"], "FOLDER_NOT_ALLOWED");

        // 放开开关后同一 folder 准入：通过 folder 校验，后续因模块未运行
        // 在 autostart 处失败（502 MODULE_START_FAILED，而非 403）
        let mut config = AppConfig::default();
        config.api.batches_allow_external_folders = true;
        let module = mock_module(&root);
        let state = test_state(root, config, vec![module]);
        let app = super::router().with_state(state);
        let resp = app.oneshot(post_json("/v1/batches", body).await).await.unwrap();
        let (status, body) = body_of(resp).await;
        assert_ne!(status, StatusCode::FORBIDDEN, "放开后不得再 403");
        assert_ne!(body["error"]["code"], "FOLDER_NOT_ALLOWED");
    }

    // ── 3. 空目录 / 扩展名全被过滤 → 400 ────────────────────────────────────

    #[tokio::test]
    async fn empty_or_fully_filtered_folder_is_400() {
        let root = unique_root("empty-folder");
        let uploads = seed_uploads(&root, &[]);
        let state = test_state(root.clone(), AppConfig::default(), vec![mock_module(&root)]);
        let app = super::router().with_state(state);
        let resp = app
            .oneshot(post_json("/v1/batches", batch_body(&uploads)).await)
            .await
            .unwrap();
        let (status, body) = body_of(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no matching files"));

        // 扩展名全被过滤
        let uploads = seed_uploads(&root, &["a.png", "b.png"]);
        let mut body = batch_body(&uploads);
        body["extensions"] = json!([".jpg"]);
        let state = test_state(root.clone(), AppConfig::default(), vec![mock_module(&root)]);
        let app = super::router().with_state(state);
        let resp = app.oneshot(post_json("/v1/batches", body).await).await.unwrap();
        let (status, _) = body_of(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── 4. 未知模块/capability → 404（校验先于 folder 副作用） ──────────────

    #[tokio::test]
    async fn unknown_module_is_404() {
        let root = unique_root("unknown-module");
        let uploads = seed_uploads(&root, &["a.txt"]);
        let state = test_state(root, AppConfig::default(), vec![]);
        let app = super::router().with_state(state);
        let mut body = batch_body(&uploads);
        body["module_id"] = json!("ghost-mod");
        let resp = app.oneshot(post_json("/v1/batches", body).await).await.unwrap();
        let (status, body) = body_of(resp).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "MODULE_NOT_FOUND");
    }

    // ── 5. 枚举：递归 + 扩展名过滤 + 确定性排序 ─────────────────────────────

    #[test]
    fn enumerate_files_filters_and_sorts() {
        let dir = std::env::temp_dir().join(format!("ep-batches-enum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
        std::fs::write(dir.join("b.png"), "x").unwrap();
        std::fs::write(dir.join("a.PNG"), "x").unwrap();
        std::fs::write(dir.join("c.jpg"), "x").unwrap();
        std::fs::write(dir.join("sub/d.png"), "x").unwrap();
        std::fs::write(dir.join("sub/deep/e.png"), "x").unwrap();

        // 一层 + 过滤（大小写不敏感）：不含 sub/
        let mut files = enumerate_files(&dir, false, &[".png".into()]).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.PNG", "b.png"], "大小写不敏感 + 排序");

        // 递归
        files = enumerate_files(&dir, true, &[".png".into()]).unwrap();
        assert_eq!(files.len(), 4, "递归应含 sub/ 与 sub/deep/ 下的 png");
        assert!(
            files.windows(2).all(|w| w[0] <= w[1]),
            "输出按路径排序"
        );

        // 无过滤 → 全部普通文件
        files = enumerate_files(&dir, false, &[]).unwrap();
        assert_eq!(files.len(), 3, "一层 3 个文件（a.PNG/b.png/c.jpg）");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 6. 组查询：未知组 → 404 GROUP_NOT_FOUND ─────────────────────────────

    #[tokio::test]
    async fn unknown_group_is_404() {
        let state = test_state(unique_root("group-404"), AppConfig::default(), vec![]);
        let app = super::router().with_state(state);
        let resp = app.clone()
            .oneshot(get("/v1/groups/grp-nope").await)
            .await
            .unwrap();
        let (status, body) = body_of(resp).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "GROUP_NOT_FOUND");

        let resp = app.oneshot(get("/v1/groups").await).await.unwrap();
        let (status, body) = body_of(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["groups"], json!([]));
    }

    async fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    // ── 7. 提交风暴容量预检：整批超过在途容量 → 429 ─────────────────────────

    #[tokio::test]
    // 该 guard 有意跨 await 持有：串行化全局任务注册表的读写，避免并发用例互相污染
    #[allow(clippy::await_holding_lock)]
    async fn batch_exceeding_queue_capacity_is_429() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("capacity");
        let uploads = seed_uploads(&root, &["a.txt", "b.txt"]);
        let state = test_state(root.clone(), AppConfig::default(), vec![mock_module(&root)]);
        // 直接填满调度器在途计数（模拟队列满载）
        execution::set_running_count_for_tests(execution::max_inflight_tasks());
        let app = super::router().with_state(state);
        let resp = app
            .oneshot(post_json("/v1/batches", batch_body(&uploads)).await)
            .await
            .unwrap();
        let (status, body) = body_of(resp).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "BATCH_QUEUE_INSUFFICIENT");
    }
}
