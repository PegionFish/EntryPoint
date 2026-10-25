//! Webhook 触发器 API（T-27：统一自动化中心，计划 4 M1）
//!
//! 端点：
//! - `GET    /api/hooks`          — 全部 Webhook 列表
//! - `POST   /api/hooks`          — 创建（服务端生成 8 位小写十六进制 hook_id）
//! - `GET    /api/hooks/{id}`     — 单条（未配置 → 404 `apiCore.hook.notFound`）
//! - `PUT    /api/hooks/{id}`     — 全量更新（校验同 POST）
//! - `DELETE /api/hooks/{id}`     — 删除（幂等）
//! - `POST   /api/hooks/{hook_id}` — **触发**关联管线 / 直跑（对外触发入口）
//!
//! ## 触发语义
//!
//! - **鉴权**：本端点位于 `/api` 前缀下，落入 auth.rs 全局访问保护域
//!   （`[server].access_protection = "token"` 时无凭据 POST → 401，
//!   GET/HEAD 的 `?token=` 例外不适用于写操作）——**不可绕过**；
//! - **pipeline 目标**：复用 cron 的提交入口
//!   [`submit_pipeline_for_schedule`]（inputs/params 模板同 /schedule 契约，
//!   `store:` 逻辑名引用由提交链解析——T-26）；
//! - **direct 目标**：复用 `/api/execute/single` 权威实现
//!   （[`execute_single_run`]：模块自动拉起、参数 schema 校验、直跑缓存全套；
//!   `lazy_start = true` 快速提交——拉起由任务内安全网完成，失败计入任务错误）；
//! - 目标存在性在创建/更新期校验（管线 404 / 模块 404 / capability 400）；
//! - 每次触发写统一事件日志 `webhook_trigger`（§5.7 同款 jsonl 追加：
//!   status ∈ {submitted, rejected}，task_terminal 仍由执行链路统一出口写入）。
//!
//! 注册表：`runtime/webhooks.json`（schedule.rs 同款惯例，见
//! [`crate::schedule::WebhookRegistry`]）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json,
    Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::get,
};
use serde_json::{Value, json};

use crate::schedule::{WebhookEntry, WebhookRegistry, WebhookTarget};
use crate::state::AppState;

use super::err_response;

/// 审查 P2-2：webhooks 注册表读-改-写进程内串行化——并发 CRUD 各自
/// load→save 会互相覆盖丢更新，且固定 tmp 路径（schedule.rs save）在并发
/// save 下互踩。文件小、临界区纯同步 IO，std Mutex 足够；跨进程无竞争
/// （单 daemon 进程）。guard 不得跨 .await 持有（见各 handler 的 outcome 结构）。
fn webhook_registry_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/hooks", get(list_hooks).post(create_hook))
        // POST /{hook_id} = 触发（对外契约：hook_id 是触发能力位，不承担鉴权——
        // 8 位时间衍生混合值可猜测；鉴权由 /api 全局中间件覆盖，见文件头与
        // AUTOMATION §4.7。access_protection=none 部署勿将端口暴露公网）
        .route(
            "/hooks/{id}",
            get(get_hook).put(update_hook).delete(delete_hook).post(trigger_hook),
        )
}

fn webhooks_path(state: &AppState) -> PathBuf {
    crate::schedule::default_webhook_registry_path(&state.root)
}

fn pipelines_dir(state: &AppState) -> PathBuf {
    state.root.join("config").join("pipelines")
}

/// 按 spec id 查找管线（与 watchers.rs 同款扫描口径，不假设文件名）
fn find_spec(
    dir: &std::path::Path,
    id: &str,
) -> Option<crate::api::pipelines::pipeline_bridge::PipelineSpec> {
    crate::api::pipelines::scan_specs_pub(dir)
        .into_iter()
        .find(|(_, spec)| spec.pipeline.id == id)
        .map(|(_, spec)| spec)
}

// ─── body 防御式提取（坏形状按缺省处理，交给校验链定夺）────────────────────

fn get_str(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn get_bool(body: &Value, key: &str, default: bool) -> bool {
    body.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// inputs/params 模板形状校验：缺省（null）或对象合法，其余拒绝
fn template_object_or_null(v: &Value) -> bool {
    v.is_null() || v.is_object()
}

type HookErr = (StatusCode, Json<Value>);

/// 创建/更新共用校验链：
/// 1. `name` 非空 → 400 `hook.nameRequired`
/// 2. `target` 存在 → 400 `hook.targetRequired`；inputs/params 非对象 → 400
/// 3. pipeline 目标：管线存在（404）；
///    direct 目标：模块存在（404）、capability 在 manifest（400）、
///    `input_path` 非空（400 `hook.inputPathRequired`）
async fn validate_hook_body(state: &Arc<AppState>, body: &Value) -> Result<(String, bool, WebhookTarget), HookErr> {
    // 1. name
    let name = get_str(body, "name").unwrap_or_default();
    if name.trim().is_empty() {
        return Err(
            err_response(state, StatusCode::BAD_REQUEST, "apiCore.hook.nameRequired", &[]).await,
        );
    }
    let enabled = get_bool(body, "enabled", true);

    // 2. target 存在且形状合法
    let Some(target_value) = body.get("target") else {
        return Err(
            err_response(state, StatusCode::BAD_REQUEST, "apiCore.hook.targetRequired", &[]).await,
        );
    };
    let target: WebhookTarget = match serde_json::from_value(target_value.clone()) {
        Ok(t) => t,
        Err(_) => {
            return Err(
                err_response(state, StatusCode::BAD_REQUEST, "apiCore.hook.targetRequired", &[])
                    .await,
            );
        }
    };

    match &target {
        WebhookTarget::Pipeline { pipeline_id, inputs, params } => {
            if pipeline_id.trim().is_empty() {
                return Err(err_response(
                    state,
                    StatusCode::BAD_REQUEST,
                    "apiCore.hook.targetInvalid",
                    &[("detail", "pipeline_id is empty".to_string())],
                )
                .await);
            }
            if !template_object_or_null(inputs) || !template_object_or_null(params) {
                return Err(err_response(
                    state,
                    StatusCode::BAD_REQUEST,
                    "apiCore.hook.targetInvalid",
                    &[("detail", "inputs/params must be objects".to_string())],
                )
                .await);
            }
            if find_spec(&pipelines_dir(state), pipeline_id).is_none() {
                return Err(
                    err_response(state, StatusCode::NOT_FOUND, "apiPipelines.pipelines.notFound", &[])
                        .await,
                );
            }
        }
        WebhookTarget::Direct { module_id, capability, input_path, params } => {
            if module_id.trim().is_empty() || capability.trim().is_empty() {
                return Err(err_response(
                    state,
                    StatusCode::BAD_REQUEST,
                    "apiCore.hook.targetInvalid",
                    &[("detail", "module_id/capability is empty".to_string())],
                )
                .await);
            }
            if !template_object_or_null(params) {
                return Err(err_response(
                    state,
                    StatusCode::BAD_REQUEST,
                    "apiCore.hook.targetInvalid",
                    &[("detail", "params must be an object".to_string())],
                )
                .await);
            }
            let manifest = crate::api::execute::find_module_manifest(state, module_id).await;
            let capability_ok = manifest
                .as_ref()
                .map(|mf| mf.interface.capabilities.iter().any(|c| c.name == *capability))
                .unwrap_or(false);
            match manifest {
                None => {
                    return Err(err_response(
                        state,
                        StatusCode::NOT_FOUND,
                        "apiCore.module.notFound",
                        &[("id", module_id.clone())],
                    )
                    .await);
                }
                Some(_) if !capability_ok => {
                    return Err(err_response(
                        state,
                        StatusCode::BAD_REQUEST,
                        "apiCore.hook.targetInvalid",
                        &[(
                            "detail",
                            format!("module {module_id} has no capability {capability}"),
                        )],
                    )
                    .await);
                }
                _ => {}
            }
            if input_path.trim().is_empty() {
                return Err(err_response(
                    state,
                    StatusCode::BAD_REQUEST,
                    "apiCore.hook.inputPathRequired",
                    &[],
                )
                .await);
            }
        }
    }
    Ok((name, enabled, target))
}

// ─── CRUD ───────────────────────────────────────────────────────────────────

/// GET /api/hooks — 全部 Webhook 列表（按 id 排序）
async fn list_hooks(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let registry = WebhookRegistry::load(&webhooks_path(&state));
    let mut ids: Vec<&String> = registry.entries.keys().collect();
    ids.sort();
    let list: Vec<Value> = ids
        .iter()
        .filter_map(|id| {
            let e = registry.entries.get(*id)?;
            serde_json::to_value(e).ok()
        })
        .collect();
    (StatusCode::OK, Json(Value::Array(list)))
}

/// GET /api/hooks/{id} — 单条（未配置 → 404）
async fn get_hook(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let registry = WebhookRegistry::load(&webhooks_path(&state));
    match registry.entries.get(&id) {
        Some(e) => (StatusCode::OK, Json(serde_json::to_value(e).unwrap_or(json!({})))),
        None => {
            err_response(&state, StatusCode::NOT_FOUND, "apiCore.hook.notFound", &[("id", id)]).await
        }
    }
}

/// POST /api/hooks — 创建；服务端生成 hook_id，返回 `{ok, id}`。
async fn create_hook(
    State(state): State<Arc<AppState>>,
    body: String,
) -> (StatusCode, Json<Value>) {
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let (name, enabled, target) = match validate_hook_body(&state, &body).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let path = webhooks_path(&state);
    let outcome = {
        let _guard = webhook_registry_lock();
        let mut registry = WebhookRegistry::load(&path);
        let id = registry.generate_hook_id();
        registry
            .entries
            .insert(id.clone(), WebhookEntry { id: id.clone(), name, enabled, target });
        registry.save(&path).map(|_| id)
    };
    let id = match outcome {
        Ok(id) => id,
        Err(e) => {
            return err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.hook.saveFailed",
                &[("detail", e.to_string())],
            )
            .await;
        }
    };
    (StatusCode::OK, Json(json!({ "ok": true, "id": id })))
}

/// PUT /api/hooks/{id} — 全量更新（校验同 POST；id 不可变）
async fn update_hook(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    body: String,
) -> (StatusCode, Json<Value>) {
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let (name, enabled, target) = match validate_hook_body(&state, &body).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let path = webhooks_path(&state);
    enum UpdateOutcome {
        Saved,
        NotFound,
        SaveFailed(String),
    }
    let outcome = {
        let _guard = webhook_registry_lock();
        let mut registry = WebhookRegistry::load(&path);
        if !registry.entries.contains_key(&id) {
            UpdateOutcome::NotFound
        } else {
            registry
                .entries
                .insert(id.clone(), WebhookEntry { id: id.clone(), name, enabled, target });
            match registry.save(&path) {
                Ok(()) => UpdateOutcome::Saved,
                Err(e) => UpdateOutcome::SaveFailed(e.to_string()),
            }
        }
    };
    match outcome {
        UpdateOutcome::Saved => (StatusCode::OK, Json(json!({ "ok": true }))),
        UpdateOutcome::NotFound => {
            err_response(&state, StatusCode::NOT_FOUND, "apiCore.hook.notFound", &[("id", id)])
                .await
        }
        UpdateOutcome::SaveFailed(detail) => {
            err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.hook.saveFailed",
                &[("detail", detail)],
            )
            .await
        }
    }
}

/// DELETE /api/hooks/{id} — 删除（幂等）
async fn delete_hook(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let path = webhooks_path(&state);
    let outcome = {
        let _guard = webhook_registry_lock();
        let mut registry = WebhookRegistry::load(&path);
        registry.entries.remove(&id);
        registry.save(&path)
    };
    if let Err(e) = outcome {
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiCore.hook.saveFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }
    (StatusCode::OK, Json(json!({ "ok": true })))
}

// ─── 触发 ───────────────────────────────────────────────────────────────────

/// `webhook_trigger` 事件写入（§5.7 同款 jsonl 追加；`append_event` 容忍尾行
/// 损坏由读路径处理）。status ∈ {submitted, rejected}。
/// `rule` 字段 = hook_id：`GET /api/events?rule={hook_id}&type=webhook_trigger`
/// 可按钩子精确过滤（与 watcher 规则共用读取路径的过滤语义）。
fn write_webhook_trigger(
    root: &std::path::Path,
    hook: &str,
    name: &str,
    target_kind: &str,
    task_id: Option<&str>,
    status: &str,
    detail: Option<&str>,
) {
    let mut event = json!({
        "ts": chrono::Utc::now().timestamp(),
        "type": "webhook_trigger",
        "rule": hook,
        "hook": hook,
        "name": name,
        "target": target_kind,
        "status": status,
    });
    if let Some(id) = task_id {
        event["task_id"] = json!(id);
    }
    if let Some(d) = detail {
        event["detail"] = json!(d);
    }
    crate::eventlog::append_event(root, &event);
}

/// `SubmitError` → HTTP（hooks 触发路径；键面与 api/execute.rs 映射同源）。
async fn submit_error_response(
    state: &Arc<AppState>,
    e: crate::api::execute::execution::SubmitError,
) -> (StatusCode, Json<Value>) {
    use crate::api::execute::execution::SubmitError;
    use ep_core::config::StoreRefError;
    match e {
        SubmitError::UnknownInputNode(node_id) => {
            err_response(state, StatusCode::BAD_REQUEST, "apiPipelines.execute.inputsUnknownNode", &[
                ("nodeId", node_id),
            ])
            .await
        }
        SubmitError::InputsNotObject(node_id) => {
            err_response(state, StatusCode::BAD_REQUEST, "apiPipelines.execute.inputsNotObject", &[
                ("nodeId", node_id),
            ])
            .await
        }
        SubmitError::CycleDetected(id) => {
            err_response(state, StatusCode::BAD_REQUEST, "apiPipelines.execute.cycleDetected", &[
                ("id", id),
            ])
            .await
        }
        SubmitError::InvalidPipeline(detail) => {
            err_response(state, StatusCode::BAD_REQUEST, "apiPipelines.specInvalid", &[
                ("detail", detail),
            ])
            .await
        }
        SubmitError::ModuleNotFound(id) => {
            err_response(state, StatusCode::NOT_FOUND, "apiCore.module.notFound", &[("id", id)])
                .await
        }
        SubmitError::CapabilityNotFound(module_id, capability) => {
            err_response(
                state,
                StatusCode::BAD_REQUEST,
                "apiPipelines.execute.capabilityNotFound",
                &[("moduleId", module_id), ("capability", capability)],
            )
            .await
        }
        SubmitError::InputMissing(path) => {
            err_response(state, StatusCode::BAD_REQUEST, "apiPipelines.execute.inputMissing", &[
                ("path", path.display().to_string()),
            ])
            .await
        }
        SubmitError::QueueFull(limit) => {
            err_response(
                state,
                StatusCode::TOO_MANY_REQUESTS,
                "apiPipelines.execute.internalError",
                &[("detail", format!("task queue is full (max {limit} in-flight tasks)"))],
            )
            .await
        }
        SubmitError::ModuleStartFailed(detail) => {
            err_response(
                state,
                StatusCode::BAD_GATEWAY,
                "apiPipelines.execute.moduleStartFailed",
                &[("detail", detail)],
            )
            .await
        }
        SubmitError::Internal(msg) => {
            err_response(
                state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiPipelines.execute.internalError",
                &[("detail", msg)],
            )
            .await
        }
        SubmitError::StoreRef(StoreRefError::UnknownStore(name)) => {
            err_response(state, StatusCode::UNPROCESSABLE_ENTITY, "apiCore.store.notFound", &[
                ("name", name),
            ])
            .await
        }
        SubmitError::StoreRef(StoreRefError::StoreDisabled(name)) => {
            err_response(state, StatusCode::UNPROCESSABLE_ENTITY, "apiCore.store.disabled", &[
                ("name", name),
            ])
            .await
        }
        SubmitError::StoreRef(StoreRefError::UnsafeRel(raw)) => {
            err_response(state, StatusCode::BAD_REQUEST, "apiCore.store.unsafePath", &[
                ("detail", raw),
            ])
            .await
        }
    }
}

/// POST /api/hooks/{hook_id} — 触发关联管线 / 直跑。
///
/// - 202 `{ok: true, task_id}`（快速提交语义：模块拉起不阻塞响应——
///   direct 目标经 lazy_start 由任务内安全网完成，失败计入任务错误）；
/// - 未配置 → 404；已停用 → 409；提交拒绝按 SubmitError 语义映射
///   （QueueFull → 429、缺 store → 422、校验失败 → 400 等）。
async fn trigger_hook(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    // 触发期可选覆盖模板（外部系统按次注入）：仅对象形状合法时生效
    let override_body: Value = match body {
        Some(Json(v)) if v.is_object() => v,
        _ => json!({}),
    };

    let registry = WebhookRegistry::load(&webhooks_path(&state));
    let Some(entry) = registry.entries.get(&id).cloned() else {
        return err_response(&state, StatusCode::NOT_FOUND, "apiCore.hook.notFound", &[("id", id)])
            .await;
    };
    if !entry.enabled {
        return err_response(&state, StatusCode::CONFLICT, "apiCore.hook.disabled", &[("id", id)])
            .await;
    }

    match entry.target.clone() {
        WebhookTarget::Pipeline { pipeline_id, inputs, params } => {
            // 逐次覆盖模板（浅对象合并：override 胜出）
            let mut merged = inputs.as_object().cloned().unwrap_or_default();
            if let Some(over) = override_body.get("inputs").and_then(|v| v.as_object()) {
                for (k, v) in over {
                    merged.insert(k.clone(), v.clone());
                }
            }
            let merged_params = override_body
                .get("params")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or(params);

            let inputs_map: HashMap<String, Value> =
                serde_json::from_value(Value::Object(merged)).unwrap_or_default();
            match crate::api::execute::execution::submit_pipeline_for_schedule_typed(
                &state,
                &pipeline_id,
                inputs_map,
                merged_params,
            )
            .await
            {
                Ok(task_id) => {
                    write_webhook_trigger(
                        &state.root,
                        &id,
                        &entry.name,
                        "pipeline",
                        Some(&task_id),
                        "submitted",
                        None,
                    );
                    (StatusCode::ACCEPTED, Json(json!({ "ok": true, "task_id": task_id })))
                }
                Err(e) => {
                    let detail = e.to_string();
                    write_webhook_trigger(
                        &state.root,
                        &id,
                        &entry.name,
                        "pipeline",
                        None,
                        "rejected",
                        Some(&detail),
                    );
                    submit_error_response(&state, e).await
                }
            }
        }
        WebhookTarget::Direct { module_id, capability, input_path, params } => {
            // 复用 /api/execute/single 权威实现（autostart / 参数校验 / 缓存全套）；
            // lazy_start=true：POST 快速返回，模块拉起由任务内安全网完成
            let mut single = json!({
                "module_id": module_id,
                "capability": capability,
                "input_path": input_path,
                "params": override_body.get("params").filter(|v| v.is_object()).cloned()
                    .unwrap_or(params),
                "lazy_start": true,
            });
            if let Some(p) = override_body.get("input_path").and_then(|v| v.as_str()) {
                single["input_path"] = json!(p);
            }
            let (status, resp_body) = crate::api::execute::execute_single_run(
                State(state.clone()),
                AxumPath(module_id),
                Some(Json(single)),
            )
            .await;
            let task_id = resp_body.get("task_id").and_then(|v| v.as_str()).map(str::to_string);
            if status == StatusCode::ACCEPTED {
                write_webhook_trigger(
                    &state.root,
                    &id,
                    &entry.name,
                    "direct",
                    task_id.as_deref(),
                    "submitted",
                    None,
                );
                (
                    StatusCode::ACCEPTED,
                    Json(json!({ "ok": true, "task_id": task_id })),
                )
            } else {
                let detail = resp_body.get("error").and_then(|v| v.as_str()).unwrap_or("rejected");
                write_webhook_trigger(
                    &state.root,
                    &id,
                    &entry.name,
                    "direct",
                    None,
                    "rejected",
                    Some(detail),
                );
                (status, resp_body)
            }
        }
    }
}

// ─── 测试 ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use ep_core::config::AppConfig;
    use ep_core::port::PortManager;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::state::AppState;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> std::path::PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("ep-api-hooks-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
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

    fn req(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
        let builder = Request::builder().method(method).uri(uri);
        match body {
            Some(v) => builder
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        }
    }

    async fn body_json(resp: axum::response::Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    /// 预置一条管线 spec（pipeline 目标存在性校验消费）
    fn seed_pipeline(root: &std::path::Path, id: &str) {
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        let spec = format!(
            r#"
[pipeline]
id = "{id}"
name = "hook-target"
description = ""

[[nodes]]
id = "in"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "out"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["in", "output"]
to = ["out", "input"]
"#
        );
        std::fs::write(dir.join(format!("{}.toml", id.replace('-', "_"))), spec).unwrap();
    }

    fn pipeline_target_body(tag: &str, pipeline_id: &str) -> Value {
        json!({
            "name": format!("钩子-{tag}"),
            "enabled": true,
            "target": {
                "type": "pipeline",
                "pipeline_id": pipeline_id,
                "inputs": {},
                "params": {}
            }
        })
    }

    // ── 创建：目标存在性校验 ───────────────────────────────────────────────

    #[tokio::test]
    async fn create_requires_name_and_target() {
        let root = unique_root("validate");
        let state = test_state(root.clone());
        let app = super::router().with_state(state.clone());

        // 缺 name
        let resp = app
            .oneshot(req("POST", "/hooks", Some(json!({"target": {"type": "pipeline", "pipeline_id": "p"}}))))
            .await
            .unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], ep_core::i18n::t("zh-CN", "apiCore.hook.nameRequired", &[]));

        // 缺 target
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("POST", "/hooks", Some(json!({"name": "x"})))).await.unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], ep_core::i18n::t("zh-CN", "apiCore.hook.targetRequired", &[]));

        // target 形状非法（未知 type）
        let app = super::router().with_state(state);
        let resp = app
            .oneshot(req("POST", "/hooks", Some(json!({"name": "x", "target": {"type": "bogus"}}))))
            .await
            .unwrap();
        assert_eq!(body_json(resp).await.0, StatusCode::BAD_REQUEST);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn create_validates_pipeline_and_direct_targets() {
        let root = unique_root("targets");
        let state = test_state(root.clone());

        // 管线不存在 → 404
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(req("POST", "/hooks", Some(pipeline_target_body("ghost", "no-such"))))
            .await
            .unwrap();
        assert_eq!(body_json(resp).await.0, StatusCode::NOT_FOUND, "管线目标必须校验存在性");

        // 管线存在 → 创建成功，返回 8 位 hook_id
        seed_pipeline(&root, "video-to-srt");
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(req("POST", "/hooks", Some(pipeline_target_body("ok", "video-to-srt"))))
            .await
            .unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        let hook_id = body["id"].as_str().unwrap().to_string();
        assert_eq!(hook_id.len(), 8);

        // direct 目标：模块不存在 → 404
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(req(
                "POST",
                "/hooks",
                Some(json!({
                    "name": "direct-ghost",
                    "target": {"type": "direct", "module_id": "ghost-mod",
                               "capability": "transcribe", "input_path": "store:media/a.wav"}
                })),
            ))
            .await
            .unwrap();
        assert_eq!(body_json(resp).await.0, StatusCode::NOT_FOUND, "模块目标必须校验存在性");

        // 注册表确有 hook 条目
        let registry = WebhookRegistry::load(&super::webhooks_path(&state));
        assert!(registry.entries.contains_key(&hook_id));

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 触发链路：pipeline 目标提交成功 + 事件落盘 + 停用 409 ──────────────

    #[tokio::test]
    async fn trigger_pipeline_target_submits_and_logs_event() {
        let root = unique_root("trigger");
        let state = test_state(root.clone());
        seed_pipeline(&root, "video-to-srt");

        // 创建
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(req("POST", "/hooks", Some(pipeline_target_body("trg", "video-to-srt"))))
            .await
            .unwrap();
        let (_, body) = body_json(resp).await;
        let hook_id = body["id"].as_str().unwrap().to_string();

        // 触发（管线存在、file_input 校验通过 → 提交入队 202）
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("POST", &format!("/hooks/{hook_id}"), None)).await.unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED, "触发应提交成功: {body}");
        assert_eq!(body["ok"], true);
        let task_id = body["task_id"].as_str().expect("task_id").to_string();
        assert!(task_id.starts_with("task-"));

        // 事件日志有 submitted 记录
        let events = crate::eventlog::read_events(&root, None, Some("webhook_trigger"), 10);
        assert_eq!(events.len(), 1, "应有一条 webhook_trigger 事件");
        assert_eq!(events[0]["hook"], hook_id.as_str());
        assert_eq!(events[0]["status"], "submitted");
        assert_eq!(events[0]["task_id"], task_id.as_str());

        // 停用后触发 → 409
        let app = super::router().with_state(state.clone());
        let mut put = pipeline_target_body("trg", "video-to-srt");
        put["enabled"] = json!(false);
        let resp = app.oneshot(req("PUT", &format!("/hooks/{hook_id}"), Some(put))).await.unwrap();
        assert_eq!(body_json(resp).await.0, StatusCode::OK);

        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("POST", &format!("/hooks/{hook_id}"), None)).await.unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], ep_core::i18n::t("zh-CN", "apiCore.hook.disabled", &[("id", hook_id.as_str())]));

        // 未知 hook → 404
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("POST", "/hooks/deadbeef", None)).await.unwrap();
        assert_eq!(body_json(resp).await.0, StatusCode::NOT_FOUND);

        // 删除（幂等）
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("DELETE", &format!("/hooks/{hook_id}"), None)).await.unwrap();
        assert_eq!(body_json(resp).await.0, StatusCode::OK);
        let app = super::router().with_state(state);
        let resp = app.oneshot(req("DELETE", &format!("/hooks/{hook_id}"), None)).await.unwrap();
        assert_eq!(body_json(resp).await.0, StatusCode::OK);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn trigger_rejects_invalid_target_state_and_logs_rejected() {
        let root = unique_root("rejected");
        let state = test_state(root.clone());
        // 目标管线在创建后被删除 → 触发期管线缺失 → rejected + 4xx
        seed_pipeline(&root, "vanishing");
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(req("POST", "/hooks", Some(pipeline_target_body("van", "vanishing"))))
            .await
            .unwrap();
        let (_, body) = body_json(resp).await;
        let hook_id = body["id"].as_str().unwrap().to_string();

        // 删除管线文件后触发
        std::fs::remove_file(root.join("config").join("pipelines").join("vanishing.toml")).unwrap();
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("POST", &format!("/hooks/{hook_id}"), None)).await.unwrap();
        let (status, _) = body_json(resp).await;
        assert!(
            status.is_client_error() || status == StatusCode::ACCEPTED,
            "提交失败应映射 4xx/5xx，实际 {status}"
        );
        assert_ne!(status, StatusCode::ACCEPTED, "管线缺失不得提交成功");

        let events = crate::eventlog::read_events(&root, None, Some("webhook_trigger"), 10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["status"], "rejected");

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 触发期逐次覆盖（inputs override）───────────────────────────────────

    #[tokio::test]
    async fn trigger_body_overrides_template() {
        let root = unique_root("override");
        let state = test_state(root.clone());
        seed_pipeline(&root, "video-to-srt");

        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(req("POST", "/hooks", Some(pipeline_target_body("ovr", "video-to-srt"))))
            .await
            .unwrap();
        let (_, body) = body_json(resp).await;
        let hook_id = body["id"].as_str().unwrap().to_string();

        // 触发体带 inputs override → 提交成功（形状覆盖仅影响注入内容；
        // 物理路径注入，store: 逻辑名解析见 stores.rs 专项测试）
        let input_file = root.join("override-input.wav");
        std::fs::write(&input_file, b"x").unwrap();
        let app = super::router().with_state(state);
        let resp = app
            .oneshot(req(
                "POST",
                &format!("/hooks/{hook_id}"),
                Some(json!({"inputs": {"in": {"path": input_file.to_string_lossy()}}})),
            ))
            .await
            .unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED, "override 触发应提交成功: {body}");

        let _ = std::fs::remove_dir_all(&root);
    }
}
