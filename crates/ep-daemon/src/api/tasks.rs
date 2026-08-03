//! 任务中心 API — Wave 2 执行代理（W2-D）
//!
//! 数据来源：`crate::api::execute::execution` 的任务注册表。
//! 注册表方案说明（为何不直接读 `state.runner.list_tasks()`）见 execution.rs
//! 头部文档：引擎执行全程持 `&mut self` 且任务存储私有、`get_task_detail`
//! 不暴露产物路径，故执行在锁外进行、任务状态由注册表维护，
//! 本文件所有查询因此永不阻塞。
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

// ─── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/tasks — 任务列表（新任务在前）
async fn list_tasks() -> Json<Vec<TaskSummaryOut>> {
    Json(execution::snapshot_all().iter().map(summary_out).collect())
}

/// GET /api/tasks/:task_id — 任务详情（含各节点状态），404 若不存在
async fn get_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    match execution::snapshot(&task_id) {
        Some(record) => (
            StatusCode::OK,
            Json(
                serde_json::to_value(detail_out(&record))
                    .expect("TaskDetailOut serialization cannot fail"),
            ),
        ),
        None => {
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
        let Some(path) = record.artifacts.get(node_id) else {
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

    use crate::api::execute::execution::{TaskState, lock_for_tests};

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
            src.display(),
            dest.display(),
        );
        let pipeline = Pipeline::from_toml_str(&toml).unwrap();
        let task_id = execution::submit_pipeline(state, pipeline, None)
            .await
            .unwrap();
        for _ in 0..300 {
            if let Some(record) = execution::snapshot(&task_id) {
                if !matches!(record.status, TaskState::Running) {
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
                if !matches!(r.status, TaskState::Running) {
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
                if !matches!(r.status, TaskState::Running) {
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

    // ── 6. 下载重定向：302 + Location；未知任务/节点/产物 → 404 ─────────────

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
                if !matches!(r.status, TaskState::Running) {
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
}
