//! 存储池 API（T-26 计划 7 S0：存储位置命名空间 + 目录浏览）
//!
//! 端点：
//! - `GET  /api/stores`                     — 存储池列表（解析后物理路径，供
//!   前端浏览对话框与「导出改写 store: 逻辑名」使用）
//! - `GET  /api/stores/{name}/browse?path=` — 目录浏览（单层列目录/文件，
//!   供前端选择器；防路径穿越）
//! - `POST /api/stores/resolve`             — `store:<name>/<相对路径>` 引用
//!   解析（缺池/禁用 → 422，§6.4 缺依赖同款"补配置"提示语义）
//!
//! ## 逻辑名契约（T-26）
//!
//! 文件入口（file_input.path / file_output.path / file_archive.dest_dir /
//! watcher 的 watch_dir/dest_dir / 直跑 input_path）接受
//! `store:<name>/<相对路径>` 形式，服务端在本 crate 的接线助手内解析为
//! 物理路径后按既有链路消费；物理路径照常可用（`Ok(None)` 透传，向后兼容）。
//!
//! - 纯词法解析在 `ep_core::config::AppConfig::resolve_store_ref`
//!   （`..` 穿越 / 绝对路径注入全拒绝）；
//! - 目录浏览复用 [`super::upload::sanitize_relative_path`] + 
//!   [`super::upload::resolve_within`] 先例：解析后校验仍在本 store 物理路径内。
//!
//! 错误统一走 `err_response` + `apiCore.store.*` i18n 键：
//! 缺池/禁用 → 422；相对部分非法 → 400；目录不存在 → 404。

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Json,
    Router,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use ep_core::pipeline::{Pipeline, PipelineNode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::state::AppState;

use super::err_response;
use super::upload::{resolve_within, sanitize_relative_path};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stores", get(list_stores))
        .route("/stores/resolve", post(resolve_ref))
        .route("/stores/locate", post(locate_paths))
        .route("/stores/{name}/browse", get(browse_store))
}

// ─── GET /api/stores ────────────────────────────────────────────────────────

/// GET /api/stores — 存储池列表（含解析后物理路径；前端浏览/导出改写消费）
async fn list_stores(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let (config, root) = {
        let cfg = state.config.read().await;
        (cfg.clone(), state.root.clone())
    };
    let stores: Vec<Value> = config
        .storage
        .stores
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "type": s.store_type,
                "path": s.resolve_path(&root).to_string_lossy(),
                "enabled": s.enabled,
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({ "stores": stores })))
}

// ─── POST /api/stores/resolve ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ResolveBody {
    value: Option<String>,
}

/// POST /api/stores/resolve — `store:` 引用 → 物理路径。
///
/// body `{ "value": "store:media/incoming/a.wav" }`：
/// - 解析成功 → `{ok: true, store, path}`；
/// - 非 `store:` 引用 → 400 `apiCore.store.notStoreRef`；
/// - 缺池 / 禁用 → 422 `apiCore.store.notFound` / `apiCore.store.disabled`；
/// - 相对部分非法 → 400 `apiCore.store.unsafePath`。
async fn resolve_ref(
    State(state): State<Arc<AppState>>,
    body: Option<Json<ResolveBody>>,
) -> (StatusCode, Json<Value>) {
    let Some(Json(body)) = body else {
        return err_response(&state, StatusCode::BAD_REQUEST, "apiCore.store.valueRequired", &[])
            .await;
    };
    let Some(value) = body.value.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return err_response(&state, StatusCode::BAD_REQUEST, "apiCore.store.valueRequired", &[])
            .await;
    };
    if !value.starts_with(ep_core::config::STORE_REF_PREFIX) {
        return err_response(&state, StatusCode::BAD_REQUEST, "apiCore.store.notStoreRef", &[])
            .await;
    }
    // 逻辑名段校验（缺池 / 禁用 / 非法名 → 422/400）
    let name = value
        .trim_start_matches(ep_core::config::STORE_REF_PREFIX)
        .split(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_string();
    if let Some(resp) = store_missing_rejection(&state, &name).await {
        return resp;
    }
    let (config, root) = config_snapshot(&state).await;
    match config.resolve_store_ref(&root, value) {
        Ok(Some(path)) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "store": name,
                "path": path.to_string_lossy(),
            })),
        ),
        Ok(None) => {
            err_response(&state, StatusCode::BAD_REQUEST, "apiCore.store.notStoreRef", &[]).await
        }
        Err(ep_core::config::StoreRefError::UnsafeRel(_)) => err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiCore.store.unsafePath",
            &[("detail", value.to_string())],
        )
        .await,
        // store_missing_rejection 已覆盖缺池/禁用；并发改配置竞态兜底
        Err(e) => err_response(
            &state,
            StatusCode::UNPROCESSABLE_ENTITY,
            "apiCore.store.notFound",
            &[("name", e.to_string())],
        )
        .await,
    }
}

// ─── POST /api/stores/locate ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LocateBody {
    /// 物理路径列表（批量；前端 TOML 导出前一次性改写）
    paths: Vec<String>,
}

/// POST /api/stores/locate — 物理路径 → `store:` 逻辑名（批量反向改写）。
///
/// body `{ "paths": ["D:/share/media/a.wav", ...] }` →
/// `{ "values": ["store:media/a.wav", null, ...] }`：
/// 落在某**已启用**池内的路径改写为 `store:` 形式；池外 / 已是 `store:` 引用
/// 的条目为 `null`（前端保持原值）。消费方：pipeline.tsx 的 TOML 导出
///（JSON 分享导出在服务端 export_pipeline 内改写，不经本端点）。
async fn locate_paths(
    State(state): State<Arc<AppState>>,
    body: Option<Json<LocateBody>>,
) -> (StatusCode, Json<Value>) {
    let Some(Json(body)) = body else {
        return err_response(&state, StatusCode::BAD_REQUEST, "apiCore.store.valueRequired", &[])
            .await;
    };
    let (config, root) = config_snapshot(&state).await;
    let values: Vec<Value> = body
        .paths
        .iter()
        .map(|p| {
            let trimmed = p.trim();
            if trimmed.starts_with(ep_core::config::STORE_REF_PREFIX) {
                return Value::Null; // 已是逻辑名，保持原样
            }
            match config.to_store_ref(&root, std::path::Path::new(trimmed)) {
                Some(reference) => json!(reference),
                None => Value::Null,
            }
        })
        .collect();
    (StatusCode::OK, Json(json!({ "values": values })))
}

// ─── GET /api/stores/{name}/browse ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct BrowseQuery {
    /// 相对池根的子路径（缺省 = 池根）
    #[serde(default)]
    path: Option<String>,
}

/// 目录条目（浏览响应 `entries[]` 元素）
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoreDirEntry {
    pub name: String,
    /// `dir` | `file`
    pub kind: &'static str,
    /// 文件字节数（目录恒 0）
    pub size: u64,
}

/// GET /api/stores/{name}/browse?path=rel — 单层目录浏览。
///
/// 防穿越（硬要求）：query `path` 先过 [`sanitize_relative_path`]
/// （拒绝 `..` / 绝对注入 / 反斜杠形态），再经 [`resolve_within`]
/// 校验拼接结果仍在池物理路径内（纵深防御）。
async fn browse_store(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<BrowseQuery>,
) -> (StatusCode, Json<Value>) {
    // 缺池 / 禁用 → 422（与文件入口解析同一语义）
    if let Some(resp) = store_missing_rejection(&state, &name).await {
        return resp;
    }
    let (config, root) = config_snapshot(&state).await;
    let Some(store) = config.store(&name) else {
        // store_missing_rejection 已覆盖；此分支仅为类型完备（并发改配置竞态）
        return err_response(&state, StatusCode::UNPROCESSABLE_ENTITY, "apiCore.store.notFound", &[
            ("name", name),
        ])
        .await;
    };
    let base = store.resolve_path(&root);

    // 相对路径清洗（upload.rs 先例：拒绝 .. / 绝对注入）
    let raw = query.path.as_deref().unwrap_or_default().trim();
    let rel = if raw.is_empty() {
        PathBuf::new()
    } else {
        match sanitize_relative_path(raw) {
            Some(rel) => rel,
            None => {
                return err_response(
                    &state,
                    StatusCode::BAD_REQUEST,
                    "apiCore.store.unsafePath",
                    &[("detail", raw.to_string())],
                )
                .await
            }
        }
    };
    // 纵深防御：拼接结果必须仍在池内
    let Some(dir) = resolve_within(&base, &rel) else {
        return err_response(
            &state,
            StatusCode::BAD_REQUEST,
            "apiCore.store.unsafePath",
            &[("detail", raw.to_string())],
        )
        .await;
    };

    // 目录读取放阻塞线程池（网络挂载盘 read_dir + stat 可能慢，T-03 教训）
    let dir_for_err = dir.clone();
    let scan = tokio::task::spawn_blocking(move || list_dir_entries(&dir)).await;
    match scan {
        Ok(Ok(entries)) => (
            StatusCode::OK,
            Json(json!({
                "store": name,
                "path": raw,
                "entries": entries,
            })),
        ),
        Ok(Err(io_err)) => {
            // NotFound → 404；其余（权限等）→ 500
            if io_err.kind() == std::io::ErrorKind::NotFound {
                err_response(
                    &state,
                    StatusCode::NOT_FOUND,
                    "apiCore.store.dirNotFound",
                    &[("detail", dir_for_err.to_string_lossy().into_owned())],
                )
                .await
            } else {
                err_response(
                    &state,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "apiCore.store.browseFailed",
                    &[("detail", io_err.to_string())],
                )
                .await
            }
        }
        Err(e) => {
            err_response(
                &state,
                StatusCode::INTERNAL_SERVER_ERROR,
                "apiCore.store.browseFailed",
                &[("detail", e.to_string())],
            )
            .await
        }
    }
}

/// 单层目录读取（同步内核，供 spawn_blocking；调用方决定状态码映射）。
/// 目标本体是符号链接时按 `metadata`（跟随）判型——池根整体可链到 NAS。
fn list_dir_entries(dir: &std::path::Path) -> std::io::Result<Vec<StoreDirEntry>> {
    let mut out: Vec<StoreDirEntry> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // 单点读失败不拖垮整列表
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        // 审查 P2-4：条目用 file_type（不跟随）而非 metadata（跟随）判型——
        // 共享/NAS 池内他人可写入的 symlink/junction 指向池外时，跟随会外泄
        // 池外目录的名字与大小；链接项不枚举、不下钻。池根本身链到 NAS 属
        // store 配置意图（read_dir 打开的是配置解析后的路径），不受影响。
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            out.push(StoreDirEntry { name, kind: "dir", size: 0 });
        } else if ft.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(StoreDirEntry { name, kind: "file", size });
        }
        // 设备/管道等其他类型：跳过
    }
    out.sort_by(|a, b| {
        a.kind.cmp(b.kind).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

// ─── 接线助手（文件入口 store: 解析的唯一共享实现）────────────────────────

/// config + root 快照（先取值再 await，C1 修复同款纪律）
async fn config_snapshot(state: &Arc<AppState>) -> (ep_core::config::AppConfig, PathBuf) {
    let config = state.config.read().await.clone();
    (config, state.root.clone())
}

/// 缺池 / 禁用 → 422 响应（`apiCore.store.*`；§6.4 缺依赖同款提示语义）。
/// 池存在且启用 → `None`。
async fn store_missing_rejection(
    state: &Arc<AppState>,
    name: &str,
) -> Option<(StatusCode, Json<Value>)> {
    let config = state.config.read().await;
    if config.storage.stores.iter().any(|s| s.name == name) {
        if config.store(name).is_none() {
            return Some(
                err_response(
                    state,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "apiCore.store.disabled",
                    &[("name", name.to_string())],
                )
                .await,
            );
        }
        return None;
    }
    Some(
        err_response(
            state,
            StatusCode::UNPROCESSABLE_ENTITY,
            "apiCore.store.notFound",
            &[("name", name.to_string())],
        )
        .await,
    )
}

/// `store:` 引用解析为物理路径的接线助手（T-26）。
///
/// 文件入口值消费方（execute.rs 直跑 input_path、execution.rs 提交改写、
/// watchers.rs 校验链）共用：
/// - 非 `store:` 前缀 → `Ok(原值)`（物理路径向后兼容，`PathBuf::from(raw)`）；
/// - 缺池 / 禁用 → `Err`（422 `apiCore.store.notFound` / `store.disabled`）；
/// - 相对部分非法 → `Err`（400 `apiCore.store.unsafePath`）。
pub(crate) async fn resolve_entry_path(
    state: &Arc<AppState>,
    raw: &str,
) -> Result<PathBuf, (StatusCode, Json<Value>)> {
    if !raw.trim().starts_with(ep_core::config::STORE_REF_PREFIX) {
        return Ok(PathBuf::from(raw));
    }
    let (config, root) = config_snapshot(state).await;
    match config.resolve_store_ref(&root, raw) {
        Ok(Some(path)) => Ok(path),
        Ok(None) => Ok(PathBuf::from(raw)),
        Err(ep_core::config::StoreRefError::UnknownStore(name)) => Err(
            err_response(
                state,
                StatusCode::UNPROCESSABLE_ENTITY,
                "apiCore.store.notFound",
                &[("name", name)],
            )
            .await,
        ),
        Err(ep_core::config::StoreRefError::StoreDisabled(name)) => Err(
            err_response(
                state,
                StatusCode::UNPROCESSABLE_ENTITY,
                "apiCore.store.disabled",
                &[("name", name)],
            )
            .await,
        ),
        Err(ep_core::config::StoreRefError::UnsafeRel(_)) => Err(
            err_response(
                state,
                StatusCode::BAD_REQUEST,
                "apiCore.store.unsafePath",
                &[("detail", raw.to_string())],
            )
            .await,
        ),
    }
}

/// 校验失败即时响应版（watchers.rs 校验链消费：`store:` 值先解析再进
/// 绝对路径校验；失败 → 即时 4xx 响应，成功 → 返回解析后的物理路径字符串）。
pub(crate) async fn resolve_entry_value(
    state: &Arc<AppState>,
    raw: &str,
) -> Result<String, (StatusCode, Json<Value>)> {
    resolve_entry_path(state, raw)
        .await
        .map(|p| p.to_string_lossy().into_owned())
}

/// 文件入口参数位（T-26 逻辑名解析覆盖面）：`(builtin 名, param 名)`。
/// file_input 源路径 / file_output 目标路径 / file_archive 归档根目录。
pub(crate) const FILE_ENTRY_PARAMS: &[(&str, &str)] =
    &[("file_input", "path"), ("file_output", "path"), ("file_archive", "dest_dir")];

/// 提交前改写：管线节点参数中的 `store:` 逻辑名引用 → 物理路径。
///
/// 单一咽喉点：cron（submit_pipeline_for_schedule）、watcher 管线模式、
/// API 执行三条提交链都经 [`submit_pipeline_full`] → 在 DAG 校验前调用本函数；
/// 非 `store:` 参数原样保留（物理路径向后兼容）。
/// 缺池 / 禁用 → `Err(StoreRefError)`（提交拒绝，handler 映射 422/400）。
pub(crate) fn rewrite_pipeline_store_refs(
    config: &ep_core::config::AppConfig,
    root: &std::path::Path,
    pipeline: &mut Pipeline,
) -> Result<(), ep_core::config::StoreRefError> {
    for node in pipeline.nodes.iter_mut() {
        rewrite_node_store_refs(config, root, node)?;
    }
    Ok(())
}

/// 单节点改写内核（单测直接消费）。
pub(crate) fn rewrite_node_store_refs(
    config: &ep_core::config::AppConfig,
    root: &std::path::Path,
    node: &mut PipelineNode,
) -> Result<(), ep_core::config::StoreRefError> {
    let builtin = match &node.kind {
        ep_core::pipeline::NodeKind::Builtin { builtin } => builtin.as_str(),
        _ => return Ok(()),
    };
    if !FILE_ENTRY_PARAMS.iter().any(|(b, _)| *b == builtin) {
        return Ok(());
    }
    let param_name = FILE_ENTRY_PARAMS
        .iter()
        .find(|(b, _)| *b == builtin)
        .map(|(_, p)| *p)
        .unwrap_or_default();
    let Some(obj) = node.params.as_object_mut() else {
        return Ok(());
    };
    let Some(Value::String(raw)) = obj.get(param_name) else {
        return Ok(());
    };
    if !raw.trim().starts_with(ep_core::config::STORE_REF_PREFIX) {
        return Ok(());
    }
    let resolved = config.resolve_store_ref(root, raw)?;
    if let Some(path) = resolved {
        obj.insert(
            param_name.to_string(),
            Value::String(path.to_string_lossy().into_owned()),
        );
    }
    Ok(())
}

/// 导出改写（T-26 换机迁移契约）：物理路径 → `store:` 逻辑名。
///
/// 消费方：`GET /api/pipelines/{id}/export`（分享 JSON）。参数值为
/// `store:` 引用时原样保留；落在某已启用池内的物理路径改写为
/// `store:<name>/<相对路径>`；池外保持原值。
pub(crate) fn rewrite_spec_nodes_to_store_refs(
    config: &ep_core::config::AppConfig,
    root: &std::path::Path,
    nodes: &mut [crate::api::pipelines::pipeline_bridge::SpecNode],
) {
    for node in nodes.iter_mut() {
        let Some(builtin) = node.builtin.as_deref() else {
            continue;
        };
        let Some((_, param_name)) = FILE_ENTRY_PARAMS.iter().find(|(b, _)| *b == builtin) else {
            continue;
        };
        let Some(obj) = node.params.as_object_mut() else {
            continue;
        };
        let Some(Value::String(raw)) = obj.get(*param_name) else {
            continue;
        };
        if raw.trim().starts_with(ep_core::config::STORE_REF_PREFIX) {
            continue; // 已是逻辑名，保持原样
        }
        if let Some(store_ref) = config.to_store_ref(root, std::path::Path::new(raw)) {
            obj.insert(
                (*param_name).to_string(),
                Value::String(store_ref),
            );
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
    use ep_core::config::{AppConfig, StoreConfig, StoreType, StorageConfig};
    use ep_core::port::PortManager;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::state::AppState;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> std::path::PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-api-stores-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// 建池配置 + 落盘目录结构：
    /// media（启用）下 incoming/（含 a.wav）与 empty/；off（禁用）
    fn seeded_config(root: &std::path::Path) -> AppConfig {
        let media = root.join("media-root");
        std::fs::create_dir_all(media.join("incoming")).unwrap();
        std::fs::write(media.join("incoming").join("a.wav"), b"x").unwrap();
        std::fs::create_dir_all(media.join("empty")).unwrap();
        std::fs::create_dir_all(root.join("off-root")).unwrap();

        let mut config = AppConfig::default();
        config.storage = StorageConfig {
            stores: vec![
                StoreConfig {
                    name: "media".into(),
                    store_type: StoreType::Local,
                    path: media.to_string_lossy().into_owned(),
                    enabled: true,
                },
                StoreConfig {
                    name: "off".into(),
                    store_type: StoreType::Local,
                    path: root.join("off-root").to_string_lossy().into_owned(),
                    enabled: false,
                },
            ],
        };
        config
    }

    fn test_state(root: std::path::PathBuf, config: AppConfig) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            config,
            vec![],
            vec![],
            PortManager::new(18000, 19000),
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

    async fn body_json(resp: axum::response::Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    // ── GET /api/stores ───────────────────────────────────────────────────

    #[tokio::test]
    async fn list_stores_returns_resolved_paths_and_enabled_flags() {
        let root = unique_root("list");
        let config = seeded_config(&root);
        let state = test_state(root.clone(), config);

        let app = super::router().with_state(state);
        let (status, body) = body_json(app.oneshot(get("/stores")).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);

        let stores = body["stores"].as_array().expect("stores array");
        assert_eq!(stores.len(), 2);
        assert_eq!(stores[0]["name"], "media");
        assert_eq!(stores[0]["type"], "local");
        assert_eq!(stores[0]["enabled"], true);
        let resolved = stores[0]["path"].as_str().unwrap();
        assert!(
            resolved.ends_with("media-root"),
            "物理路径应解析到 media-root: {resolved}"
        );
        assert!(std::path::Path::new(resolved).is_absolute());
        assert_eq!(stores[1]["name"], "off");
        assert_eq!(stores[1]["enabled"], false);

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 目录浏览：正常 / 越界拒绝 / 缺池 422 / 禁用 422 ────────────────────

    #[tokio::test]
    async fn browse_store_lists_entries_dirs_first() {
        let root = unique_root("browse2");
        let config = seeded_config(&root);
        let state = test_state(root.clone(), config);

        let app = super::router().with_state(state);
        let resp = app.oneshot(get("/stores/media/browse")).await.unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        let entries = body["entries"].as_array().expect("entries");
        // 目录排前（empty < incoming），文件类无
        let kinds: Vec<&str> = entries.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["dir", "dir"], "两个目录且目录在前: {body}");
        assert_eq!(entries[0]["name"], "empty");
        assert_eq!(entries[1]["name"], "incoming");
        assert_eq!(body["store"], "media");

        // 子路径：incoming/ → 含 a.wav 文件条目 + 大小
        let app2 = super::router().with_state(test_state(root.clone(), seeded_config(&root)));
        let resp = app2
            .oneshot(get("/stores/media/browse?path=incoming"))
            .await
            .unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        let entries = body["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "a.wav");
        assert_eq!(entries[0]["kind"], "file");
        assert_eq!(entries[0]["size"], 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 防穿越硬要求：`..` 与绝对注入全拒绝（400 unsafePath）
    #[tokio::test]
    async fn browse_rejects_traversal_and_absolute_injection() {
        let root = unique_root("traverse");
        let _config = seeded_config(&root);

        for (uri, desc) in [
            ("/stores/media/browse?path=..%2F..%2Fetc", "URL 编码 ../"),
            ("/stores/media/browse?path=..", "单段 .."),
            ("/stores/media/browse?path=%2Fetc", "POSIX 绝对注入"),
            ("/stores/media/browse?path=C%3A%5CWindows", "Windows 盘符注入"),
        ] {
            let app = super::router().with_state(test_state(root.clone(), seeded_config(&root)));
            let resp = app.oneshot(get(uri)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{desc}");
        }

        // 越界后文件确实未被泄露（响应体不含 etc 字样内容泄漏断言交给 400 状态）
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 缺池 → 422；禁用池 → 422（§6.4 缺依赖同款语义）
    #[tokio::test]
    async fn browse_unknown_or_disabled_store_is_422() {
        let root = unique_root("missing");
        let config = seeded_config(&root);
        let state = test_state(root.clone(), config);

        let app = super::router().with_state(state.clone());
        let resp = app.oneshot(get("/stores/nope/browse")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body["error"].as_str().unwrap().contains("nope"),
            "错误信息含池名: {body}"
        );

        // 禁用池同 422
        let app = super::router().with_state(state);
        let resp = app.oneshot(get("/stores/off/browse")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "禁用池同 422");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 目录不存在 → 404（池有效但子目录缺失）
    #[tokio::test]
    async fn browse_missing_subdir_is_404() {
        let root = unique_root("missdir");
        let config = seeded_config(&root);
        let state = test_state(root.clone(), config);

        let app = super::router().with_state(state);
        let resp = app
            .oneshot(get("/stores/media/browse?path=no-such-dir"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── POST /api/stores/resolve：422 缺池 / round-trip ────────────────────

    #[tokio::test]
    async fn resolve_endpoint_roundtrip_and_422() {
        let root = unique_root("resolve");
        let _config = seeded_config(&root);

        // round-trip：store:media/incoming/a.wav → 物理路径（池内）
        let app = super::router().with_state(test_state(root.clone(), seeded_config(&root)));
        let resp = app
            .oneshot(post_json(
                "/stores/resolve",
                r#"{ "value": "store:media/incoming/a.wav" }"#,
            ))
            .await
            .unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["store"], "media");
        let path = body["path"].as_str().unwrap();
        assert!(std::path::Path::new(path).is_absolute());
        assert!(path.ends_with("media-root/incoming/a.wav") || path.ends_with("media-root\\incoming\\a.wav"));

        // 缺池 → 422
        let app = super::router().with_state(test_state(root.clone(), seeded_config(&root)));
        let resp = app
            .oneshot(post_json("/stores/resolve", r#"{ "value": "store:ghost/x" }"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "缺 store 必须 422");

        // 禁用 → 422
        let app = super::router().with_state(test_state(root.clone(), seeded_config(&root)));
        let resp = app
            .oneshot(post_json("/stores/resolve", r#"{ "value": "store:off/x" }"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // 穿越注入 → 400
        let app = super::router().with_state(test_state(root.clone(), seeded_config(&root)));
        let resp = app
            .oneshot(post_json("/stores/resolve", r#"{ "value": "store:media/../x" }"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 非 store: 引用 → 400（该端点只收逻辑名）
        let app = super::router().with_state(test_state(root.clone(), seeded_config(&root)));
        let resp = app
            .oneshot(post_json("/stores/resolve", r#"{ "value": "D:/x" }"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── POST /api/stores/locate：物理 → store: 反向改写 ────────────────────

    #[tokio::test]
    async fn locate_endpoint_rewrites_inside_paths_only() {
        let root = unique_root("locate");
        let media_root = seeded_config(&root)
            .store("media")
            .unwrap()
            .resolve_path(&root);

        // CI rust job 首轮修复：原实现硬编码 Windows 反斜杠分隔符，POSIX 上
        // 反斜杠只是文件名字符、locate 判池外——按平台构造池内路径
        let (inside, outside) = if cfg!(windows) {
            (
                format!(
                    r#"{media}\\in\\a.wav"#,
                    media = media_root.to_string_lossy().replace('\\', "\\\\")
                ),
                "D:/outside/x.wav".to_string(),
            )
        } else {
            (
                format!("{media}/in/a.wav", media = media_root.display()),
                "/outside/x.wav".to_string(),
            )
        };

        let app = super::router().with_state(test_state(root.clone(), seeded_config(&root)));
        let resp = app
            .oneshot(post_json(
                "/stores/locate",
                &format!(r#"{{ "paths": ["{inside}", "{outside}", "store:media/keep"] }}"#),
            ))
            .await
            .unwrap();
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        let values = body["values"].as_array().expect("values array");
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], json!("store:media/in/a.wav"), "池内物理路径改写");
        assert_eq!(values[1], Value::Null, "池外保持 null");
        assert_eq!(values[2], Value::Null, "已是逻辑名保持 null");

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 提交改写内核：resolve ↔ rewrite round-trip / 非 store 原样 ──────────

    #[test]
    fn rewrite_node_resolves_store_refs_and_keeps_physical() {
        let root = std::env::temp_dir().join(format!("ep-rewrite-{}", std::process::id()));
        let config = seeded_config(&root);
        let media_root = config.store("media").unwrap().resolve_path(&root);

        let mk_node = |builtin: &str, param: &str, value: &str| PipelineNode {
            id: "n1".into(),
            kind: ep_core::pipeline::NodeKind::Builtin { builtin: builtin.into() },
            label: String::new(),
            params: serde_json::json!({ param: value }),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };

        // file_input.path：store: → 物理路径
        let mut node = mk_node("file_input", "path", "store:media/incoming/a.wav");
        super::rewrite_node_store_refs(&config, &root, &mut node).unwrap();
        assert_eq!(
            node.params["path"],
            serde_json::Value::String(
                media_root.join("incoming").join("a.wav").to_string_lossy().into_owned()
            )
        );

        // file_archive.dest_dir：同语义
        let mut node = mk_node("file_archive", "dest_dir", "store:media/out");
        super::rewrite_node_store_refs(&config, &root, &mut node).unwrap();
        assert_eq!(
            node.params["dest_dir"],
            serde_json::Value::String(media_root.join("out").to_string_lossy().into_owned())
        );

        // 物理路径原样保留（向后兼容）
        let mut node = mk_node("file_input", "path", "D:/raw/path.wav");
        super::rewrite_node_store_refs(&config, &root, &mut node).unwrap();
        assert_eq!(node.params["path"], "D:/raw/path.wav");

        // 缺池 → Err（提交链映射 422）
        let mut node = mk_node("file_input", "path", "store:ghost/x");
        assert!(super::rewrite_node_store_refs(&config, &root, &mut node).is_err());

        // 模块节点 / 非 store: 值 → no-op
        let mut module_node = PipelineNode {
            id: "m".into(),
            kind: ep_core::pipeline::NodeKind::Module {
                module_id: "m1".into(),
                capability: "cap".into(),
                model_id: None,
                device: None,
            },
            label: String::new(),
            params: serde_json::json!({ "path": "store:media/x" }),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };
        super::rewrite_node_store_refs(&config, &root, &mut module_node).unwrap();
        assert_eq!(module_node.params["path"], "store:media/x", "模块节点参数不改写");

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 导出改写内核：物理 → store:（JSON 分享导出消费）───────────────────

    #[test]
    fn rewrite_spec_nodes_export_physical_to_store_refs() {
        let root = std::env::temp_dir().join(format!("ep-specrw-{}", std::process::id()));
        let config = seeded_config(&root);
        let media_root = config.store("media").unwrap().resolve_path(&root);

        let mk_spec_node = |builtin: &str, param: &str, value: &str| {
            crate::api::pipelines::pipeline_bridge::SpecNode {
                id: "n1".into(),
                label: String::new(),
                kind: crate::api::pipelines::pipeline_bridge::SpecNodeKind::Builtin,
                builtin: Some(builtin.into()),
                module_id: None,
                capability: None,
                model: None,
                device: None,
                params: serde_json::json!({ param: value }),
                position: None,
                timeout_secs: None,
                retry_count: None,
            }
        };

        let mut nodes = vec![
            mk_spec_node("file_input", "path", media_root.join("in/a.wav").to_string_lossy().as_ref()),
            mk_spec_node("file_output", "path", "D:/outside/x.wav"),
            mk_spec_node("file_archive", "dest_dir", "store:media/already"),
        ];
        super::rewrite_spec_nodes_to_store_refs(&config, &root, &mut nodes);

        assert_eq!(nodes[0].params["path"], "store:media/in/a.wav", "池内物理路径改写为逻辑名");
        assert_eq!(nodes[1].params["path"], "D:/outside/x.wav", "池外保持原值");
        assert_eq!(nodes[2].params["dest_dir"], "store:media/already", "已是逻辑名原样保留");

        let _ = std::fs::remove_dir_all(&root);
    }
}
