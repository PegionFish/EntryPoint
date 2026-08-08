//! 管线执行 API — Wave 2 执行代理（W2-D）
//!
//! POST /api/pipelines/execute：
//! - `pipeline_id` → 扫描 `config/pipelines/*.toml`，按 `[pipeline].id` 匹配后
//!   经 `ep_core::pipeline::load_pipeline` 加载；
//! - `spec`（前端 React Flow 结构）→ W2-C 桥接 `spec_to_pipeline`；
//! - `inputs`（node_id → 参数对象）→ 覆盖节点 params（如 file_input 的 path）。
//!
//! 引擎调度、任务注册表与进度回调接线位于 `execution.rs`（顶层文件），
//! 由本文件通过 `#[path]` 声明为子模块 —— ep-daemon 为纯 bin crate 且
//! main.rs 非本代理所有（与 pipeline_bridge.rs 同款做法）。
//! 外部使用方请经 `crate::api::execute::execution::*` 访问。

#[path = "../execution.rs"]
pub mod execution;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{Router, extract::State, http::StatusCode, routing::post, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;

use ep_core::module::manifest::{CapabilityDecl, ModuleManifest, ParamSchema};
use ep_core::pipeline::dag::Pipeline;
use ep_core::pipeline::load_pipeline;

use super::autostart::{self, AutoStartError};
use crate::api::err_response;
use crate::api::pipelines::pipeline_bridge::{spec_to_pipeline, PipelineSpec};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/pipelines/execute", post(execute_pipeline))
        .route("/execute/single", post(execute_single))
}

/// POST /api/pipelines/execute 请求体（前端 ExecutePipelineRequest 契约）
#[derive(Debug, Deserialize)]
struct ExecuteRequest {
    #[serde(default)]
    pipeline_id: Option<String>,
    #[serde(default)]
    spec: Option<PipelineSpec>,
    #[serde(default)]
    inputs: Option<HashMap<String, Value>>,
    /// §6.5 同步模式：阻塞至终态，响应直接带 status + artifacts
    #[serde(default)]
    wait: Option<bool>,
    /// §6.5 完成回调：终态时 POST {task_id, status, artifacts}（best-effort）
    #[serde(default)]
    callback_url: Option<String>,
}

/// POST /api/pipelines/execute — 提交管线执行
///
/// 成功 → 202 + `{"task_id": "..."}`（异步执行，进度走 /ws，结果查 /api/tasks）。
async fn execute_pipeline(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExecuteRequest>,
) -> (StatusCode, Json<Value>) {
    // 二选一校验：两者都缺/都给 → 400
    let pipeline = match (req.pipeline_id, req.spec) {
        (None, None) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.execute.eitherRequired",
                &[],
            )
            .await
        }
        (Some(_), Some(_)) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.execute.mutuallyExclusive",
                &[],
            )
            .await
        }
        (Some(id), None) => match find_builtin_pipeline(&state.root, &id) {
            Some(pipeline) => pipeline,
            None => {
                return err_response(
                    &state,
                    StatusCode::NOT_FOUND,
                    "apiPipelines.execute.pipelineNotFound",
                    &[("id", id)],
                )
                .await
            }
        },
        (None, Some(spec)) => match spec_to_pipeline(&spec) {
            Ok(pipeline) => pipeline,
            // 结构错误（缺字段/重复 id/非法边等）→ 400；
            // bridge 的 anyhow 消息为英文技术细节，经 {{detail}} 透传
            Err(e) => {
                return err_response(
                    &state,
                    StatusCode::BAD_REQUEST,
                    "apiPipelines.specInvalid",
                    &[("detail", e.to_string())],
                )
                .await
            }
        },
    };

    // §6.5：wait 同步模式与 callback_url 经完整选项版提交入口接线
    let options = execution::SubmitOptions {
        wait: req.wait.unwrap_or(false),
        callback_url: req.callback_url,
    };
    match execution::submit_pipeline_full(&state, pipeline, req.inputs, options).await {
        Ok(outcome) => {
            if let Some(record) = outcome.record {
                // wait 模式：直接返回终态 + artifacts（§6.5 响应契约）
                let artifacts: Vec<Value> = record
                    .artifacts
                    .iter()
                    .map(|(node_id, path)| {
                        json!({
                            "node_id": node_id,
                            "path": path.display().to_string(),
                        })
                    })
                    .collect();
                (
                    StatusCode::OK,
                    Json(json!({
                        "task_id": outcome.task_id,
                        "status": record.status.as_str(),
                        "artifacts": artifacts,
                    })),
                )
            } else {
                (
                    StatusCode::ACCEPTED,
                    Json(json!({ "task_id": outcome.task_id })),
                )
            }
        }
        Err(execution::SubmitError::UnknownInputNode(node_id)) => {
            err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.execute.inputsUnknownNode",
                &[("nodeId", node_id)],
            )
            .await
        }
        Err(execution::SubmitError::InputsNotObject(node_id)) => {
            err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.execute.inputsNotObject",
                &[("nodeId", node_id)],
            )
            .await
        }
        Err(execution::SubmitError::CycleDetected(id)) => {
            err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.execute.cycleDetected",
                &[("id", id)],
            )
            .await
        }
        // ── Wave 2 B3 新增变体（P2-11 提交前校验 + §5.3 直跑 + §6.5 自动拉起）──
        Err(execution::SubmitError::InvalidPipeline(detail)) => {
            err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.specInvalid",
                &[("detail", detail)],
            )
            .await
        }
        Err(execution::SubmitError::ModuleNotFound(id)) => {
            err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiCore.module.notFound",
                &[("id", id)],
            )
            .await
        }
        Err(execution::SubmitError::CapabilityNotFound(module_id, capability)) => {
            err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.execute.capabilityNotFound",
                &[("moduleId", module_id), ("capability", capability)],
            )
            .await
        }
        Err(execution::SubmitError::InputMissing(path)) => {
            err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.execute.inputMissing",
                &[("path", path.display().to_string())],
            )
            .await
        }
        // P1 修复：在途任务达容量上限 → 429（复用 internalError 键位透传技术细节；
        // i18n 键由 ep-core 侧所有，此处不新增）
        Err(execution::SubmitError::QueueFull(limit)) => {
            err_response(
                &state,
                StatusCode::TOO_MANY_REQUESTS,
                "apiPipelines.execute.internalError",
                &[("detail", format!("task queue is full (max {limit} in-flight tasks)"))],
            )
            .await
        }
        Err(execution::SubmitError::ModuleStartFailed(detail)) => {
            err_response(
                &state,
                StatusCode::BAD_GATEWAY,
                "apiPipelines.execute.moduleStartFailed",
                &[("detail", detail)],
            )
            .await
        }
        Err(execution::SubmitError::Internal(msg)) => {
            err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiPipelines.execute.internalError",
                &[("detail", msg)],
            )
            .await
        }
    }
}

/// 扫描 `config/pipelines/*.toml`，返回 `[pipeline].id` 匹配的管线定义。
///
/// 目录缺失/无匹配 → None（handler 映射为 404）；
/// 单个文件解析失败仅告警跳过，不影响其他管线的查找。
fn find_builtin_pipeline(root: &Path, pipeline_id: &str) -> Option<Pipeline> {
    let dir = root.join("config").join("pipelines");
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        match load_pipeline(&path) {
            Ok(pipeline) if pipeline.id == pipeline_id => return Some(pipeline),
            Ok(_) => {}
            Err(e) => {
                warn!(
                    file = %path.display(),
                    error = %e,
                    "failed to parse pipeline file, skipping"
                );
            }
        }
    }
    None
}

// ─── 单模型直跑（§5.3 / §8.1）───────────────────────────────────────────────

/// 单条参数校验失败（纯数据，单测直接断言；handler 映射 400 + i18n 键）
#[derive(Debug, PartialEq)]
enum ParamError {
    /// 必填参数缺失（schema 无 default 且请求未提供）
    Missing(String),
    /// 类型不符：(参数名, 期望类型描述)
    TypeMismatch { name: String, expected: String },
    /// 取值不在 enum_values 内
    EnumMismatch(String),
}

/// 按 capability schema 做基础参数校验（必填/类型/枚举），并注入缺省值。
///
/// 规则：
/// - 声明了 `default` 的参数缺失 → 注入默认值；无 `default` 且缺失 → `Missing`（必填）；
/// - 类型按 `ParamSchema.type` 基础核对：string / integer / float|number / boolean；
///   未知类型不校验（前向兼容，如模块自定义类型）；
/// - `enum_values` 非空且值为字符串 → 必须在列表内；
/// - 请求中的未声明参数原样透传（宽容，引擎侧不读即无副作用）。
///
/// 返回最终提交给引擎的参数对象（请求值 + 注入的默认值）。
fn validate_and_fill_params(
    capability: &CapabilityDecl,
    request_params: Value,
) -> Result<Value, ParamError> {
    let mut params = request_params.as_object().cloned().unwrap_or_default();
    let Some(schema) = capability.params.as_ref() else {
        return Ok(Value::Object(params));
    };

    for (name, decl) in schema {
        match params.get(name) {
            None => {
                // 缺失：有默认值 → 注入；否则必填校验失败
                if let Some(default) = &decl.default {
                    params.insert(name.clone(), default.clone());
                } else {
                    return Err(ParamError::Missing(name.clone()));
                }
            }
            Some(value) => {
                check_param_type(name, decl, value)?;
            }
        }
    }
    Ok(Value::Object(params))
}

/// 单参数类型/枚举核对
fn check_param_type(name: &str, decl: &ParamSchema, value: &Value) -> Result<(), ParamError> {
    let expected = decl.param_type.as_str();
    let ok = match expected {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "float" | "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        // 未知类型（模块自定义）不校验
        _ => true,
    };
    if !ok {
        return Err(ParamError::TypeMismatch {
            name: name.to_string(),
            expected: expected.to_string(),
        });
    }
    if let Some(enum_values) = &decl.enum_values {
        let in_enum = value
            .as_str()
            .map(|s| enum_values.iter().any(|v| v == s))
            .unwrap_or(false);
        if !in_enum {
            return Err(ParamError::EnumMismatch(name.to_string()));
        }
    }
    Ok(())
}

/// 按 module_id 查找模块清单
async fn find_module_manifest(state: &AppState, module_id: &str) -> Option<ModuleManifest> {
    let modules = state.modules.read().await;
    modules
        .iter()
        .find(|m| {
            m.manifest
                .as_ref()
                .map(|mf| mf.module.id == module_id)
                .unwrap_or(false)
        })
        .and_then(|m| m.manifest.clone())
}

/// POST /api/execute/single — 单模型直跑（§5.3）
///
/// 请求体：`{module_id, capability, params?, input_path}`。
/// 流程：字段/模块/capability/参数/输入文件校验 → 模块未运行则自动拉起并等健康
/// （[`autostart::ensure_module_running`]，修 P1-2 直跑侧）→
/// [`execution::submit_direct`]（B3：退化三节点 DAG）→ 202 `{"task_id"}`。
///
/// 状态码：400 字段/capability/参数/输入文件错误；404 模块不存在；
/// 409 模型未就绪；500 venv/端口/启动/提交失败；504 自动拉起后等健康超时。
async fn execute_single(
    State(state): State<Arc<AppState>>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    let body = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));

    // ── 1. 请求体字段解析 ──
    let module_id = match body.get("module_id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.missingModuleId",
                &[],
            )
            .await
        }
    };
    let capability = match body.get("capability").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.missingCapability",
                &[],
            )
            .await
        }
    };
    let input_path = match body.get("input_path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.missingInputPath",
                &[],
            )
            .await
        }
    };
    let request_params = match body.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(p) if p.is_object() => p.clone(),
        Some(_) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.paramsNotObject",
                &[],
            )
            .await
        }
    };

    // ── 2. 模块与 capability 校验（capability 必须在 manifest 声明内） ──
    let manifest = match find_module_manifest(&state, &module_id).await {
        Some(mf) => mf,
        None => {
            return err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiCore.module.notFound",
                &[("id", module_id)],
            )
            .await
        }
    };
    let cap = match manifest
        .interface
        .capabilities
        .iter()
        .find(|c| c.name == capability)
    {
        Some(c) => c.clone(),
        None => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.capabilityNotFound",
                &[
                    ("module_id", module_id),
                    ("capability", capability),
                ],
            )
            .await
        }
    };

    // ── 3. 参数按 schema 基础校验（必填/类型/枚举）+ 默认值注入 ──
    let params = match validate_and_fill_params(&cap, request_params) {
        Ok(p) => p,
        Err(ParamError::Missing(name)) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.paramMissing",
                &[("param", name)],
            )
            .await
        }
        Err(ParamError::TypeMismatch { name, expected }) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.paramTypeInvalid",
                &[("param", name), ("expected", expected)],
            )
            .await
        }
        Err(ParamError::EnumMismatch(name)) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.paramEnumInvalid",
                &[("param", name)],
            )
            .await
        }
    };

    // ── 4. 输入文件必须已存在于服务器本地（含 workspace/uploads 暂存） ──
    if !input_path.is_file() {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiPipelines.single.inputNotFound",
            &[("path", input_path.display().to_string())],
        )
        .await;
    }

    // ── 5. 模块自动拉起（§6.5：未运行 → 启动并等健康；超时计入任务错误语义） ──
    if let Err(e) = autostart::ensure_module_running(&state, &module_id).await {
        return autostart_error_response(&state, e).await;
    }

    // ── 6. 提交退化三节点 DAG（B3 实现；任务/产物/WS 全套复用） ──
    match execution::submit_direct(&state, &module_id, &capability, params, input_path).await {
        Ok(task_id) => (StatusCode::ACCEPTED, Json(json!({ "task_id": task_id }))),
        Err(e) => {
            err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiPipelines.single.submitFailed",
                &[("detail", e.to_string())],
            )
            .await
        }
    }
}

/// 自动拉起错误 → HTTP 状态码 + i18n 键映射
async fn autostart_error_response(
    state: &Arc<AppState>,
    e: AutoStartError,
) -> (StatusCode, Json<Value>) {
    match e {
        AutoStartError::ModuleNotFound(id) => {
            err_response(
                state,
                StatusCode::NOT_FOUND,
                "apiCore.module.notFound",
                &[("id", id)],
            )
            .await
        }
        AutoStartError::InvalidManifest(id) => {
            err_response(
                state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.module.invalidManifest",
                &[("id", id)],
            )
            .await
        }
        AutoStartError::ModelNotReady { module_id: _, model } => {
            err_response(
                state,
                StatusCode::CONFLICT,
                "apiCore.module.modelNotReady",
                &[("model", model)],
            )
            .await
        }
        AutoStartError::VenvPrepFailed(detail) => {
            err_response(
                state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiModels.venvPrepFailed",
                &[("detail", detail)],
            )
            .await
        }
        AutoStartError::PortAllocationFailed(detail) => {
            err_response(
                state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.module.portAllocationFailed",
                &[("detail", detail)],
            )
            .await
        }
        AutoStartError::StartFailed(detail) => {
            err_response(
                state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.module.startFailed",
                &[("detail", detail)],
            )
            .await
        }
        AutoStartError::HealthTimeout {
            module_id,
            timeout_secs,
        } => {
            err_response(
                state,
                StatusCode::GATEWAY_TIMEOUT,
                "apiPipelines.single.autostartTimeout",
                &[
                    ("module_id", module_id),
                    ("secs", timeout_secs.to_string()),
                ],
            )
            .await
        }
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // 测试锁的唯一目的就是跨 await 串行化这些共享静态注册表的测试；
    // 锁内临界区全部是极短同步操作，不存在持锁阻塞运行时的风险。
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tower::ServiceExt;

    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;

    use crate::api::pipelines::pipeline_bridge::{PipelineMeta, SpecNode, SpecNodeKind};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> std::path::PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-execapi-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_state(root: std::path::PathBuf) -> Arc<AppState> {
        test_state_lang(root, "zh-CN")
    }

    fn test_state_lang(root: std::path::PathBuf, language: &str) -> Arc<AppState> {
        let mut config = AppConfig::default();
        config.general.language = language.to_string();
        Arc::new(AppState::new(
            root,
            config,
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    fn exec_request(body: Value) -> ExecuteRequest {
        serde_json::from_value(body).unwrap()
    }

    /// 轮询等待任务终结
    async fn wait_terminal(task_id: &str) -> Option<execution::TaskRecord> {
        for _ in 0..300 {
            if let Some(record) = execution::snapshot(task_id) {
                if !matches!(record.status, execution::TaskState::Running) {
                    return Some(record);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    // ── 1. 缺 pipeline_id 与 spec → 400 ─────────────────────────────────────

    #[tokio::test]
    async fn test_execute_missing_both_fields_400() {
        let state = test_state(unique_root("empty"));
        let (status, body) =
            execute_pipeline(State(state), Json(exec_request(json!({}))))
                .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.0["error"]
            .as_str()
            .unwrap()
            .contains("必须提供"));
    }

    // ── 2. 同时提供 pipeline_id 与 spec → 400 ───────────────────────────────

    #[tokio::test]
    async fn test_execute_both_fields_400() {
        let state = test_state(unique_root("both"));
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({
                "pipeline_id": "video-to-srt",
                "spec": {
                    "pipeline": { "id": "x", "name": "x", "description": "" },
                    "nodes": [],
                    "edges": []
                }
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.0["error"].as_str().unwrap().contains("不能同时"));
    }

    // ── 3. pipeline_id 不存在 → 404（目录缺失 / 无匹配两种情形） ────────────

    #[tokio::test]
    async fn test_execute_unknown_pipeline_id_404() {
        // 目录缺失
        let state = test_state(unique_root("nodir"));
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({ "pipeline_id": "ghost-pipeline" }))),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.0["error"]
            .as_str()
            .unwrap()
            .contains("管线不存在"));

        // 目录存在但 id 无匹配
        let root = unique_root("nomatch");
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sample.toml"),
            "[pipeline]\nid = \"other-id\"\nname = \"其他\"\n\n[[nodes]]\nid = \"input\"\nkind = \"builtin\"\nbuiltin = \"file_input\"\n",
        )
        .unwrap();
        let state = test_state(root);
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({ "pipeline_id": "ghost-pipeline" }))),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.0["error"]
            .as_str()
            .unwrap()
            .contains("ghost-pipeline"));
    }

    // ── 4. spec 结构错误 → 400 ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_execute_invalid_spec_400() {
        let state = test_state(unique_root("badspec"));
        // nodes 为空 → 桥接层结构校验失败
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({
                "spec": {
                    "pipeline": { "id": "p", "name": "n", "description": "" },
                    "nodes": [],
                    "edges": []
                }
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.0["error"]
            .as_str()
            .unwrap()
            .contains("spec 结构无效"));
    }

    // ── 5. inputs 引用未知节点 → 400 ────────────────────────────────────────

    #[tokio::test]
    async fn test_execute_inputs_unknown_node_400() {
        let root = unique_root("badinputs");
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("p.toml"),
            "[pipeline]\nid = \"p1\"\nname = \"P1\"\n\n[[nodes]]\nid = \"input\"\nkind = \"builtin\"\nbuiltin = \"file_input\"\n",
        )
        .unwrap();
        let state = test_state(root);
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({
                "pipeline_id": "p1",
                "inputs": { "ghost": { "path": "/x" } }
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.0["error"].as_str().unwrap().contains("ghost"));
    }

    // ── 5b. inputs 节点值非对象 → 400（handler 层 InputsNotObject 映射面，
    //         execution 层直调已有覆盖，此处防路由接线漂移） ─────────────

    #[tokio::test]
    async fn test_execute_inputs_not_object_400() {
        let root = unique_root("notobj");
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("p.toml"),
            "[pipeline]\nid = \"p-no\"\nname = \"NO\"\n\n[[nodes]]\nid = \"input\"\nkind = \"builtin\"\nbuiltin = \"file_input\"\n",
        )
        .unwrap();
        let state = test_state(root);
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({
                "pipeline_id": "p-no",
                "inputs": { "input": "just-a-string" }
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let error = body.0["error"].as_str().unwrap();
        assert!(error.contains("input"), "{error}");
    }

    // ── 6. pipeline_id 命中 → 202 + 后台真实执行（纯 builtin 管线） ─────────

    #[tokio::test]
    async fn test_execute_by_pipeline_id_success_202_and_completes() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("ok");
        let src = root.join("in.txt");
        let dest = root.join("out.txt");
        std::fs::write(&src, "execute api e2e").unwrap();

        // 管线 TOML 故意不写 path 参数，全部经 inputs 覆盖注入
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("copy.toml"),
            r#"[pipeline]
id = "copy-pipe"
name = "复制管线"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
        )
        .unwrap();

        let state = test_state(root);
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({
                "pipeline_id": "copy-pipe",
                "inputs": {
                    "input": { "path": src.display().to_string() },
                    "output": { "path": dest.display().to_string() }
                }
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = body.0["task_id"].as_str().unwrap().to_string();
        assert!(task_id.starts_with("task-"));

        // 后台执行真实完成，输出文件落盘
        let record = wait_terminal(&task_id).await.expect("任务应终结");
        assert_eq!(record.status, execution::TaskState::Completed);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "execute api e2e");
    }

    // ── 7. spec 命中 → 202 + 后台真实执行 ───────────────────────────────────

    #[tokio::test]
    async fn test_execute_by_spec_success_202_and_completes() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("spec-ok");
        let src = root.join("spec-in.txt");
        let dest = root.join("spec-out.txt");
        std::fs::write(&src, "via spec").unwrap();

        let spec = PipelineSpec {
            pipeline: PipelineMeta {
                id: "spec-pipe".into(),
                name: "spec 管线".into(),
                description: String::new(),
                // B7（wave-2 返工）PipelineMeta 新增 §6.8 max_instances 后的
                // 机械补齐（仲裁 #11 同款模式，测试字面量无行为含义）
                max_instances: None,
                // 缺陷 #3 PipelineMeta 新增 node_timeout_secs 后的同款机械补齐
                node_timeout_secs: None,
            },
            nodes: vec![
                SpecNode {
                    id: "input".into(),
                    label: String::new(),
                    kind: SpecNodeKind::Builtin,
                    builtin: Some("file_input".into()),
                    module_id: None,
                    capability: None,
                    // B7（wave-2）SpecNode 新增 §6.2/P1-11 字段后的机械补齐
                    //（仲裁 #11 同款模式，测试字面量无行为含义）
                    model: None,
                    device: None,
                    params: json!({ "path": src.display().to_string() }),
                    position: None,
                    timeout_secs: None,
                    retry_count: None,
                },
                SpecNode {
                    id: "output".into(),
                    label: String::new(),
                    kind: SpecNodeKind::Builtin,
                    builtin: Some("file_output".into()),
                    module_id: None,
                    capability: None,
                    model: None,
                    device: None,
                    params: json!({ "path": dest.display().to_string() }),
                    position: None,
                    timeout_secs: None,
                    retry_count: None,
                },
            ],
            edges: vec![ep_core::pipeline::dag::Edge {
                from: ("input".into(), "output".into()),
                to: ("output".into(), "input".into()),
            }],
        };

        let state = test_state(root);
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({ "spec": serde_json::to_value(&spec).unwrap() }))),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = body.0["task_id"].as_str().unwrap().to_string();
        let record = wait_terminal(&task_id).await.expect("任务应终结");
        assert_eq!(record.status, execution::TaskState::Completed);
        assert_eq!(record.pipeline_id, "spec-pipe");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "via spec");
    }

    // ── 8. language=en → 错误文案为英文（i18n 按配置切换） ──────────────────

    #[tokio::test]
    async fn test_execute_errors_in_english_when_language_en() {
        let state = test_state_lang(unique_root("en"), "en");

        // 缺参 → 英文文案
        let (status, body) =
            execute_pipeline(State(state.clone()), Json(exec_request(json!({}))))
                .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.0["error"],
            "Either pipeline_id or spec must be provided"
        );

        // 管线不存在 → 英文文案 + id 插值
        let (status, body) = execute_pipeline(
            State(state.clone()),
            Json(exec_request(json!({ "pipeline_id": "ghost-pipeline" }))),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0["error"], "Pipeline not found: ghost-pipeline");

        // spec 无效 → 英文本地化前缀 + 英文 bridge 技术细节（{{detail}} 透传）
        let (status, body) = execute_pipeline(
            State(state),
            Json(exec_request(json!({
                "spec": {
                    "pipeline": { "id": "p", "name": "n", "description": "" },
                    "nodes": [],
                    "edges": []
                }
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = body.0["error"].as_str().unwrap();
        assert!(msg.starts_with("Pipeline spec is invalid: "), "got: {msg}");
        assert!(msg.contains("at least one node"), "got: {msg}");
    }

    // ─── /api/execute/single 测试（Router::oneshot）────────────────────────

    /// 直跑测试模块清单：capability `run` 带三类参数——
    /// `beam_size` integer 无默认（必填）、`language` string 有默认、
    /// `mode` string 枚举。`ready_timeout_secs = 1` 加速自动拉起失败路径；
    /// start_command 为跨平台保活命令（自动拉起路径测试需要存活子进程）。
    fn direct_manifest_toml() -> String {
        let keepalive = if cfg!(target_os = "windows") {
            "ping -n 30 127.0.0.1 > NUL"
        } else {
            "sleep 30"
        };
        format!(
            r#"
[module]
id = "direct-mod"
name = "直跑测试模块"
version = "0.1.0"
description = "direct exec test module"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = {{ "test" = "test" }}
start_command = "{keepalive}"

[compute]
backends = ["cpu"]

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 1

[[interface.capabilities]]
name = "run"
description = "run it"
input_type = "file"
output_type = "file"

[interface.capabilities.params]
beam_size = {{ type = "integer", min = 1, max = 20 }}
language = {{ type = "string", default = "auto" }}
mode = {{ type = "string", default = "fast", enum = ["fast", "slow"] }}
"#
        )
    }

    fn direct_test_state(root: std::path::PathBuf) -> Arc<AppState> {
        let manifest: ModuleManifest = toml::from_str(&direct_manifest_toml()).unwrap();
        let module = ep_core::module::discovery::DiscoveredModule {
            path: root.join("modules").join("direct-mod"),
            manifest: Some(manifest),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        };
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![module],
            // 独立区间（避开生产默认 18000-19000）：本组测试断言"无服务
            // 响应 /health → 504/任务 Failed"，与并发真实 daemon 的 adapter
            // 端口区间重叠时探测可能误判就绪（环境性 flake）。
            PortManager::new(48330, 48340),
        ))
    }

    /// 构造 Router::oneshot 用的 JSON POST 请求
    fn single_request(body: Value) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri("/execute/single")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    }

    async fn single_response(
        resp: axum::response::Response,
    ) -> (StatusCode, Value) {
        use http_body_util::BodyExt;
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("响应不是合法 JSON: {e}; body={bytes:?}"));
        (status, json)
    }

    // ── 1. 未知模块 → 404（复用 apiCore.module.notFound 现有键） ──────────

    #[tokio::test]
    async fn test_single_unknown_module_404() {
        let state = direct_test_state(unique_root("s-nomod"));
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        let resp = app
            .oneshot(single_request(json!({
                "module_id": "ghost",
                "capability": "run",
                "input_path": "/tmp/x.txt"
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"], "模块不存在：ghost");
    }

    // ── 2. 未知 capability → 400 ───────────────────────────────────────────

    #[tokio::test]
    async fn test_single_unknown_capability_400() {
        let state = direct_test_state(unique_root("s-nocap"));
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "fly",
                "input_path": "/tmp/x.txt"
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // C8 已落盘：断言真实 zh 文案（默认语言 zh-CN）
        assert_eq!(json["error"], "模块 'direct-mod' 不存在能力 'fly'");
    }

    // ── 3. 缺必填参数（beam_size 无默认）→ 400 ─────────────────────────────

    #[tokio::test]
    async fn test_single_missing_required_param_400() {
        let root = unique_root("s-missparam");
        let input = root.join("in.txt");
        std::fs::write(&input, "data").unwrap();

        let state = direct_test_state(root.clone());
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "language": "zh" },
                "input_path": input.display().to_string()
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "缺少必填参数 'beam_size'");
    }

    // ── 4. 参数类型不符 → 400 ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_single_param_type_invalid_400() {
        let root = unique_root("s-badtype");
        let input = root.join("in.txt");
        std::fs::write(&input, "data").unwrap();

        let state = direct_test_state(root.clone());
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": "five" },
                "input_path": input.display().to_string()
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "参数 'beam_size' 类型无效（期望 integer）");
    }

    // ── 5. 枚举越界 → 400 ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_single_param_enum_invalid_400() {
        let root = unique_root("s-badenum");
        let input = root.join("in.txt");
        std::fs::write(&input, "data").unwrap();

        let state = direct_test_state(root.clone());
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5, "mode": "turbo" },
                "input_path": input.display().to_string()
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "参数 'mode' 取值不在可选列表内");
    }

    // ── 6. 输入文件不存在 → 400 ────────────────────────────────────────────

    #[tokio::test]
    async fn test_single_input_not_found_400() {
        let state = direct_test_state(unique_root("s-noinput"));
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": "/definitely/not/here.txt"
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "输入文件不存在: /definitely/not/here.txt");
    }

    // ── 7. 缺字段（空 body / params 非对象）→ 400 ──────────────────────────

    #[tokio::test]
    async fn test_single_missing_fields_400() {
        let state = direct_test_state(unique_root("s-empty"));
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state.clone());

        // 空 body → 缺 module_id
        let resp = app.oneshot(single_request(json!({}))).await.unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "缺少 module_id 字段");

        // 缺 capability
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state.clone());
        let resp = app
            .oneshot(single_request(json!({ "module_id": "direct-mod" })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "缺少 capability 字段");

        // params 非对象
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": "not-an-object",
                "input_path": "/tmp/x.txt"
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "params 必须是对象");
    }

    // ── 8. 自动拉起失败 → 504（等健康超时计入调用方错误语义） ───────────────

    #[tokio::test]
    async fn test_single_autostart_health_timeout_504() {
        let root = unique_root("s-timeout");
        let input = root.join("in.txt");
        std::fs::write(&input, "data").unwrap();

        // manifest ready_timeout_secs = 1：外层等待预算 1s，先于
        // monitor_process 内部 ~2s 的 Error 翻转 → HealthTimeout → 504。
        // 端口范围内无任何服务监听 → 健康探测必然不成功。
        let state = direct_test_state(root.clone());
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state.clone());

        let started = std::time::Instant::now();
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": input.display().to_string()
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "响应: {json}");
        assert_eq!(json["error"], "模块 'direct-mod' 自动拉起后 1s 内未就绪");
        // 不应久等：预算 1s + 清理余量
        assert!(started.elapsed() < std::time::Duration::from_secs(10));

        // 失败清理：模块被停止、端口被释放
        {
            let pm = state.process_manager.read().await;
            assert_eq!(
                pm.get_status("direct-mod"),
                Some(&ep_core::types::ServiceStatus::Stopped)
            );
        }
        assert!(state.port_manager.read().await.get_port("direct-mod").is_none());
    }

    // ── 9. validate_and_fill_params 纯函数单测 ──────────────────────────────

    fn direct_capability() -> CapabilityDecl {
        let manifest: ModuleManifest = toml::from_str(&direct_manifest_toml()).unwrap();
        manifest.interface.capabilities.into_iter().next().unwrap()
    }

    #[test]
    fn test_validate_params_fills_defaults() {
        let cap = direct_capability();
        let out = validate_and_fill_params(&cap, json!({ "beam_size": 5 })).unwrap();
        assert_eq!(out["beam_size"], 5);
        assert_eq!(out["language"], "auto"); // schema 默认值注入
        assert_eq!(out["mode"], "fast");
    }

    #[test]
    fn test_validate_params_missing_required() {
        let cap = direct_capability();
        let err = validate_and_fill_params(&cap, json!({})).unwrap_err();
        assert_eq!(err, ParamError::Missing("beam_size".to_string()));
    }

    #[test]
    fn test_validate_params_type_mismatch() {
        let cap = direct_capability();
        let err = validate_and_fill_params(&cap, json!({ "beam_size": 2.5 })).unwrap_err();
        // 2.5 是 number 但不是 integer → 类型不符
        assert_eq!(
            err,
            ParamError::TypeMismatch {
                name: "beam_size".to_string(),
                expected: "integer".to_string()
            }
        );
    }

    #[test]
    fn test_validate_params_enum_check() {
        let cap = direct_capability();
        assert!(validate_and_fill_params(&cap, json!({ "beam_size": 5, "mode": "slow" })).is_ok());
        let err = validate_and_fill_params(&cap, json!({ "beam_size": 5, "mode": "warp" }))
            .unwrap_err();
        assert_eq!(err, ParamError::EnumMismatch("mode".to_string()));
    }

    #[test]
    fn test_validate_params_unknown_type_skipped() {
        // 无 params schema 的 capability：请求参数原样透传
        let mut cap = direct_capability();
        cap.params = None;
        let out = validate_and_fill_params(&cap, json!({ "anything": [1, 2] })).unwrap();
        assert_eq!(out["anything"], json!([1, 2]));
    }

    // ── 10. autostart 错误映射（504 分支直测，不依赖真实进程） ─────────────

    #[tokio::test]
    async fn test_autostart_error_mapping() {
        let state = test_state(unique_root("s-errmap"));

        let (status, body) = autostart_error_response(
            &state,
            autostart::AutoStartError::HealthTimeout {
                module_id: "m".into(),
                timeout_secs: 30,
            },
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(body.0["error"], "模块 'm' 自动拉起后 30s 内未就绪");

        let (status, body) = autostart_error_response(
            &state,
            autostart::AutoStartError::ModelNotReady {
                module_id: "m".into(),
                model: "large-v3".into(),
            },
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.0["error"], "模型未就绪：large-v3，请先在模型管理页下载或导入");
    }
}
