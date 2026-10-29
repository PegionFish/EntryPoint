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

use axum::{
    Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::post,
    Json,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use ep_core::module::manifest::{CapabilityDecl, ModuleManifest, ParamSchema};
use ep_core::pipeline::dag::Pipeline;
use ep_core::pipeline::load_pipeline;

use super::autostart::{self, AutoStartError};
use crate::api::err_response;
use crate::api::pipelines::pipeline_bridge::{spec_to_pipeline, PipelineSpec};
use crate::api::upload::{staging_id, store_input_file, UploadError};
use crate::state::AppState;

/// 路由（docs/API_DESIGN.md §运行资源）：
/// - `POST /pipelines/runs` — canonical 集合级 run 创建（body 带 pipeline_id
///   或内联 spec，跑未落盘管线）；`POST /pipelines/{pipeline_id}/runs`（路径
///   指名糖）在 pipelines.rs 注册；
/// - `POST /pipelines/execute`、`POST /execute/single` — 弃用别名（前者 =
///   集合级创建的旧名，后者 canonical 为 `POST /modules/{module_id}/runs`，
///   在 modules.rs 注册）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/pipelines/runs", post(execute_pipeline))
        .route("/pipelines/execute", post(execute_pipeline))
        .route("/execute/single", post(execute_single))
}

/// POST /api/pipelines/execute 请求体（前端 ExecutePipelineRequest 契约）；
/// pub(crate) 供 run 资源化包装器 [`execute_pipeline_run`] 复用
#[derive(Debug, Deserialize)]
pub(crate) struct ExecuteRequest {
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
    /// T-15 调度优先级（0-49 高优先，缺省 = 50；同优先级保持入队 FIFO）
    #[serde(default)]
    priority: Option<u8>,
    /// T-16 直跑缓存旁路（仅 /execute/single 消费；管线执行忽略）
    #[serde(default)]
    bypass_cache: bool,
    /// §6.8b force（T-22）：true 时跳过动态 VRAM 准入的排队等待直接执行；
    /// 任务元数据带 `forced_overcommit = true` + `accepted_risk = "user"`，
    /// 提交响应附带 force 附加字段（缺省/不传时响应形状零变化）
    #[serde(default)]
    force: bool,
}

/// POST /api/pipelines/{pipeline_id}/runs — 提交管线执行（run 资源化端点，
/// docs/API_DESIGN.md §运行资源；Airflow `POST /dags/{id}/dagRuns` 同型）。
///
/// `pipeline_id` 自路径注入：请求体缺省 `pipeline_id` 时自动补齐，同传时以
/// 路径为准（宽容旧客户端把旧字段一并带上）。请求体仍可内联 `spec` 跑未落盘
/// 管线？不允许——路径已指名管线，与内联 spec 并存沿用 both-given 400 语义。
/// 其余行为与弃用别名 `POST /api/pipelines/execute` 完全一致：202 +
/// `{"task_id"}`，`wait=true` 同步阻塞至终态，`callback_url` 终态回调。
pub(crate) async fn execute_pipeline_run(
    State(state): State<Arc<AppState>>,
    AxumPath(pipeline_id): AxumPath<String>,
    Json(mut req): Json<ExecuteRequest>,
) -> (StatusCode, Json<Value>) {
    req.pipeline_id = Some(pipeline_id);
    execute_pipeline(State(state), Json(req)).await
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

    // T-22：force 提交响应的各池超支投影需要模块表（提交会移走 pipeline，
    // 先行取好）
    let module_ids = execution::pipeline_module_ids(&pipeline);
    // §6.5：wait 同步模式与 callback_url 经完整选项版提交入口接线
    let options = execution::SubmitOptions {
        wait: req.wait.unwrap_or(false),
        callback_url: req.callback_url,
        group: None,
        priority: req.priority,
        bypass_cache: req.bypass_cache,
        force: req.force,
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
                let mut body = json!({
                    "task_id": outcome.task_id,
                    "status": record.status.as_str(),
                    "artifacts": artifacts,
                });
                apply_force_response_fields(&state, &mut body, req.force, &module_ids).await;
                (StatusCode::OK, Json(body))
            } else {
                let mut body = json!({ "task_id": outcome.task_id });
                apply_force_response_fields(&state, &mut body, req.force, &module_ids).await;
                (StatusCode::ACCEPTED, Json(body))
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
        // T-26：文件入口 `store:` 逻辑名引用解析失败——缺池/禁用 → 422
        //（§6.4 缺依赖同款"补配置"提示语义）；穿越/注入 → 400
        Err(execution::SubmitError::StoreRef(
            ep_core::config::StoreRefError::UnknownStore(name),
        )) => {
            err_response(
                &state,
                StatusCode::UNPROCESSABLE_ENTITY,
                "apiCore.store.notFound",
                &[("name", name)],
            )
            .await
        }
        Err(execution::SubmitError::StoreRef(
            ep_core::config::StoreRefError::StoreDisabled(name),
        )) => {
            err_response(
                &state,
                StatusCode::UNPROCESSABLE_ENTITY,
                "apiCore.store.disabled",
                &[("name", name)],
            )
            .await
        }
        Err(execution::SubmitError::StoreRef(ep_core::config::StoreRefError::UnsafeRel(raw))) => {
            err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiCore.store.unsafePath",
                &[("detail", raw)],
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
///
/// `pub(crate)`：inference.rs（v1 门面）复用同一校验语义，错误码映射
/// 各自负责（v1 用稳定机读码，不走 i18n）。
#[derive(Debug, PartialEq)]
pub(crate) enum ParamError {
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
pub(crate) fn validate_and_fill_params(
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

// ─── 直跑输入二选一（QUICK_RUN_PLAN D2：input_text 文本输入） ────────────────

/// 直跑输入来源（纯数据，单测直接断言；handler 据此分流落位）
#[derive(Debug, PartialEq)]
pub(crate) enum SingleInput {
    /// 服务器已有文件路径（既有链路，行为不变）
    Path(PathBuf),
    /// 文本内容（UTF-8 原文；handler 物化为 uploads 下 .txt 后走同一文件链路）
    Text(String),
}

/// 二选一校验失败（handler 映射 400 + 既有 i18n 键，响应形状不变）
#[derive(Debug, PartialEq)]
pub(crate) enum SingleInputError {
    /// 均缺失（含空白串）→ `apiPipelines.single.missingInputPath`（与旧契约一致）
    Missing,
    /// 同时给出 → 400（复用既有互斥键位）
    BothProvided,
}

/// 空白串视为未给出（对齐既有 `input_path` 非空校验口径）
fn single_input_present(raw: Option<&str>) -> bool {
    raw.map(str::trim).filter(|s| !s.is_empty()).is_some()
}

/// 调度优先级统一解析口径（T-22，执行报告 §7.9 的 T-15 follow-up）：
/// 4 个提交入口共用——宽容语义，数值可无损表示为 u8 才采用，任何失败
/// （缺失/非数字/越界）→ `None` = 按缺省优先级 50 调度，绝不因 priority
/// 字段拒绝请求。
///
/// 各入口的介质适配：
/// - JSON 数值路径（本函数直收 `&Value`，execute_single）：`as_u64` 后核域；
/// - 表单文本路径（inference multipart）：`str::parse::<u64>` 后调本 helper
///   的核心 [`parse_priority_u64`]（`parse::<u8>` 与「u64 解析 + u8 核域」
///   对一切输入等价）；
/// - serde 强类型路径（`ExecuteRequest` / `InferenceJsonRequest` 的
///   `#[serde(default)] priority: Option<u8>`）：0-255 域由反序列化强制，
///   越域整体 400——口径本就严格，不经本 helper（行为保持不变）。
pub(crate) fn parse_priority(value: Option<&Value>) -> Option<u8> {
    parse_priority_u64(value.and_then(Value::as_u64))
}

/// [`parse_priority`] 的数值核心（文本路径先 parse::<u64> 再复用）。
pub(crate) fn parse_priority_u64(raw: Option<u64>) -> Option<u8> {
    raw.and_then(|v| u8::try_from(v).ok())
}

// ─── T-54：dry-run 预演 + 提交幂等键（Agent 平台链第一环） ──────────────────

/// `idempotency_key` 最大长度（字符数，T-54）：超长 → 400，防无界键撑爆
/// 进程内侧表。
pub(crate) const IDEMPOTENCY_KEY_MAX_LEN: usize = 128;

/// `idempotency_key` 字段统一解析（T-54，提交系三个入口共用——
/// execute/single 请求体、v1 JSON 请求体、v1 multipart 文本字段）：
/// 缺失/空白串 = 未指定（`Ok(None)`，宽容口径与其余可选字段一致）；
/// 超 [`IDEMPOTENCY_KEY_MAX_LEN`] 字符 → `Err(技术细节)`（handler 映射 400；
/// v1 门面映射 INPUT_INVALID）。字符数按 `chars().count()` 计（CJK 友好）。
pub(crate) fn validate_idempotency_key(raw: Option<&str>) -> Result<Option<String>, String> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(key) if key.chars().count() > IDEMPOTENCY_KEY_MAX_LEN => Err(format!(
            "`idempotency_key` exceeds the maximum length of {IDEMPOTENCY_KEY_MAX_LEN} characters"
        )),
        Some(key) => Ok(Some(key.to_string())),
    }
}

/// dry-run 计划摘要组装（T-54；`pub(crate)`：inference.rs v1 门面复用同一形状）。
///
/// **纯读零副作用**——取数路径复用 vram-budget（§6.3）同一数据源：
/// - 变体解析 = `config.active_models` → manifest `default=true` → 首个
///   （§5.2 单槽位三级回退的预算口径，与 pipelines.rs vram_budget 一致）；
/// - 显存/内存 = [`ep_core::module::manifest::ModuleManifest::resolve_vram_estimate`]
///   / `resolve_ram_estimate`（变体级优先、模块级兜底；未声明则字段缺省不出）；
/// - 设备 = 显式 hint（调用方已硬校验，原样透出）或 [`select_module_device`]
///   （D-4 调度器统一选择，与真实启动路径同一入口）；
/// - `input` 描述符由调用方按输入介质给：`{"kind":"path","path":…}` /
///   `{"kind":"text","chars":…}` / `{"kind":"file","name":…}`。
pub(crate) async fn build_dry_run_plan(
    state: &Arc<AppState>,
    module_id: &str,
    capability: &str,
    resolved_params: Value,
    device_hint: Option<&str>,
    input: Value,
) -> Value {
    let manifest = find_module_manifest(state, module_id)
        .await
        .expect("dry-run 前置校验已确认 manifest 存在");
    let variant = {
        let cfg = state.config.read().await;
        cfg.active_models
            .get(module_id)
            .cloned()
            .or_else(|| {
                manifest
                    .models
                    .iter()
                    .find(|m| m.default)
                    .or(manifest.models.first())
                    .map(|m| m.id.clone())
            })
    };
    let (vram_mb, ram_mb) = match &variant {
        Some(v) => (
            manifest.resolve_vram_estimate(v),
            manifest.resolve_ram_estimate(v),
        ),
        None => (None, None),
    };
    let (device, device_source) = match device_hint {
        Some(hint) => (hint.to_string(), "explicit"),
        None => (
            super::select_module_device(state, &manifest).await.to_string(),
            "auto",
        ),
    };
    let mut plan = json!({
        "module_id": module_id,
        "capability": capability,
        "params": resolved_params,
        "device": device,
        "device_source": device_source,
        "input": input,
    });
    if let Some(v) = variant {
        plan["model_variant"] = json!(v);
    }
    if let Some(mb) = vram_mb {
        plan["vram_estimate_mb"] = json!(mb);
    }
    if let Some(mb) = ram_mb {
        plan["ram_estimate_mb"] = json!(mb);
    }
    plan
}

/// `input_path` / `input_text` 二选一判定（D2，纯函数单测直接断言）：
/// 均缺失或同时给出 → [`SingleInputError`]；文本原样保留（不做 trim）。
pub(crate) fn choose_single_input(
    input_path: Option<&str>,
    input_text: Option<&str>,
) -> Result<SingleInput, SingleInputError> {
    match (
        single_input_present(input_path),
        single_input_present(input_text),
    ) {
        (false, false) => Err(SingleInputError::Missing),
        (true, true) => Err(SingleInputError::BothProvided),
        (true, false) => Ok(SingleInput::Path(PathBuf::from(
            input_path.unwrap_or_default(),
        ))),
        (false, true) => Ok(SingleInput::Text(
            input_text.unwrap_or_default().to_string(),
        )),
    }
}

/// D2：文本输入物化 —— 先写系统临时目录，再经共享助手
/// [`store_input_file`] 落盘 `workspace/uploads/<module_id>-<capability>-<unix_ts>.txt`
/// （UTF-8 原文），与 v1 门面（inference.rs submit_json）同一落盘口径；
/// 重名由 place_input_file 追加序号消解。返回最终路径供既有 input_path 链路流转。
async fn materialize_input_text(
    state: &Arc<AppState>,
    module_id: &str,
    capability: &str,
    text: &str,
) -> Result<PathBuf, UploadError> {
    // 暂存目录/命名对齐 inference.rs 先例（staging_id：纳秒 + 序号 + pid）
    let temp_dir = std::env::temp_dir().join(format!("ep-execute-single-{}", std::process::id()));
    if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
        return Err(UploadError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            key: "apiModels.uploadStagingFailed",
            params: vec![("detail", e.to_string())],
        });
    }
    let temp_path = temp_dir.join(format!("single-text-{}.txt", staging_id()));
    let stored = match tokio::fs::write(&temp_path, text).await {
        Ok(()) => {
            let ts = chrono::Utc::now().timestamp();
            store_input_file(
                state,
                &temp_path,
                &format!("{module_id}-{capability}-{ts}.txt"),
            )
            .await
        }
        Err(e) => Err(UploadError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            key: "apiModels.uploadWriteFailed",
            params: vec![("detail", e.to_string())],
        }),
    };
    // 落盘成败与否都清理暂存文件（inference.rs 同款收尾）
    let _ = tokio::fs::remove_file(&temp_path).await;
    stored
}

/// 按 module_id 查找模块清单
///
/// `pub(crate)`：inference.rs（v1 门面）共用同一查找口径。
pub(crate) async fn find_module_manifest(state: &AppState, module_id: &str) -> Option<ModuleManifest> {
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

/// POST /api/modules/{module_id}/runs — 单模型直跑（run 资源化端点，
/// docs/API_DESIGN.md §运行资源）。`module_id` 自路径注入：请求体缺省时
/// 补齐，同传时以路径为准。其余请求体字段与语义和弃用别名
/// `POST /api/execute/single` 完全一致。
pub(crate) async fn execute_single_run(
    State(state): State<Arc<AppState>>,
    AxumPath(module_id): AxumPath<String>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    let mut v = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    v["module_id"] = json!(module_id);
    execute_single(State(state), Some(Json(v))).await
}

/// POST /api/execute/single — 单模型直跑（§5.3）
///
/// 请求体：`{module_id, capability, params?, input_path?, input_text?, lazy_start?}`
///（QUICK_RUN_PLAN §5.1 仅增可选字段，向后兼容）。
/// 流程：字段/模块/capability/参数/输入校验 → 模块未运行则自动拉起并等健康
///（[`autostart::ensure_module_running`]，修 P1-2 直跑侧）→
/// [`execution::submit_direct`]（B3：退化三节点 DAG）→ 202 `{"task_id"}`。
///
/// - `input_path` / `input_text` 二选一（D2）：均缺失或同传 → 400；
///   文本经 [`materialize_input_text`] 落盘 uploads 后走既有文件链路，
///   `build_direct_pipeline` 零改动；
/// - `lazy_start = true` 时跳过提交前同步预启动（D3）：其余校验照旧同步
///   快失败，模块拉起改由任务内 `ensure_pipeline_modules` 幂等安全网完成，
///   失败计入任务错误；缺省 false 与现状完全一致。
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
    // input_path / input_text 二选一（D2）：均缺失/同传 → 400（沿用既有
    // 响应形状；判定位置与旧版 missingInputPath 同步——先于模块查找，旧客户端
    // 错误优先级不变）。文本物化延后到校验全部通过后，非法请求不留垃圾文件。
    let input_choice = match choose_single_input(
        body.get("input_path").and_then(|v| v.as_str()),
        body.get("input_text").and_then(|v| v.as_str()),
    ) {
        Ok(choice) => choice,
        Err(SingleInputError::Missing) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.missingInputPath",
                &[],
            )
            .await
        }
        Err(SingleInputError::BothProvided) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.inputConflict",
                &[],
            )
            .await
        }
    };
    // lazy_start（D3）：true = 跳过提交前同步预启动；缺省 false 与现状一致。
    // 非 bool 值宽容按缺省处理（与本 handler 其余可选字段的宽松口径一致）。
    let lazy_start = body
        .get("lazy_start")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // device（D-Device 完整版）：显式指定算力源（"cuda:0"/"rocm:0"/"cpu"…，
    // 空串与 "auto" 视为未指定）。值语义校验在 manifest 解析后（需后端信息）。
    let device_hint: Option<String> = body
        .get("device")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
        .map(str::to_string);
    // priority（T-15）：可选调度优先级；缺省 50。非数字/越界宽容按缺省处理
    // （与本 handler 其余可选字段的宽松口径一致；解析口径统一走 parse_priority）
    let priority: Option<u8> = parse_priority(body.get("priority"));
    // bypass_cache（T-16）：true 时跳过直跑缓存的读取与写入；缺省 false
    let bypass_cache = body
        .get("bypass_cache")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // §6.8b force（T-22）：true 时跳过动态 VRAM 准入排队等待；缺省 false。
    // 非 bool 值宽容按缺省处理（与本 handler 其余可选 bool 字段口径一致）
    let force = body.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    // T-54 dry-run：true = 全部校验通过后不入队，返回「将执行的计划」摘要
    //（零副作用：不建任务、不占端口、不拉模块、文本输入不物化）。
    let dry_run = body
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // T-54 提交幂等键（≤128 字符）：超长必须快失败 400（此字段不宽容——
    // 静默截断/忽略会破坏 Agent 重试安全语义）
    let idempotency_key = match validate_idempotency_key(
        body.get("idempotency_key").and_then(|v| v.as_str()),
    ) {
        Ok(key) => key,
        Err(_) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.single.idempotencyKeyTooLong",
                &[("max", IDEMPOTENCY_KEY_MAX_LEN.to_string())],
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

    // ── 4. 设备指定硬校验（D-Device）：非法 hint 快速失败 400，不占闸门 ──
    if let Some(hint) = device_hint.as_deref() {
        if let Err(msg) =
            super::resolve_device_hint(&state, &manifest, Some(hint)).await
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("设备指定非法：{msg}") })),
            );
        }
    }

    // ── 4.5 幂等键查重（T-54）：字段/模块/capability/参数/设备校验全部通过
    //    后查进程内侧表，命中 → 直接重放首个 task_id（零副作用：不物化输入、
    //    不拉起模块、不入队；wait 同步语义亦不适用于重放——立即返回）。 ──
    if let Some(key) = &idempotency_key {
        if let Some(first) = execution::idempotency_lookup(key) {
            return (
                StatusCode::ACCEPTED,
                Json(json!({ "task_id": first, "idempotent_replay": true })),
            );
        }
    }

    // ── 4.6 dry-run 预演（T-54）：校验全部通过 → 返回「将执行的计划」摘要
    //    并就此打住（不入队零副作用）。Path 输入的存在性校验属纯读，照常
    //    执行（预演同样要求输入真实可用）；Text 输入不物化。 ──
    if dry_run {
        let input_desc = match &input_choice {
            SingleInput::Path(p) => {
                // T-26：store: 逻辑名解析 + 存在性（与步骤 5 同一校验，纯读）
                let resolved = match super::stores::resolve_entry_path(
                    &state,
                    &p.to_string_lossy(),
                )
                .await
                {
                    Ok(path) => path,
                    Err(resp) => return resp,
                };
                if !resolved.is_file() {
                    return err_response(
                        &state,
                        StatusCode::BAD_REQUEST,
                        "apiPipelines.single.inputNotFound",
                        &[("path", resolved.display().to_string())],
                    )
                    .await;
                }
                json!({ "kind": "path", "path": resolved.display().to_string() })
            }
            SingleInput::Text(text) => {
                json!({ "kind": "text", "chars": text.chars().count() })
            }
        };
        let plan = build_dry_run_plan(
            &state,
            &module_id,
            &capability,
            params,
            device_hint.as_deref(),
            input_desc,
        )
        .await;
        return (
            StatusCode::OK,
            Json(json!({ "would_submit": true, "dry_run": true, "plan": plan })),
        );
    }

    // ── 5. 输入落位：Path 校验既有存在性；Text 物化为 uploads 下 .txt（D2）──
    let input_path = match &input_choice {
        SingleInput::Path(p) => {
            // T-26：`store:<name>/<相对路径>` 逻辑名引用先解析为物理路径
            //（缺池/禁用 → 422、注入 → 400）；普通物理路径原样透传（向后兼容）
            let resolved = match super::stores::resolve_entry_path(&state, &p.to_string_lossy())
                .await
            {
                Ok(path) => path,
                Err(resp) => return resp,
            };
            // 输入文件必须已存在于服务器本地（含 workspace/uploads 暂存）
            if !resolved.is_file() {
                return err_response(
                    &state,
                    StatusCode::BAD_REQUEST,
                    "apiPipelines.single.inputNotFound",
                    &[("path", resolved.display().to_string())],
                )
                .await;
            }
            resolved
        }
        SingleInput::Text(text) => {
            match materialize_input_text(&state, &module_id, &capability, text).await {
                Ok(path) => path,
                Err(ue) => return err_response(&state, ue.status, ue.key, &ue.params).await,
            }
        }
    };

    // ── 6. 模块自动拉起（§6.5：未运行 → 启动并等健康；超时计入任务错误语义）
    //    D3：lazy_start=true 跳过本步——拉起改由任务准入后 ensure_pipeline_modules
    //    幂等安全网完成，冷启动全程任务可见可追踪，失败计入任务错误。
    //    D-Device：预启动带显式设备指定（已硬校验）。 ──
    if lazy_start {
        info!(module_id = %module_id, "lazy_start=true：跳过提交前预启动，模块拉起由任务内安全网完成");
    } else if let Err(e) =
        autostart::ensure_module_running_on_device(&state, &module_id, device_hint.as_deref()).await
    {
        return autostart_error_response(&state, e).await;
    }

    // ── 7. 提交退化三节点 DAG（B3 实现；任务/产物/WS 全套复用） ──
    match execution::submit_direct_full(
        &state,
        &module_id,
        &capability,
        params,
        input_path,
        device_hint.as_deref(),
        execution::SubmitOptions {
            priority,
            bypass_cache,
            force,
            ..Default::default()
        },
    )
    .await
    {
        Ok(outcome) => {
            // T-54：提交成功（任务已创建）即登记幂等键（先到先得；wait 模式
            // 亦在此登记——重放只要求返回首个 task_id，不重放等待）。
            // Err = 并发同键竞态下首个任务已在案，本任务照常执行。
            if let Some(key) = &idempotency_key {
                let _ = execution::idempotency_register(key, &outcome.task_id);
            }
            let mut body = json!({ "task_id": outcome.task_id });
            apply_force_response_fields(&state, &mut body, force, &[module_id]).await;
            (StatusCode::ACCEPTED, Json(body))
        }
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

/// §6.8b force 提交响应附加字段（T-22；`pub(crate)`：inference.rs v1 门面
/// 复用同一响应形状）。
///
/// 仅 `force = true` 时写入——缺省/不传 force 的响应字节级不变（硬验收）：
/// - `forced_overcommit: true` + `accepted_risk: "user"`：提交模式元数据；
/// - `overcommit_pools`：各池超支明细（复用 T-20 pools 形状，
///   `execution::forced_overcommit_pools` 投影），无超支时字段缺省不出。
pub(crate) async fn apply_force_response_fields(
    state: &Arc<AppState>,
    body: &mut Value,
    force: bool,
    module_ids: &[String],
) {
    if !force {
        return;
    }
    body["forced_overcommit"] = json!(true);
    body["accepted_risk"] = json!(execution::FORCE_ACCEPTED_RISK);
    let pools = execution::forced_overcommit_pools(state, module_ids).await;
    if !pools.is_empty() {
        if let Ok(v) = serde_json::to_value(&pools) {
            body["overcommit_pools"] = v;
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
        }        AutoStartError::PortAllocationFailed(detail) => {
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
        AutoStartError::DeviceHint(msg) => {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("设备指定非法：{msg}") })),
            )
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
        // 仅在真终态返回（queued/running 均继续等）：提交路径的准入已
        // spawn 化（QUICK_RUN_PLAN G4），202 之后任务可能短暂处于 queued，
        // 首拍即返会误判。
        for _ in 0..300 {
            if let Some(record) = execution::snapshot(task_id) {
                if record.status.is_terminal() {
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
        assert_eq!(
            status,
            StatusCode::ACCEPTED,
            "unexpected submit status, body={}",
            body.0
        );
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

    // ── 11. D2/D3：choose_single_input 纯函数单测（二选一判定） ─────────────

    #[test]
    fn test_choose_single_input_classification() {
        // 仅 input_path → Path（原样，不 trim 值本身）
        assert_eq!(
            choose_single_input(Some("/a/b.txt"), None).unwrap(),
            SingleInput::Path(PathBuf::from("/a/b.txt"))
        );
        // 仅 input_text → Text（UTF-8 原文保留，不做 trim）
        assert_eq!(
            choose_single_input(None, Some("  你好 world\n")).unwrap(),
            SingleInput::Text("  你好 world\n".to_string())
        );
        // 均缺失 → Missing
        assert_eq!(
            choose_single_input(None, None).unwrap_err(),
            SingleInputError::Missing
        );
        // 空白串等价缺失（对齐旧版 input_path 非空校验口径）
        assert_eq!(
            choose_single_input(Some(""), None).unwrap_err(),
            SingleInputError::Missing
        );
        assert_eq!(
            choose_single_input(None, Some("   ")).unwrap_err(),
            SingleInputError::Missing
        );
        // 同传 → BothProvided
        assert_eq!(
            choose_single_input(Some("/a"), Some("b")).unwrap_err(),
            SingleInputError::BothProvided
        );
        // 一侧为空白串不算"给出"，另一侧有效值正常放行
        assert_eq!(
            choose_single_input(Some("  "), Some("b")).unwrap(),
            SingleInput::Text("b".to_string())
        );
        assert_eq!(
            choose_single_input(Some("/a"), Some("")).unwrap(),
            SingleInput::Path(PathBuf::from("/a"))
        );
    }

    // ── 12. D2：双输入缺失 / 同传 → 400 ─────────────────────────────────────

    #[tokio::test]
    async fn test_single_both_inputs_missing_400() {
        let state = direct_test_state(unique_root("s-noinput-either"));
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        // 双缺 → 400，文案与旧契约逐字节一致（向后兼容证明）
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 }
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "缺少 input_path 字段");

        // 空白串同样视为缺失
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(direct_test_state(unique_root("s-blankpath")));
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": "   "
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "缺少 input_path 字段");

        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(direct_test_state(unique_root("s-blanktext")));
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_text": ""
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "缺少 input_path 字段");
    }

    #[tokio::test]
    async fn test_single_both_inputs_provided_400() {
        let state = direct_test_state(unique_root("s-bothinputs"));
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        let resp = app
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": "/tmp/x.txt",
                "input_text": "hello"
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // D2 专属互斥文案（W3 冒烟修正：不再借用管线端点的
        // execute.mutuallyExclusive，避免「pipeline_id 与 spec…」误导）
        assert_eq!(json["error"], "input_path 与 input_text 只能提供其一");
    }

    // ── 13. D2：input_text 物化成功（uploads 落盘 + 任务走该路径） ──────────

    #[tokio::test]
    async fn test_single_input_text_materializes_to_uploads_and_submits() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("s-textin");
        let state = direct_test_state(root.clone());

        let text = "快速调用文本 hello";
        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state.clone())
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_text": text,
                "lazy_start": true
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED, "响应: {body}");
        let task_id = body["task_id"].as_str().unwrap().to_string();

        // workspace/uploads 下生成 <module_id>-<capability>-<unix_ts>.txt，
        // 内容与 UTF-8 原文逐字节一致
        let uploads = root.join("workspace").join("uploads");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&uploads)
            .unwrap_or_else(|e| panic!("uploads 目录应已创建: {e}"))
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("direct-mod-run-") && name.ends_with(".txt") {
                assert_eq!(
                    std::fs::read_to_string(entry.path()).unwrap(),
                    text,
                    "物化内容必须保留 UTF-8 原文"
                );
                found.push(name);
            }
        }
        assert_eq!(found.len(), 1, "应恰好物化一个文本文件，实际: {found:?}");

        // 任务提交走该路径：直跑命名空间入册（direct/<module_id>）
        let record = execution::snapshot(&task_id).expect("任务应已入册");
        assert_eq!(record.pipeline_id, "direct/direct-mod");

        // lazy_start：任务内安全网拉起失败（端口无服务）→ Failed，
        // 冷启动失败全程任务可见可追踪（D3 预期语义）
        let record = wait_terminal(&task_id).await.expect("任务应终结");
        assert_eq!(record.status, execution::TaskState::Failed);
    }

    // ── 14. D3：lazy_start=true 跳过提交前同步预启动 ─────────────────────────

    /// 懒启动测试模块清单：与 direct_manifest_toml 同构（id=lazy-mod），
    /// 但声明了 target_dir 永不存在的默认模型变体 → 提交前预启动必然
    /// 409 ModelNotReady（确定性失败面，且不会拉起任何进程）。
    fn lazy_manifest_toml() -> String {
        let keepalive = if cfg!(target_os = "windows") {
            "ping -n 30 127.0.0.1 > NUL"
        } else {
            "sleep 30"
        };
        format!(
            r#"
[module]
id = "lazy-mod"
name = "懒启动测试模块"
version = "0.1.0"
description = "lazy start test module"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = {{ "test" = "test" }}
start_command = "{keepalive}"

[compute]
backends = ["cpu"]

[[models]]
id = "never-shipped"
name = "未下载模型变体"
source = "url"
url = "auto"
target_dir = "never-downloaded-lazy"
default = true

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

    fn lazy_test_state(root: std::path::PathBuf) -> Arc<AppState> {
        let manifest: ModuleManifest = toml::from_str(&lazy_manifest_toml()).unwrap();
        let module = ep_core::module::discovery::DiscoveredModule {
            path: root.join("modules").join("lazy-mod"),
            manifest: Some(manifest),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        };
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![module],
            // 独立区间，避开 direct 组（48330-48340）
            PortManager::new(48350, 48360),
        ))
    }

    #[tokio::test]
    async fn test_single_lazy_start_skips_preflight_autostart() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("s-lazy");
        let input = root.join("in.txt");
        std::fs::write(&input, "data").unwrap();

        let state = lazy_test_state(root.clone());

        // 对照组：缺省（false）→ 同步预启动生效，模型未就绪 → 409
        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state.clone())
            .oneshot(single_request(json!({
                "module_id": "lazy-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": input.display().to_string()
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::CONFLICT, "响应: {json}");
        assert!(json["error"].as_str().unwrap().contains("模型未就绪"));

        // 实验组：lazy_start=true → 跳过预启动，其余校验照旧 → 202 入队
        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state.clone())
            .oneshot(single_request(json!({
                "module_id": "lazy-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": input.display().to_string(),
                "lazy_start": true
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED, "响应: {json}");
        let task_id = json["task_id"].as_str().unwrap().to_string();

        // "跳过"的行为学证明：同步路径上既无进程实例也无端口占用
        assert!(state
            .process_manager
            .read()
            .await
            .get_instance("lazy-mod")
            .is_none());
        assert!(state
            .port_manager
            .read()
            .await
            .get_port("lazy-mod")
            .is_none());

        // 任务已入册；模块拉起由任务内安全网完成并失败 → Failed
        //（冷启动失败计入任务错误语义，而非请求报错）
        let record = execution::snapshot(&task_id).expect("任务应已入册");
        assert_eq!(record.pipeline_id, "direct/lazy-mod");
        let record = wait_terminal(&task_id).await.expect("任务应终结");
        assert_eq!(record.status, execution::TaskState::Failed);
    }

    // ── 15. D-Device：显式算力源指定（快速调用算力源切换） ─────────────────────

    /// 带设备快照的测试态：devices = [CPU]（direct_manifest_toml backends 即 cpu）。
    fn device_test_state(root: std::path::PathBuf) -> Arc<AppState> {
        let manifest: ModuleManifest = toml::from_str(&direct_manifest_toml()).unwrap();
        let module = ep_core::module::discovery::DiscoveredModule {
            path: root.join("modules").join("direct-mod"),
            manifest: Some(manifest),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        };
        let cpu = ep_core::types::ComputeDevice {
            id: ep_core::types::DeviceId::Cpu,
            backend: ep_core::types::ComputeBackend::Cpu,
            name: "CPU".to_string(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        };
        let rocm = ep_core::types::ComputeDevice {
            id: ep_core::types::DeviceId::Rocm(0),
            backend: ep_core::types::ComputeBackend::Rocm,
            name: "AMD RX 7900".to_string(),
            total_memory_mb: None,
            used_memory_mb: None,
            utilization: None,
            temperature: None,
        };
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![cpu, rocm],
            vec![module],
            PortManager::new(48360, 48370),
        ))
    }

    /// 非法设备（不存在/不在线）→ 400，且不占闸门、无需预启动
    #[tokio::test]
    async fn test_single_device_hint_unknown_400() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();
        let state = device_test_state(unique_root("d-hint-unknown"));
        let input = state.root.join("in.txt");
        std::fs::write(&input, "data").unwrap();
        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state)
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": input.display().to_string(),
                "device": "cuda:0"
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "响应: {json}");
        assert!(json["error"].as_str().unwrap().contains("cuda:0"));
    }

    /// 兼容性硬校验：hint 指向的设备后端与 manifest backends 不兼容 → 400
    #[tokio::test]
    async fn test_single_device_hint_incompatible_400() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();
        // 单测态设备池加 cuda 设备（manifest 只声明 cpu）→ 不兼容命中
        let state = device_test_state(unique_root("d-hint-incompat"));
        let input = state.root.join("in.txt");
        std::fs::write(&input, "data").unwrap();
        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state)
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": input.display().to_string(),
                "device": "rocm:0"
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "响应: {json}");
        assert!(json["error"].as_str().unwrap().contains("不兼容"));
    }

    /// 兼容 hint + lazy_start → 202（hint 校验通过并随任务流转）
    #[tokio::test]
    async fn test_single_device_hint_compatible_202() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();
        let state = device_test_state(unique_root("d-hint-ok"));
        let input = state.root.join("in.txt");
        std::fs::write(&input, "data").unwrap();
        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state)
            .oneshot(single_request(json!({
                "module_id": "direct-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": input.display().to_string(),
                "lazy_start": true,
                "device": "cpu"
            })))
            .await
            .unwrap();
        let (status, json) = single_response(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED, "响应: {json}");
        let task_id = json["task_id"].as_str().unwrap().to_string();
        let record = execution::snapshot(&task_id).expect("任务应已入册");
        assert_eq!(record.pipeline_id, "direct/direct-mod");
    }

    // ── §6.8b force 提交（T-22）：响应附加字段仅 force 请求出现 ─────────────

    /// 不传 force → 202 响应恰为 `{"task_id"}`（逐字节回归锚点：无任何新字段）
    #[tokio::test]
    async fn test_execute_without_force_response_is_unchanged() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("noforce");
        let src = root.join("nf-in.txt");
        let dest = root.join("nf-out.txt");
        std::fs::write(&src, "no force").unwrap();
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("copy.toml"),
            r#"[pipeline]
id = "nf-pipe"
name = "无 force 管线"

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
                "pipeline_id": "nf-pipe",
                "inputs": {
                    "input": { "path": src.display().to_string() },
                    "output": { "path": dest.display().to_string() }
                }
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            body.0,
            json!({ "task_id": body.0["task_id"] }),
            "不传 force 的响应不得出现 force 相关新字段: {}",
            body.0
        );
        assert!(body.0.get("forced_overcommit").is_none());
        assert!(body.0.get("accepted_risk").is_none());
        assert!(body.0.get("overcommit_pools").is_none());
    }

    /// force=true → 202 带 forced_overcommit + accepted_risk；无超支时不带
    /// overcommit_pools（纯 builtin 管线无模块预算，必然无池超支）
    #[tokio::test]
    async fn test_execute_with_force_response_fields() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("force-ok");
        let src = root.join("f-in.txt");
        let dest = root.join("f-out.txt");
        std::fs::write(&src, "force submit").unwrap();
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("copy.toml"),
            r#"[pipeline]
id = "f-pipe"
name = "force 管线"

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
                "pipeline_id": "f-pipe",
                "force": true,
                "inputs": {
                    "input": { "path": src.display().to_string() },
                    "output": { "path": dest.display().to_string() }
                }
            }))),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body.0["forced_overcommit"], json!(true));
        assert_eq!(body.0["accepted_risk"], "user");
        assert!(
            body.0.get("overcommit_pools").is_none(),
            "无模块预算 → 无超支池 → 字段缺省不出: {}",
            body.0
        );
        // 任务元数据已登记（任务详情可见）
        let task_id = body.0["task_id"].as_str().unwrap().to_string();
        assert!(execution::task_forced_overcommit(&task_id));
    }

    // ── priority 统一解析 helper（T-22 §7.9 follow-up）：宽容口径行为不变 ────

    #[test]
    fn parse_priority_lenient_semantics_unchanged() {
        use super::{parse_priority, parse_priority_u64};
        // 有效域 0..=255
        assert_eq!(parse_priority(Some(&json!(10))), Some(10));
        assert_eq!(parse_priority(Some(&json!(0))), Some(0));
        assert_eq!(parse_priority(Some(&json!(255))), Some(255));
        // 越界 / 非数字 / 负数 / 字符串 / 缺失 → None（缺省 50，不拒绝请求）
        assert_eq!(parse_priority(Some(&json!(256))), None);
        assert_eq!(parse_priority(Some(&json!(-1))), None);
        assert_eq!(parse_priority(Some(&json!("5"))), None);
        assert_eq!(parse_priority(Some(&json!(null))), None);
        assert_eq!(parse_priority(None), None);
        // u64 核心（multipart 文本路径）：parse::<u64> 成功后同一核域
        assert_eq!(parse_priority_u64(Some(50)), Some(50));
        assert_eq!(parse_priority_u64(Some(300)), None);
        assert_eq!(parse_priority_u64(None), None);
    }

    // ── T-54：dry-run 预演（/execute/single 与 /modules/{id}/runs 共用） ────

    /// dry-run 专用模块清单：带默认模型变体（变体级 vram/ram 声明）+
    /// 模块级 vram 兜底，供计划摘要的取数路径断言；ready_timeout_secs=1
    /// 让真实提交路径的失败快速终结（幂等测试复用）。
    fn dryrun_manifest_toml() -> String {
        r#"
[module]
id = "dryrun-mod"
name = "dry-run 测试模块"
version = "0.1.0"
description = "dry run test module"
category = "asr"
genre = "test"

[runtime]
type = "native"
binaries = { "test" = "test" }
start_command = "definitely-not-a-real-command"

[compute]
backends = ["cpu"]
vram_estimate_mb = 2048

[[models]]
id = "base"
name = "Base"
source = "url"
url = "auto"
target_dir = "never-downloaded-dryrun"
default = true
vram_estimate_mb = 1024
ram_estimate_mb = 256

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
beam_size = { type = "integer", min = 1, max = 20 }
language = { type = "string", default = "auto" }
"#
        .to_string()
    }

    fn dryrun_test_state(root: std::path::PathBuf) -> Arc<AppState> {
        let manifest: ModuleManifest = toml::from_str(&dryrun_manifest_toml()).unwrap();
        let module = ep_core::module::discovery::DiscoveredModule {
            path: root.join("modules").join("dryrun-mod"),
            manifest: Some(manifest),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        };
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![module],
            // 独立区间（避开 direct 48330-48340 / lazy 48350-48360 / device 48360-48370）
            PortManager::new(48400, 48410),
        ))
    }

    /// dry_run=true：校验通过 → 200 计划摘要（would_submit + plan 字段完整，
    /// 显存取数复用 vram-budget 数据路径），且零副作用（无新任务记录、无进程
    /// 实例、无端口占用）
    #[tokio::test]
    async fn test_single_dry_run_returns_plan_without_side_effects() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("dryrun-plan");
        let input = root.join("in.txt");
        std::fs::write(&input, "data").unwrap();
        let state = dryrun_test_state(root.clone());
        let tasks_before = execution::snapshot_all().len();

        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state.clone())
            .oneshot(single_request(json!({
                "module_id": "dryrun-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": input.display().to_string(),
                "dry_run": true
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::OK, "响应: {body}");
        assert_eq!(body["would_submit"], json!(true));
        assert_eq!(body["dry_run"], json!(true));

        let plan = &body["plan"];
        assert_eq!(plan["module_id"], "dryrun-mod");
        assert_eq!(plan["capability"], "run");
        // params 为校验 + 默认值注入后的最终形态
        assert_eq!(plan["params"]["beam_size"], 5);
        assert_eq!(plan["params"]["language"], "auto");
        // 设备：devices 快照为空 → select_module_device CPU 兜底；auto 来源
        assert_eq!(plan["device"], "cpu");
        assert_eq!(plan["device_source"], "auto");
        // 变体解析（active_models 缺省 → default 变体）+ 显存/内存预估
        assert_eq!(plan["model_variant"], "base");
        assert_eq!(plan["vram_estimate_mb"], 1024, "变体级声明优先于模块级 2048");
        assert_eq!(plan["ram_estimate_mb"], 256);
        // 输入描述符（path 介质带解析后路径）
        assert_eq!(plan["input"]["kind"], "path");
        assert_eq!(
            plan["input"]["path"],
            json!(input.display().to_string())
        );

        // 零副作用：无新任务记录、无进程实例、无端口占用
        assert_eq!(
            execution::snapshot_all().len(),
            tasks_before,
            "dry-run 不得创建任务记录"
        );
        assert!(state
            .process_manager
            .read()
            .await
            .get_instance("dryrun-mod")
            .is_none());
        assert!(state
            .port_manager
            .read()
            .await
            .get_port("dryrun-mod")
            .is_none());
    }

    /// dry_run=true 跳过模块自动拉起：模型未就绪的模块（真实提交会 409
    /// ModelNotReady）预演仍 200——行为学证明预演不含拉起步骤
    #[tokio::test]
    async fn test_single_dry_run_skips_autostart() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("dryrun-noautostart");
        let input = root.join("in.txt");
        std::fs::write(&input, "data").unwrap();
        let state = dryrun_test_state(root.clone());

        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state.clone())
            .oneshot(single_request(json!({
                "module_id": "dryrun-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": input.display().to_string(),
                "dry_run": true
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::OK, "预演不得触发拉起: {body}");
        assert!(state
            .process_manager
            .read()
            .await
            .get_instance("dryrun-mod")
            .is_none());
    }

    /// dry_run=true 时校验失败照常 400：缺必填参数 / 输入文件不存在
    #[tokio::test]
    async fn test_single_dry_run_validation_failure_still_400() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("dryrun-badval");
        let state = dryrun_test_state(root.clone());
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        // 缺必填参数（beam_size 无默认）→ 400（先于 dry-run 分支）
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "dryrun-mod",
                "capability": "run",
                "input_path": "/tmp/x.txt",
                "dry_run": true
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "缺少必填参数 'beam_size'");

        // 输入文件不存在 → 400（dry-run 分支内的纯读存在性校验）
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(dryrun_test_state(unique_root("dryrun-badinput")));
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "dryrun-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_path": "/definitely/not/here.txt",
                "dry_run": true
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("/definitely/not/here.txt")
        );
    }

    /// dry_run=true + input_text：文本不物化（uploads 目录零新增文件——
    /// 真实提交路径会在 uploads 物化 .txt，预演必须跳过）
    #[tokio::test]
    async fn test_single_dry_run_text_input_not_materialized() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("dryrun-text");
        let state = dryrun_test_state(root.clone());
        let resp = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state)
            .oneshot(single_request(json!({
                "module_id": "dryrun-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_text": "预演文本，不应落盘",
                "dry_run": true
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::OK, "响应: {body}");
        assert_eq!(body["plan"]["input"]["kind"], "text");
        assert_eq!(
            body["plan"]["input"]["chars"],
            json!("预演文本，不应落盘".chars().count())
        );
        let uploads = root.join("workspace").join("uploads");
        assert!(
            !uploads.exists()
                || std::fs::read_dir(&uploads)
                    .map(|it| it.count())
                    .unwrap_or(0)
                    == 0,
            "dry-run 不得在 uploads 物化文本文件"
        );
    }

    // ── T-54：提交幂等键（同键重放 / 异键新任务 / 终态后仍可查 / 超长 400） ──

    /// 同键两次提交 → 同一 task_id + idempotent_replay 标记；不同键 → 新任务；
    /// 任务终态后同键仍返回首个 task_id
    #[tokio::test]
    async fn test_single_idempotency_key_replays_first_task() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("idem-cycle");
        let state = dryrun_test_state(root.clone());
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);

        // 首次提交（lazy_start=true：跳过预启动，任务入队后由安全网拉起失败）
        let first = json!({
            "module_id": "dryrun-mod",
            "capability": "run",
            "params": { "beam_size": 5 },
            "input_text": "幂等重试语义验证",
            "lazy_start": true,
            "idempotency_key": "agent-retry-key-1"
        });
        let resp = app.clone().oneshot(single_request(first.clone())).await.unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED, "响应: {body}");
        let task_id_1 = body["task_id"].as_str().unwrap().to_string();
        assert!(body.get("idempotent_replay").is_none(), "首次提交无重放标记");

        // 同键第二次提交 → 同 task_id + idempotent_replay: true（零新任务）
        let resp = app.clone().oneshot(single_request(first)).await.unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["task_id"], json!(task_id_1), "同键必须重放首个任务");
        assert_eq!(body["idempotent_replay"], json!(true));

        // 不同键 → 全新任务（不同 task_id）
        let resp = app
            .clone()
            .oneshot(single_request(json!({
                "module_id": "dryrun-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_text": "幂等重试语义验证",
                "lazy_start": true,
                "idempotency_key": "agent-retry-key-2"
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id_2 = body["task_id"].as_str().unwrap().to_string();
        assert_ne!(task_id_2, task_id_1, "不同键必须产生新任务");
        assert!(body.get("idempotent_replay").is_none());

        // 等首个任务终态（模块拉起失败 → Failed），同键重放仍返回首个 task_id
        let record = wait_terminal(&task_id_1).await.expect("任务应终结");
        assert_eq!(record.status, execution::TaskState::Failed);
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "dryrun-mod",
                "capability": "run",
                "params": { "beam_size": 5 },
                "input_text": "幂等重试语义验证",
                "lazy_start": true,
                "idempotency_key": "agent-retry-key-1"
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["task_id"], json!(task_id_1), "终态后条目保留（在案期内）");
        assert_eq!(body["idempotent_replay"], json!(true));

        // 收尾前等 task_2 也终结：测试锁释放时不留任何在途后台任务
        //（否则其准入/入册写入会串染下一个拿锁测试的注册表断言）
        let record = wait_terminal(&task_id_2).await.expect("第二个任务应终结");
        assert_eq!(record.status, execution::TaskState::Failed);
    }

    /// 幂等键超 128 字符 → 400（快失败，不宽容截断）
    #[tokio::test]
    async fn test_single_idempotency_key_too_long_400() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();

        let state = dryrun_test_state(unique_root("idem-toolong"));
        let app = Router::new()
            .route("/execute/single", post(execute_single))
            .with_state(state);
        let long_key = "k".repeat(IDEMPOTENCY_KEY_MAX_LEN + 1);
        let resp = app
            .oneshot(single_request(json!({
                "module_id": "dryrun-mod",
                "capability": "run",
                "input_text": "x",
                "idempotency_key": long_key
            })))
            .await
            .unwrap();
        let (status, body) = single_response(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains(&IDEMPOTENCY_KEY_MAX_LEN.to_string())
        );
    }

    /// validate_idempotency_key 纯函数口径（缺失/空白 = 未指定；字符数计数）
    #[test]
    fn validate_idempotency_key_boundaries() {
        assert_eq!(validate_idempotency_key(None).unwrap(), None);
        assert_eq!(validate_idempotency_key(Some("  ")).unwrap(), None);
        assert_eq!(
            validate_idempotency_key(Some(" agent-1 ")).unwrap(),
            Some("agent-1".to_string())
        );
        let maxed = "k".repeat(IDEMPOTENCY_KEY_MAX_LEN);
        assert_eq!(
            validate_idempotency_key(Some(&maxed)).unwrap(),
            Some(maxed.clone())
        );
        assert!(validate_idempotency_key(Some(&(maxed + "k"))).is_err());
        // CJK 字符按字符数计（128 个汉字合法，129 个拒绝）
        let cjk = "键".repeat(IDEMPOTENCY_KEY_MAX_LEN);
        assert!(validate_idempotency_key(Some(&cjk)).is_ok());
        assert!(validate_idempotency_key(Some(&(cjk + "键"))).is_err());
    }

    /// 幂等侧表先到先得：register 已有键 → Err(首个 task_id)，查询稳定
    #[test]
    fn idempotency_table_first_wins() {
        let _guard = execution::lock_for_tests();
        execution::clear_registry_for_tests();
        assert_eq!(execution::idempotency_lookup("tbl-k"), None);
        execution::idempotency_register("tbl-k", "task-a").unwrap();
        assert_eq!(
            execution::idempotency_lookup("tbl-k"),
            Some("task-a".to_string())
        );
        assert_eq!(
            execution::idempotency_register("tbl-k", "task-b"),
            Err("task-a".to_string()),
            "并发同键先到先得，首个条目不被覆盖"
        );
        assert_eq!(
            execution::idempotency_lookup("tbl-k"),
            Some("task-a".to_string())
        );
        execution::clear_registry_for_tests();
        assert_eq!(execution::idempotency_lookup("tbl-k"), None, "测试清空钩子生效");
    }
}
