//! 工作区整体导出/迁移 API（Wave2 T-31，handoff §6 + 审计报告·补充建议 2）。
//!
//! 服务「旧机 → 新机整体迁移」：config + 管线 + cron + watcher（含水位线与
//! T-25 产物登记表）+ webhook 注册表 + 模块注册状态，一键导出为**单个 JSON
//! 迁移包**、在新机一键导入。
//!
//! # 载体选型：JSON 单文件（非 zip）
//!
//! 各段本就是结构化 JSON（AppConfig serde / entrypoint-pipeline/v1 描述符 /
//! runtime/*.json 注册表原样），包内**不含任何二进制内容**（venv / 模型权重 /
//! 模块源码目录明确排除）；单 JSON 零新增依赖、可直接 diff 与人工检查，
//! zip 只在携带二进制时有收益——超出本票范围。
//!
//! # 端点
//!
//! - `GET  /api/workspace/export?include_secrets=`
//!   默认（`include_secrets` 缺省/false）：config 段密钥掩码导出——
//!   `server.token` / `api.token` / `llm.providers[].api_key` 置空串，
//!   是否已配置经包顶层 `server_token_set` / `api_token_set` 表达
//!   （照 ConfigView 模式，api/config.rs T-16）。
//!   `include_secrets=true` / `=1`：明文密钥入包，响应携带 `secret_warning`
//!   风险说明；两种形态响应头均 `Cache-Control: no-store`。
//! - `POST /api/workspace/import?overwrite=`
//!   body 为迁移包 JSON。`format_version` 缺失或不支持 → 400（损坏包整体
//!   拒绝，config 段形状非法同样 400，不做半套迁移）。**不自动覆盖既有
//!   同名资产**：管线 id / cron key（管线 id）/ watcher id / hook id 冲突
//!   逐条列入 `skipped_conflicts` 并默认跳过；`overwrite=true` 时才覆盖。
//!   导入后逐段报告（imported / skipped_conflicts / invalid 计数）。
//!
//! # 段语义
//!
//! - **config**：整包配置（含 `[storage.stores]` 逻辑名）原样迁移；导入走
//!   `PUT /api/config` 同一实现（`put_config`，T-16 语义）——掩码空串 =
//!   「不修改」保留导入端既有密钥，明文值 = 覆盖为新值；换机后物理路径
//!   不匹配时按 T-26 语义保留逻辑名（用户改新机 `[storage.stores].path`
//!   重绑即可，管线内 `store:` 引用不动）。
//! - **pipelines**：复用 `GET /api/pipelines/{id}/export` 的
//!   `entrypoint-pipeline/v1` 描述符（池内物理路径已改写 `store:` 逻辑名，
//!   池外路径原样）；导入经执行层视角全量校验（`spec_to_pipeline`）。
//! - **schedules / watchers**：注册表原样迁移，**水位线（`last_checked` /
//!   `checkpoint`）、在途稳定表（`in_flight`）与速览（`recent`）逐字段
//!   保留**——只搬运数据，不改任何语义；导入落盘后由 main.rs 巡检循环
//!   下轮自动重载（cron 30s / watcher 10s）。
//! - **processed_registry**（T-25 产物登记表）：合并导入（按归一路径去重，
//!   复刻 `record` 语义）；该表在 daemon 内为进程级内存缓存（OnceLock 惰性
//!   加载），**导入后需重启 daemon 才对扫描侧生效**（登记的旧机路径在新机
//!   通常不存在，启动期 `prune_missing` 会自动清扫）。
//! - **webhooks**：hook_id 原样迁移（对外触发 URL 不变）。
//! - **modules**：模块注册状态快照（id/name/version，来自发现表）。
//!   模块本体（`modules/<id>/` 含 venv）不入包——requirements 由新机按
//!   arch 重解析（`resolve_requirements`），模块经既有
//!   `POST /api/modules/import` 重新导入；导入响应的 `modules_missing`
//!   列出包内有、本机未装的模块 id（纯提示，无副作用）。
//!
//! # 明确不做
//!
//! - 不搬运 venv / 模型缓存 / 任务产物 / 事件日志（历史数据，非运行配置）；
//! - 水位线 / in_flight / FileSig 语义零改动；
//! - WebUI 集成留后续票（本票 API 优先，curl 可完成迁移）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

use ep_core::config::AppConfig;

use super::err_response;
use super::pipelines::pipeline_bridge::{self, PipelineSpec};
use crate::schedule::{ScheduleEntry, ScheduleRegistry, WebhookEntry, WebhookRegistry};
use crate::state::AppState;
use crate::watcher::{ProcessedRegistry, WatchRule, WatchRegistry};

/// 迁移包格式锚点：读取方（含未来版本导入器）据此判定包形状。
/// 字段必留——向后兼容的唯一依据。
pub const FORMAT_VERSION: &str = "workspace-migration/v1";

/// T-25 产物登记表落盘文件名（watcher.rs `PROCESSED_FILE_NAME` 同名惯例；
/// 该常量私有，此处按其文件头注释的固定名对齐）。
const PROCESSED_FILE_NAME: &str = "processed_registry.json";

/// 导入请求体上限：管线/注册表均为小体量 JSON，32 MiB 覆盖极端规模
/// （默认 2 MiB 对百级管线场景偏紧）。
const IMPORT_BODY_LIMIT: usize = 32 * 1024 * 1024;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/workspace/export", get(export_workspace))
        // 导入包体积随管线数线性增长：放宽默认 body 限制
        .route(
            "/workspace/import",
            post(import_workspace).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT)),
        )
}

fn pipelines_dir(state: &AppState) -> PathBuf {
    state.root.join("config").join("pipelines")
}

fn runtime_file(state: &AppState, name: &str) -> PathBuf {
    state.root.join("runtime").join(name)
}

fn query_flag(query: &HashMap<String, String>, key: &str) -> bool {
    query
        .get(key)
        .map(|v| v == "true" || v == "1" || v == "yes")
        .unwrap_or(false)
}

/// config 段密钥掩码（ConfigView 同款三处：server.token / api.token /
/// llm.providers[].api_key 一律空串）。直接在包 JSON 上操作，避免构造
/// 中间 AppConfig；`*_set` 布尔由导出端另行置于包顶层。
fn mask_config_secrets(config: &mut Value) {
    if let Some(server) = config.get_mut("server").and_then(Value::as_object_mut) {
        server.insert("token".to_string(), Value::String(String::new()));
    }
    if let Some(api) = config.get_mut("api").and_then(Value::as_object_mut) {
        // api.token 是 Option：统一成空串（与 ConfigView 同款，响应形状稳定）
        api.insert("token".to_string(), Value::String(String::new()));
    }
    if let Some(providers) = config
        .get_mut("llm")
        .and_then(|llm| llm.get_mut("providers"))
        .and_then(Value::as_array_mut)
    {
        for provider in providers.iter_mut() {
            if let Some(obj) = provider.as_object_mut() {
                obj.insert("api_key".to_string(), Value::String(String::new()));
            }
        }
    }
}

/// 收官审查 P2-3：递归掩码疑似密钥的 params 键值（键名含 api_key/token/
/// secret/password，大小写不敏感）。默认导出生效；include_secrets=true 保留
/// 明文（操作员显式选择）。启发式可能误伤同名合法参数——掩码值恒为空串，
/// 导入端可选参数空串=未设（openai-bridge 口径），影响面可接受。
fn mask_sensitive_params(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let lower = key.to_ascii_lowercase();
                if ["api_key", "apikey", "token", "secret", "password"]
                    .iter()
                    .any(|s| lower.contains(s))
                {
                    if val.is_string() {
                        *val = Value::String(String::new());
                    }
                } else {
                    mask_sensitive_params(val);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                mask_sensitive_params(item);
            }
        }
        _ => {}
    }
}

/// GET /api/workspace/export?include_secrets= — 工作区整体导出（T-31）。
///
/// 响应为单个 JSON 迁移包（形状见模块文档）；`include_secrets=true` 时
/// 携带明文密钥并附 `secret_warning`，两种形态响应头均禁缓存
/// （凭据面端点对齐 /api/config 的 T-16 纪律）。
async fn export_workspace(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let include_secrets = query_flag(&query, "include_secrets");

    let config_snapshot = state.config.read().await.clone();
    let server_token_set = !config_snapshot.server.token.trim().is_empty();
    let api_token_set = config_snapshot
        .api
        .token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty());
    let mut config_json = serde_json::to_value(&config_snapshot).unwrap_or(Value::Null);
    if !include_secrets {
        mask_config_secrets(&mut config_json);
    }

    // 管线：复用 entrypoint-pipeline/v1 导出描述符（store: 逻辑名改写同款）
    let dir = pipelines_dir(&state);
    let pipelines: Vec<Value> = super::pipelines::scan_specs_pub(&dir)
        .into_iter()
        .map(|(_, mut spec)| {
            super::stores::rewrite_spec_nodes_to_store_refs(
                &config_snapshot,
                &state.root,
                &mut spec.nodes,
            );
            let mut spec_value = serde_json::to_value(&spec).unwrap_or(Value::Null);
            if !include_secrets {
                // 收官审查 P2-3：节点 params 可能被用户手工填入密钥（如
                // openai-bridge 的 api_key——module.toml 明言勿落盘但无强制），
                // 默认导出对疑似密钥键做启发式掩码
                mask_sensitive_params(&mut spec_value);
            }
            json!({
                "format": "entrypoint-pipeline/v1",
                "exported_at": chrono::Utc::now().to_rfc3339(),
                "spec": spec_value,
            })
        })
        .collect();

    // 注册表段：文件原样反序列化（缺失/损坏按空表——与 daemon 启动加载同款容错）
    let schedules = serde_json::to_value(ScheduleRegistry::load(&runtime_file(
        &state,
        "schedules.json",
    )))
    .unwrap_or(Value::Null);
    let watchers = serde_json::to_value(WatchRegistry::load(&runtime_file(
        &state,
        "watchers.json",
    )))
    .unwrap_or(Value::Null);
    let processed = serde_json::to_value(ProcessedRegistry::load(&runtime_file(
        &state,
        PROCESSED_FILE_NAME,
    )))
    .unwrap_or(Value::Null);
    let webhooks = serde_json::to_value(WebhookRegistry::load(&runtime_file(
        &state,
        "webhooks.json",
    )))
    .unwrap_or(Value::Null);

    // 模块注册状态快照（发现表内清单合法的模块；不含 venv / 模块文件本体）
    let modules: Vec<Value> = {
        let discovered = state.modules.read().await;
        discovered
            .iter()
            .filter_map(|m| m.manifest.as_ref())
            .map(|mf| {
                json!({
                    "id": mf.module.id,
                    "name": mf.module.name,
                    "version": mf.module.version,
                })
            })
            .collect()
    };

    let mut body = json!({
        "format_version": FORMAT_VERSION,
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "include_secrets": include_secrets,
        "server_token_set": server_token_set,
        "api_token_set": api_token_set,
        "config": config_json,
        "pipelines": pipelines,
        "schedules": schedules,
        "watchers": watchers,
        "processed_registry": processed,
        "webhooks": webhooks,
        "modules": modules,
    });
    if include_secrets {
        body["secret_warning"] = json!(
            "本迁移包含明文密钥（server.token / api.token / llm api_key），请妥善保管传输与存储介质，导入完成后建议删除该文件"
        );
    }

    let mut resp = (StatusCode::OK, Json(body)).into_response();
    // 凭据面端点：禁缓存（掩码响应同样带，与 /api/config 纪律一致）
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_static("attachment; filename=\"entrypoint-workspace-export.json\""),
    );
    resp
}

/// 单条注册表条目的导入结局。
enum EntryOutcome {
    /// 已落表（新增或 overwrite 覆盖）
    Imported,
    /// 键已存在且未 overwrite → 冲突跳过
    Conflict,
    /// 条目形状非法
    Invalid,
}

/// 注册表段导入的统一形状（schedule / watcher / webhook 三段同构）：
/// load → 逐条解析与冲突判定 → 有变更才原子落盘。
trait RegistryImport: Sized {
    fn load_from(path: &Path) -> Self;
    fn save_to(&self, path: &Path) -> std::io::Result<()>;
    /// 解析一条并按冲突语义落表（键已存在且未 overwrite → Conflict）
    fn upsert(&mut self, key: &str, value: &Value, overwrite: bool) -> EntryOutcome;
}

/// 单段导入账目（imported / skipped_conflicts / invalid 逐条列出）。
#[derive(Default)]
struct SectionReport {
    imported: Vec<String>,
    skipped_conflicts: Vec<String>,
    invalid: Vec<Value>,
}

impl SectionReport {
    fn record(&mut self, key: &str, outcome: EntryOutcome) {
        match outcome {
            EntryOutcome::Imported => self.imported.push(key.to_string()),
            EntryOutcome::Conflict => self.skipped_conflicts.push(key.to_string()),
            EntryOutcome::Invalid => self.invalid.push(json!({
                "id": key,
                "detail": "malformed entry",
            })),
        }
    }

    /// 收官审查 P2-2：段级失败（如落盘失败）标记——把已计入的 Imported 降级
    /// 为 invalid（detail 携带原因），操作员不会误信该段已迁移成功
    fn mark_failed(&mut self, detail: String) {
        for key in std::mem::take(&mut self.imported) {
            self.invalid.push(json!({ "id": key, "detail": detail }));
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "imported": self.imported,
            "skipped_conflicts": self.skipped_conflicts,
            "invalid": self.invalid,
        })
    }
}

/// 注册表段导入共用骨架：段缺失/非对象 → 空报告；逐条 upsert → 有变更才落盘。
fn import_registry_section<R: RegistryImport>(
    path: &Path,
    section: Option<&Value>,
    overwrite: bool,
) -> SectionReport {
    let mut report = SectionReport::default();
    let Some(map) = section.and_then(Value::as_object) else {
        return report;
    };
    let mut registry = R::load_from(path);
    let mut changed = false;
    for (key, value) in map {
        let outcome = registry.upsert(key, value, overwrite);
        if matches!(outcome, EntryOutcome::Imported) {
            changed = true;
        }
        report.record(key, outcome);
    }
    if changed {
        // 收官审查 P2-2：落盘失败不得仍记 Imported（操作员会误信迁移成功）
        if let Err(e) = registry.save_to(path) {
            tracing::warn!(
                file = %path.display(),
                error = %e,
                "workspace import: failed to persist registry"
            );
            report.mark_failed(format!(
                "registry persist failed: {e}（该段条目仅在内存，未落盘）"
            ));
        }
    }
    report
}

impl RegistryImport for ScheduleRegistry {
    fn load_from(path: &Path) -> Self {
        ScheduleRegistry::load(path)
    }
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        self.save(path)
    }
    fn upsert(&mut self, key: &str, value: &Value, overwrite: bool) -> EntryOutcome {
        let entry: ScheduleEntry = match serde_json::from_value(value.clone()) {
            Ok(entry) => entry,
            Err(_) => return EntryOutcome::Invalid,
        };
        if self.entries.contains_key(key) && !overwrite {
            return EntryOutcome::Conflict;
        }
        self.entries.insert(key.to_string(), entry);
        EntryOutcome::Imported
    }
}

impl RegistryImport for WatchRegistry {
    fn load_from(path: &Path) -> Self {
        WatchRegistry::load(path)
    }
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        self.save(path)
    }
    fn upsert(&mut self, key: &str, value: &Value, overwrite: bool) -> EntryOutcome {
        let rule: WatchRule = match serde_json::from_value(value.clone()) {
            Ok(rule) => rule,
            Err(_) => return EntryOutcome::Invalid,
        };
        if self.rules.contains_key(key) && !overwrite {
            return EntryOutcome::Conflict;
        }
        self.rules.insert(key.to_string(), rule);
        EntryOutcome::Imported
    }
}

impl RegistryImport for WebhookRegistry {
    fn load_from(path: &Path) -> Self {
        WebhookRegistry::load(path)
    }
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        self.save(path)
    }
    fn upsert(&mut self, key: &str, value: &Value, overwrite: bool) -> EntryOutcome {
        let entry: WebhookEntry = match serde_json::from_value(value.clone()) {
            Ok(entry) => entry,
            Err(_) => return EntryOutcome::Invalid,
        };
        if self.entries.contains_key(key) && !overwrite {
            return EntryOutcome::Conflict;
        }
        self.entries.insert(key.to_string(), entry);
        EntryOutcome::Imported
    }
}

/// POST /api/workspace/import?overwrite= — 导入迁移包（T-31）。
///
/// 语义见模块文档。返回 200 + 逐段报告；包级损坏（非 JSON / format_version
/// 缺失或不支持 / config 段形状非法）→ 400 且不做任何落盘。
async fn import_workspace(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    body: String,
) -> (StatusCode, Json<Value>) {
    let overwrite = query_flag(&query, "overwrite");

    let package: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiCore.workspace.invalidPackage",
                &[("detail", e.to_string())],
            )
            .await;
        }
    };
    if package.get("format_version").and_then(Value::as_str) != Some(FORMAT_VERSION) {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiCore.workspace.unsupportedFormat",
            &[(
                "detail",
                format!(
                    "format_version must be \"{FORMAT_VERSION}\" (got {:?})",
                    package.get("format_version").cloned().unwrap_or(Value::Null)
                ),
            )],
        )
        .await;
    }

    // ── config 段：先整体形状校验（损坏包 400，不做半套迁移），再走
    //    PUT /api/config 同一实现——T-16 密钥语义免费获得：掩码空串 =
    //    「不修改」（take_token_value / preserve_llm_provider_keys 保留
    //    导入端既有值），明文值 = 覆盖；requires_restart 一并回报。
    let mut config_report = json!({ "applied": false, "requires_restart": false });
    if let Some(config_value) = package.get("config").filter(|v| !v.is_null()).cloned() {
        if let Err(e) = serde_json::from_value::<AppConfig>(config_value.clone()) {
            return err_response(
                &state,
                StatusCode::BAD_REQUEST,
                "apiCore.workspace.invalidPackage",
                &[("detail", format!("config section: {e}"))],
            )
            .await;
        }
        match super::config::put_config::<Value>(State(state.clone()), Json(config_value)).await {
            Ok(resp) => {
                config_report = json!({
                    "applied": true,
                    "requires_restart": resp.0.requires_restart,
                });
            }
            // put_config 的 400（patch 非法）/ 500（落盘失败）原样透传
            Err((code, body)) => return (code, body),
        }
    }

    // ── pipelines 段：描述符 → spec → 执行层视角校验 → 落盘
    //    （冲突 = 同 id 管线已存在；overwrite 时复刻 PUT 的旧文件清理）
    let mut pipelines_report = SectionReport::default();
    if let Some(list) = package.get("pipelines").and_then(Value::as_array) {
        let dir = pipelines_dir(&state);
        for item in list {
            let spec_id = item
                .pointer("/spec/pipeline/id")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let spec: PipelineSpec = match item
                .get("spec")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
            {
                Some(spec) => spec,
                None => {
                    pipelines_report.invalid.push(json!({
                        "id": spec_id,
                        "detail": "malformed pipeline descriptor",
                    }));
                    continue;
                }
            };
            if let Err(e) = pipeline_bridge::spec_to_pipeline(&spec) {
                pipelines_report.invalid.push(json!({
                    "id": spec.pipeline.id,
                    "detail": e.to_string(),
                }));
                continue;
            }
            let id = spec.pipeline.id.clone();
            let existing = super::pipelines::scan_specs_pub(&dir);
            let conflict = existing.iter().any(|(_, s)| s.pipeline.id == id);
            if conflict && !overwrite {
                pipelines_report.skipped_conflicts.push(id);
                continue;
            }
            let target = dir.join(format!("{}.toml", id.replace('-', "_")));
            if conflict {
                // 清理同一 id 的非规范旧文件（PUT /api/pipelines/{id} 同款）
                for (path, existing) in &existing {
                    if existing.pipeline.id == id && path != &target {
                        if let Err(e) = std::fs::remove_file(path) {
                            tracing::warn!(
                                file = %path.display(),
                                error = %e,
                                "workspace import: failed to clean up stale pipeline file"
                            );
                        }
                    }
                }
            }
            match pipeline_bridge::save_spec(&spec, &target) {
                Ok(()) => pipelines_report.imported.push(id),
                Err(e) => pipelines_report.invalid.push(json!({
                    "id": spec.pipeline.id,
                    "detail": e.to_string(),
                })),
            }
        }
    }

    // ── schedules 段：键 = 管线 id；水位线 last_checked 随条目原样保留
    let schedules_report = import_registry_section::<ScheduleRegistry>(
        &runtime_file(&state, "schedules.json"),
        package.get("schedules"),
        overwrite,
    );

    // ── watchers 段：规则原样落表（checkpoint / in_flight / recent 保留）
    let watchers_report = import_registry_section::<WatchRegistry>(
        &crate::watcher::default_registry_path(&state.root),
        package.get("watchers"),
        overwrite,
    );

    // ── webhooks 段：hook_id 原样迁移（对外触发 URL 不变）
    let webhooks_report = import_registry_section::<WebhookRegistry>(
        &crate::schedule::default_webhook_registry_path(&state.root),
        package.get("webhooks"),
        overwrite,
    );

    // ── processed_registry 段（T-25）：合并导入（按归一路径去重，record 语义）；
    //    内存表为进程级缓存，导入后需重启 daemon 才对扫描侧生效（模块文档）
    let mut processed_report = json!({ "merged": 0 });
    if let Some(value) = package.get("processed_registry").filter(|v| !v.is_null()) {
        if let Ok(incoming) = serde_json::from_value::<ProcessedRegistry>(value.clone()) {
            let path = runtime_file(&state, PROCESSED_FILE_NAME);
            let mut local = ProcessedRegistry::load(&path);
            let mut merged = 0usize;
            for entry in incoming.entries.iter() {
                local.record(
                    Path::new(&entry.path),
                    &entry.rule_id,
                    &entry.task_id,
                    entry.ts,
                    entry.sha256.clone(),
                );
                merged += 1;
            }
            if merged > 0 {
                if let Err(e) = local.save(&path) {
                    tracing::warn!(
                        file = %path.display(),
                        error = %e,
                        "workspace import: failed to persist processed registry"
                    );
                }
            }
            processed_report = json!({ "merged": merged });
        }
    }

    // ── modules 段：纯提示性 diff（包内有、本机未装的模块 id），无副作用
    let mut modules_missing: Vec<String> = Vec::new();
    if let Some(list) = package.get("modules").and_then(Value::as_array) {
        let local_ids: std::collections::HashSet<String> = {
            let discovered = state.modules.read().await;
            discovered
                .iter()
                .filter_map(|m| m.manifest.as_ref().map(|mf| mf.module.id.clone()))
                .collect()
        };
        for m in list {
            if let Some(id) = m.get("id").and_then(Value::as_str) {
                if !local_ids.contains(id) {
                    modules_missing.push(id.to_string());
                }
            }
        }
    }

    let report = json!({
        "ok": true,
        "overwrite": overwrite,
        "config": config_report,
        "pipelines": pipelines_report.to_json(),
        "schedules": schedules_report.to_json(),
        "watchers": watchers_report.to_json(),
        "webhooks": webhooks_report.to_json(),
        "processed_registry": processed_report,
        "modules_missing": modules_missing,
    });
    (StatusCode::OK, Json(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::schedule::WebhookTarget;
    use crate::watcher::{FileSig, PipelineAction};

    use super::super::pipelines::scan_specs_pub;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-workspace-{tag}-{}-{seq}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn state_at(root: PathBuf) -> Arc<AppState> {
        state_with_modules(root, vec![])
    }

    fn state_with_modules(
        root: PathBuf,
        modules: Vec<ep_core::module::discovery::DiscoveredModule>,
    ) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            ep_core::config::AppConfig::default(),
            vec![],
            modules,
            ep_core::port::PortManager::new(18000, 19000),
        ))
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn post_json(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn oneshot(
        state: &Arc<AppState>,
        req: Request<Body>,
    ) -> (StatusCode, std::collections::HashMap<String, String>, Value) {
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let headers: std::collections::HashMap<String, String> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, value)
    }

    // ── fixtures ──────────────────────────────────────────────────────────

    fn seed_pipeline(root: &Path, id: &str, name: &str) {
        let dir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&dir).unwrap();
        let toml = format!(
            r#"
[pipeline]
id = "{id}"
name = "{name}"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "work"
kind = "builtin"
builtin = "file_output"
"#
        );
        std::fs::write(dir.join(format!("{}.toml", id.replace('-', "_"))), toml).unwrap();
    }

    fn seed_schedule(root: &Path, key: &str, cron: &str, last_checked: i64) {
        let path = crate::schedule::default_registry_path(root);
        let mut reg = ScheduleRegistry::load(&path);
        reg.entries.insert(
            key.to_string(),
            ScheduleEntry {
                cron: cron.to_string(),
                enabled: true,
                inputs: Value::Null,
                params: Value::Null,
                last_checked,
                last_task_id: None,
            },
        );
        reg.save(&path).unwrap();
    }

    fn seed_watcher(root: &Path, id: &str, checkpoint: i64) {
        let path = crate::watcher::default_registry_path(root);
        let mut reg = WatchRegistry::load(&path);
        let mut in_flight = std::collections::HashMap::new();
        in_flight.insert("a.wav".to_string(), FileSig { mtime: 111, size: 222 });
        reg.rules.insert(
            id.to_string(),
            WatchRule {
                id: id.to_string(),
                name: "watch-x".to_string(),
                enabled: true,
                watch_dir: std::env::temp_dir().to_string_lossy().to_string(),
                recursive: false,
                extensions: vec![],
                exclude: vec![],
                dedup: crate::watcher::WatcherDedupConfig::default(),
                include_modified: false,
                stability_secs: 30,
                backfill: false,
                checkpoint,
                in_flight,
                max_batch: 16,
                direct: None,
                pipeline: Some(PipelineAction {
                    pipeline_id: "pipe-x".to_string(),
                    input_node: "input".to_string(),
                }),
                output: None,
                last_task_id: None,
                recent: VecDeque::new(),
            },
        );
        reg.save(&path).unwrap();
    }

    fn seed_webhook(root: &Path, id: &str, name: &str) {
        let path = crate::schedule::default_webhook_registry_path(root);
        let mut reg = WebhookRegistry::load(&path);
        reg.entries.insert(
            id.to_string(),
            WebhookEntry {
                id: id.to_string(),
                name: name.to_string(),
                enabled: true,
                target: WebhookTarget::Pipeline {
                    pipeline_id: "pipe-x".to_string(),
                    inputs: Value::Null,
                    params: Value::Null,
                },
            },
        );
        reg.save(&path).unwrap();
    }

    fn seed_processed(root: &Path, product_path: &str) {
        let path = root.join("runtime").join(PROCESSED_FILE_NAME);
        let mut reg = ProcessedRegistry::load(&path);
        reg.record(
            Path::new(product_path),
            "waaa1111",
            "task-1",
            1_700_000_000,
            None,
        );
        reg.save(&path).unwrap();
    }

    fn demo_module() -> ep_core::module::discovery::DiscoveredModule {
        let manifest: ep_core::module::manifest::ModuleManifest = toml::from_str(
            r#"
[module]
id = "demo-mod"
name = "Demo"
version = "1.2.3"
description = "t"
category = "image"
genre = "test"

[runtime]
type = "native"
binaries = { "x" = "x" }

[compute]
backends = ["cpu"]

[interface]
type = "http"
"#,
        )
        .unwrap();
        ep_core::module::discovery::DiscoveredModule {
            manifest: Some(manifest),
            path: PathBuf::from("modules/demo-mod"),
            status: ep_core::module::discovery::DiscoveryStatus::Valid,
        }
    }

    const WATCHER_ID: &str = "waaa1111";
    const HOOK_ID: &str = "hookaaa1";
    const WATERMARK_SCHEDULE: i64 = 1_700_000_123;
    const WATERMARK_CHECKPOINT: i64 = 1_700_000_456;

    // ── 导出：全部段 + format_version ─────────────────────────────────────

    /// 导出包含全部段且 format_version 在；各段内容与落盘注册表一致。
    #[tokio::test]
    async fn export_contains_all_sections_and_format_version() {
        let root = unique_root("export-all");
        seed_pipeline(&root, "pipe-x", "X");
        seed_schedule(&root, "pipe-x", "0 3 * * *", WATERMARK_SCHEDULE);
        seed_watcher(&root, WATCHER_ID, WATERMARK_CHECKPOINT);
        seed_webhook(&root, HOOK_ID, "hook");
        seed_processed(&root, "/tmp/old/out.png");
        let state = state_at(root);

        let (status, headers, body) = oneshot(&state, get("/workspace/export")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("cache-control").map(String::as_str), Some("no-store"));

        assert_eq!(body["format_version"], FORMAT_VERSION);
        assert_eq!(body["include_secrets"], false);
        assert_eq!(body["server_token_set"], false);
        assert_eq!(body["api_token_set"], false);
        assert!(body["config"].is_object(), "config 段必须在");
        assert!(body["schedules"].is_object(), "schedules 段必须在");
        assert!(body["watchers"].is_object(), "watchers 段必须在");
        assert!(body["processed_registry"].is_object(), "processed_registry 段必须在");
        assert!(body["webhooks"].is_object(), "webhooks 段必须在");
        assert!(body["modules"].is_array(), "modules 段必须在");

        // 管线段 = entrypoint-pipeline/v1 描述符数组
        let pipelines = body["pipelines"].as_array().unwrap();
        assert_eq!(pipelines.len(), 1);
        assert_eq!(pipelines[0]["format"], "entrypoint-pipeline/v1");
        assert_eq!(pipelines[0]["spec"]["pipeline"]["id"], "pipe-x");

        // 注册表段内容与落盘一致（flatten map 形状）
        assert_eq!(body["schedules"]["pipe-x"]["cron"], "0 3 * * *");
        assert_eq!(body["watchers"][WATCHER_ID]["checkpoint"], WATERMARK_CHECKPOINT);
        assert_eq!(body["webhooks"][HOOK_ID]["name"], "hook");
        assert_eq!(
            body["processed_registry"]["entries"].as_array().map(Vec::len),
            Some(1)
        );

        let _ = std::fs::remove_dir_all(&state.root);
    }

    // ── 密钥安全（P0）：默认掩码零泄漏 + include_secrets 才有明文 ─────────

    fn secrets_config() -> ep_core::config::AppConfig {
        let mut config = ep_core::config::AppConfig::default();
        config.server.token = "server-secret-xyz".to_string();
        config.api.token = Some("api-secret-xyz".to_string());
        config.llm.providers = vec![ep_core::config::LlmProviderConfig {
            id: "prov1".to_string(),
            name: "P1".to_string(),
            base_url: "https://api.example.com".to_string(),
            api_key: "llm-secret-xyz".to_string(),
            timeout_secs: ep_core::config::LlmProviderConfig::DEFAULT_TIMEOUT_SECS,
            rpm: None,
            concurrency: None,
            proxy: None,
            enabled: true,
        }];
        config
    }

    /// 收官审查 P2-3：params 疑似密钥键的启发式掩码（递归、大小写不敏感、
    /// 非字符串值原样保留）
    #[test]
    fn mask_sensitive_params_heuristic() {
        let mut v = json!({
            "nodes": {
                "n1": { "params": { "api_key": "sk-123", "Prompt": "keep", "voice": "af_heart" } },
                "n2": { "params": { "X-Token": "t", "timeout": 30 } },
                "nested": [ { "secret_value": "s", "n": 1 } ]
            }
        });
        mask_sensitive_params(&mut v);
        assert_eq!(v["nodes"]["n1"]["params"]["api_key"], "");
        assert_eq!(v["nodes"]["n1"]["params"]["Prompt"], "keep");
        assert_eq!(v["nodes"]["n1"]["params"]["voice"], "af_heart");
        assert_eq!(v["nodes"]["n2"]["params"]["X-Token"], "");
        assert_eq!(v["nodes"]["n2"]["params"]["timeout"], 30);
        assert_eq!(v["nodes"]["nested"][0]["secret_value"], "");
        assert_eq!(v["nodes"]["nested"][0]["n"], 1);
    }

    /// 默认导出：三处密钥明文零泄漏；*_set 布尔照 ConfigView 模式表达到位。
    #[tokio::test]
    async fn export_masks_secrets_by_default_with_zero_leak() {
        let root = unique_root("export-mask");
        let state = state_at(root);
        *state.config.write().await = secrets_config();

        let (status, headers, body) = oneshot(&state, get("/workspace/export")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("cache-control").map(String::as_str), Some("no-store"));

        // 明文零泄漏断言：整个响应序列化后不含任一密钥
        let text = serde_json::to_string(&body).unwrap();
        assert!(!text.contains("server-secret-xyz"), "server.token 明文泄漏");
        assert!(!text.contains("api-secret-xyz"), "api.token 明文泄漏");
        assert!(!text.contains("llm-secret-xyz"), "llm api_key 明文泄漏");

        // 掩码形状 + 已配置布尔（ConfigView 模式）
        assert_eq!(body["config"]["server"]["token"], "");
        assert_eq!(body["config"]["api"]["token"], "");
        assert_eq!(body["config"]["llm"]["providers"][0]["api_key"], "");
        assert_eq!(body["server_token_set"], true);
        assert_eq!(body["api_token_set"], true);
        assert!(body.get("secret_warning").is_none(), "掩码导出不应带风险说明");

        let _ = std::fs::remove_dir_all(&state.root);
    }

    /// include_secrets=true：明文入包 + secret_warning 风险说明。
    #[tokio::test]
    async fn export_include_secrets_emits_plaintext_and_warning() {
        let root = unique_root("export-plain");
        let state = state_at(root);
        *state.config.write().await = secrets_config();

        let (status, _, body) =
            oneshot(&state, get("/workspace/export?include_secrets=true")).await;
        assert_eq!(status, StatusCode::OK);

        let text = serde_json::to_string(&body).unwrap();
        assert!(text.contains("server-secret-xyz"), "server.token 明文应在包内");
        assert!(text.contains("api-secret-xyz"), "api.token 明文应在包内");
        assert!(text.contains("llm-secret-xyz"), "llm api_key 明文应在包内");
        assert_eq!(body["include_secrets"], true);
        assert!(body["secret_warning"].is_string(), "明文导出必须带风险说明");

        let _ = std::fs::remove_dir_all(&state.root);
    }

    // ── 导入：新路径恢复 + 水位线保留 ─────────────────────────────────────

    /// 导出包导入全新 root：管线/cron/watcher/webhook 全恢复，
    /// 水位线（last_checked / checkpoint）与在途表逐字段保留。
    #[tokio::test]
    async fn import_restores_assets_on_fresh_root_with_watermarks() {
        let root_a = unique_root("mig-a");
        seed_pipeline(&root_a, "pipe-x", "X");
        seed_schedule(&root_a, "pipe-x", "0 3 * * *", WATERMARK_SCHEDULE);
        seed_watcher(&root_a, WATCHER_ID, WATERMARK_CHECKPOINT);
        seed_webhook(&root_a, HOOK_ID, "hook");
        seed_processed(&root_a, "/tmp/old-machine/out.png");
        let state_a = state_with_modules(root_a, vec![demo_module()]);

        let (_, _, package) = oneshot(&state_a, get("/workspace/export")).await;
        let package_text = serde_json::to_string(&package).unwrap();

        // 全新机器：空 root、无模块
        let state_b = state_at(unique_root("mig-b"));
        let (status, _, report) =
            oneshot(&state_b, post_json("/workspace/import", &package_text)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(report["ok"], true);
        assert_eq!(report["config"]["applied"], true);
        assert_eq!(report["pipelines"]["imported"], json!(["pipe-x"]));
        assert_eq!(report["schedules"]["imported"], json!(["pipe-x"]));
        assert_eq!(report["watchers"]["imported"], json!([WATCHER_ID]));
        assert_eq!(report["webhooks"]["imported"], json!([HOOK_ID]));
        assert_eq!(report["processed_registry"]["merged"], 1);
        // 包内有、本机未装的模块 → 提示性清单
        assert_eq!(report["modules_missing"], json!(["demo-mod"]));

        // 管线文件恢复（扫描口径按 spec id，不假设文件名）
        let restored = scan_specs_pub(&pipelines_dir(&state_b));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].1.pipeline.id, "pipe-x");

        // cron 水位线保留：last_checked 原样
        let schedules = ScheduleRegistry::load(&crate::schedule::default_registry_path(&state_b.root));
        let entry = schedules.entries.get("pipe-x").expect("cron 条目应恢复");
        assert_eq!(entry.last_checked, WATERMARK_SCHEDULE, "cron 水位线必须保留");
        assert_eq!(entry.cron, "0 3 * * *");

        // watcher 水位线 + 在途表保留
        let watchers =
            WatchRegistry::load(&crate::watcher::default_registry_path(&state_b.root));
        let rule = watchers.rules.get(WATCHER_ID).expect("watcher 应恢复");
        assert_eq!(rule.checkpoint, WATERMARK_CHECKPOINT, "watcher 水位线必须保留");
        assert!(
            rule.in_flight.contains_key("a.wav"),
            "在途稳定表必须随规则迁移"
        );

        // webhook 原样（hook_id 不变 → 对外触发 URL 不变）
        let webhooks = WebhookRegistry::load(&crate::schedule::default_webhook_registry_path(
            &state_b.root,
        ));
        assert_eq!(webhooks.entries[HOOK_ID].name, "hook");

        // T-25 产物登记表落盘恢复
        let processed =
            ProcessedRegistry::load(&state_b.root.join("runtime").join(PROCESSED_FILE_NAME));
        assert_eq!(processed.entries.len(), 1);
        assert_eq!(processed.entries[0].task_id, "task-1");

        let _ = std::fs::remove_dir_all(&state_a.root);
        let _ = std::fs::remove_dir_all(&state_b.root);
    }

    // ── 导入：冲突跳过 + overwrite 覆盖 ───────────────────────────────────

    /// 同名资产默认逐条跳过（本地值不动）；overwrite=true 才覆盖。
    #[tokio::test]
    async fn import_conflicts_skipped_then_overwritten() {
        let root_a = unique_root("conf-a");
        seed_pipeline(&root_a, "pipe-x", "FromA");
        seed_schedule(&root_a, "pipe-x", "0 3 * * *", 111_111_111);
        seed_watcher(&root_a, WATCHER_ID, 222_222_222);
        seed_webhook(&root_a, HOOK_ID, "FromA");
        let state_a = state_at(root_a);
        let (_, _, package) = oneshot(&state_a, get("/workspace/export")).await;
        let package_text = serde_json::to_string(&package).unwrap();

        let root_b = unique_root("conf-b");
        seed_pipeline(&root_b, "pipe-x", "LocalB");
        seed_schedule(&root_b, "pipe-x", "59 23 * * *", 42);
        seed_watcher(&root_b, WATCHER_ID, 7);
        seed_webhook(&root_b, HOOK_ID, "LocalB");
        let state_b = state_at(root_b);

        // 默认：四类同名资产全部冲突跳过，本地值不动
        let (status, _, report) =
            oneshot(&state_b, post_json("/workspace/import", &package_text)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(report["pipelines"]["skipped_conflicts"], json!(["pipe-x"]));
        assert_eq!(report["schedules"]["skipped_conflicts"], json!(["pipe-x"]));
        assert_eq!(report["watchers"]["skipped_conflicts"], json!([WATCHER_ID]));
        assert_eq!(report["webhooks"]["skipped_conflicts"], json!([HOOK_ID]));

        let schedules =
            ScheduleRegistry::load(&crate::schedule::default_registry_path(&state_b.root));
        assert_eq!(schedules.entries["pipe-x"].cron, "59 23 * * *", "冲突时本地 cron 不动");
        assert_eq!(schedules.entries["pipe-x"].last_checked, 42);
        let watchers =
            WatchRegistry::load(&crate::watcher::default_registry_path(&state_b.root));
        assert_eq!(watchers.rules[WATCHER_ID].checkpoint, 7, "冲突时本地水位线不动");
        let restored = scan_specs_pub(&pipelines_dir(&state_b));
        assert_eq!(restored[0].1.pipeline.name, "LocalB", "冲突时本地管线不动");

        // overwrite=true：同名资产被包内值覆盖
        let (status, _, report) = oneshot(
            &state_b,
            post_json("/workspace/import?overwrite=true", &package_text),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(report["pipelines"]["imported"], json!(["pipe-x"]));
        assert_eq!(report["schedules"]["imported"], json!(["pipe-x"]));
        assert_eq!(report["watchers"]["imported"], json!([WATCHER_ID]));
        assert_eq!(report["webhooks"]["imported"], json!([HOOK_ID]));

        let schedules =
            ScheduleRegistry::load(&crate::schedule::default_registry_path(&state_b.root));
        assert_eq!(schedules.entries["pipe-x"].cron, "0 3 * * *", "overwrite 后 cron 为包内值");
        assert_eq!(schedules.entries["pipe-x"].last_checked, 111_111_111);
        let watchers =
            WatchRegistry::load(&crate::watcher::default_registry_path(&state_b.root));
        assert_eq!(watchers.rules[WATCHER_ID].checkpoint, 222_222_222, "overwrite 后水位线为包内值");
        let restored = scan_specs_pub(&pipelines_dir(&state_b));
        assert_eq!(restored.len(), 1, "overwrite 不得留下重复定义");
        assert_eq!(restored[0].1.pipeline.name, "FromA");

        let _ = std::fs::remove_dir_all(&state_a.root);
        let _ = std::fs::remove_dir_all(&state_b.root);
    }

    // ── 导入：损坏包 400（无半套迁移） ────────────────────────────────────

    #[tokio::test]
    async fn import_corrupt_package_is_rejected_with_400() {
        let state = state_at(unique_root("corrupt"));

        // 非 JSON
        let (status, _, _) = oneshot(&state, post_json("/workspace/import", "not-json")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 缺 format_version
        let (status, _, _) =
            oneshot(&state, post_json("/workspace/import", "{\"config\":{}}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 不支持的版本
        let (status, _, _) = oneshot(
            &state,
            post_json(
                "/workspace/import",
                "{\"format_version\":\"workspace-migration/v9\"}",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // config 段形状非法（server.port 类型错误）→ 400，且无半套迁移：
        // 合法段（pipelines）不得已落盘
        let root_a = unique_root("corrupt-src");
        seed_pipeline(&root_a, "pipe-x", "X");
        let state_a = state_at(root_a);
        let (_, _, package) = oneshot(&state_a, get("/workspace/export")).await;
        let mut broken = package.clone();
        broken["config"]["server"]["port"] = json!("not-a-number");
        let (status, _, _) = oneshot(
            &state,
            post_json("/workspace/import", &serde_json::to_string(&broken).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            scan_specs_pub(&pipelines_dir(&state)).is_empty(),
            "config 段非法时不得半套迁移（管线不应落盘）"
        );

        let _ = std::fs::remove_dir_all(&state.root);
        let _ = std::fs::remove_dir_all(&state_a.root);
    }

    // ── 导入：config 段密钥语义（T-16 对齐） ──────────────────────────────

    /// 掩码包导入 = 保留导入端既有密钥；明文包导入 = 覆盖为新值。
    #[tokio::test]
    async fn import_config_tokens_follow_t16_semantics() {
        let root_a = unique_root("tok-a");
        let state_a = state_at(root_a);
        *state_a.config.write().await = secrets_config();
        let (_, _, masked) = oneshot(&state_a, get("/workspace/export")).await;
        let (_, _, plain) = oneshot(
            &state_a,
            get("/workspace/export?include_secrets=true"),
        )
        .await;

        let root_b = unique_root("tok-b");
        let state_b = state_at(root_b);
        state_b.config.write().await.server.token = "b-local-token".to_string();

        // 掩码包：B 的既有 token 保留（空串 = 不修改）
        let (status, _, report) = oneshot(
            &state_b,
            post_json("/workspace/import", &serde_json::to_string(&masked).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(report["config"]["applied"], true);
        assert_eq!(
            state_b.config.read().await.server.token,
            "b-local-token",
            "掩码包导入不得清掉既有密钥"
        );

        // 明文包：token 覆盖为包内明文（整迁移语义）
        let (status, _, _) = oneshot(
            &state_b,
            post_json("/workspace/import", &serde_json::to_string(&plain).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        {
            let config = state_b.config.read().await;
            assert_eq!(config.server.token, "server-secret-xyz", "明文包应覆盖 server.token");
            assert_eq!(config.api.token.as_deref(), Some("api-secret-xyz"));
            assert_eq!(config.llm.providers[0].api_key, "llm-secret-xyz");
        }

        let _ = std::fs::remove_dir_all(&state_a.root);
        let _ = std::fs::remove_dir_all(&state_b.root);
    }
}
