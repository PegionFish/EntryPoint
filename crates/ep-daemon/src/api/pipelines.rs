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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json,
};
use serde::Deserialize;
use serde_json::{Value, json};

use ep_core::pipeline::vram::{self, DeviceCapacity, VramNodeEstimate};
use ep_core::task_registry::TaskState;

use crate::api::err_response;
use crate::api::execute::execution;
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
        // §6.8 管线级任务视图（P1-5：替代坏掉的 {id}/status 的运维面）
        .route("/pipelines/{id}/tasks", get(pipeline_tasks))
        // §6.3 VRAM 预算（编辑器实时计算，S2 前端形状，仲裁 #3）
        .route("/pipelines/vram-budget", post(vram_budget))
        // P1-11 任务取消（TaskStatus::Cancelled 产生路径）。
        // 归属说明：取消逻辑在 execution.rs（B3 所有）；tasks.rs 未列入波次
        // 所有权矩阵，故该路由暂挂本 router（路径仍在 /api/tasks 命名空间下）。
        .route("/tasks/{task_id}/cancel", post(cancel_task))
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

/// GET /api/pipelines/:id/status — 兼容端点（P1-5 修复：改查任务注册表）
///
/// 旧实现查 `AppState` 预置的共享 runner（`state.runner`）——该 runner
/// 从未被执行路径使用（每次执行自建 runner），故恒返回 `unknown`；
/// 该死字段已于 Wave 4 D2 移除。现基于 ep-core 任务注册表按 `pipeline_id`
/// 聚合：返回该管线**最新一条**任务的状态；无任务记录 → `unknown`。
///
/// 文档化决策（任务 5）：本端点保留为注册表聚合（向后兼容），
/// 完整运维面请改用 `GET /api/pipelines/{id}/tasks`。
async fn pipeline_status(AxumPath(id): AxumPath<String>) -> Json<Value> {
    let latest = execution::snapshot_by_pipeline(&id)
        .first()
        .map(|r| r.status.as_str().to_string());
    match latest {
        Some(s) => Json(json!({ "status": s })),
        None => Json(json!({ "status": "unknown" })),
    }
}

// ─── §6.8 管线级任务视图（P1-5 替代面） ─────────────────────────────────────

/// GET /api/pipelines/:id/tasks 查询参数（前端 PipelineTasksQuery 契约）
#[derive(Debug, Deserialize)]
struct PipelineTasksQuery {
    /// 按状态过滤（queued/running/completed/failed/cancelled；未知值 → 空列表）
    #[serde(default)]
    status: Option<String>,
    /// 条数上限（缺省 100）
    #[serde(default)]
    limit: Option<usize>,
}

/// GET /api/pipelines/:id/tasks — 该管线执行历史/在跑任务（§6.8）
///
/// 数据源为任务注册表按 `pipeline_id` 索引（持久化，重启不丢）。
/// 响应条目含 `queue_position`（queued 时）与 `started_running_at`
/// （实际开始时间，排队耗时可算）。管线不存在 → 404。
async fn pipeline_tasks(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<PipelineTasksQuery>,
) -> (StatusCode, Json<Value>) {
    let dir = pipelines_dir(&state);
    if find_spec_file(&dir, &id).is_none() {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiPipelines.pipelines.notFound",
            &[],
        )
        .await;
    }

    let mut tasks = execution::snapshot_by_pipeline(&id);
    if let Some(status_filter) = query.status.as_deref() {
        match TaskState::parse(status_filter) {
            Some(want) => tasks.retain(|t| t.status == want),
            None => tasks.clear(), // 未知状态值 → 空列表（容错，不 400）
        }
    }
    tasks.truncate(query.limit.unwrap_or(100));

    let list: Vec<Value> = tasks
        .iter()
        .map(|t| {
            let completed = t
                .nodes
                .values()
                .filter(|n| n.state == "completed")
                .count();
            let mut v = json!({
                "id": t.id,
                "pipeline_id": t.pipeline_id,
                "pipeline_name": t.pipeline_id,
                "status": t.status.as_str(),
                "started_at": t.started_at.to_rfc3339(),
                "node_count": t.nodes.len(),
                "completed_nodes": completed,
            });
            if let Some(finished) = t.finished_at {
                v["finished_at"] = json!(finished.to_rfc3339());
            }
            if let Some(running_since) = t.started_running_at {
                v["started_running_at"] = json!(running_since.to_rfc3339());
            }
            if let Some(pos) = t.queue_position {
                v["queue_position"] = json!(pos);
            }
            if let Some(error) = &t.error {
                v["error"] = json!(error);
            }
            v
        })
        .collect();
    (StatusCode::OK, Json(Value::Array(list)))
}

// ─── §6.3 VRAM 预算 ─────────────────────────────────────────────────────────

/// POST /api/pipelines/vram-budget 请求体（前端 VramBudgetRequest：`{spec}`）
#[derive(Debug, Deserialize)]
struct VramBudgetRequest {
    spec: VramSpec,
}

/// spec 的预算视角投影：只取算法所需字段（容忍 B7 扩展中的 model/device 字段）
#[derive(Debug, Deserialize)]
struct VramSpec {
    #[serde(default)]
    nodes: Vec<VramSpecNode>,
    #[serde(default)]
    edges: Vec<ep_core::pipeline::dag::Edge>,
}

#[derive(Debug, Deserialize)]
struct VramSpecNode {
    id: String,
    /// module 节点的模块 id（builtin 节点无 VRAM，忽略）
    #[serde(default)]
    module_id: Option<String>,
    /// 变体 pin `<qualified_id>@<variant>`（§6.2；缺省 = 激活变体/默认变体）
    #[serde(default)]
    model: Option<String>,
    /// 设备绑定软约束（§6.2："auto" | "cuda:0" | …；缺省 = auto）
    #[serde(default)]
    device: Option<String>,
}

/// POST /api/pipelines/vram-budget — 每设备 VRAM 预算分解（§6.3）
///
/// 算法在 ep-core `pipeline::vram`（纯计算，桌面端直连复用）；本 handler
/// 负责数据拼装：节点 vram 取 `manifest.resolve_vram_estimate`（pin 变体
/// 优先 → 激活变体 → 默认变体；变体级优先、模块级兜底，A6 数据源），
/// 设备容量取 `state.devices`，`allow_overcommit` 取 `compute.allow_overcommit`。
///
/// 响应形状（S2 前端提议，仲裁 #3）：
/// `{devices:[{device_id,total_mb,used_mb,pipeline_mb,items:[{node_id,mb}],over}], unassigned:[{node_id,mb}], unassigned_mb, allow_overcommit}`
async fn vram_budget(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VramBudgetRequest>,
) -> (StatusCode, Json<Value>) {
    if req.spec.nodes.is_empty() {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiPipelines.vramBudget.specEmpty",
            &[],
        )
        .await;
    }

    // manifest 查找表 + 激活变体（单次读锁取快照）
    let (manifests, active_models) = {
        let modules = state.modules.read().await;
        let cfg = state.config.read().await;
        let manifests: HashMap<String, ep_core::module::manifest::ModuleManifest> = modules
            .iter()
            .filter_map(|m| m.manifest.clone())
            .map(|mf| (mf.module.id.clone(), mf))
            .collect();
        (manifests, cfg.active_models.clone())
    };
    let allow_overcommit = state.config.read().await.compute.allow_overcommit;

    // 节点 → VRAM 估算（module 节点查 manifest；builtin/未知模块 = None）
    let estimates: Vec<VramNodeEstimate> = req
        .spec
        .nodes
        .iter()
        .map(|node| {
            let (device, vram_mb) = match node.module_id.as_deref() {
                Some(module_id) => {
                    let variant = node
                        .model
                        .as_deref()
                        .and_then(|pin| pin.rsplit('@').next())
                        .filter(|v| !v.is_empty())
                        .map(str::to_string)
                        .or_else(|| active_models.get(module_id).cloned());
                    let mb = manifests.get(module_id).and_then(|mf| {
                        let variant_id = variant
                            .clone()
                            .or_else(|| {
                                mf.models
                                    .iter()
                                    .find(|m| m.default)
                                    .or(mf.models.first())
                                    .map(|m| m.id.clone())
                            })
                            .unwrap_or_default();
                        mf.resolve_vram_estimate(&variant_id)
                    });
                    (node.device.clone().unwrap_or_else(|| "auto".into()), mb)
                }
                None => ("auto".to_string(), None),
            };
            VramNodeEstimate {
                node_id: node.id.clone(),
                device,
                vram_mb,
            }
        })
        .collect();

    let edges: Vec<(String, String)> = req
        .spec
        .edges
        .iter()
        .map(|e| (e.from.0.clone(), e.to.0.clone()))
        .collect();

    let devices: Vec<DeviceCapacity> = {
        let devs = state.devices.read().await;
        devs.iter()
            .map(|d| DeviceCapacity {
                device_id: d.id.to_string(),
                total_mb: d.total_memory_mb.map(u64::from),
                used_mb: d.used_memory_mb.map(u64::from),
            })
            .collect()
    };

    match vram::compute_budget(&estimates, &edges, &devices, allow_overcommit) {
        Ok(report) => (
            StatusCode::OK,
            Json(serde_json::to_value(&report).expect("VramBudgetReport serialization cannot fail")),
        ),
        Err(vram::VramBudgetError::CycleDetected) => {
            err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.vramBudget.cycleDetected",
                &[],
            )
            .await
        }
    }
}

// ─── 任务取消（P1-11） ──────────────────────────────────────────────────────

/// POST /api/tasks/:task_id/cancel — 取消任务
///
/// 排队中 → 立即终结且不执行；运行中 → 逻辑终态 `cancelled`（引擎无外部
/// 中断点，后台收尾被忽略，见 execution.rs 模块文档）。
/// 404 任务不存在 / 409 已是终态。
async fn cancel_task(
    State(state): State<Arc<AppState>>,
    AxumPath(task_id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    match execution::request_cancel(&state, &task_id).await {
        execution::CancelOutcome::Cancelled => (
            StatusCode::OK,
            Json(json!({ "ok": true, "status": "cancelled" })),
        ),
        execution::CancelOutcome::AlreadyTerminal(status) => {
            err_response(
                &state,
                StatusCode::CONFLICT,
                "apiPipelines.tasks.cancelAlreadyTerminal",
                &[("status", status.as_str().to_string())],
            )
            .await
        }
        execution::CancelOutcome::NotFound => {
            err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiPipelines.tasks.taskNotFound",
                &[("taskId", task_id)],
            )
            .await
        }
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // 测试锁（execution::TEST_LOCK）跨 await 串行化共享静态注册表测试，
    // 与 execution/execute/tasks 测试模块同款豁免。
    #![allow(clippy::await_holding_lock)]

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
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();
        let body = pipeline_status(AxumPath("any-pipe".to_string())).await;
        assert_eq!(body.0["status"], "unknown");
    }

    // ── 状态聚合：基于任务注册表（P1-5 修复） ───────────────────────────────

    #[tokio::test]
    async fn test_status_aggregates_from_registry() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let state = test_state();
        let src = state.root.join("st-src.txt");
        let dest = state.root.join("st-out.txt");
        std::fs::write(&src, "status aggregate").unwrap();
        let toml = format!(
            r#"
[pipeline]
id = "status-pipe"
name = "状态聚合"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            src.display().to_string().replace('\\', "\\\\"),
            dest.display().to_string().replace('\\', "\\\\"),
        );
        let pipeline = ep_core::pipeline::dag::Pipeline::from_toml_str(&toml).unwrap();
        execution::submit_pipeline(&state, pipeline, None)
            .await
            .unwrap();

        // 轮询直至注册表出现终态记录
        let mut status = String::new();
        for _ in 0..300 {
            let body = pipeline_status(AxumPath("status-pipe".to_string())).await;
            status = body.0["status"].as_str().unwrap().to_string();
            if ["completed", "failed", "cancelled"].contains(&status.as_str()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(status, "completed", "注册表聚合应反映最新任务终态");
    }

    // ── GET /pipelines/{id}/tasks（§6.8 管线级任务视图，P1-5 替代面） ───────

    fn write_pipeline_file(state: &Arc<AppState>, id: &str) {
        let dir = pipelines_dir(state);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.toml", id.replace('-', "_"))),
            format!("[pipeline]\nid = \"{id}\"\nname = \"t\"\n"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_pipeline_tasks_endpoint_shape_and_filters() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let state = test_state();
        write_pipeline_file(&state, "tasks-pipe");
        let src = state.root.join("pt-src.txt");
        let dest1 = state.root.join("pt-out-1.txt");
        let dest2 = state.root.join("pt-out-2.txt");
        std::fs::write(&src, "pipeline tasks").unwrap();

        for dest in [&dest1, &dest2] {
            let toml = format!(
                r#"
[pipeline]
id = "tasks-pipe"
name = "任务视图"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
                src.display().to_string().replace('\\', "\\\\"),
                dest.display().to_string().replace('\\', "\\\\"),
            );
            let pipeline = ep_core::pipeline::dag::Pipeline::from_toml_str(&toml).unwrap();
            let task_id = execution::submit_pipeline(&state, pipeline, None)
                .await
                .unwrap();
            // 等待终结
            for _ in 0..300 {
                if let Some(r) = execution::snapshot(&task_id) {
                    if r.status.is_terminal() {
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }

        // 全量列表：两条记录，新任务在前，含 pipeline_id 身份字段（§6.8）
        let (status, body) = pipeline_tasks(
            State(state.clone()),
            AxumPath("tasks-pipe".to_string()),
            Query(PipelineTasksQuery {
                status: None,
                limit: None,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let arr = body.0.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        for entry in arr {
            assert_eq!(entry["pipeline_id"], "tasks-pipe");
            assert_eq!(entry["status"], "completed");
            assert!(entry["started_at"].is_string());
            assert!(entry["finished_at"].is_string());
            assert_eq!(entry["node_count"], 2);
        }

        // status 过滤
        let (_, body) = pipeline_tasks(
            State(state.clone()),
            AxumPath("tasks-pipe".to_string()),
            Query(PipelineTasksQuery {
                status: Some("completed".into()),
                limit: None,
            }),
        )
        .await;
        assert_eq!(body.0.as_array().unwrap().len(), 2);
        let (_, body) = pipeline_tasks(
            State(state.clone()),
            AxumPath("tasks-pipe".to_string()),
            Query(PipelineTasksQuery {
                status: Some("queued".into()),
                limit: None,
            }),
        )
        .await;
        assert_eq!(body.0.as_array().unwrap().len(), 0);
        // 未知状态值 → 空列表（容错）
        let (_, body) = pipeline_tasks(
            State(state.clone()),
            AxumPath("tasks-pipe".to_string()),
            Query(PipelineTasksQuery {
                status: Some("bogus".into()),
                limit: None,
            }),
        )
        .await;
        assert_eq!(body.0.as_array().unwrap().len(), 0);

        // limit 生效
        let (_, body) = pipeline_tasks(
            State(state.clone()),
            AxumPath("tasks-pipe".to_string()),
            Query(PipelineTasksQuery {
                status: None,
                limit: Some(1),
            }),
        )
        .await;
        assert_eq!(body.0.as_array().unwrap().len(), 1);

        // 未知管线 → 404
        let (status, _) = pipeline_tasks(
            State(state),
            AxumPath("ghost-pipe".to_string()),
            Query(PipelineTasksQuery {
                status: None,
                limit: None,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ── POST /pipelines/vram-budget（§6.3） ─────────────────────────────────

    fn gpu_device(idx: u32, total: Option<u32>, used: Option<u32>) -> ep_core::types::ComputeDevice {
        ep_core::types::ComputeDevice {
            id: ep_core::types::DeviceId::Cuda(idx),
            backend: ep_core::types::ComputeBackend::Cuda,
            name: format!("Test GPU {idx}"),
            total_memory_mb: total,
            used_memory_mb: used,
            utilization: None,
            temperature: None,
        }
    }

    /// fixture manifest：两个变体（small=2048 模块级兜底 4096）
    fn vram_manifest(module_id: &str) -> ep_core::module::manifest::ModuleManifest {
        toml::from_str(&format!(
            r#"
[module]
id = "{module_id}"
name = "VRAM Fixture"
version = "0.1.0"
description = "test"
category = "asr"
genre = "test"
license = "MIT"

[runtime]
type = "python"

[compute]
backends = ["cuda"]
vram_estimate_mb = 4096

[interface]
type = "http"

[[models]]
id = "small"
name = "Small"
source = "huggingface"
target_dir = "{module_id}-small"
vram_estimate_mb = 2048

[[models]]
id = "large"
name = "Large"
source = "huggingface"
target_dir = "{module_id}-large"
default = true
"#,
        ))
        .unwrap()
    }

    fn state_with_vram_module() -> Arc<AppState> {
        use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
        let manifest = vram_manifest("fixture-asr");
        let module = DiscoveredModule {
            manifest: Some(manifest),
            path: std::env::temp_dir().join("modules/fixture-asr"),
            status: DiscoveryStatus::Valid,
        };
        Arc::new(AppState::new(
            std::env::temp_dir().join(format!(
                "ep-vram-mod-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::SeqCst)
            )),
            AppConfig::default(),
            vec![gpu_device(0, Some(24576), Some(1024))],
            vec![module],
            PortManager::new(18000, 19000),
        ))
    }

    #[tokio::test]
    async fn test_vram_budget_pin_variant_and_device() {
        let state = state_with_vram_module();
        // spec：module 节点 pin small 变体 + 绑定 cuda:0；builtin 节点不计
        let body = json!({
            "spec": {
                "pipeline": { "id": "vram-pipe", "name": "vram", "description": "" },
                "nodes": [
                    { "id": "input", "kind": "builtin", "builtin": "file_input" },
                    { "id": "asr", "kind": "module", "module_id": "fixture-asr",
                      "model": "ep.x.fixture-asr@small", "device": "cuda:0" }
                ],
                "edges": [ { "from": ["input", "output"], "to": ["asr", "input"] } ]
            }
        });
        let (status, resp) =
            vram_budget(State(state), Json(serde_json::from_value(body).unwrap())).await;
        assert_eq!(status, StatusCode::OK);
        let v = resp.0;
        // S2 形状（仲裁 #3）：devices[].device_id/total_mb/used_mb/pipeline_mb/items
        let devices = v["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["device_id"], "cuda:0");
        assert_eq!(devices[0]["total_mb"], 24576);
        assert_eq!(devices[0]["used_mb"], 1024);
        assert_eq!(devices[0]["pipeline_mb"], 2048, "pin 变体 small=2048 生效");
        assert_eq!(devices[0]["items"][0]["node_id"], "asr");
        assert_eq!(devices[0]["items"][0]["mb"], 2048);
        assert_eq!(devices[0]["over"], false);
        assert_eq!(v["unassigned"], json!([]));
        assert_eq!(v["unassigned_mb"], 0);
        assert_eq!(v["allow_overcommit"], true, "默认配置 allow_overcommit=true");
    }

    #[tokio::test]
    async fn test_vram_budget_auto_nodes_unassigned_and_module_fallback() {
        let state = state_with_vram_module();
        // device 缺省(auto) + 未 pin 变体（默认 large 无变体级值 → 模块级 4096 兜底）
        let body = json!({
            "spec": {
                "nodes": [
                    { "id": "asr", "kind": "module", "module_id": "fixture-asr" }
                ],
                "edges": []
            }
        });
        let (status, resp) =
            vram_budget(State(state), Json(serde_json::from_value(body).unwrap())).await;
        assert_eq!(status, StatusCode::OK);
        let v = resp.0;
        assert_eq!(v["unassigned_mb"], 4096, "auto 节点入未分配池，模块级兜底 4096");
        assert_eq!(v["unassigned"][0]["node_id"], "asr");
        assert_eq!(v["unassigned"][0]["mb"], 4096);
        // cuda:0 仍在账本中（容量快照给定），管线需求为 0
        assert_eq!(v["devices"][0]["pipeline_mb"], 0);
    }

    #[tokio::test]
    async fn test_vram_budget_over_flag_and_empty_spec() {
        let state = state_with_vram_module();
        // used 1024 + pipeline 2048 < 8192 → 换成小容量设备验证 over=true
        {
            let small = Arc::new(AppState::new(
                std::env::temp_dir().join(format!(
                    "ep-vram-over-{}-{}",
                    std::process::id(),
                    SEQ.fetch_add(1, Ordering::SeqCst)
                )),
                AppConfig::default(),
                vec![gpu_device(0, Some(3000), Some(1500))],
                vec![ep_core::module::discovery::DiscoveredModule {
                    manifest: Some(vram_manifest("fixture-asr")),
                    path: std::env::temp_dir().join("modules/fixture-asr"),
                    status: ep_core::module::discovery::DiscoveryStatus::Valid,
                }],
                PortManager::new(18000, 19000),
            ));
            let body = json!({
                "spec": {
                    "nodes": [
                        { "id": "asr", "kind": "module", "module_id": "fixture-asr",
                          "model": "ep.x.fixture-asr@small", "device": "cuda:0" }
                    ],
                    "edges": []
                }
            });
            let (status, resp) =
                vram_budget(State(small), Json(serde_json::from_value(body).unwrap())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(resp.0["devices"][0]["over"], true, "1500+2048 > 3000 → 超预算");
        }

        // 空 spec → 400
        let (status, _) = vram_budget(
            State(state),
            Json(serde_json::from_value(json!({ "spec": { "nodes": [] } })).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_vram_budget_cycle_rejected() {
        let state = state_with_vram_module();
        let body = json!({
            "spec": {
                "nodes": [
                    { "id": "a", "kind": "module", "module_id": "fixture-asr", "device": "cuda:0" },
                    { "id": "b", "kind": "module", "module_id": "fixture-asr", "device": "cuda:0" }
                ],
                "edges": [
                    { "from": ["a", "output"], "to": ["b", "input"] },
                    { "from": ["b", "output"], "to": ["a", "input"] }
                ]
            }
        });
        let (status, _) =
            vram_budget(State(state), Json(serde_json::from_value(body).unwrap())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── POST /tasks/{task_id}/cancel ────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_cancel_endpoint_404_and_409() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let state = test_state();
        // 未知任务 → 404
        let (status, body) =
            cancel_task(State(state.clone()), AxumPath("task-ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.0["error"].as_str().unwrap().contains("任务不存在"));

        // 终态任务 → 409
        let src = state.root.join("cx-src.txt");
        let dest = state.root.join("cx-out.txt");
        std::fs::write(&src, "cancel endpoint").unwrap();
        let toml = format!(
            r#"
[pipeline]
id = "cancel-endpoint-pipe"
name = "取消端点"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            src.display().to_string().replace('\\', "\\\\"),
            dest.display().to_string().replace('\\', "\\\\"),
        );
        let pipeline = ep_core::pipeline::dag::Pipeline::from_toml_str(&toml).unwrap();
        let task_id = execution::submit_pipeline(&state, pipeline, None)
            .await
            .unwrap();
        for _ in 0..300 {
            if let Some(r) = execution::snapshot(&task_id) {
                if r.status.is_terminal() {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let (status, _) =
            cancel_task(State(state.clone()), AxumPath(task_id.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT);

        // 成功取消排队任务 → 200（用全局闸门满制造排队）
        state.config.write().await.pipeline.max_parallel = 1;
        // 先占闸：提交一个会被钩子阻塞的任务
        execution::set_test_run_hook_for_pipelines_test();
        let hold_toml = format!(
            r#"
[pipeline]
id = "cancel-hold"
name = "持闸"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
            src.display().to_string().replace('\\', "\\\\"),
        );
        let hold_pipeline = ep_core::pipeline::dag::Pipeline::from_toml_str(&hold_toml).unwrap();
        let _hold_id = execution::submit_pipeline(&state, hold_pipeline, None)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let queued_pipeline =
            ep_core::pipeline::dag::Pipeline::from_toml_str(&hold_toml.replace("cancel-hold", "cancel-q2")).unwrap();
        let queued_id = execution::submit_pipeline(&state, queued_pipeline, None)
            .await
            .unwrap();
        assert_eq!(execution::snapshot(&queued_id).unwrap().status, execution::TaskState::Queued);

        let (status, body) =
            cancel_task(State(state.clone()), AxumPath(queued_id.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.0["status"], "cancelled");
        assert_eq!(
            execution::snapshot(&queued_id).unwrap().status,
            execution::TaskState::Cancelled
        );

        execution::release_test_run_hook_for_pipelines_test();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}
