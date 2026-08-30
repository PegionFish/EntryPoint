//! 触发规则 API（PLAN_TRIGGER_UNIFIED_LOG §5.3）
//!
//! 端点：
//! - `GET  /api/watchers`       — 全部规则列表（含 `recent` 速览）
//! - `POST /api/watchers`       — 创建（服务端生成 8 位小写十六进制 id）
//! - `GET  /api/watchers/{id}`  — 单条规则（未配置 → 404 `apiCore.watcher.notFound`）
//! - `PUT  /api/watchers/{id}`  — 全量更新（校验同 POST，保留运行态字段）
//! - `DELETE /api/watchers/{id}`— 删除（幂等）
//!
//! POST/PUT 共用 §5.3 的 7 步校验链（顺序冻结，不得调整）：
//! 1. `name` 非空 → `watcher.nameRequired`
//! 2. `watch_dir` 非空 → `watchDirRequired`；非绝对 → `watchDirNotAbsolute`
//!    （目录暂不存在仅告警不拒绝）
//! 3. `direct` / `pipeline` 恰好一个 → `actionRequired` / `actionConflict`
//! 4. pipeline 模式：管线存在（复用 scan_specs/find_spec_file 口径，否则 404
//!    `apiPipelines.pipelines.notFound`）且 `input_node` 为 spec 实际节点 →
//!    否则 `inputNodeInvalid`
//! 5. direct-Module：模块与 capability 存在（复用 modules 注册表）→
//!    否则 `capabilityInvalid`
//! 6. direct 模式：`output` 必填且 `dest_dir` 绝对 → `outputRequired`
//! 7. `stability_secs` 钳制 ≥ 5；`extensions` 去点转小写归一
//!
//! 错误响应统一走 `err_response` + i18n 键；注册表落盘失败 → 500
//! `watcher.saveFailed`。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::get,
    Json,
};
use serde_json::{Value, json};

use crate::api::err_response;
use crate::state::AppState;
use crate::watcher::{
    DirectAction, DirectKind, OutputConfig, PipelineAction, WatchRule, WatchRegistry,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/watchers", get(list_watchers).post(create_watcher))
        .route(
            "/watchers/{id}",
            get(get_watcher).put(update_watcher).delete(delete_watcher),
        )
}

fn watchers_path(state: &AppState) -> PathBuf {
    crate::watcher::default_registry_path(&state.root)
}

fn pipelines_dir(state: &AppState) -> PathBuf {
    state.root.join("config").join("pipelines")
}

/// 按 spec id 查找管线（复用 pipelines.rs 的扫描口径，不假设文件名）
fn find_spec(
    dir: &Path,
    id: &str,
) -> Option<crate::api::pipelines::pipeline_bridge::PipelineSpec> {
    crate::api::pipelines::scan_specs_pub(dir)
        .into_iter()
        .find(|(_, spec)| spec.pipeline.id == id)
        .map(|(_, spec)| spec)
}

// ─── body 字段防御式提取（坏形状字段按缺省处理，交给冻结校验链定夺） ────────

fn get_str(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn get_bool(body: &Value, key: &str, default: bool) -> bool {
    body.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn get_u64(body: &Value, key: &str, default: u64) -> u64 {
    body.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
}

fn get_usize(body: &Value, key: &str, default: usize) -> usize {
    body.get(key).and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(default)
}

fn get_opt<T: serde::de::DeserializeOwned>(body: &Value, key: &str) -> Option<T> {
    body.get(key)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .flatten()
}

/// 校验链第 7 步：extensions 去点转小写归一（丢弃空条目）
fn normalize_extensions(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = raw
        .iter()
        .map(|e| {
            e.trim()
                .trim_start_matches('.')
                .to_lowercase()
                .to_string()
        })
        .filter(|e| !e.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

// ─── Handlers ───────────────────────────────────────────────────────────────

/// GET /api/watchers — 全部规则列表（含 recent 速览）
async fn list_watchers(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let registry = WatchRegistry::load(&watchers_path(&state));
    let mut rules: Vec<&WatchRule> = registry.rules.values().collect();
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    let list: Vec<Value> = rules
        .iter()
        .map(|r| {
            let mut v = serde_json::to_value(r).unwrap_or_else(|_| json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("id".to_string(), json!(r.id));
            }
            v
        })
        .collect();
    (StatusCode::OK, Json(Value::Array(list)))
}

/// GET /api/watchers/{id} — 单条规则（未配置 → 404）
async fn get_watcher(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let registry = WatchRegistry::load(&watchers_path(&state));
    match registry.rules.get(&id) {
        Some(rule) => {
            let mut v = serde_json::to_value(rule).unwrap_or_else(|_| json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("id".to_string(), json!(id));
            }
            (StatusCode::OK, Json(v))
        }
        None => {
            err_response(
                &state,
                StatusCode::NOT_FOUND,
                "apiCore.watcher.notFound",
                &[("id", id)],
            )
            .await
        }
    }
}

/// POST /api/watchers — 创建规则；body 不含 id，服务端生成。
/// 返回 `{ok, id}`。
async fn create_watcher(
    State(state): State<Arc<AppState>>,
    body: String,
) -> (StatusCode, Json<Value>) {
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let draft = match validate_rule_body(&state, &body).await {
        Ok(draft) => draft,
        Err(resp) => return resp,
    };

    let path = watchers_path(&state);
    let mut registry = WatchRegistry::load(&path);
    let id = registry.generate_rule_id();
    let now = chrono::Utc::now().timestamp();
    let rule = draft.into_rule(&id, now, None);
    registry.rules.insert(id.clone(), rule);
    if let Err(e) = registry.save(&path) {
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiCore.watcher.saveFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }
    (StatusCode::OK, Json(json!({ "ok": true, "id": id })))
}

/// PUT /api/watchers/{id} — 全量更新（校验同 POST；保留运行态字段
/// checkpoint / in_flight / recent / last_task_id；watch_dir 变更时重置基线）
async fn update_watcher(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    body: String,
) -> (StatusCode, Json<Value>) {
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let draft = match validate_rule_body(&state, &body).await {
        Ok(draft) => draft,
        Err(resp) => return resp,
    };

    let path = watchers_path(&state);
    let mut registry = WatchRegistry::load(&path);
    let Some(prev) = registry.rules.get(&id).cloned() else {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiCore.watcher.notFound",
            &[("id", id)],
        )
        .await;
    };
    let rule = draft.into_rule(&id, chrono::Utc::now().timestamp(), Some(&prev));
    registry.rules.insert(id, rule);
    if let Err(e) = registry.save(&path) {
        return err_response(
            &state,
            StatusCode::INTERNAL_SERVER_ERROR,
            "apiCore.watcher.saveFailed",
            &[("detail", e.to_string())],
        )
        .await;
    }
    (StatusCode::OK, Json(json!({ "ok": true })))
}

/// DELETE /api/watchers/{id} — 删除（幂等，与 schedule 三端点同款风格）
async fn delete_watcher(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let path = watchers_path(&state);
    let mut registry = WatchRegistry::load(&path);
    registry.rules.remove(&id);
    let ok = registry.save(&path).is_ok();
    (StatusCode::OK, Json(json!({ "ok": ok })))
}

// ─── §5.3 七步校验链（顺序冻结）─────────────────────────────────────────────

/// 校验后的规则草稿（未分配运行态字段）
struct RuleDraft {
    name: String,
    enabled: bool,
    watch_dir: String,
    recursive: bool,
    extensions: Vec<String>,
    include_modified: bool,
    stability_secs: u64,
    backfill: bool,
    max_batch: usize,
    direct: Option<DirectAction>,
    pipeline: Option<PipelineAction>,
    output: Option<OutputConfig>,
}

impl RuleDraft {
    /// 装配为可落盘规则：POST 传 `prev=None`（服务端生成基线）；
    /// PUT 传 `prev=Some`（保留运行态；watch_dir 变更时重置基线与在途表）。
    fn into_rule(self, id: &str, now: i64, prev: Option<&WatchRule>) -> WatchRule {
        let mut rule = WatchRule {
            id: id.to_string(),
            name: self.name,
            enabled: self.enabled,
            watch_dir: self.watch_dir.clone(),
            recursive: self.recursive,
            extensions: self.extensions,
            include_modified: self.include_modified,
            stability_secs: self.stability_secs,
            backfill: self.backfill,
            checkpoint: if self.backfill { 0 } else { now },
            in_flight: Default::default(),
            max_batch: self.max_batch,
            direct: self.direct,
            pipeline: self.pipeline,
            output: self.output,
            last_task_id: None,
            recent: VecDeque::new(),
        };
        if let Some(prev) = prev {
            rule.last_task_id = prev.last_task_id.clone();
            rule.recent = prev.recent.clone();
            if prev.watch_dir == rule.watch_dir {
                // 运行态水位线与在途表保留（全量更新不改扫描进度）
                rule.checkpoint = prev.checkpoint;
                rule.in_flight = prev.in_flight.clone();
            } else {
                // 监控目录变更：重置基线（backfill 交给哨兵；否则 now 起步）
                rule.checkpoint = if rule.backfill { 0 } else { now };
            }
        }
        rule
    }
}

type ValidateErr = (StatusCode, Json<Value>);

async fn validate_rule_body(state: &Arc<AppState>, body: &Value) -> Result<RuleDraft, ValidateErr> {
    // 1. name 非空
    let name = get_str(body, "name").unwrap_or_default();
    if name.trim().is_empty() {
        return Err(err_response(state, StatusCode::BAD_REQUEST, "apiCore.watcher.nameRequired", &[]).await);
    }

    // 2. watch_dir 非空 + 绝对路径（目录暂不存在仅告警不拒绝）
    let watch_dir = get_str(body, "watch_dir").unwrap_or_default();
    if watch_dir.trim().is_empty() {
        return Err(err_response(state, StatusCode::BAD_REQUEST, "apiCore.watcher.watchDirRequired", &[]).await);
    }
    if !Path::new(&watch_dir).is_absolute() {
        return Err(err_response(state, StatusCode::BAD_REQUEST, "apiCore.watcher.watchDirNotAbsolute", &[]).await);
    }
    if !Path::new(&watch_dir).exists() {
        tracing::warn!(dir = %watch_dir, "watch 目录暂不存在，创建规则时仅告警不拒绝");
    }

    // 3. direct / pipeline 必须恰好一个
    let direct: Option<DirectAction> = get_opt(body, "direct");
    let pipeline: Option<PipelineAction> = get_opt(body, "pipeline");
    match (&direct, &pipeline) {
        (None, None) => {
            return Err(err_response(state, StatusCode::BAD_REQUEST, "apiCore.watcher.actionRequired", &[]).await);
        }
        (Some(_), Some(_)) => {
            return Err(err_response(state, StatusCode::BAD_REQUEST, "apiCore.watcher.actionConflict", &[]).await);
        }
        _ => {}
    }

    // 4. pipeline 模式：管线存在 + input_node 为 spec 实际节点
    if let Some(p) = &pipeline {
        let pdir = pipelines_dir(state);
        let Some(spec) = find_spec(&pdir, &p.pipeline_id) else {
            return Err(err_response(
                state,
                StatusCode::NOT_FOUND,
                "apiPipelines.pipelines.notFound",
                &[],
            )
            .await);
        };
        if p.input_node.trim().is_empty() || !spec.nodes.iter().any(|n| n.id == p.input_node) {
            return Err(err_response(state, StatusCode::BAD_REQUEST, "apiCore.watcher.inputNodeInvalid", &[]).await);
        }
    }

    // 5. direct-Module：模块与 capability 存在（复用 modules 注册表）
    if let Some(d) = &direct {
        if let DirectKind::Module { module_id, capability } = &d.kind {
            let manifest = crate::api::execute::find_module_manifest(state, module_id).await;
            let capability_ok = manifest
                .as_ref()
                .map(|mf| mf.interface.capabilities.iter().any(|c| &c.name == capability))
                .unwrap_or(false);
            if !capability_ok {
                return Err(err_response(
                    state,
                    StatusCode::BAD_REQUEST,
                    "apiCore.watcher.capabilityInvalid",
                    &[("detail", format!("{module_id}/{capability}"))],
                )
                .await);
            }
        }
    }

    // 6. direct 模式：output 必填且 dest_dir 绝对
    let output: Option<OutputConfig> = get_opt(body, "output");
    if direct.is_some() {
        match &output {
            Some(o) if Path::new(&o.dest_dir).is_absolute() => {}
            _ => {
                return Err(err_response(state, StatusCode::BAD_REQUEST, "apiCore.watcher.outputRequired", &[]).await);
            }
        }
    }

    // 7. stability_secs 钳制 ≥ 5；extensions 去点转小写归一
    let stability_secs = get_u64(body, "stability_secs", 30).max(5);
    let max_batch = get_usize(body, "max_batch", 16).max(1);
    let extensions = normalize_extensions(&get_string_vec(body, "extensions"));

    Ok(RuleDraft {
        name,
        enabled: get_bool(body, "enabled", true),
        watch_dir,
        recursive: get_bool(body, "recursive", false),
        extensions,
        include_modified: get_bool(body, "include_modified", false),
        stability_secs,
        backfill: get_bool(body, "backfill", false),
        max_batch,
        direct,
        pipeline,
        output,
    })
}

/// 提取字符串数组字段（形状不符按空数组处理，交由白名单语义兜底）
fn get_string_vec(body: &Value, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ep_core::module::discovery::{DiscoveredModule, DiscoveryStatus};
    use http_body_util::BodyExt;
    use tower::ServiceExt; // oneshot

    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn test_root(tag: &str) -> std::path::PathBuf {
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-watchers-api-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        root
    }

    fn state_at(root: std::path::PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            ep_core::config::AppConfig::default(),
            vec![],
            vec![],
            ep_core::port::PortManager::new(49000, 49500),
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

    async fn collect(resp: axum::response::Response) -> Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn valid_direct_body(tag: &str) -> Value {
        json!({
            "name": format!("规则-{tag}"),
            "watch_dir": "/tmp/ep-watch-src",
            "direct": { "kind": { "type": "Archive" } },
            "output": {
                "dest_dir": "/tmp/ep-watch-dest",
                "name_template": "{name}.{ext}",
                "on_conflict": "suffix"
            }
        })
    }

    // ── 校验链顺序（冻结）───────────────────────────────────────────────

    // 1. 空 body / 缺 name → nameRequired（校验链第一步）
    #[tokio::test]
    async fn create_empty_body_fails_name_required() {
        let root = test_root("namereq");
        let state = state_at(root.clone());
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(req("POST", "/watchers", Some(json!({}))))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = collect(resp).await;
        assert_eq!(
            body["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.nameRequired", &[]),
            "校验链第一步必须是 nameRequired"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // 2. name 通过后缺 watch_dir → watchDirRequired；相对路径 → watchDirNotAbsolute
    #[tokio::test]
    async fn create_watch_dir_chain() {
        let root = test_root("watchdir");
        let state = state_at(root.clone());
        let app = super::router().with_state(state.clone());

        let resp = app
            .oneshot(req("POST", "/watchers", Some(json!({"name": "x"}))))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.watchDirRequired", &[])
        );

        let app = super::router().with_state(state.clone());
        let mut relative = valid_direct_body("rel");
        relative["watch_dir"] = json!("relative/path");
        let resp = app.oneshot(req("POST", "/watchers", Some(relative))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.watchDirNotAbsolute", &[])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // 3. 动作互斥：两者皆无 → actionRequired；两者皆有 → actionConflict
    #[tokio::test]
    async fn create_action_mutual_exclusion() {
        let root = test_root("action");
        let state = state_at(root.clone());

        let app = super::router().with_state(state.clone());
        let no_action = json!({"name": "x", "watch_dir": "/tmp/a"});
        let resp = app.oneshot(req("POST", "/watchers", Some(no_action))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.actionRequired", &[])
        );

        let app = super::router().with_state(state.clone());
        let mut both = valid_direct_body("both");
        both["pipeline"] = json!({"pipeline_id": "p", "input_node": "input"});
        let resp = app.oneshot(req("POST", "/watchers", Some(both))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.actionConflict", &[])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // 4. 管线模式：管线不存在 → 404 pipelines.notFound；input_node 非法 → inputNodeInvalid
    #[tokio::test]
    async fn create_pipeline_mode_validation() {
        let root = test_root("pipe");
        // 预置一条真实管线（文件名与 id 可不同，扫描匹配）
        let pdir = root.join("config").join("pipelines");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(
            pdir.join("pipe_x.toml"),
            r#"
[pipeline]
id = "pipe-x"
name = "X"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "work"
kind = "builtin"
builtin = "file_output"
"#,
        )
        .unwrap();
        let state = state_at(root.clone());
        let app = super::router().with_state(state.clone());
        let missing = json!({
            "name": "x", "watch_dir": "/tmp/a",
            "pipeline": {"pipeline_id": "no-such-pipe", "input_node": "input"}
        });
        let resp = app.oneshot(req("POST", "/watchers", Some(missing))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiPipelines.pipelines.notFound", &[])
        );

        // 真实管线（pipe-x）+ 非法 input_node
        let app = super::router().with_state(state.clone());
        let bad_node = json!({
            "name": "x", "watch_dir": "/tmp/a",
            "pipeline": {"pipeline_id": "pipe-x", "input_node": "ghost-node"}
        });
        let resp = app.oneshot(req("POST", "/watchers", Some(bad_node))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.inputNodeInvalid", &[])
        );

        // 合法注入节点（spec 实际节点）→ 创建成功
        let app = super::router().with_state(state.clone());
        let good = json!({
            "name": "pipe-ok", "watch_dir": "/tmp/a",
            "pipeline": {"pipeline_id": "pipe-x", "input_node": "input"}
        });
        let resp = app.oneshot(req("POST", "/watchers", Some(good))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "合法管线规则应创建成功");
        let _ = std::fs::remove_dir_all(&root);
    }

    // 5. direct-Module：模块/capability 不存在 → capabilityInvalid
    #[tokio::test]
    async fn create_module_capability_invalid() {
        let root = test_root("cap");
        let state = state_at(root.clone());
        let app = super::router().with_state(state.clone());
        let body = json!({
            "name": "x", "watch_dir": "/tmp/a",
            "direct": {"kind": {"type": "Module", "module_id": "ghost-mod", "capability": "ghost-cap"}},
            "output": {"dest_dir": "/tmp/b"}
        });
        let resp = app.oneshot(req("POST", "/watchers", Some(body))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t(
                "zh-CN",
                "apiCore.watcher.capabilityInvalid",
                &[("detail", "ghost-mod/ghost-cap")]
            )
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // 5b. direct-Module：模块与 capability 均在注册表内 → 校验通过并可创建
    #[tokio::test]
    async fn create_module_capability_valid() {
        use ep_core::module::manifest::ModuleManifest;
        let root = test_root("capok");
        let manifest: ModuleManifest = toml::from_str(
            r#"
[module]
id = "demo-mod"
name = "Demo"
version = "0.1.0"
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

[[interface.capabilities]]
name = "remove-bg"
description = "t"
input_type = "file"
output_type = "file"
max_file_size_mb = 100
"#,
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            root.clone(),
            ep_core::config::AppConfig::default(),
            vec![],
            vec![DiscoveredModule {
                manifest: Some(manifest),
                path: PathBuf::from("modules/demo-mod"),
                status: DiscoveryStatus::Valid,
            }],
            ep_core::port::PortManager::new(49000, 49500),
        ));
        let app = super::router().with_state(state.clone());
        let body = json!({
            "name": "cap-ok",
            "watch_dir": "/tmp/ep-watch-src",
            "direct": {"kind": {"type": "Module", "module_id": "demo-mod", "capability": "remove-bg"}},
            "output": {"dest_dir": "/tmp/ep-watch-dest"}
        });
        let resp = app.oneshot(req("POST", "/watchers", Some(body))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "合法模块直调规则应创建成功");
        let body = collect(resp).await;
        assert_eq!(body["ok"], true);
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(id.len(), 8, "id 必须为 8 位");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let _ = std::fs::remove_dir_all(&root);
    }

    // 6. direct 模式：output 缺失 / dest_dir 相对 → outputRequired
    #[tokio::test]
    async fn create_direct_requires_absolute_output() {
        let root = test_root("out");
        let state = state_at(root.clone());

        let app = super::router().with_state(state.clone());
        let no_output = json!({
            "name": "x", "watch_dir": "/tmp/a",
            "direct": {"kind": {"type": "Archive"}}
        });
        let resp = app.oneshot(req("POST", "/watchers", Some(no_output))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.outputRequired", &[])
        );

        let app = super::router().with_state(state.clone());
        let mut rel_dest = valid_direct_body("reldest");
        rel_dest["output"]["dest_dir"] = json!("relative/dest");
        let resp = app.oneshot(req("POST", "/watchers", Some(rel_dest))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.outputRequired", &[])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // 7. 创建 → 列表 → 单查 → 修改 → 删除 全链路 + 默认值落盘
    #[tokio::test]
    async fn crud_lifecycle_and_defaults() {
        let root = test_root("crud");
        let state = state_at(root.clone());
        let app = super::router().with_state(state.clone());
        let resp = app
            .oneshot(req("POST", "/watchers", Some(valid_direct_body("crud"))))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let id = collect(resp).await["id"].as_str().unwrap().to_string();

        // 列表含 recent 与默认字段（stability 30 / max_batch 16 / checkpoint = now 基线）
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("GET", "/watchers", None)).await.unwrap();
        let list = collect(resp).await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["id"], id.as_str());
        assert_eq!(list[0]["stability_secs"], 30);
        assert_eq!(list[0]["max_batch"], 16);
        assert!(list[0]["checkpoint"].as_i64().unwrap() > 0, "非回灌规则基线即 now");
        assert!(list[0]["direct"].is_object());
        assert!(list[0]["pipeline"].is_null(), "None 字段不得序列化");

        // 单查
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("GET", &format!("/watchers/{id}"), None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(collect(resp).await["id"], id.as_str());

        // 修改：stability 钳制 + 扩展名归一 + 名称更新；checkpoint 保留
        let before = {
            let reg = WatchRegistry::load(&watchers_path(&state));
            reg.rules[&id].checkpoint
        };
        let app = super::router().with_state(state.clone());
        let mut upd = valid_direct_body("crud");
        upd["name"] = json!("updated");
        upd["stability_secs"] = json!(1);
        upd["extensions"] = json!([".MKV", "JPG", " mkv "]);
        let resp = app.oneshot(req("PUT", &format!("/watchers/{id}"), Some(upd))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reg = WatchRegistry::load(&watchers_path(&state));
        let rule = &reg.rules[&id];
        assert_eq!(rule.name, "updated");
        assert_eq!(rule.stability_secs, 5, "stability_secs 必须钳制 ≥ 5");
        assert_eq!(rule.extensions, vec!["jpg", "mkv"], "去点转小写并去重");
        assert_eq!(rule.checkpoint, before, "PUT 保留运行态水位线");

        // 未配置 id → 404
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("GET", "/watchers/no-such", None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            collect(resp).await["error"],
            ep_core::i18n::t("zh-CN", "apiCore.watcher.notFound", &[("id", "no-such")])
        );

        // 删除（幂等）
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("DELETE", &format!("/watchers/{id}"), None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(collect(resp).await["ok"], true);
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("DELETE", &format!("/watchers/{id}"), None)).await.unwrap();
        assert_eq!(collect(resp).await["ok"], true, "DELETE 幂等");
        let reg = WatchRegistry::load(&watchers_path(&state));
        assert!(reg.rules.is_empty(), "删除后注册表条目消失");
        let _ = std::fs::remove_dir_all(&root);
    }

    // 落盘文件损坏 → 按空表启动（API 层表现为空列表 / 404）
    #[tokio::test]
    async fn corrupt_registry_starts_empty() {
        let root = test_root("corrupt");
        let state = state_at(root.clone());
        std::fs::write(watchers_path(&state), "{broken").unwrap();
        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(req("GET", "/watchers", None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(collect(resp).await.as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
