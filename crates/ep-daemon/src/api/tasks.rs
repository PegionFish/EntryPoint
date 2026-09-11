//! 任务中心 API — Wave 2 执行代理（W2-D）
//!
//! 数据来源：`crate::api::execute::execution` 的任务注册表。
//! 注册表方案说明见 execution.rs 头部文档：引擎执行全程持 `&mut self`
//! 且任务存储私有、`get_task_detail` 不暴露产物路径，故执行在锁外进行、
//! 任务状态由注册表维护，本文件所有查询因此永不阻塞。
//!
//! ## 产物下载（流式，不以整文件进内存）
//!
//! ep-daemon 未直接依赖 `tower`/`tower-service`，handler 内无法驱动
//! `ServeFile` service，故下载走两段式：
//! 1. `GET /api/tasks/:id/artifacts/:node_id` 校验任务/节点/产物后
//!    **302 重定向**到 `/api/task-files/{task_id}/files/{node_id}/{filename}`；
//! 2. `/api/task-files/*` 由 `tower_http::services::ServeDir`（流式、支持
//!    Range）承接，叠加一层中间件补 `Content-Disposition: attachment`。
//!
//! 产物在执行收尾时已硬链接/复制进 `{workspace}/tasks/{task_id}/files/`
//! （见 execution.rs 归集逻辑），全部位于 ServeDir 根内。

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode, header, HeaderValue},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json,
};
use serde::Serialize;
use serde_json::{Value, json};
use tower_http::services::ServeDir;

use ep_core::config::{self, AppConfig};

use crate::api::err_response;
use crate::api::execute::execution::{self, TaskRecord};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/tasks", get(list_tasks))
        .route("/tasks/{task_id}", get(get_task))
        .route("/tasks/{task_id}/artifacts", get(list_task_artifacts))
        .route(
            "/tasks/{task_id}/artifacts/{node_id}",
            get(get_task_artifact),
        )
        // 产物文件流式下载通道（handler 302 到此处，ServeDir 负责流式发送）
        .nest("/task-files", task_files_router())
}

/// ServeDir 根 = `{workspace}/tasks`（与 execution.rs 的任务目录同源）。
///
/// 路由构造先于 AppState 注入，无法读取运行期状态，故按启动同款方式
/// （resolve_root + 磁盘 config）重新解析。运行期经 PUT /api/config 修改
/// workspace_dir 不会热跟随——与 main.rs 对静态目录 static_dir 的处理一致。
fn task_files_router() -> Router<Arc<AppState>> {
    let root = config::resolve_root();
    let cfg = AppConfig::load(&root.join("config")).unwrap_or_default();
    let tasks_root = cfg.resolve_workspace_dir(&root).join("tasks");
    Router::new()
        .fallback_service(ServeDir::new(tasks_root))
        .layer(middleware::from_fn(attachment_disposition))
}

/// 为 ServeDir 的成功响应补 `Content-Disposition: attachment; filename=...`
///
/// ServeDir 本身只按 inline 语义提供文件；文件名取 URI 最后一段
/// （百分号编码形态），按 RFC 5987 同时给出 `filename*`（UTF-8）。
async fn attachment_disposition(req: Request<Body>, next: Next) -> Response {
    let raw_name = req
        .uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let mut resp = next.run(req).await;
    if resp.status().is_success() && !raw_name.is_empty() {
        let decoded = percent_decode(&raw_name);
        let mut fallback: String = decoded
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if fallback.is_empty() {
            fallback = "artifact".to_string();
        }
        let value = format!(
            "attachment; filename=\"{fallback}\"; filename*=UTF-8''{raw_name}"
        );
        if let Ok(v) = HeaderValue::from_str(&value) {
            resp.headers_mut()
                .insert(header::CONTENT_DISPOSITION, v);
        }
    }
    resp
}

// ─── 响应形状（与前端 TaskSummary / TaskDetail / TaskArtifact 契约一致） ─────

#[derive(Serialize)]
struct TaskSummaryOut {
    id: String,
    pipeline_name: String,
    /// 小写状态字符串：pending / running / completed / failed
    status: String,
    started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
    node_count: usize,
    completed_nodes: usize,
    /// 任务↔管线身份（§6.8；直跑为 `direct/{module_id}`、批量组为
    /// `batch/{module_id}`）。历史任务字段缺失时省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_id: Option<String>,
    /// 聚合条目类型：`"group"` = 外部批量任务组（同组子任务折叠为一条）；
    /// 普通任务省略。前端据此渲染批量徽章并隐藏产物下载入口。
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
}

#[derive(Serialize)]
struct NodeDetailOut {
    node_id: String,
    /// pending / running / completed / failed / skipped
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct TaskDetailOut {
    #[serde(flatten)]
    summary: TaskSummaryOut,
    nodes: Vec<NodeDetailOut>,
}

#[derive(Serialize)]
struct ArtifactOut {
    node_id: String,
    /// 文件名
    name: String,
    /// 字节数
    size: u64,
}

fn summary_out(record: &TaskRecord) -> TaskSummaryOut {
    TaskSummaryOut {
        id: record.id.clone(),
        // 与引擎 TaskSummary.pipeline_name 语义一致（= pipeline.id）
        pipeline_name: record.pipeline_id.clone(),
        status: record.status.as_str().to_string(),
        started_at: record.started_at.to_rfc3339(),
        finished_at: record.finished_at.map(|t| t.to_rfc3339()),
        node_count: record.nodes.len(),
        completed_nodes: record
            .nodes
            .values()
            .filter(|n| n.state == "completed")
            .count(),
        pipeline_id: Some(record.pipeline_id.clone()),
        kind: None,
    }
}

/// 任务组聚合条目（外部批量接入）：同组成员折叠为一条，
/// 进度 = 子任务完成数 / 总数，状态 = 组滚动语义（partial = 部分失败）
fn group_summary_out(group_id: &str, members: &[TaskRecord]) -> TaskSummaryOut {
    use ep_core::task_registry::rollup_group;
    let rollup = rollup_group(&members.iter().collect::<Vec<_>>());
    let (module_id, _) = crate::api::batches::group_display(members);
    let started_at = members
        .iter()
        .map(|m| m.started_at)
        .min()
        .expect("group members are non-empty");
    let finished_at = if members.iter().all(|m| m.status.is_terminal()) {
        members.iter().filter_map(|m| m.finished_at).max()
    } else {
        None
    };
    TaskSummaryOut {
        id: group_id.to_string(),
        pipeline_name: module_id.clone(),
        status: rollup.status.as_str().to_string(),
        started_at: started_at.to_rfc3339(),
        finished_at: finished_at.map(|t| t.to_rfc3339()),
        node_count: rollup.total,
        completed_nodes: rollup.completed,
        // 前端直跑判别用 pipeline_id 前缀；批量组统一 `batch/` 命名空间
        pipeline_id: Some(format!("batch/{module_id}")),
        kind: Some("group"),
    }
}

fn detail_out(record: &TaskRecord) -> TaskDetailOut {
    let nodes = record
        .node_order
        .iter()
        .filter_map(|node_id| {
            record.nodes.get(node_id).map(|n| NodeDetailOut {
                node_id: node_id.clone(),
                state: n.state.clone(),
                error: n.error.clone(),
            })
        })
        .collect();
    TaskDetailOut {
        summary: summary_out(record),
        nodes,
    }
}

/// 任务组详情：成员任务映射为节点形状（node_id = 子任务 id），
/// 展开视图零前端契约改动即可逐成员展示状态与错误
fn group_detail_out(group_id: &str, members: &[TaskRecord]) -> TaskDetailOut {
    let nodes = members
        .iter()
        .map(|m| NodeDetailOut {
            node_id: m.id.clone(),
            state: m.status.as_str().to_string(),
            error: m.error.clone(),
        })
        .collect();
    TaskDetailOut {
        summary: group_summary_out(group_id, members),
        nodes,
    }
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/tasks — 任务列表（新任务在前）
///
/// 外部批量任务组折叠为一条聚合条目（`kind: "group"`），成员子任务不再
/// 单独出现；普通任务逐条透出。组内成员仍可经 `GET /api/tasks/{group_id}`
/// 展开，或走 v1 逐条契约。
async fn list_tasks() -> Json<Vec<TaskSummaryOut>> {
    let all = execution::snapshot_all();
    let mut grouped: std::collections::BTreeMap<String, Vec<&TaskRecord>> =
        std::collections::BTreeMap::new();
    let mut plain: Vec<TaskSummaryOut> = Vec::with_capacity(all.len());
    for record in &all {
        match &record.group {
            Some(g) => grouped.entry(g.clone()).or_default().push(record),
            None => plain.push(summary_out(record)),
        }
    }
    let mut group_entries: Vec<TaskSummaryOut> = grouped
        .into_iter()
        .map(|(id, members)| {
            let owned: Vec<TaskRecord> = members.into_iter().cloned().collect();
            group_summary_out(&id, &owned)
        })
        .collect();
    let mut out = plain;
    out.append(&mut group_entries);
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Json(out)
}

/// GET /api/tasks/:task_id — 任务详情（含各节点状态），404 若不存在。
///
/// task_id 命中任务组（外部批量）时返回组详情：成员任务以节点形状
/// （node_id = 子任务 id）逐条展示状态与错误。
async fn get_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let payload = match execution::snapshot(&task_id) {
        Some(record) => serde_json::to_value(detail_out(&record))
            .expect("TaskDetailOut serialization cannot fail"),
        None => {
            // 组 id 兜底：成员存在 → 组详情；否则 404
            let members = execution::snapshot_group(&task_id);
            if members.is_empty() {
                return err_response(
                    &state,
                    StatusCode::NOT_FOUND,
                    "apiPipelines.tasks.taskNotFound",
                    &[("taskId", task_id)],
                )
                .await;
            }
            serde_json::to_value(group_detail_out(&task_id, &members))
                .expect("TaskDetailOut serialization cannot fail")
        }
    };
    (StatusCode::OK, Json(payload))
}

/// GET /api/tasks/:task_id/artifacts — 产物列表，无产物返回空数组
///
/// 形状：`[{node_id, name(文件名), size(字节)}]`（前端 TaskArtifact 契约）。
async fn list_task_artifacts(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(record) = execution::snapshot(&task_id) else {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiPipelines.tasks.taskNotFound",
            &[("taskId", task_id)],
        )
        .await;
    };
    let mut out = Vec::new();
    for node_id in &record.node_order {
        // 优先归集副本（served_artifacts，finalize 后恒在 files/{node_id}/ 下），
        // 回退原始产物路径：任务终结后 workspace 清理会删除原始文件
        // （keep_workspace=false），旧实现只读 record.artifacts → metadata 失败
        // 全被跳过 → 返回空数组（任务 #10：与任务记录 json 不一致的根因）。
        let Some(path) = record
            .served_artifacts
            .get(node_id)
            .filter(|p| p.is_file())
            .or_else(|| record.artifacts.get(node_id))
        else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(path) else {
            continue; // 文件已不存在 → 不列入
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(ArtifactOut {
            node_id: node_id.clone(),
            name,
            size: meta.len(),
        });
    }
    (StatusCode::OK, Json(json!(out)))
}

/// GET /api/tasks/:task_id/artifacts/:node_id — 单节点产物下载
///
/// 校验通过后 302 到 `/api/task-files/...`（ServeDir 流式发送，支持大文件
/// 与 Range）；任务/节点/产物不存在 → 404。
async fn get_task_artifact(
    State(state): State<Arc<AppState>>,
    Path((task_id, node_id)): Path<(String, String)>,
) -> Response {
    let Some(record) = execution::snapshot(&task_id) else {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiPipelines.tasks.taskNotFound",
            &[("taskId", task_id)],
        )
        .await
        .into_response();
    };
    if !record.nodes.contains_key(&node_id) {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiPipelines.tasks.nodeNotFound",
            &[("nodeId", node_id)],
        )
        .await
        .into_response();
    }
    let Some(served) = execution::ensure_served_artifact(&task_id, &node_id)
    else {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiPipelines.tasks.noArtifact",
            &[("nodeId", node_id)],
        )
        .await
        .into_response();
    };
    let Some(file_name) = served.file_name() else {
        return err_response(
            &state,
            StatusCode::NOT_FOUND,
            "apiPipelines.tasks.artifactPathInvalid",
            &[("nodeId", node_id)],
        )
        .await
        .into_response();
    };

    // ServeDir 根内的归集布局：{task_id}/files/{node_id}/{filename}
    let location = format!(
        "/api/task-files/{}/{}/{}/{}",
        percent_encode(&task_id),
        "files",
        percent_encode(&node_id),
        percent_encode(&file_name.to_string_lossy()),
    );
    (
        StatusCode::FOUND,
        [(header::LOCATION, location)],
    )
        .into_response()
}

// ─── 百分号编解码（URL 段安全，零额外依赖） ─────────────────────────────────

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.'
            | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // 测试锁的唯一目的就是跨 await 串行化这些共享静态注册表的测试；
    // 锁内临界区全部是极短同步操作，不存在持锁阻塞运行时的风险。
    #![allow(clippy::await_holding_lock)]

    use super::*;

    use ep_core::config::AppConfig;
    use ep_core::pipeline::dag::Pipeline;
    use ep_core::port::PortManager;

    use crate::api::execute::execution::{self, lock_for_tests, SubmitOptions};

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-tasksapi-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_state(root: PathBuf) -> Arc<AppState> {
        Arc::new(AppState::new(
            root,
            AppConfig::default(),
            vec![],
            vec![],
            PortManager::new(18000, 19000),
        ))
    }

    /// 路径转义为 TOML basic string（Windows 路径含 `\`，不转义会被解析为转义序列）
    fn toml_path(p: &std::path::Path) -> String {
        p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
    }

    /// 提交 file_input→file_output 纯 builtin 管线并等待终结
    async fn run_copy_task(
        state: &Arc<AppState>,
        pipeline_id: &str,
        src: &std::path::Path,
        dest: &std::path::Path,
    ) -> String {
        let toml = format!(
            r#"
[pipeline]
id = "{pipeline_id}"
name = "任务接口测试"

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
            toml_path(src),
            toml_path(dest),
        );
        let pipeline = Pipeline::from_toml_str(&toml).unwrap();
        let task_id = execution::submit_pipeline(state, pipeline, None)
            .await
            .unwrap();
        for _ in 0..300 {
            if let Some(record) = execution::snapshot(&task_id) {
                if record.status.is_terminal() {
                    return task_id;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("任务未在超时前终结");
    }

    // ── 1. 空列表形状：GET /api/tasks → [] ──────────────────────────────────

    #[tokio::test]
    async fn test_list_tasks_empty() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let json = list_tasks().await;
        let value = serde_json::to_value(&json.0).unwrap();
        assert_eq!(value, json!([]));
    }

    // ── 2. 未知任务详情 → 404 + 中文错误（默认 zh-CN） ──────────────────────

    #[tokio::test]
    async fn test_get_task_unknown_404() {
        let state = test_state(unique_root("ghost"));
        let (status, body) =
            get_task(State(state), Path("task-ghost".to_string())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.0["error"].as_str().unwrap().contains("任务不存在"));
        assert!(body.0["error"].as_str().unwrap().contains("task-ghost"));
    }

    // ── 3. 列表/详情状态为小写字符串 + 字段形状 ─────────────────────────────

    #[tokio::test]
    async fn test_task_list_and_detail_status_string_shape() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("shape");
        let state = test_state(root.clone());
        let src = root.join("shape-in.txt");
        let dest = root.join("shape-out.txt");
        std::fs::write(&src, "shape").unwrap();
        let task_id = run_copy_task(&state, "shape-pipe", &src, &dest).await;

        // 列表形状
        let list = serde_json::to_value(&list_tasks().await.0).unwrap();
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let entry = &arr[0];
        assert_eq!(entry["id"], task_id);
        assert_eq!(entry["pipeline_name"], "shape-pipe");
        // status 必须是字符串（前端契约），且为小写
        assert_eq!(entry["status"], "completed");
        assert!(entry["status"].is_string());
        assert_eq!(entry["node_count"], 2);
        assert_eq!(entry["completed_nodes"], 2);
        assert!(entry["started_at"].is_string());
        assert!(entry["finished_at"].is_string());

        // 详情形状：TaskSummary 字段 + nodes[]
        let (status, detail) =
            get_task(State(state.clone()), Path(task_id.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail.0["status"], "completed");
        let nodes = detail.0["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["node_id"], "input");
        assert_eq!(nodes[0]["state"], "completed");
        assert!(nodes[0]["state"].is_string());
        assert!(nodes[0].get("error").is_none());
        // nodes 按定义顺序输出
        assert_eq!(nodes[1]["node_id"], "output");
    }

    // ── 4. 失败任务：状态 failed、节点 failed/skipped 字符串 ────────────────

    #[tokio::test]
    async fn test_failed_task_status_strings() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("failed");
        let state = test_state(root);
        let toml = r#"
[pipeline]
id = "fail-shape"
name = "失败形状"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { path = "/nonexistent/no-such-file.txt" }

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = { path = "/tmp/ep-no.txt" }

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml).unwrap();
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
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let list = serde_json::to_value(&list_tasks().await.0).unwrap();
        assert_eq!(list[0]["status"], "failed");

        let (_, detail) = get_task(State(state), Path(task_id)).await;
        let nodes = detail.0["nodes"].as_array().unwrap();
        let input = nodes.iter().find(|n| n["node_id"] == "input").unwrap();
        let output = nodes.iter().find(|n| n["node_id"] == "output").unwrap();
        assert_eq!(input["state"], "failed");
        assert!(input["error"].as_str().unwrap().contains("not found"));
        assert_eq!(output["state"], "skipped");
    }

    // ── 5. 产物列表形状：有产物 / 空数组 ────────────────────────────────────

    #[tokio::test]
    async fn test_artifacts_shape_and_empty() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("arts");
        let state = test_state(root.clone());
        let src = root.join("art-in.txt");
        let dest = root.join("art-out.txt");
        std::fs::write(&src, "artifact-body").unwrap();
        let ok_task = run_copy_task(&state, "art-pipe", &src, &dest).await;

        // 有产物：两个文件产物，形状 {node_id, name, size}
        let (status, body) =
            list_task_artifacts(State(state.clone()), Path(ok_task.clone())).await;
        assert_eq!(status, StatusCode::OK);
        let arr = body.0.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let names: Vec<&str> = arr.iter().map(|a| a["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"art-in.txt"));
        assert!(names.contains(&"art-out.txt"));
        for artifact in arr {
            assert!(artifact["node_id"].is_string());
            assert!(artifact["name"].is_string());
            assert!(artifact["size"].as_u64().unwrap() > 0);
        }

        // 失败任务无产物 → 空数组
        let toml = r#"
[pipeline]
id = "art-empty"
name = "无产物"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { path = "/nonexistent/missing-art.txt" }
"#;
        let pipeline = Pipeline::from_toml_str(toml).unwrap();
        let empty_task = execution::submit_pipeline(&state, pipeline, None)
            .await
            .unwrap();
        for _ in 0..300 {
            if let Some(r) = execution::snapshot(&empty_task) {
                if r.status.is_terminal() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (status, body) =
            list_task_artifacts(State(state.clone()), Path(empty_task)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.0, json!([]));

        // 未知任务 → 404
        let (status, body) = list_task_artifacts(
            State(state),
            Path("task-ghost".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.0["error"].as_str().unwrap().contains("任务不存在"));
    }

    // ── 5b. 回归（任务 #10）：原始产物文件被清理后，列表仍返回真实产物 ──────
    //     （旧实现只读 record.artifacts 原始路径 → metadata 失败 → 空数组；
    //      修复后优先读 served_artifacts 归集副本）

    #[tokio::test]
    async fn test_artifacts_list_survives_original_cleanup() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("arts-served");
        let state = test_state(root.clone());
        let src = root.join("served-in.txt");
        let dest = root.join("served-out.txt");
        std::fs::write(&src, "served-body").unwrap();
        let task_id = run_copy_task(&state, "served-pipe", &src, &dest).await;

        // 前置：任务记录中两类产物路径均已登记
        let record = execution::snapshot(&task_id).expect("任务应存在");
        assert!(!record.artifacts.is_empty(), "原始产物应有登记");
        assert!(!record.served_artifacts.is_empty(), "归集副本应有登记");
        drop(record);

        // 模拟 workspace 清理：删除原始产物文件，保留 files/{node_id}/ 归集副本
        let record = execution::snapshot(&task_id).unwrap();
        for path in record.artifacts.values() {
            let _ = std::fs::remove_file(path);
        }

        let (status, body) =
            list_task_artifacts(State(state.clone()), Path(task_id.clone())).await;
        assert_eq!(status, StatusCode::OK);
        let arr = body.0.as_array().unwrap();
        assert_eq!(
            arr.len(),
            2,
            "原始文件被清理后仍应从归集副本读出完整列表"
        );
        let names: Vec<&str> = arr.iter().map(|a| a["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"served-in.txt"));
        assert!(names.contains(&"served-out.txt"));
        for artifact in arr {
            assert!(artifact["size"].as_u64().unwrap() > 0);
        }
    }

    // ── 6. 下载重定向：302 + Location；未知任务/节点/产物 → 404 ───────────

    #[tokio::test]
    async fn test_artifact_download_redirect_and_404s() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("dl");
        let state = test_state(root.clone());
        let src = root.join("下载 输入.txt"); // 含中文与空格，验证百分号编码
        let dest = root.join("dl-out.txt");
        std::fs::write(&src, "download me").unwrap();
        let task_id = run_copy_task(&state, "dl-pipe", &src, &dest).await;

        // output 节点产物 → 302 + 编码后的 Location
        let resp = get_task_artifact(
            State(state.clone()),
            Path((task_id.clone(), "output".to_string())),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FOUND);
        let location = resp
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            location.starts_with("/api/task-files/"),
            "Location 应指向 task-files 通道: {location}"
        );
        assert!(location.contains(&format!("{task_id}/files/output/")));
        assert!(location.contains("dl-out.txt"));

        // input 节点产物（文件名含中文/空格）→ 必须百分号编码
        let resp = get_task_artifact(
            State(state.clone()),
            Path((task_id.clone(), "input".to_string())),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FOUND);
        let location = resp
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(location.contains("%E4%B8%8B%E8%BD%BD"), "中文应被编码: {location}");
        assert!(!location.contains(' '), "Location 不允许裸空格: {location}");

        // 未知节点 → 404
        let resp = get_task_artifact(
            State(state.clone()),
            Path((task_id.clone(), "ghost".to_string())),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        // 未知任务 → 404
        let resp = get_task_artifact(
            State(state.clone()),
            Path(("task-ghost".to_string(), "output".to_string())),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // 节点存在但无产物 → 404（用失败任务）
        let toml = r#"
[pipeline]
id = "dl-empty"
name = "无产物下载"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { path = "/nonexistent/missing-dl.txt" }
"#;
        let pipeline = Pipeline::from_toml_str(toml).unwrap();
        let empty_task = execution::submit_pipeline(&state, pipeline, None)
            .await
            .unwrap();
        for _ in 0..300 {
            if let Some(r) = execution::snapshot(&empty_task) {
                if r.status.is_terminal() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let resp = get_task_artifact(
            State(state),
            Path((empty_task, "input".to_string())),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── 7. 百分号编解码往返 ─────────────────────────────────────────────────

    #[test]
    fn test_percent_codec_roundtrip() {
        for s in [
            "plain.txt",
            "中文 文件名.srt",
            "a%b&c=d",
            "空格 与/斜杠不可能",
        ] {
            let encoded = percent_encode(s);
            assert!(encoded.is_ascii(), "编码结果必须 ASCII: {encoded}");
            assert_eq!(percent_decode(&encoded), s);
        }
    }

    // ── 8. tasks_root 可解析（ServeDir 根构造不 panic） ─────────────────────

    #[test]
    fn test_task_files_router_builds() {
        let router = task_files_router();
        let _ = router; // 能构造即通过（ServeDir 根目录不存在也不报错）
    }

    // ── 9. 任务组聚合（外部批量接入）：同组子任务折叠为一条 kind=group 条目，
    //      组详情以节点形状逐成员展示 ────────────────────────────────────────

    /// file_input→file_output 拷贝管线 TOML（指定管线 id）
    fn copy_toml(pipeline_id: &str, src: &std::path::Path, dest: &std::path::Path) -> String {
        format!(
            r#"
[pipeline]
id = "{pipeline_id}"
name = "组聚合测试"

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
            toml_path(src),
            toml_path(dest),
        )
    }

    #[tokio::test]
    async fn test_group_tasks_fold_into_single_entry() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("group");
        let state = test_state(root.clone());
        let src = root.join("grp-in.txt");
        std::fs::write(&src, "group-body").unwrap();
        let dest = root.join("grp-out.txt");

        // 同组提交两条拷贝任务（组 A：completed ×2）+ 一条游离任务
        let group_id = execution::new_group_id();
        let mut member_ids = Vec::new();
        for pid in ["grp-pipe-a", "grp-pipe-b"] {
            let pipeline = Pipeline::from_toml_str(&copy_toml(pid, &src, &dest)).unwrap();
            let options = SubmitOptions {
                group: Some(group_id.clone()),
                ..Default::default()
            };
            member_ids.push(
                execution::submit_pipeline_full(&state, pipeline, None, options)
                    .await
                    .unwrap()
                    .task_id,
            );
        }
        let solo_pipeline = Pipeline::from_toml_str(&copy_toml("solo-pipe", &src, &dest)).unwrap();
        let solo_id = execution::submit_pipeline(&state, solo_pipeline, None)
            .await
            .unwrap();

        // 等待全部终结
        for _ in 0..300 {
            let all_terminal = [member_ids.as_slice(), &[solo_id.clone()]].concat();
            let done = all_terminal.iter().all(|id| {
                execution::snapshot(id).map(|r| r.status.is_terminal()).unwrap_or(false)
            });
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // 列表：3 条物理任务折叠为 2 条（1 条 group + 1 条游离），无成员泄漏
        let list = serde_json::to_value(&list_tasks().await.0).unwrap();
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 2, "同组成员应折叠为一条: {list}");
        let group_entry = arr
            .iter()
            .find(|e| e["kind"] == "group")
            .expect("应有 group 条目");
        assert_eq!(group_entry["id"], group_id);
        assert_eq!(group_entry["status"], "completed");
        assert_eq!(group_entry["node_count"], 2);
        assert_eq!(group_entry["completed_nodes"], 2);
        assert!(
            group_entry["pipeline_id"].as_str().unwrap().starts_with("batch/"),
            "组条目 pipeline_id 应为 batch/ 命名空间"
        );
        assert!(
            arr.iter().all(|e| e["id"] != json!(member_ids[0]) && e["id"] != json!(member_ids[1])),
            "成员子任务不得单独出现在列表"
        );

        // 游离任务条目不带 kind，pipeline_id 透出（直跑/管线判别数据源）
        let solo_entry = arr.iter().find(|e| e["id"] == json!(solo_id)).unwrap();
        assert!(solo_entry.get("kind").is_none());
        assert_eq!(solo_entry["pipeline_id"], "solo-pipe");

        // 组详情：成员以节点形状逐条展示（node_id = 子任务 id）
        let (status, detail) =
            get_task(State(state), Path(group_id.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail.0["kind"], "group");
        assert_eq!(detail.0["status"], "completed");
        let nodes = detail.0["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        let node_ids: Vec<&str> = nodes.iter().map(|n| n["node_id"].as_str().unwrap()).collect();
        assert!(node_ids.contains(&member_ids[0].as_str()));
        assert!(node_ids.contains(&member_ids[1].as_str()));
        assert!(nodes.iter().all(|n| n["state"] == "completed"));
    }

    // ── 10. 部分失败组：completed + failed → 组状态 partial ─────────────────

    #[tokio::test]
    async fn test_group_partial_failure_rollup() {
        let _guard = lock_for_tests();
        execution::clear_registry_for_tests();

        let root = unique_root("group-partial");
        let state = test_state(root.clone());
        let src = root.join("grp-partial-in.txt");
        std::fs::write(&src, "partial").unwrap();
        let dest = root.join("grp-partial-out.txt");
        let group_id = execution::new_group_id();

        // 成员 1：正常拷贝（completed）
        let pipeline = Pipeline::from_toml_str(&copy_toml("partial-ok", &src, &dest)).unwrap();
        let options = SubmitOptions {
            group: Some(group_id.clone()),
            ..Default::default()
        };
        execution::submit_pipeline_full(&state, pipeline, None, options)
            .await
            .unwrap();
        // 成员 2：输入缺失（failed）
        let pipeline =
            Pipeline::from_toml_str(&copy_toml("partial-bad", std::path::Path::new("/nonexistent/x"), &dest))
                .unwrap();
        let options = SubmitOptions {
            group: Some(group_id.clone()),
            ..Default::default()
        };
        execution::submit_pipeline_full(&state, pipeline, None, options)
            .await
            .unwrap();

        for _ in 0..300 {
            let members = execution::snapshot_group(&group_id);
            if members.len() == 2 && members.iter().all(|m| m.status.is_terminal()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let list = serde_json::to_value(&list_tasks().await.0).unwrap();
        let arr = list.as_array().unwrap();
        let group_entry = arr.iter().find(|e| e["kind"] == "group").unwrap();
        assert_eq!(group_entry["status"], "partial", "completed+failed 应滚动为 partial");
        assert_eq!(group_entry["node_count"], 2);
        assert_eq!(group_entry["completed_nodes"], 1);
    }
}
