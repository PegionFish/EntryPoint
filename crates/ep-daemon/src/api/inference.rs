//! 统一推理 API v1 门面（`/api/v1/*`，外部稳定契约）
//!
//! 三视角（WebUI / 自动化集成 / 外部系统）共用的稳定推理入口，与
//! `/api` 其余 WebUI 内部端点分离：本文件端点的请求/响应形状与错误码
//! 属对外契约，变更须走版本演进（v2），不复用 `err_response` 的 i18n
//! 本地化文案（WebUI 依赖）。
//!
//! 端点（挂 `/api` 前缀下）：
//! - `GET  /v1/capabilities`                        — 能力目录聚合（纯只读）
//! - `POST /v1/inference/{module_id}/{capability}`  — 提交推理（multipart / JSON）
//! - `GET  /v1/inference/result/{task_id}`          — 任务状态与产物查询
//!
//! 错误契约：`{"error": {"code": <MACHINE_CODE>, "message": <可读文案>}}`。
//!
//! 鉴权：`config.api.token` 配置后要求 `Authorization: Bearer <token>` 或
//! `X-API-Key: <token>`（常量时间比较）；未配置或 `enabled=false` 直通。
//! 中间件仅 layer 在本文件 v1 子 Router，不影响 `/api` 其余端点。
//!
//! 安全约束：`input_path` 仅接受 workspace/uploads 前缀路径（canonicalize
//! 后前缀校验，防符号链接/`..` 穿越）；产物一律返回相对下载 URL，
//! 绝不回传服务器绝对路径。

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::{BodyExt, Limited};
use serde::Deserialize;
use serde_json::{json, Value};

use ep_core::module::manifest::CapabilityDecl;

use crate::api::autostart::{self, AutoStartError};
use crate::api::execute::execution::{self, SubmitError, SubmitOptions};
use crate::api::execute::{
    find_module_manifest, validate_and_fill_params, ParamError,
};
use crate::api::upload::{input_uploads_dir, staging_id, store_input_file};
use crate::state::AppState;

// ─── 错误契约（稳定机读码；不走 i18n，不改动 err_response） ─────────────────

/// v1 稳定错误响应：`{"error": {"code": <MACHINE_CODE>, "message": <文案>}}`
fn v1_error(status: StatusCode, code: &str, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message.into() } })),
    )
}

/// 参数校验失败 → 400 PARAM_INVALID（携带具体原因）
fn param_error(detail: impl Into<String>) -> (StatusCode, Json<Value>) {
    v1_error(StatusCode::BAD_REQUEST, "PARAM_INVALID", detail)
}

/// 输入非法（缺失/越权/形态错误）→ 400 INPUT_INVALID
fn input_error(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    v1_error(StatusCode::BAD_REQUEST, "INPUT_INVALID", message)
}

/// manifest / capability 存在性解析（纯只读、无副作用，m5）：前移到输入
/// 物化之前执行，避免非法请求（未知模块/capability）向 uploads 留垃圾文件。
async fn resolve_capability(
    state: &Arc<AppState>,
    module_id: &str,
    capability: &str,
) -> Result<CapabilityDecl, (StatusCode, Json<Value>)> {
    let manifest = match find_module_manifest(state, module_id).await {
        Some(mf) => mf,
        None => {
            return Err(v1_error(
                StatusCode::NOT_FOUND,
                "MODULE_NOT_FOUND",
                format!("module `{module_id}` not found"),
            ))
        }
    };
    match manifest
        .interface
        .capabilities
        .iter()
        .find(|c| c.name == capability)
    {
        Some(c) => Ok(c.clone()),
        None => Err(v1_error(
            StatusCode::NOT_FOUND,
            "CAPABILITY_NOT_FOUND",
            format!("module `{module_id}` has no capability `{capability}`"),
        )),
    }
}

/// `ParamError` → 400 PARAM_INVALID 映射（携带具体原因）
fn map_param_error(e: ParamError) -> (StatusCode, Json<Value>) {
    match e {
        ParamError::Missing(name) => {
            param_error(format!("required parameter `{name}` is missing"))
        }
        ParamError::TypeMismatch { name, expected } => param_error(format!(
            "parameter `{name}` type mismatch, expected {expected}"
        )),
        ParamError::EnumMismatch(name) => param_error(format!(
            "parameter `{name}` value is not in the declared enum"
        )),
    }
}

/// [`store_input_file`] 失败 → v1 契约映射（m2：按 ue.status 分流；
/// UploadError 的 i18n 键面向 WebUI，v1 门面只借 status 与归类）：
/// 5xx → INTERNAL；4xx → INPUT_INVALID（与 INFERENCE_API.md 错误码表一致）。
fn store_failure_response(ue: crate::api::upload::UploadError) -> (StatusCode, Json<Value>) {
    let code = if ue.status.is_server_error() { "INTERNAL" } else { "INPUT_INVALID" };
    v1_error(
        ue.status,
        code,
        format!("failed to store input file ({})", ue.key),
    )
}

/// `SubmitError` → v1 错误契约映射
fn submit_error_response(e: SubmitError) -> (StatusCode, Json<Value>) {
    match e {
        SubmitError::ModuleNotFound(id) => v1_error(
            StatusCode::NOT_FOUND,
            "MODULE_NOT_FOUND",
            format!("module `{id}` not found"),
        ),
        SubmitError::CapabilityNotFound(module_id, capability) => v1_error(
            StatusCode::NOT_FOUND,
            "CAPABILITY_NOT_FOUND",
            format!("module `{module_id}` has no capability `{capability}`"),
        ),
        SubmitError::InputMissing(path) => input_error(format!(
            "input file does not exist: {}",
            path.display()
        )),
        SubmitError::QueueFull(limit) => v1_error(
            StatusCode::TOO_MANY_REQUESTS,
            "QUEUE_FULL",
            format!("task queue is full (max {limit} in-flight tasks)"),
        ),
        SubmitError::ModuleStartFailed(detail) => v1_error(
            StatusCode::BAD_GATEWAY,
            "MODULE_START_FAILED",
            format!("failed to auto-start module: {detail}"),
        ),
        // 直跑退化 DAG 为内建三节点图，下列变体理论上不可达；
        // 兜底统一归入 INTERNAL，保持契约面完整
        other => v1_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            other.to_string(),
        ),
    }
}

/// `AutoStartError` → v1 错误契约映射（对齐 execute_single 的状态码语义）
fn autostart_error_response(e: AutoStartError) -> (StatusCode, Json<Value>) {
    match e {
        AutoStartError::ModuleNotFound(id) => v1_error(
            StatusCode::NOT_FOUND,
            "MODULE_NOT_FOUND",
            format!("module `{id}` not found"),
        ),
        AutoStartError::ModelNotReady { module_id, model } => v1_error(
            StatusCode::CONFLICT,
            "MODEL_NOT_READY",
            format!("active model `{model}` of module `{module_id}` is not ready"),
        ),
        other => v1_error(
            StatusCode::BAD_GATEWAY,
            "MODULE_START_FAILED",
            other.to_string(),
        ),
    }
}

// ─── token 中间件（仅保护 v1 子 Router） ─────────────────────────────────────

/// 常量时间字节串比较（防计时侧信道）。
///
/// ep-daemon 未引入 subtle crate：长度比较分支只泄漏长度（token 为部署方
/// 自配长随机串，可接受）；逐字节 XOR 累积比较不提前退出。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// Bearer / X-API-Key 鉴权中间件：仅挂 v1 子 Router。
///
/// 经 [`axum::middleware::from_fn_with_state`] 注入 state（与 main.rs
/// ip_filter 同惯例）：`from_fn` 的层状态为 `()`，其 `State<S>` 提取器
/// 类型上无法匹配路由 state，故 router 装配时显式传入 state 实例。
async fn require_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // C1 修复：先拷出 (enabled, token) 并 drop 读守卫，再判断放行——
    // 此前两个直通分支在 config 读锁守卫存活期间 `await next.run(request)`，
    // 缺省配置（token 未配置）下每个 v1 请求（wait=true 可达 630s+）全程持
    // 读锁；tokio RwLock 写优先排队，一次 PUT /api/config 写锁到来后会阻塞
    // 全 daemon 所有 config 读取。
    let (enabled, token) = {
        let cfg = state.config.read().await;
        (cfg.api.enabled, cfg.api.token.clone())
    };
    // enabled=false 视为门面未启用鉴权要求（直通）；token 未配置/空亦直通
    if !enabled {
        return next.run(request).await;
    }
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return next.run(request).await;
    };

    // RFC 6750：scheme 大小写不敏感（"bearer" 同样有效）
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_bearer_token);
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim);

    let ok = bearer
        .map(|v| ct_eq(v.as_bytes(), token.as_bytes()))
        .unwrap_or(false)
        || api_key
            .map(|v| ct_eq(v.as_bytes(), token.as_bytes()))
            .unwrap_or(false);

    if ok {
        next.run(request).await
    } else {
        v1_error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "missing or invalid token: provide `Authorization: Bearer <token>` or `X-API-Key: <token>`",
        )
        .into_response()
    }
}

/// RFC 6750 Bearer 凭据解析：scheme 大小写不敏感（`bearer` 等价），
/// 首个空白字符分割取凭据并 trim。非 Bearer scheme 返回 None（调用侧
/// 继续回退 X-API-Key 校验）。
fn parse_bearer_token(value: &str) -> Option<&str> {
    let (scheme, credentials) = value.split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    Some(credentials.trim())
}

/// JSON 分支 body 上限（字节）：推理提交的文本/params 为 KB 量级，
/// 8MB 留足余量的同时封顶无限缓冲（OOM DoS 防护，M1）
const JSON_BODY_LIMIT: usize = 8 * 1024 * 1024;

// ─── 路由装配 ────────────────────────────────────────────────────────────────

/// v1 子 Router（挂 `/api` 前缀下，见 api/mod.rs）。
///
/// `state` 参数仅供 token 中间件 `from_fn_with_state` 注入（路由本身仍
/// 是 `Router<Arc<AppState>>`，照常经 `with_state` 装配）。
///
/// Body 上限（M1：不整体 disable，防无上限缓冲 OOM）：
/// - 推理提交路由按 Content-Type 分流，multipart 大文件流式落盘 →
///   仅该路由关闭 axum 默认 2MB 上限，字段级限制由 multer constraints
///   承担（见 submit_multipart）；JSON 分支手工有限缓冲
///   （[`JSON_BODY_LIMIT`]，http_body_util::Limited）；
/// - capabilities/result 为无 body 读取的 GET，保留默认上限。
///
/// layer 顺序（后加者最外层）：body 限制 → token 中间件（先于 body 读取鉴权）。
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    let inference_route = Router::new()
        .route("/v1/inference/{module_id}/{capability}", post(submit_inference))
        // multipart 分支 file 字段可能很大（流式落盘，字段级限制由 multer
        // constraints 承担）：仅本路由关闭默认 2MB 上限，与 upload.rs 同口径
        .layer(DefaultBodyLimit::disable());
    Router::new()
        .route("/v1/capabilities", get(list_capabilities))
        .route("/v1/inference/result/{task_id}", get(get_result))
        .merge(inference_route)
        .layer(middleware::from_fn_with_state(state, require_token))
}

// ─── GET /v1/capabilities ───────────────────────────────────────────────────

/// 能力目录：聚合所有已发现模块 manifest 的 capability 声明。
///
/// 纯只读（读 state.modules 快照），永不调用模块进程；无模块时返回空列表。
async fn list_capabilities(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let modules = state.modules.read().await;
    let mut capabilities = Vec::new();
    for module in modules.iter() {
        let Some(manifest) = module.manifest.as_ref() else {
            continue;
        };
        let module_id = manifest.module.id.clone();
        for cap in &manifest.interface.capabilities {
            capabilities.push(json!({
                "module_id": module_id,
                "capability": cap.name,
                "description": cap.description,
                "input_type": cap.input_type,
                "output_type": cap.output_type,
                "max_file_size_mb": cap.max_file_size_mb,
                "params": cap.params,
            }));
        }
    }
    (StatusCode::OK, Json(json!({ "capabilities": capabilities })))
}

// ─── POST /v1/inference/{module_id}/{capability} ────────────────────────────

/// JSON 形态请求体：`input_text`（纯文本）与 `input_path`（uploads 前缀
/// 路径）二选一
#[derive(Debug, Deserialize)]
struct InferenceJsonRequest {
    input_text: Option<String>,
    input_path: Option<String>,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    wait: bool,
    callback_url: Option<String>,
}

/// 提交推理（双形态）：按 Content-Type 分流。
///
/// 单 `Request` 提取器手工分流（axum Handler 仅允许最后一个提取器消费
/// body，`Option<Multipart>` 无法与后续 body 提取器共存）：
/// multipart/form-data → multer 手工解析（版本与 axum 内部一致，字段级
/// constraints 见 submit_multipart）；其余 → JSON body（有限缓冲，
/// [`JSON_BODY_LIMIT`]）。boundary 非法 → 400。
async fn submit_inference(
    State(state): State<Arc<AppState>>,
    UrlPath((module_id, capability)): UrlPath<(String, String)>,
    request: axum::extract::Request,
) -> (StatusCode, Json<Value>) {
    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if content_type.to_lowercase().starts_with("multipart/form-data") {
        let boundary = match multer::parse_boundary(&content_type) {
            Ok(b) => b,
            Err(_) => {
                return input_error("multipart request has missing or invalid boundary")
            }
        };
        // multer 解析器在 submit_multipart 内构造（constraints 需先解析
        // capability 声明的 file 上限，见该函数；版本与 axum 内部一致）
        submit_multipart(&state, &module_id, &capability, request.into_body(), boundary).await
    } else {
        // M1：JSON 分支有限缓冲（本路由已 disable axum 默认上限，此处自行
        // 封顶 [`JSON_BODY_LIMIT`]，超限即错，防无上限缓冲 OOM）
        let body = match Limited::new(request.into_body(), JSON_BODY_LIMIT)
            .collect()
            .await
        {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                return input_error(format!("request body too large or unreadable: {e}"))
            }
        };
        let parsed: InferenceJsonRequest = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return v1_error(
                    StatusCode::BAD_REQUEST,
                    "INPUT_INVALID",
                    format!("invalid JSON body: {e}"),
                )
            }
        };
        submit_json(&state, &module_id, &capability, parsed).await
    }
}

/// multipart 形态：字段 `file`（必需）+ 可选 `params`（JSON 字符串）+
/// 可选 `wait`（"true"/"false"）。落盘复用 upload.rs 共享助手
/// [`store_input_file`]（文件名清洗/防穿越逻辑保留其中）。
///
/// M1：multer constraints 字段级限制——params/wait 文本字段限 1MB；file
/// 字段限制与能力声明 `max_file_size_mb` 对齐（未声明 → 2GB 兜底），超限
/// 提交侧拒绝 400 INPUT_INVALID（见 INFERENCE_API.md §2）；大文件保留
/// 逐 chunk 流式落盘语义（不整块进内存）。m4：重复 file 字段先删除已
/// 暂存文件；暂存命名复用 upload.rs 口径（纳秒 hex + 原子序号 + pid）。
/// m5：manifest/capability 校验前移至输入落盘之前（纯读，无副作用）。
async fn submit_multipart(
    state: &Arc<AppState>,
    module_id: &str,
    capability: &str,
    body: axum::body::Body,
    boundary: String,
) -> (StatusCode, Json<Value>) {
    // m5：manifest/capability 校验前移（纯读无副作用），非法请求不留垃圾文件
    let cap = match resolve_capability(state, module_id, capability).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    // M1：字段级限制。文本字段（params/wait）1MB；file 与能力声明对齐
    const TEXT_FIELD_LIMIT: u64 = 1024 * 1024;
    const FILE_LIMIT_FALLBACK: u64 = 2 * 1024 * 1024 * 1024; // 未声明时 2GB 兜底
    let file_limit = cap
        .max_file_size_mb
        .map(|mb| u64::from(mb).saturating_mul(1024 * 1024))
        .unwrap_or(FILE_LIMIT_FALLBACK);
    // 与 axum Multipart 提取器同口径：Body → data stream → multer；
    // constraints 限字段尺寸，whole_stream 不设（大文件流式落盘）
    let constraints = multer::Constraints::new().size_limit(
        multer::SizeLimit::new()
            .per_field(TEXT_FIELD_LIMIT)
            .for_field("file", file_limit),
    );
    let mut multipart =
        multer::Multipart::with_constraints(body.into_data_stream(), boundary, constraints);

    let mut file_staged: Option<(PathBuf, String)> = None;
    let mut params = json!({});
    let mut wait = false;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                cleanup_staged(&file_staged).await;
                return if is_size_limit_error(&e) {
                    input_error("multipart field exceeds the declared size limit")
                } else {
                    input_error(format!("failed to read multipart body: {e}"))
                };
            }
        };
        let mut field = field;
        match field.name().unwrap_or("") {
            "file" => {
                // m4：重复 file 字段 → 先删除已暂存文件（防泄漏），以最后一个为准
                if let Some((old, _)) = file_staged.take() {
                    let _ = tokio::fs::remove_file(&old).await;
                }
                let raw_name = field.file_name().unwrap_or("input").to_string();
                let temp_dir = std::env::temp_dir().join(format!(
                    "ep-v1-inference-{}",
                    std::process::id()
                ));
                if tokio::fs::create_dir_all(&temp_dir).await.is_err() {
                    return v1_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL",
                        "failed to create staging directory",
                    );
                }
                // m4：对齐 upload.rs staging 命名口径（纳秒 hex + 原子序号 + pid），
                // 消除纯纳秒命名的同刻并发覆盖竞态
                let temp_path = temp_dir.join(format!("v1-upload-{}.part", staging_id()));
                let mut writer = match tokio::fs::File::create(&temp_path).await {
                    Ok(f) => f,
                    Err(e) => {
                        return v1_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "INTERNAL",
                            format!("failed to stage input file: {e}"),
                        )
                    }
                };
                use tokio::io::AsyncWriteExt;
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            if writer.write_all(&chunk).await.is_err() {
                                let _ = tokio::fs::remove_file(&temp_path).await;
                                return v1_error(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "INTERNAL",
                                    "failed to write staged input file",
                                );
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            let _ = tokio::fs::remove_file(&temp_path).await;
                            return multipart_field_error("file", &e, Some(file_limit));
                        }
                    }
                }
                file_staged = Some((temp_path, raw_name));
            }
            "params" => {
                let text = match field.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        cleanup_staged(&file_staged).await;
                        return multipart_field_error("params", &e, Some(TEXT_FIELD_LIMIT));
                    }
                };
                if !text.trim().is_empty() {
                    match serde_json::from_str(&text) {
                        Ok(Value::Object(_)) | Ok(Value::Null) => {
                            let parsed: Value = serde_json::from_str(&text).unwrap_or(json!({}));
                            // m5：参数尽早校验（params 字段先于 file 到达时，
                            // 非法参数直接拒绝，避免继续接收后续大文件）
                            if let Err(e) = validate_and_fill_params(&cap, parsed.clone()) {
                                cleanup_staged(&file_staged).await;
                                return map_param_error(e);
                            }
                            params = parsed;
                        }
                        _ => {
                            cleanup_staged(&file_staged).await;
                            return param_error(format!(
                                "multipart field `params` is not a JSON object: {text}"
                            ));
                        }
                    }
                }
            }
            "wait" => {
                wait = match field.text().await {
                    Ok(text) => text.trim() == "true",
                    Err(e) => {
                        cleanup_staged(&file_staged).await;
                        return multipart_field_error("wait", &e, Some(TEXT_FIELD_LIMIT));
                    }
                };
            }
            _ => {} // 未知字段忽略（multer 自动丢弃未读内容）
        }
    }

    let (temp_path, raw_name) = match file_staged {
        Some(staged) => staged,
        None => return input_error("multipart field `file` is required"),
    };

    let stored = match store_input_file(state, &temp_path, &raw_name).await {
        Ok(path) => path,
        Err(ue) => {
            // m2：落盘失败按 ue.status 分流（5xx → INTERNAL，4xx → INPUT_INVALID）
            let _ = tokio::fs::remove_file(&temp_path).await;
            return store_failure_response(ue);
        }
    };
    let _ = tokio::fs::remove_file(&temp_path).await;

    run_inference(
        state,
        module_id,
        &cap,
        params,
        stored,
        wait,
        None,
    )
    .await
}

/// 清理已暂存的临时输入文件（错误提前返回路径调用，防泄漏）
async fn cleanup_staged(file_staged: &Option<(PathBuf, String)>) {
    if let Some((path, _)) = file_staged {
        let _ = tokio::fs::remove_file(path).await;
    }
}

/// multer 错误是否命中尺寸约束（字段级或流级）
fn is_size_limit_error(e: &multer::Error) -> bool {
    matches!(
        e,
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. }
    )
}

/// multer 字段读取错误 → v1 契约：命中尺寸约束（含能力声明的 file 上限与
/// 文本字段 1MB 上限，见 INFERENCE_API.md §2）或其他读取失败均为形态非法
/// → 400 INPUT_INVALID。
fn multipart_field_error(
    field: &str,
    e: &multer::Error,
    limit: Option<u64>,
) -> (StatusCode, Json<Value>) {
    if is_size_limit_error(e) {
        let desc = limit
            .map(|l| format!("{} MB", l / (1024 * 1024)))
            .unwrap_or_else(|| "1 MB".to_string());
        input_error(format!("multipart field `{field}` exceeds size limit ({desc})"))
    } else {
        input_error(format!("failed to read multipart field `{field}`: {e}"))
    }
}

/// JSON 形态：`input_text` 与 `input_path` 互斥（m3：同传/双缺 → 400
/// INPUT_INVALID，对齐 execute.rs 双字段 400 口径）；`input_text` 物化为
/// uploads 下 .txt；`input_path` 仅限 uploads 前缀（canonicalize 后校验，
/// 绝不透传任意绝对路径）。m5：manifest/capability/参数校验前移至输入
/// 物化之前（纯读，无副作用）。
async fn submit_json(
    state: &Arc<AppState>,
    module_id: &str,
    capability: &str,
    req: InferenceJsonRequest,
) -> (StatusCode, Json<Value>) {
    let request_params = if req.params.is_null() {
        json!({})
    } else if req.params.is_object() {
        req.params
    } else {
        return param_error("`params` must be a JSON object");
    };

    // m3：input_text 与 input_path 互斥 —— 双缺或同传 → 400 INPUT_INVALID
    match (&req.input_text, &req.input_path) {
        (Some(_), Some(_)) => {
            return input_error(
                "`input_text` and `input_path` are mutually exclusive — provide exactly one",
            )
        }
        (None, None) => {
            return input_error("either `input_text` or `input_path` is required")
        }
        _ => {}
    }

    // m5：manifest/capability/参数校验前移至输入物化之前（纯读无副作用），
    // 避免非法请求向 uploads 留垃圾文件
    let cap = match resolve_capability(state, module_id, capability).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let params = match validate_and_fill_params(&cap, request_params) {
        Ok(p) => p,
        Err(e) => return map_param_error(e),
    };

    let input_path = match (&req.input_text, &req.input_path) {
        (Some(text), _) => {
            // 纯文本物化：写临时文件 → 共享助手落盘 uploads（统一前缀口径）
            let temp_dir = std::env::temp_dir().join(format!(
                "ep-v1-inference-{}",
                std::process::id()
            ));
            if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
                return v1_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL",
                    format!("failed to create staging directory: {e}"),
                );
            }
            // m4：暂存命名对齐 upload.rs 口径（纳秒 hex + 原子序号 + pid）
            let temp_path = temp_dir.join(format!("v1-text-{}.txt", staging_id()));
            if let Err(e) = tokio::fs::write(&temp_path, text).await {
                return v1_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL",
                    format!("failed to materialize input_text: {e}"),
                );
            }
            let stored = match store_input_file(state, &temp_path, "input.txt").await {
                Ok(path) => path,
                Err(ue) => {
                    // m2：落盘失败按 ue.status 分流（5xx → INTERNAL，4xx → INPUT_INVALID）
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    return store_failure_response(ue);
                }
            };
            let _ = tokio::fs::remove_file(&temp_path).await;
            stored
        }
        (None, Some(p)) => {
            // 前缀规范化校验：文件必须存在，且 canonicalize 后位于 uploads 内
            // （解析符号链接与 `..`，杜绝越权引用任意服务器路径）
            let candidate = PathBuf::from(p);
            if !candidate.is_file() {
                return input_error(format!("input_path does not exist or is not a file: {p}"));
            }
            let uploads = input_uploads_dir(state).await;
            let (canon_file, canon_uploads) =
                match (candidate.canonicalize(), uploads.canonicalize()) {
                    (Ok(f), Ok(u)) => (f, u),
                    _ => {
                        return input_error(format!(
                            "input_path must be inside workspace/uploads: {p}"
                        ))
                    }
                };
            if !canon_file.starts_with(&canon_uploads) {
                return input_error(format!(
                    "input_path must be inside workspace/uploads (rejected: {p})"
                ));
            }
            canon_file
        }
        (None, None) => {
            return input_error("either `input_text` or `input_path` is required");
        }
    };

    run_inference(
        state,
        module_id,
        &cap,
        params,
        input_path,
        req.wait,
        req.callback_url,
    )
    .await
}

/// 公共执行序列（对齐 execute_single 校验顺序）：参数 schema 校验与默认值
/// 注入 → autostart → submit_direct_full。
///
/// m5：manifest/capability/参数校验已前移至分流阶段输入物化之前
/// （[`resolve_capability`] + [`map_param_error`]）；输入文件存在性亦在
/// 分流阶段保证（落盘/前缀校验），此处不再重复。
async fn run_inference(
    state: &Arc<AppState>,
    module_id: &str,
    cap: &CapabilityDecl,
    request_params: Value,
    input_path: PathBuf,
    wait: bool,
    callback_url: Option<String>,
) -> (StatusCode, Json<Value>) {
    // 1. 参数 schema 校验与默认值注入（复用 execute.rs 同一实现；
    //    multipart 路径 params 字段解析时已尽早校验过一次，此处幂等重验）
    let params = match validate_and_fill_params(cap, request_params) {
        Ok(p) => p,
        Err(e) => return map_param_error(e),
    };

    // 2. 模块自动拉起（未运行 → 启动并等健康）
    if let Err(e) = autostart::ensure_module_running(state, module_id).await {
        return autostart_error_response(e);
    }

    // 3. 提交（wait/callback 语义镜像 submit_pipeline_full）
    let options = SubmitOptions { wait, callback_url };
    match execution::submit_direct_full(
        state,
        module_id,
        &cap.name,
        params,
        input_path,
        None,
        options,
    )
    .await
    {
        Ok(outcome) => {
            if wait {
                let record = outcome
                    .record
                    .expect("wait=true 时 submit_direct_full 必携带终态快照");
                (StatusCode::OK, Json(wait_response(&outcome.task_id, &record)))
            } else {
                // 提交后立即快照：仍在排队则附带队列位置
                let mut body = json!({ "task_id": outcome.task_id });
                if let Some(record) = execution::snapshot(&outcome.task_id) {
                    if let Some(pos) = record.queue_position {
                        body["queue_position"] = json!(pos);
                    }
                }
                (StatusCode::ACCEPTED, Json(body))
            }
        }
        Err(e) => submit_error_response(e),
    }
}

/// wait 同步响应组装：`{task_id, status, output_url?, error?}`。
/// 产物一律相对下载 URL（`/api/tasks/{task_id}/artifacts/{node_id}`），
/// 绝不回传服务器绝对路径；output 节点产物优先。
fn wait_response(task_id: &str, record: &ep_core::task_registry::TaskRecord) -> Value {
    let mut body = json!({
        "task_id": task_id,
        "status": record.status.as_str(),
    });
    if let Some(url) = first_artifact_url(task_id, record) {
        body["output_url"] = json!(url);
    }
    if let Some(err) = &record.error {
        body["error"] = json!(err);
    }
    body
}

/// 取任务首个可用产物的相对下载 URL：优先 `output` 节点，其次 node_order
/// 顺序中任何已归集（served）产物
fn first_artifact_url(
    task_id: &str,
    record: &ep_core::task_registry::TaskRecord,
) -> Option<String> {
    let has_served = |node_id: &str| -> bool {
        record.served_artifacts.contains_key(node_id) || record.artifacts.contains_key(node_id)
    };
    // 优先级：output 节点 > 其余节点按序（跳过 input——那是输入文件不是结果，
    // 直跑 JSON/Text 输出物化在 run 节点）> 兜底任意产物（含 input）
    let picked = if has_served("output") {
        Some("output".to_string())
    } else {
        record
            .node_order
            .iter()
            .find(|node_id| *node_id != "input" && has_served(node_id))
            .cloned()
            .or_else(|| record.node_order.iter().find(|n| has_served(n)).cloned())
    };
    picked.map(|node_id| format!("/api/tasks/{task_id}/artifacts/{node_id}"))
}

// ─── GET /v1/inference/result/{task_id} ─────────────────────────────────────

/// 任务结果查询：复用 `execution::snapshot`（queued 带实时队列位置）。
/// 文件产物一律相对下载 URL，绝不返回服务器绝对路径。
async fn get_result(
    UrlPath(task_id): UrlPath<String>,
) -> (StatusCode, Json<Value>) {
    let Some(record) = execution::snapshot(&task_id) else {
        return v1_error(
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            format!("task `{task_id}` not found"),
        );
    };

    let outputs: Vec<Value> = record
        .node_order
        .iter()
        .filter(|node_id| {
            record.served_artifacts.contains_key(*node_id)
                || record.artifacts.contains_key(*node_id)
        })
        .map(|node_id| {
            json!({
                "node_id": node_id,
                "url": format!("/api/tasks/{task_id}/artifacts/{node_id}"),
            })
        })
        .collect();

    let mut body = json!({
        "task_id": task_id,
        "status": record.status.as_str(),
        "outputs": outputs,
    });
    if let Some(pos) = record.queue_position {
        body["queue_position"] = json!(pos);
    }
    if let Some(err) = &record.error {
        body["error"] = json!(err);
    }
    (StatusCode::OK, Json(body))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use ep_core::config::{ApiConfig, AppConfig};
    use ep_core::port::PortManager;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-v1-api-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// 沿用 api/mod.rs test_state 模式；可注入 api 配置与模块清单
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

    fn app(state: Arc<AppState>) -> Router {
        router(state.clone()).with_state(state)
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn get_with_headers(uri: &str, headers: Vec<(&str, &str)>) -> Request<Body> {
        let mut builder = Request::builder().uri(uri);
        for (k, v) in headers {
            builder = builder.header(k, v);
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn json_of(resp: Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("响应不是合法 JSON: {e}; body={bytes:?}"));
        (status, value)
    }

    /// 断言 v1 错误契约形状并返回 code
    fn assert_error_code(body: &Value, expected: &str) {
        assert_eq!(body["error"]["code"], expected, "错误体: {body}");
        assert!(body["error"]["message"].is_string());
    }

    // a) token 未配置 → 请求直通（capabilities 200）
    #[tokio::test]
    async fn token_unset_passes_through() {
        let state = test_state(unique_root("no-token"), AppConfig::default(), vec![]);
        let resp = app(state).oneshot(get("/v1/capabilities")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // b) token 配置 + 无 header → 401 UNAUTHORIZED
    #[tokio::test]
    async fn token_set_without_header_is_401() {
        let mut config = AppConfig::default();
        config.api = ApiConfig {
            enabled: true,
            token: Some("s3cret-token".to_string()),
        };
        let state = test_state(unique_root("token-401"), config, vec![]);
        let resp = app(state).oneshot(get("/v1/capabilities")).await.unwrap();
        let (status, body) = json_of(resp).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_error_code(&body, "UNAUTHORIZED");
    }

    // c) token 配置 + 正确 Bearer → 放行
    #[tokio::test]
    async fn token_set_with_bearer_passes() {
        let mut config = AppConfig::default();
        config.api = ApiConfig {
            enabled: true,
            token: Some("s3cret-token".to_string()),
        };
        let state = test_state(unique_root("bearer-ok"), config, vec![]);
        let resp = app(state)
            .oneshot(get_with_headers(
                "/v1/capabilities",
                vec![("authorization", "Bearer s3cret-token")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // 错误 Bearer → 401（常量时间比较不误放）
    #[tokio::test]
    async fn token_set_with_wrong_bearer_is_401() {
        let mut config = AppConfig::default();
        config.api = ApiConfig {
            enabled: true,
            token: Some("s3cret-token".to_string()),
        };
        let state = test_state(unique_root("bearer-bad"), config, vec![]);
        let resp = app(state)
            .oneshot(get_with_headers(
                "/v1/capabilities",
                vec![("authorization", "Bearer wrong-token")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // d) X-API-Key 头亦可
    #[tokio::test]
    async fn token_set_with_api_key_passes() {
        let mut config = AppConfig::default();
        config.api = ApiConfig {
            enabled: true,
            token: Some("s3cret-token".to_string()),
        };
        let state = test_state(unique_root("apikey-ok"), config, vec![]);
        let resp = app(state)
            .oneshot(get_with_headers(
                "/v1/capabilities",
                vec![("x-api-key", "s3cret-token")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // m6：RFC 6750 — Bearer scheme 大小写不敏感（"bearer" 小写亦放行）
    #[tokio::test]
    async fn token_set_with_lowercase_bearer_passes() {
        let mut config = AppConfig::default();
        config.api = ApiConfig {
            enabled: true,
            token: Some("s3cret-token".to_string()),
        };
        let state = test_state(unique_root("bearer-lower"), config, vec![]);
        let resp = app(state)
            .oneshot(get_with_headers(
                "/v1/capabilities",
                vec![("authorization", "bearer s3cret-token")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // m6：非 Bearer scheme（如 Basic）不误放，回退 X-API-Key 校验后仍 401
    #[tokio::test]
    async fn token_set_with_basic_scheme_is_401() {
        let mut config = AppConfig::default();
        config.api = ApiConfig {
            enabled: true,
            token: Some("s3cret-token".to_string()),
        };
        let state = test_state(unique_root("basic-scheme"), config, vec![]);
        let resp = app(state)
            .oneshot(get_with_headers(
                "/v1/capabilities",
                vec![("authorization", "Basic s3cret-token")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // e) capabilities 聚合形状：无模块 → 空列表不报错
    #[tokio::test]
    async fn capabilities_empty_when_no_modules() {
        let state = test_state(unique_root("caps-empty"), AppConfig::default(), vec![]);
        let resp = app(state).oneshot(get("/v1/capabilities")).await.unwrap();
        let (status, body) = json_of(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["capabilities"], json!([]));
    }

    // f) input_path 越权（uploads 前缀之外）→ 400 INPUT_INVALID
    #[tokio::test]
    async fn input_path_outside_uploads_is_rejected() {
        use ep_core::module::manifest::ModuleManifest;

        let root = unique_root("path-guard");
        // 单能力模块 fixture（校验序列须穿过 manifest/capability/params 才到输入校验）
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
        let module = ep_core::module::discovery::DiscoveredModule {
            manifest: Some(manifest),
            path: root.join("modules/mock-mod"),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        };
        let state = test_state(root.clone(), AppConfig::default(), vec![module]);

        // uploads 之外的真实存在文件（存在性先过，前缀校验拒绝）
        let outside = root.join("outside.txt");
        std::fs::write(&outside, "escape attempt").unwrap();
        let body = json!({ "input_path": outside.display().to_string() });
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/inference/mock-mod/run")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app(state).oneshot(req).await.unwrap();
        let (status, body) = json_of(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error_code(&body, "INPUT_INVALID");
    }

    // g) 结果端点对不存在 task → 404 TASK_NOT_FOUND
    #[tokio::test]
    async fn result_unknown_task_is_404() {
        let state = test_state(unique_root("result-404"), AppConfig::default(), vec![]);
        let resp = app(state)
            .oneshot(get("/v1/inference/result/no-such-task"))
            .await
            .unwrap();
        let (status, body) = json_of(resp).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_error_code(&body, "TASK_NOT_FOUND");
    }

    // h) m3：input_text 与 input_path 同传 → 400 INPUT_INVALID（互斥，
    //    对齐 execute.rs 双字段 400 口径；校验先于模块解析，无需 fixture）
    #[tokio::test]
    async fn input_text_and_input_path_together_are_rejected() {
        let state = test_state(unique_root("both-inputs"), AppConfig::default(), vec![]);
        let req_body = json!({
            "input_text": "hello",
            "input_path": "workspace/uploads/a.txt",
        });
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/inference/any-mod/any-cap")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
            .unwrap();
        let resp = app(state).oneshot(req).await.unwrap();
        let (status, body) = json_of(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error_code(&body, "INPUT_INVALID");
    }
}
