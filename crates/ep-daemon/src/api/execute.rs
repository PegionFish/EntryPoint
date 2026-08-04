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
use std::path::Path;
use std::sync::Arc;

use axum::{Router, extract::State, http::StatusCode, routing::post, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;

use ep_core::pipeline::dag::Pipeline;
use ep_core::pipeline::load_pipeline;

use crate::api::err_response;
use crate::api::pipelines::pipeline_bridge::{spec_to_pipeline, PipelineSpec};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/pipelines/execute", post(execute_pipeline))
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

    match execution::submit_pipeline(&state, pipeline, req.inputs).await {
        Ok(task_id) => (
            StatusCode::ACCEPTED,
            Json(json!({ "task_id": task_id })),
        ),
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

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // 测试锁的唯一目的就是跨 await 串行化这些共享静态注册表的测试；
    // 锁内临界区全部是极短同步操作，不存在持锁阻塞运行时的风险。
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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
}
