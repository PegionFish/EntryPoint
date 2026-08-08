//! 管线执行引擎 — 状态机 + 节点执行辅助函数
//!
//! PipelineTask 提供任务状态管理和节点状态转换。
//! 模块底部的辅助函数实现各类型节点的实际执行逻辑。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::{Artifact, TaskStatus};

use super::dag::{NodeKind, Pipeline, PipelineNode};

// ─── 节点状态 ────────────────────────────────────────────────────────────────

/// 单个节点的执行状态
#[derive(Debug, Clone, PartialEq)]
pub enum NodeState {
    /// 等待执行
    Pending,
    /// 正在执行
    Running,
    /// 执行完成，附带输出产物
    Completed { artifact: Option<Artifact> },
    /// 执行失败
    Failed {
        error: String,
        /// 是否为可重试错误（连接失败 / 超时等瞬态故障）
        retryable: bool,
    },
    /// 因上游失败而跳过
    Skipped,
}

impl NodeState {
    /// 节点是否已终结（不会再变化）
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. } | Self::Skipped)
    }
}

// ─── 管线任务 ────────────────────────────────────────────────────────────────

/// 一次管线执行实例
#[derive(Debug, Clone)]
pub struct PipelineTask {
    /// 任务唯一 ID
    pub id: String,
    /// 关联的管线 ID
    pub pipeline_id: String,
    /// 任务整体状态
    pub status: TaskStatus,
    /// 各节点状态
    pub node_states: HashMap<String, NodeState>,
    /// 任务工作目录
    pub work_dir: PathBuf,
    /// 任务启动时间
    pub started_at: DateTime<Utc>,
    /// 任务完成时间（所有节点终结时设置）
    pub finished_at: Option<DateTime<Utc>>,
}

impl PipelineTask {
    /// 创建新的管线任务
    ///
    /// 所有节点初始状态为 `Pending`，任务状态为 `Pending`。
    pub fn new(pipeline: &Pipeline, work_dir: PathBuf) -> Self {
        let node_states = pipeline
            .nodes
            .iter()
            .map(|n| (n.id.clone(), NodeState::Pending))
            .collect();

        Self {
            id: Uuid::new_v4().to_string(),
            pipeline_id: pipeline.id.clone(),
            status: TaskStatus::Pending,
            node_states,
            work_dir,
            started_at: Utc::now(),
            finished_at: None,
        }
    }

    /// 执行一层节点 — 将该层所有 Pending 节点标记为 Running
    ///
    /// TODO: 实际执行逻辑（HTTP 调用 / spawn 进程 / 内置函数）
    pub fn execute_layer(&mut self, layer: &[String]) {
        // 首次执行时将任务状态设为 Running
        if self.status == TaskStatus::Pending {
            self.status = TaskStatus::Running;
        }

        for node_id in layer {
            if let Some(state) = self.node_states.get_mut(node_id) {
                if *state == NodeState::Pending {
                    *state = NodeState::Running;
                }
            }
        }
    }

    /// 标记节点执行完成
    pub fn mark_completed(&mut self, node_id: &str, artifact: Artifact) {
        if let Some(state) = self.node_states.get_mut(node_id) {
            *state = NodeState::Completed {
                artifact: Some(artifact),
            };
        }
        self.check_completion();
    }

    /// 标记节点执行失败，并将其余未执行的节点标记为 Skipped
    ///
    /// 限制（P1 修复）：本方法签名不含 pipeline 引用，无法计算精确的
    /// 传递下游闭包（精确版本见 [`Self::mark_failed_with_pipeline`]）。
    /// 保守起见将**所有仍为 Pending 的节点**一并置 Skipped —— 在
    /// 「首个失败即终止管线」的执行模型下，任一 Pending 节点都不会再
    /// 执行，该超集跳过不会改变任务终态，同时保证 `is_complete()` 终能
    /// 成立、`check_completion` 正确置 Failed（修复旧实现空跳过导致的
    /// 状态机永不终结）。
    pub fn mark_failed(&mut self, node_id: &str, error: String, retryable: bool) {
        if let Some(state) = self.node_states.get_mut(node_id) {
            *state = NodeState::Failed { error, retryable };
        }

        // 无管线引用：所有剩余 Pending 节点保守置 Skipped
        for state in self.node_states.values_mut() {
            if *state == NodeState::Pending {
                *state = NodeState::Skipped;
            }
        }

        self.check_completion();
    }

    /// 标记节点失败并跳过其下游（需要管线引用以计算传递闭包）
    pub fn mark_failed_with_pipeline(&mut self, node_id: &str, error: String, retryable: bool, pipeline: &Pipeline) {
        if let Some(state) = self.node_states.get_mut(node_id) {
            *state = NodeState::Failed { error, retryable };
        }

        // 获取所有传递下游并标记为 Skipped
        let downstream = pipeline.all_downstream_of(node_id);
        for ds_id in downstream {
            if let Some(state) = self.node_states.get_mut(ds_id) {
                if *state == NodeState::Pending {
                    *state = NodeState::Skipped;
                }
            }
        }

        self.check_completion();
    }

    /// 任务是否已全部完成（所有节点都处于终结状态）
    pub fn is_complete(&self) -> bool {
        self.node_states.values().all(|s| s.is_terminal())
    }

    /// 获取指定节点的状态
    pub fn node_state(&self, node_id: &str) -> Option<&NodeState> {
        self.node_states.get(node_id)
    }

    // ─── 内部方法 ────────────────────────────────────────────────────────────

    /// 将当前层中仍为 Running 的节点一并置 Skipped（P0 状态机泄漏修复）。
    ///
    /// 层内某个节点失败/被取消后，同层其余兄弟节点已被 `execute_layer`
    /// 置为 Running，但 runner 的失败分支会 `break` 提前终止本层执行，
    /// 若不处理它们将永远保持 Running，导致 `is_complete()` 恒 false、
    /// 任务卡在 Running 且外层误报成功。置 Skipped 后再次触发
    /// `check_completion`：全部节点终结时任务终态（Failed/Cancelled）落地。
    ///
    /// 仅处理 Running → Skipped：Completed/Failed/Skipped 保持不动，
    /// Pending 不在当前层（本层已被 execute_layer 置位）。
    pub(crate) fn skip_layer_remaining(&mut self, layer: &[String]) {
        for node_id in layer {
            if let Some(state) = self.node_states.get_mut(node_id) {
                if *state == NodeState::Running {
                    *state = NodeState::Skipped;
                }
            }
        }
        self.check_completion();
    }

    /// 检查是否所有节点都已终结，更新任务整体状态
    fn check_completion(&mut self) {
        if !self.is_complete() {
            return;
        }

        // 记录完成时间
        if self.finished_at.is_none() {
            self.finished_at = Some(Utc::now());
        }

        let has_failure = self
            .node_states
            .values()
            .any(|s| matches!(s, NodeState::Failed { .. }));

        if has_failure {
            let first_error = self
                .node_states
                .values()
                .find_map(|s| match s {
                    NodeState::Failed { error, .. } => Some(error.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            self.status = TaskStatus::Failed(first_error);
        } else {
            self.status = TaskStatus::Completed;
        }
    }
}

// ─── 单元测试 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::dag::Pipeline;
    use crate::pipeline::runner::PipelineRunnerImpl;
    use crate::types::{PipelineRunner, TaskStatus};

    fn test_pipeline() -> Pipeline {
        let toml_str = r#"
[pipeline]
id = "test-exec"
name = "Execution test"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "process"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["process", "input"]

[[edges]]
from = ["process", "output"]
to = ["save", "input"]
"#;
        Pipeline::from_toml_str(toml_str).unwrap()
    }

    #[test]
    fn test_task_creation() {
        let pipeline = test_pipeline();
        let task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        assert_eq!(task.pipeline_id, "test-exec");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.node_states.len(), 3);
        assert!(task.node_states.values().all(|s| *s == NodeState::Pending));
    }

    #[test]
    fn test_execute_layer() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        task.execute_layer(&["input".to_string()]);

        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.node_states["input"], NodeState::Running);
        assert_eq!(task.node_states["process"], NodeState::Pending);
    }

    #[test]
    fn test_mark_completed() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        task.execute_layer(&["input".to_string()]);
        task.mark_completed("input", Artifact::File(PathBuf::from("/tmp/test/input.wav")));

        assert_eq!(
            task.node_states["input"],
            NodeState::Completed {
                artifact: Some(Artifact::File(PathBuf::from("/tmp/test/input.wav")))
            }
        );
    }

    #[test]
    fn test_full_completion() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        // 逐层执行
        task.execute_layer(&["input".to_string()]);
        task.mark_completed("input", Artifact::File(PathBuf::from("input.wav")));

        task.execute_layer(&["process".to_string()]);
        task.mark_completed("process", Artifact::Text("hello".to_string()));

        task.execute_layer(&["save".to_string()]);
        task.mark_completed("save", Artifact::File(PathBuf::from("output.srt")));

        assert!(task.is_complete());
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[test]
    fn test_mark_failed_with_downstream_skip() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        task.execute_layer(&["input".to_string()]);
        task.mark_completed("input", Artifact::File(PathBuf::from("input.wav")));

        task.execute_layer(&["process".to_string()]);
        task.mark_failed_with_pipeline(
            "process",
            "model not found".to_string(),
            false,
            &pipeline,
        );

        assert_eq!(
            task.node_states["process"],
            NodeState::Failed {
                error: "model not found".to_string(),
                retryable: false,
            }
        );
        assert_eq!(task.node_states["save"], NodeState::Skipped);
        assert!(task.is_complete());
        assert!(matches!(task.status, TaskStatus::Failed(_)));
    }

    #[test]
    fn test_mark_failed_skips_remaining_pending() {
        let pipeline = test_pipeline();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        task.execute_layer(&["input".to_string()]);
        task.mark_completed("input", Artifact::File(PathBuf::from("in.wav")));
        task.execute_layer(&["process".to_string()]);
        task.mark_failed("process", "boom".to_string(), false);

        assert_eq!(
            task.node_states["process"],
            NodeState::Failed {
                error: "boom".to_string(),
                retryable: false,
            }
        );
        // 无 pipeline 引用：剩余 Pending 节点保守置 Skipped（不再空跳过）
        assert_eq!(task.node_states["save"], NodeState::Skipped);
        assert!(task.is_complete());
        assert!(matches!(task.status, TaskStatus::Failed(_)));
    }

    /// P0 状态机泄漏修复：同层兄弟失败后，剩余 Running 兄弟必须被置
    /// Skipped、全部节点终结、任务终态 Failed（修复前兄弟永久 Running，
    /// is_complete 恒 false、check_completion 不置 Failed）。
    #[test]
    fn test_skip_layer_remaining_after_failure() {
        let toml_str = r#"
[pipeline]
id = "test-skip-layer"
name = "Skip layer"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "process"
kind = "module"
module_id = "m"
capability = "c"

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "output"]
to = ["process", "input"]

[[edges]]
from = ["input", "output"]
to = ["save", "input"]
"#;
        let pipeline = Pipeline::from_toml_str(toml_str).unwrap();
        let mut task = PipelineTask::new(&pipeline, PathBuf::from("/tmp/test"));

        task.execute_layer(&["input".to_string()]);
        task.mark_completed("input", Artifact::File(PathBuf::from("in.wav")));

        // process 与 save 同层，均为 Running
        task.execute_layer(&["process".to_string(), "save".to_string()]);
        assert_eq!(task.node_states["save"], NodeState::Running);

        // process 失败 → 下游跳过 + 同层剩余 Running 兄弟置 Skipped
        task.mark_failed_with_pipeline("process", "boom".into(), false, &pipeline);
        task.skip_layer_remaining(&["process".to_string(), "save".to_string()]);

        assert_eq!(task.node_states["save"], NodeState::Skipped, "同层兄弟须置 Skipped");
        assert_eq!(
            task.node_states["process"],
            NodeState::Failed {
                error: "boom".to_string(),
                retryable: false,
            }
        );
        assert!(task.is_complete(), "全部节点应终结");
        assert!(matches!(task.status, TaskStatus::Failed(_)), "终态应为 Failed");
    }

    /// timeout_secs=0 语义（P3）：显式 0 = 无硬超时（返回 0，客户端不设超时），
    /// 与 runner 侧「0 即无 wall-clock 包裹」一致；None 才回退缺省 300s。
    #[test]
    fn test_node_timeout_secs_zero_means_no_timeout() {
        let node = |secs: Option<u32>| PipelineNode {
            id: "n".to_string(),
            kind: NodeKind::Builtin {
                builtin: "ffmpeg".to_string(),
            },
            label: String::new(),
            params: serde_json::json!({}),
            position: None,
            timeout_secs: secs,
            retry_count: None,
        };

        assert_eq!(node_timeout_secs(&node(Some(0))), 0, "Some(0) = 无硬超时");
        assert_eq!(node_timeout_secs(&node(None)), HTTP_TIMEOUT_SECS, "缺省回退 300s");
        assert_eq!(node_timeout_secs(&node(Some(7))), 7);
    }

    // ─── ffmpeg args 占位符替换（P0：shipped 管线依赖 {input}/{output}） ────

    /// 探测 ffmpeg 二进制是否可用（与执行器同款解析逻辑）
    fn ffmpeg_available() -> bool {
        std::process::Command::new(resolve_ffmpeg_path())
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// 创建临时工作目录
    fn ffmpeg_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ep_ffmpeg_ph_{label}_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 清理临时目录
    fn cleanup_ffmpeg_dir(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 用 ffmpeg lavfi 生成真实小音频文件（1s 正弦波）
    fn generate_audio_file(path: &Path, codec_args: &[&str]) -> bool {
        let status = std::process::Command::new(resolve_ffmpeg_path())
            .args(["-y", "-hide_banner", "-loglevel", "error"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
            .args(codec_args)
            .arg(path)
            .status();
        matches!(status, Ok(s) if s.success())
    }

    /// 构造 ffmpeg builtin 节点
    fn ffmpeg_node(id: &str, params: serde_json::Value) -> PipelineNode {
        PipelineNode {
            id: id.to_string(),
            kind: NodeKind::Builtin {
                builtin: "ffmpeg".to_string(),
            },
            label: String::new(),
            params,
            position: None,
            timeout_secs: None,
            retry_count: None,
        }
    }

    // ── 纯函数级：substitute_ffmpeg_placeholders ─────────────────────────────

    #[test]
    fn test_substitute_placeholders_input_and_output() {
        let args: Vec<String> = ["-i", "{input}", "-vn", "-acodec", "copy", "{output}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let input = PathBuf::from("/tmp/media/video.mp4");
        let output = PathBuf::from("/tmp/work/extract_output.m4a");

        let (replaced, output_substituted) =
            substitute_ffmpeg_placeholders(&args, "extract", Some(&input), &output).unwrap();

        assert_eq!(
            replaced,
            vec![
                "-i",
                "/tmp/media/video.mp4",
                "-vn",
                "-acodec",
                "copy",
                "/tmp/work/extract_output.m4a",
            ]
        );
        assert!(
            output_substituted,
            "no output argument may be appended once {{output}} has been substituted"
        );
        // 无残留占位符字面量进入命令行
        assert!(replaced
            .iter()
            .all(|a| !a.contains("{input}") && !a.contains("{output}")));
    }

    #[test]
    fn test_substitute_placeholders_absent_is_identity() {
        // lavfi 风格自包含 args（既有 shipped 测试同款）：无占位符 → 原样透传
        let args: Vec<String> = [
            "-f", "lavfi", "-i", "testsrc=duration=1:size=32x32:rate=1", "-frames:v", "1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let (replaced, output_substituted) =
            substitute_ffmpeg_placeholders(&args, "encode", None, Path::new("/tmp/o")).unwrap();

        assert_eq!(replaced, args);
        assert!(
            !output_substituted,
            "no {{output}} placeholder → keep trailing-append behavior"
        );
    }

    #[test]
    fn test_substitute_placeholders_missing_upstream_error() {
        let args = vec!["-i".to_string(), "{input}".to_string(), "{output}".to_string()];

        let err =
            substitute_ffmpeg_placeholders(&args, "extract-audio", None, Path::new("/tmp/o"))
                .expect_err("must error when {input} is present without an upstream file");
        let msg = err.to_string();

        assert!(msg.contains("extract-audio"), "error should name the node id: {msg}");
        assert!(
            msg.contains("placeholder") && msg.contains("upstream"),
            "error message should be English: {msg}"
        );
    }

    #[test]
    fn test_substitute_placeholders_multiple_and_inline_occurrences() {
        let args = vec![
            "{input}".to_string(),
            "{input}".to_string(),
            "concat:{output}".to_string(),
        ];

        let (replaced, output_substituted) = substitute_ffmpeg_placeholders(
            &args,
            "n",
            Some(Path::new("a.mp4")),
            Path::new("o.wav"),
        )
        .unwrap();

        assert_eq!(replaced, vec!["a.mp4", "a.mp4", "concat:o.wav"]);
        assert!(output_substituted);
    }

    #[test]
    fn test_resolve_output_path_honors_output_extension() {
        let node = ffmpeg_node(
            "extract-audio",
            serde_json::json!({ "output_extension": "wav" }),
        );
        let path = resolve_ffmpeg_output_path(&node, Path::new("/work"));
        assert_eq!(path, PathBuf::from("/work/extract-audio_output.wav"));

        // 显式 output 参数优先，且不叠加扩展名
        let node = ffmpeg_node(
            "x",
            serde_json::json!({ "output": "/out/custom.bin", "output_extension": "wav" }),
        );
        assert_eq!(
            resolve_ffmpeg_output_path(&node, Path::new("/work")),
            PathBuf::from("/out/custom.bin")
        );

        // 无 output_extension → 保持旧路径形状（向后兼容）
        let node = ffmpeg_node("y", serde_json::json!({}));
        assert_eq!(
            resolve_ffmpeg_output_path(&node, Path::new("/work")),
            PathBuf::from("/work/y_output")
        );
    }

    // ── e2e：真实 ffmpeg 二进制 + tempdir 小文件 ─────────────────────────────

    /// shipped video_to_srt.toml extract-audio 节点同款 args：
    /// {input}/{output} 均被替换、输出落盘、无 `{input}` 字面量残留
    /// （字面量残留时 ffmpeg 会报 "No such file or directory"，测试即失败）
    #[tokio::test]
    async fn test_ffmpeg_placeholder_e2e_video_to_srt_shape() {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not available");
            return;
        }

        let work_dir = ffmpeg_temp_dir("v2s");
        let input_file = work_dir.join("source.wav");
        if !generate_audio_file(&input_file, &["-ar", "16000", "-ac", "1"]) {
            eprintln!("SKIP: failed to generate test audio");
            cleanup_ffmpeg_dir(&work_dir);
            return;
        }

        let node = ffmpeg_node(
            "extract-audio",
            serde_json::json!({
                "args": ["-i", "{input}", "-vn", "-acodec", "pcm_s16le", "-ar", "16000", "-ac", "1", "{output}"],
                "output_extension": "wav",
            }),
        );

        let artifact =
            execute_builtin_ffmpeg(&node, &[Artifact::File(input_file)], &work_dir)
                .await
                .expect("ffmpeg should succeed after placeholder substitution");

        let output = match artifact {
            Artifact::File(p) => p,
            other => panic!("expected file artifact, got {other:?}"),
        };
        assert_eq!(output, work_dir.join("extract-audio_output.wav"));
        assert!(output.exists(), "output file should be created");
        assert!(std::fs::metadata(&output).unwrap().len() > 0);

        cleanup_ffmpeg_dir(&work_dir);
    }

    /// args 无占位符且无 -i：保持旧行为（前置上游输入 + 末尾追加输出）
    #[tokio::test]
    async fn test_ffmpeg_legacy_args_backward_compat() {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not available");
            return;
        }

        let work_dir = ffmpeg_temp_dir("legacy");
        let input_file = work_dir.join("in.wav");
        if !generate_audio_file(&input_file, &["-ar", "16000", "-ac", "1"]) {
            eprintln!("SKIP: failed to generate test audio");
            cleanup_ffmpeg_dir(&work_dir);
            return;
        }

        let node = ffmpeg_node(
            "encode",
            serde_json::json!({
                "args": ["-c", "copy"],
                "output_extension": "wav",
            }),
        );

        let artifact =
            execute_builtin_ffmpeg(&node, &[Artifact::File(input_file)], &work_dir)
                .await
                .expect("legacy behavior without placeholders should be unchanged");

        let Artifact::File(output) = artifact else {
            panic!("expected file artifact");
        };
        assert_eq!(output, work_dir.join("encode_output.wav"));
        assert!(output.exists());
        assert!(std::fs::metadata(&output).unwrap().len() > 0);

        cleanup_ffmpeg_dir(&work_dir);
    }

    /// P3：ffmpeg 命令以 0 退出但未产出目标文件 → 必须报错（防止下游拿到
    /// 悬空路径）。输出指向空设备（NUL //dev/null）时命令成功但无常规文件。
    #[tokio::test]
    async fn test_ffmpeg_success_without_output_file_errors() {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not available");
            return;
        }

        let work_dir = ffmpeg_temp_dir("noout");
        let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let node = ffmpeg_node(
            "enc",
            serde_json::json!({
                "args": ["-f", "lavfi", "-i", "testsrc=duration=1:size=32x32:rate=1", "-frames:v", "1", "-f", "rawvideo", "-y"],
                "output": null_device,
            }),
        );

        let err = execute_builtin_ffmpeg(&node, &[], &work_dir)
            .await
            .expect_err("ffmpeg 成功但无产物时应报错");
        assert!(
            err.to_string().contains("did not produce"),
            "错误应说明产物缺失: {err}"
        );

        cleanup_ffmpeg_dir(&work_dir);
    }

    /// file_input 目标命名回归（P2）：目标路径含节点 id —— 同管线两个
    /// 同名源文件（不同目录）不互相覆盖；源与目标为同一文件时跳过复制。
    #[tokio::test]
    async fn test_file_input_node_id_in_dest_name() {
        let work_dir = std::env::temp_dir().join(format!("ep_fi_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&work_dir).unwrap();
        let dir_a = work_dir.join("a");
        let dir_b = work_dir.join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let src_a = dir_a.join("audio.wav");
        let src_b = dir_b.join("audio.wav");
        std::fs::write(&src_a, b"AAAA").unwrap();
        std::fs::write(&src_b, b"BBBB").unwrap();

        let node = |id: &str, path: &std::path::Path| PipelineNode {
            id: id.to_string(),
            kind: NodeKind::Builtin {
                builtin: "file_input".to_string(),
            },
            label: String::new(),
            params: serde_json::json!({ "path": path.to_string_lossy() }),
            position: None,
            timeout_secs: None,
            retry_count: None,
        };

        let Artifact::File(dest_a) =
            execute_builtin_file_input(&node("left", &src_a), &work_dir).await.unwrap()
        else {
            panic!("expected file artifact");
        };
        let Artifact::File(dest_b) =
            execute_builtin_file_input(&node("right", &src_b), &work_dir).await.unwrap()
        else {
            panic!("expected file artifact");
        };

        assert_ne!(dest_a, dest_b, "同管线同名源文件目标不得互相覆盖");
        assert_eq!(std::fs::read(&dest_a).unwrap(), b"AAAA");
        assert_eq!(std::fs::read(&dest_b).unwrap(), b"BBBB");
        assert!(dest_a.to_string_lossy().contains("left"), "目标名应含节点 id: {dest_a:?}");
        assert!(dest_b.to_string_lossy().contains("right"), "目标名应含节点 id: {dest_b:?}");

        // 源位于工作目录内：复制到带节点 id 前缀的目标（不会原地覆盖源），
        // 内容完整 —— 目标命名含节点 id 后，dest 与源路径不再可能词法相等，
        // 同文件判定退化为防御性分支（源=目标时经 canonicalize 跳过复制）
        let in_work = work_dir.join("input.txt");
        std::fs::write(&in_work, b"in work dir").unwrap();
        let Artifact::File(dest) =
            execute_builtin_file_input(&node("self", &in_work), &work_dir).await.unwrap()
        else {
            panic!("expected file artifact");
        };
        assert_eq!(dest, work_dir.join("self").join("input.txt"), "目标应放入按节点 id 隔离的子目录");
        assert_eq!(std::fs::read(&dest).unwrap(), b"in work dir");
        assert_eq!(std::fs::read(&in_work).unwrap(), b"in work dir", "源文件不受影响");

        let _ = std::fs::remove_dir_all(&work_dir);
    }

    /// {input} 占位符但无上游产物 → 中文报错（不需要 ffmpeg 二进制，不 spawn）
    #[tokio::test]
    async fn test_ffmpeg_placeholder_without_upstream_fails() {
        let work_dir = ffmpeg_temp_dir("noin");

        let node = ffmpeg_node(
            "extract",
            serde_json::json!({
                "args": ["-i", "{input}", "-c", "copy", "{output}"],
            }),
        );

        let err = execute_builtin_ffmpeg(&node, &[], &work_dir)
            .await
            .expect_err("must fail without upstream input");
        let msg = err.to_string();
        assert!(
            msg.contains("upstream") && msg.contains("placeholder"),
            "expected English error: {msg}"
        );

        cleanup_ffmpeg_dir(&work_dir);
    }

    /// 完整管线（audio_extract.toml 同款形状）：
    /// file_input → ffmpeg({input}/{output} 占位符) → file_output
    #[test]
    fn test_pipeline_audio_extract_shape_with_placeholders() {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not available");
            return;
        }

        let work_dir = ffmpeg_temp_dir("pipe");
        // m4a(AAC) 输入 → "-acodec copy" 可无损复制到 m4a 容器
        let source_file = work_dir.join("media.m4a");
        if !generate_audio_file(&source_file, &["-c:a", "aac"]) {
            eprintln!("SKIP: failed to generate test audio");
            cleanup_ffmpeg_dir(&work_dir);
            return;
        }
        let final_output = work_dir.join("final.m4a");

        let toml_str = format!(
            r#"
[pipeline]
id = "audio-extract-shape"
name = "Audio extraction (shipped shape)"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "extract"
kind = "builtin"
builtin = "ffmpeg"

[nodes.params]
args = ["-i", "{{input}}", "-vn", "-acodec", "copy", "{{output}}"]
output_extension = "m4a"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["extract", "input"]

[[edges]]
from = ["extract", "output"]
to = ["output", "input"]
"#,
            source_file.to_string_lossy().replace('\\', "/"),
            final_output.to_string_lossy().replace('\\', "/"),
        );

        let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
        let mut runner = PipelineRunnerImpl::new(work_dir.clone());

        let result = runner.execute(&pipeline, &work_dir);
        assert!(result.is_ok(), "shipped-shape pipeline should succeed: {result:?}");
        assert_eq!(*runner.task_status(), TaskStatus::Completed);

        // ffmpeg 节点中间产物：output_extension 生效
        let mid = work_dir.join("extract_output.m4a");
        assert!(mid.exists(), "ffmpeg node output should be written");

        // file_output 复制产物
        assert!(final_output.exists(), "final output should be written");
        let mid_bytes = std::fs::read(&mid).unwrap();
        let out_bytes = std::fs::read(&final_output).unwrap();
        assert!(!mid_bytes.is_empty());
        assert_eq!(mid_bytes, out_bytes);

        cleanup_ffmpeg_dir(&work_dir);
    }

    // ─── ffmpeg 字符串 args 防御性拆分（P0-2 后端侧） ───────────────────────

    #[test]
    fn test_split_shell_words_rules() {
        // 基本空白分隔
        assert_eq!(
            split_shell_words("-i {input} -c copy {output}"),
            vec!["-i", "{input}", "-c", "copy", "{output}"]
        );
        // 双引号保留空格
        assert_eq!(
            split_shell_words(r#"-metadata title="hello world" -c copy"#),
            vec!["-metadata", "title=hello world", "-c", "copy"]
        );
        // 单引号原样保留
        assert_eq!(
            split_shell_words("-filter 'a, b' -x"),
            vec!["-filter", "a, b", "-x"]
        );
        // 双引号内转义 \" 与 \\
        assert_eq!(
            split_shell_words(r#""say \"hi\" \\ ok""#),
            vec![r#"say "hi" \ ok"#]
        );
        // 引号外反斜杠转义
        assert_eq!(split_shell_words(r"a\ b"), vec!["a b"]);
        // 空引号对 → 空词条；全空白 → 空数组
        assert_eq!(split_shell_words(r#"a "" b"#), vec!["a", "", "b"]);
        assert!(split_shell_words("   \t ").is_empty());
        assert!(split_shell_words("").is_empty());
        // 未闭合引号宽容处理到末尾
        assert_eq!(split_shell_words("-t \"unterminated"), vec!["-t", "unterminated"]);
    }

    /// 字符串形状 args（前端历史形状）与数组形状等价执行（P0-2）
    #[tokio::test]
    async fn test_ffmpeg_string_args_split_e2e() {
        if !ffmpeg_available() {
            eprintln!("SKIP: ffmpeg not available");
            return;
        }

        let work_dir = ffmpeg_temp_dir("strargs");
        let input_file = work_dir.join("in.wav");
        if !generate_audio_file(&input_file, &["-ar", "16000", "-ac", "1"]) {
            eprintln!("SKIP: failed to generate test audio");
            cleanup_ffmpeg_dir(&work_dir);
            return;
        }

        let node = ffmpeg_node(
            "extract",
            serde_json::json!({
                // 字符串形状：含占位符，执行前按 shell 词法拆分为数组
                "args": "-i {input} -vn -acodec pcm_s16le -ar 16000 -ac 1 {output}",
                "output_extension": "wav",
            }),
        );

        let artifact = execute_builtin_ffmpeg(&node, &[Artifact::File(input_file)], &work_dir)
            .await
            .expect("string args should be split and executed like array args");
        let Artifact::File(output) = artifact else {
            panic!("expected file artifact");
        };
        assert_eq!(output, work_dir.join("extract_output.wav"));
        assert!(output.exists());
        assert!(std::fs::metadata(&output).unwrap().len() > 0);

        cleanup_ffmpeg_dir(&work_dir);
    }

    // ─── LLM 节点：纯函数（消息构建 / 参数解析 / 输入提取） ─────────────────

    #[test]
    fn test_build_llm_messages_placeholder_rules() {
        // 含 {input} 占位符 → 替换后作为唯一 user 消息
        let msgs = build_llm_messages(
            Some("请把以下内容翻译成中文，只输出译文：{input}"),
            "hello",
        );
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(
            msgs[0]["content"],
            "请把以下内容翻译成中文，只输出译文：hello"
        );

        // 无占位符 → system + user 两条
        let msgs = build_llm_messages(Some("你是翻译助手"), "你好");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "你是翻译助手");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "你好");

        // 无 system_prompt → 仅 user
        let msgs = build_llm_messages(None, "raw");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "raw");

        // 空白 system_prompt 视同缺省
        let msgs = build_llm_messages(Some("   "), "raw");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn test_parse_llm_request_required_params_and_validation() {
        let empty = serde_json::json!({});

        // 缺 base_url
        let err = parse_llm_request("n", &empty, None, None, "x").unwrap_err();
        assert!(err.to_string().contains("base_url"), "got: {err}");

        // 缺 model
        let err = parse_llm_request(
            "n",
            &serde_json::json!({"base_url": "http://127.0.0.1:9/v1"}),
            None,
            None,
            "x",
        )
        .unwrap_err();
        assert!(err.to_string().contains("'model'"), "got: {err}");

        let base = serde_json::json!({
            "base_url": "http://127.0.0.1:9/v1/",
            "model": "m",
        });

        // URL 拼接 + 尾斜杠修剪
        let p = parse_llm_request("n", &base, None, None, "x").unwrap();
        assert_eq!(p.url, "http://127.0.0.1:9/v1/chat/completions");
        assert_eq!(p.body["model"], "m");
        assert!(p.api_key.is_none());
        assert_eq!(p.output_format, LlmOutputFormat::Text);
        // temperature/max_tokens 缺省不出现在请求体
        assert!(p.body.get("temperature").is_none());
        assert!(p.body.get("max_tokens").is_none());

        // 非法 output_format
        let mut bad = base.clone();
        bad["output_format"] = serde_json::json!("xml");
        let err = parse_llm_request("n", &bad, None, None, "x").unwrap_err();
        assert!(err.to_string().contains("'text' or 'json'"), "got: {err}");

        // temperature 越界 / 非数
        let mut bad = base.clone();
        bad["temperature"] = serde_json::json!(2.5);
        assert!(parse_llm_request("n", &bad, None, None, "x")
            .unwrap_err()
            .to_string()
            .contains("[0.0, 2.0]"));
        let mut bad = base.clone();
        bad["temperature"] = serde_json::json!("abc");
        assert!(parse_llm_request("n", &bad, None, None, "x")
            .unwrap_err()
            .to_string()
            .contains("temperature"));

        // max_tokens = 0 / 负数 / 小数
        let mut bad = base.clone();
        bad["max_tokens"] = serde_json::json!(0);
        assert!(parse_llm_request("n", &bad, None, None, "x")
            .unwrap_err()
            .to_string()
            .contains("positive integer"));
        let mut bad = base.clone();
        bad["max_tokens"] = serde_json::json!(-5);
        assert!(parse_llm_request("n", &bad, None, None, "x").is_err());

        // 合法完整参数 → 请求体形状
        let mut ok = base.clone();
        ok["temperature"] = serde_json::json!(0.3);
        ok["max_tokens"] = serde_json::json!(2048);
        ok["output_format"] = serde_json::json!("json");
        ok["system_prompt"] = serde_json::json!("翻译：{input}");
        let p = parse_llm_request("n", &ok, None, None, "hi").unwrap();
        assert_eq!(p.body["temperature"], 0.3);
        assert_eq!(p.body["max_tokens"], 2048);
        assert_eq!(p.output_format, LlmOutputFormat::Json);
        assert_eq!(p.body["messages"][0]["content"], "翻译：hi");
    }

    #[test]
    fn test_parse_llm_request_api_key_from_env() {
        // 唯一变量名避免并行测试互扰
        let var_set = format!("EP_B7_LLM_KEY_SET_{}", Uuid::new_v4().simple());
        let var_unset = format!("EP_B7_LLM_KEY_UNSET_{}", Uuid::new_v4().simple());
        let var_empty = format!("EP_B7_LLM_KEY_EMPTY_{}", Uuid::new_v4().simple());
        std::env::set_var(&var_set, "sk-test-secret");
        std::env::remove_var(&var_unset);
        std::env::set_var(&var_empty, "   ");

        let base = serde_json::json!({
            "base_url": "http://127.0.0.1:9/v1",
            "model": "m",
        });

        // 已设置 → 读取成功
        let mut params = base.clone();
        params["api_key_env"] = serde_json::json!(var_set);
        let p = parse_llm_request("n", &params, None, None, "x").unwrap();
        assert_eq!(p.api_key.as_deref(), Some("sk-test-secret"));

        // 未设置 → 报错且消息含变量名
        let mut params = base.clone();
        params["api_key_env"] = serde_json::json!(var_unset);
        let err = parse_llm_request("n", &params, None, None, "x").unwrap_err();
        assert!(err.to_string().contains(&var_unset), "got: {err}");

        // 空白值 → 报错
        let mut params = base.clone();
        params["api_key_env"] = serde_json::json!(var_empty);
        assert!(parse_llm_request("n", &params, None, None, "x")
            .unwrap_err()
            .to_string()
            .contains("empty"));

        // kind 级 api_key_env（遗留 external_api 形状）同样生效
        let p = parse_llm_request("n", &base, None, Some(&var_set), "x").unwrap();
        assert_eq!(p.api_key.as_deref(), Some("sk-test-secret"));
        // kind 级 endpoint 作为 base_url 回退来源
        let p = parse_llm_request(
            "n",
            &serde_json::json!({"model": "m"}),
            Some("http://legacy.example/v1"),
            None,
            "x",
        )
        .unwrap();
        assert_eq!(p.url, "http://legacy.example/v1/chat/completions");

        std::env::remove_var(&var_set);
        std::env::remove_var(&var_empty);
    }

    #[test]
    fn test_llm_endpoint_is_local_rules() {
        assert!(llm_endpoint_is_local("http://127.0.0.1:11434/v1"));
        assert!(llm_endpoint_is_local("http://localhost:8080"));
        assert!(llm_endpoint_is_local("http://[::1]:9000"));
        assert!(!llm_endpoint_is_local("https://api.openai.com/v1"));
        assert!(!llm_endpoint_is_local("http://192.168.1.10:11434"));
        assert!(!llm_endpoint_is_local("not a url"));
    }

    #[test]
    fn test_llm_input_text_extraction() {
        // Text / Json 产物
        assert_eq!(
            llm_input_text(&[Artifact::Text("hi".into())], "n").unwrap(),
            "hi"
        );
        assert_eq!(
            llm_input_text(&[Artifact::Json(serde_json::json!("inner"))], "n").unwrap(),
            "inner"
        );
        assert_eq!(
            llm_input_text(&[Artifact::Json(serde_json::json!({"a": 1}))], "n").unwrap(),
            "{\"a\":1}"
        );
        // 无上游 → 空串
        assert_eq!(llm_input_text(&[], "n").unwrap(), "");

        // 文件产物：文本类扩展名读内容
        let dir = std::env::temp_dir().join(format!("ep_llm_in_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let txt = dir.join("a.txt");
        std::fs::write(&txt, "file text").unwrap();
        assert_eq!(
            llm_input_text(&[Artifact::File(txt.clone())], "n").unwrap(),
            "file text"
        );
        // 非文本扩展名 → 报错
        let wav = dir.join("a.wav");
        std::fs::write(&wav, b"binary").unwrap();
        let err = llm_input_text(&[Artifact::File(wav)], "n").unwrap_err();
        assert!(err.to_string().contains("non-text extension"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── LLM 节点：mock HTTP 端点（127.0.0.1 随机端口） ─────────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// mock 端点行为
    enum MockLlmBehavior {
        /// 返回固定状态码 + JSON body
        Respond { status: u16, body: String },
        /// 接受连接并读取请求后不响应（触发客户端超时）
        Hang,
    }

    /// 最小 OpenAI 兼容 mock 服务器：记录收到的原始请求文本
    struct MockLlmServer {
        base_url: String,
        requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    impl MockLlmServer {
        async fn start(behavior: MockLlmBehavior) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind 127.0.0.1 random port");
            let addr = listener.local_addr().unwrap();
            let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let req_clone = requests.clone();
            let behavior = std::sync::Arc::new(behavior);

            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let reqs = req_clone.clone();
                    let beh = behavior.clone();
                    tokio::spawn(async move {
                        let (mut rd, mut wr) = stream.into_split();
                        let mut buf = Vec::new();
                        let mut tmp = [0u8; 4096];
                        // 读完 headers + Content-Length 声明的 body
                        loop {
                            let n = rd.read(&mut tmp).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(pos) = find_header_end(&buf) {
                                let headers = String::from_utf8_lossy(&buf[..pos]);
                                let content_length = headers
                                    .lines()
                                    .find_map(|l| {
                                        let (k, v) = l.split_once(':')?;
                                        k.trim()
                                            .eq_ignore_ascii_case("content-length")
                                            .then(|| v.trim().parse::<usize>().unwrap_or(0))
                                    })
                                    .unwrap_or(0);
                                let body_end = pos + 4 + content_length;
                                while buf.len() < body_end {
                                    let n = rd.read(&mut tmp).await.unwrap_or(0);
                                    if n == 0 {
                                        break;
                                    }
                                    buf.extend_from_slice(&tmp[..n]);
                                }
                                break;
                            }
                        }
                        reqs.lock().unwrap().push(String::from_utf8_lossy(&buf).into_owned());

                        match beh.as_ref() {
                            MockLlmBehavior::Respond { status, body } => {
                                let reason = if (200..300).contains(status) { "OK" } else { "Error" };
                                let resp = format!(
                                    "HTTP/1.1 {status} {reason}\r\n\
                                     Content-Type: application/json\r\n\
                                     Content-Length: {}\r\n\
                                     Connection: close\r\n\r\n{body}",
                                    body.len()
                                );
                                let _ = wr.write_all(resp.as_bytes()).await;
                            }
                            MockLlmBehavior::Hang => {
                                // 保持连接但永不响应，直到客户端超时
                                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                            }
                        }
                    });
                }
            });

            Self {
                base_url: format!("http://127.0.0.1:{}", addr.port()),
                requests,
            }
        }

        fn captured(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn llm_node_with(builtin: &str, params: serde_json::Value) -> PipelineNode {
        PipelineNode {
            id: "translate".to_string(),
            kind: NodeKind::Builtin {
                builtin: builtin.to_string(),
            },
            label: String::new(),
            params,
            position: None,
            timeout_secs: None,
            retry_count: None,
        }
    }

    fn llm_chat_response(content: &str) -> String {
        serde_json::json!({
            "id": "mock-1",
            "choices": [{ "message": { "role": "assistant", "content": content } }],
        })
        .to_string()
    }

    #[tokio::test]
    async fn test_llm_success_text_output_and_request_shape() {
        let key_var = format!("EP_B7_LLM_AUTH_{}", Uuid::new_v4().simple());
        std::env::set_var(&key_var, "sk-mock-secret");

        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: llm_chat_response("你好，世界"),
        })
        .await;

        let node = llm_node_with(
            "llm",
            serde_json::json!({
                "base_url": server.base_url,
                "model": "qwen2.5-7b-instruct",
                "api_key_env": key_var,
                "system_prompt": "把 {input} 翻译成中文",
                "temperature": 0.3,
                "max_tokens": 1024,
            }),
        );

        let artifact = execute_llm_node(&node, &[Artifact::Text("hello world".into())], None, None)
            .await
            .expect("llm call should succeed");
        assert_eq!(artifact, Artifact::Text("你好，世界".into()));

        // 请求形状断言：URL 路径 / Authorization / body 字段
        let reqs = server.captured();
        assert_eq!(reqs.len(), 1);
        let req = &reqs[0];
        assert!(req.starts_with("POST /chat/completions"), "got: {req}");
        assert!(req.contains("authorization: Bearer sk-mock-secret"), "got: {req}");
        let body = req.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(body).expect("request body is JSON");
        assert_eq!(v["model"], "qwen2.5-7b-instruct");
        assert_eq!(v["temperature"], 0.3);
        assert_eq!(v["max_tokens"], 1024);
        // {input} 占位符已替换为上游文本，且只有一条 user 消息
        assert_eq!(v["messages"].as_array().unwrap().len(), 1);
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], "把 hello world 翻译成中文");

        std::env::remove_var(&key_var);
    }

    #[tokio::test]
    async fn test_llm_json_output_format_validation() {
        // 合法 JSON → Artifact::Json
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: llm_chat_response(r#"{"translation": "你好"}"#),
        })
        .await;
        let node = llm_node_with(
            "llm",
            serde_json::json!({
                "base_url": server.base_url,
                "model": "m",
                "output_format": "json",
            }),
        );
        let artifact = execute_llm_node(&node, &[Artifact::Text("hi".into())], None, None)
            .await
            .unwrap();
        assert_eq!(
            artifact,
            Artifact::Json(serde_json::json!({"translation": "你好"}))
        );

        // 非法 JSON → 报错（不可重试）
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: llm_chat_response("not json at all"),
        })
        .await;
        let node = llm_node_with(
            "llm",
            serde_json::json!({
                "base_url": server.base_url,
                "model": "m",
                "output_format": "json",
            }),
        );
        let err = execute_llm_node(&node, &[Artifact::Text("hi".into())], None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
    }

    #[tokio::test]
    async fn test_llm_http_error_status_not_retryable() {
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 401,
            body: r#"{"error": "invalid api key"}"#.to_string(),
        })
        .await;
        let node = llm_node_with(
            "llm",
            serde_json::json!({ "base_url": server.base_url, "model": "m" }),
        );
        let err = execute_llm_node(&node, &[Artifact::Text("hi".into())], None, None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("401"), "error should carry status code: {msg}");
        // 非 2xx 不重试：只收到 1 次请求
        assert_eq!(server.captured().len(), 1);
        // 错误不可重试
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(!mce.retryable);
    }

    #[tokio::test]
    async fn test_llm_timeout_is_retryable_error() {
        let server = MockLlmServer::start(MockLlmBehavior::Hang).await;
        let node = PipelineNode {
            id: "translate".to_string(),
            kind: NodeKind::Builtin {
                builtin: "llm".to_string(),
            },
            label: String::new(),
            params: serde_json::json!({ "base_url": server.base_url, "model": "m" }),
            position: None,
            // 节点级超时 1s + retry_count=0（单尝试，避免重试间隔拉长测试）
            timeout_secs: Some(1),
            retry_count: Some(0),
        };
        let err = execute_llm_node(&node, &[Artifact::Text("hi".into())], None, None)
            .await
            .unwrap_err();
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(mce.retryable, "timeout must be retryable: {mce}");
        assert!(mce.message.contains("timed out"), "got: {}", mce.message);
    }

    #[tokio::test]
    async fn test_llm_alias_external_api_builtin_equivalent() {
        // builtin = "external_api" 与 "llm" 执行完全等价（§6.7 别名）
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: llm_chat_response("via alias"),
        })
        .await;
        let node = llm_node_with(
            "external_api",
            serde_json::json!({ "base_url": server.base_url, "model": "m" }),
        );
        let artifact = execute_llm_node(&node, &[Artifact::Text("hi".into())], None, None)
            .await
            .unwrap();
        assert_eq!(artifact, Artifact::Text("via alias".into()));
    }

    #[tokio::test]
    async fn test_llm_legacy_kind_external_api_fields() {
        // 遗留 kind 级 endpoint/api_key_env 作为参数来源（base_url 走 endpoint）
        let key_var = format!("EP_B7_LLM_LEGACY_{}", Uuid::new_v4().simple());
        std::env::set_var(&key_var, "sk-legacy");

        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: llm_chat_response("legacy ok"),
        })
        .await;

        let node = llm_node_with("llm", serde_json::json!({ "model": "m" }));
        let artifact = execute_llm_node(
            &node,
            &[Artifact::Text("hi".into())],
            Some(&server.base_url),
            Some(&key_var),
        )
        .await
        .expect("legacy kind-level fields should drive the call");
        assert_eq!(artifact, Artifact::Text("legacy ok".into()));

        let req = server.captured().pop().unwrap();
        assert!(req.contains("authorization: Bearer sk-legacy"), "got: {req}");

        std::env::remove_var(&key_var);
    }

    #[tokio::test]
    async fn test_llm_missing_api_key_env_fails_without_http() {
        // env 缺失 → 本地即失败，不产生任何 HTTP 请求
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: llm_chat_response("unused"),
        })
        .await;
        let node = llm_node_with(
            "llm",
            serde_json::json!({
                "base_url": server.base_url,
                "model": "m",
                "api_key_env": format!("EP_B7_UNSET_{}", Uuid::new_v4().simple()),
            }),
        );
        let err = execute_llm_node(&node, &[Artifact::Text("hi".into())], None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not set"), "got: {err}");
        assert!(server.captured().is_empty(), "no HTTP request may be sent");
    }

    // ─── 模块节点：HTTP 推理路径（GAP-1 G1 补测） ───────────────────────────
    // 复用上方 MockLlmServer —— 它实际是通用的"记录原始请求 + 固定响应/Hang"
    // mock HTTP 端点；模块协议契约通过断言其捕获的原始请求锁住。

    /// 模块节点测试夹具（节点 id 取 capability，与 output_path 派生口径一致）
    fn module_node_with(
        module_id: &str,
        capability: &str,
        params: serde_json::Value,
    ) -> PipelineNode {
        PipelineNode {
            id: capability.to_string(),
            kind: NodeKind::Module {
                module_id: module_id.to_string(),
                capability: capability.to_string(),
                model_id: None,
                device: None,
            },
            label: String::new(),
            params,
            position: None,
            timeout_secs: None,
            retry_count: None,
        }
    }

    /// mock 服务器端口（base_url 形状固定为 http://127.0.0.1:{port}）
    fn mock_server_port(server: &MockLlmServer) -> u16 {
        server.base_url.rsplit(':').next().unwrap().parse().unwrap()
    }

    /// 单模块端口注册表
    fn single_port_map(module_id: &str, server: &MockLlmServer) -> HashMap<String, u16> {
        let mut m = HashMap::new();
        m.insert(module_id.to_string(), mock_server_port(server));
        m
    }

    /// 模块成功响应体（锁定 ModuleResponse 反序列化形状）
    fn module_ok_response(output_type: Option<&str>, result: serde_json::Value) -> String {
        serde_json::json!({
            "status": "completed",
            "output_type": output_type,
            "result": result,
        })
        .to_string()
    }

    /// 从原始请求头提取 multipart boundary
    fn multipart_boundary(raw: &str) -> String {
        raw.lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("content-type")
                    .then(|| v.to_string())
            })
            .expect("multipart request must carry Content-Type header")
            .split(';')
            .find_map(|seg| seg.trim().strip_prefix("boundary=").map(str::to_string))
            .expect("Content-Type must carry boundary parameter")
    }

    /// 解析 multipart body 为 (字段名, 文件名, 内容) 列表
    /// （测试夹具文件使用纯 ASCII 内容，保证 lossy 捕获下可无损比较）
    fn multipart_fields(raw: &str) -> Vec<(String, Option<String>, String)> {
        let boundary = multipart_boundary(raw);
        let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
        let mut fields = Vec::new();
        for seg in body.split(&format!("--{boundary}")) {
            let seg = seg.trim_start_matches("\r\n");
            // 首段为空（前导 --boundary），末段为 "--" 收尾标记
            if seg.is_empty() || seg.starts_with("--") {
                continue;
            }
            let Some((head, content)) = seg.split_once("\r\n\r\n") else {
                continue;
            };
            let content = content.strip_suffix("\r\n").unwrap_or(content);
            let disp = head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("content-disposition"))
                .unwrap_or_default();
            let name = disp
                .split("name=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or_default()
                .to_string();
            let filename = disp
                .split("filename=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .map(str::to_string);
            fields.push((name, filename, content.to_string()));
        }
        fields
    }

    /// 文本输入 → JSON body 形状正确；响应 output_type="text" → Artifact::Text
    #[tokio::test]
    async fn test_module_node_text_input_json_request_shape() {
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(Some("text"), serde_json::json!("你好，世界")),
        })
        .await;

        let node = module_node_with(
            "faster-whisper",
            "transcribe",
            serde_json::json!({ "language": "zh", "beam_size": 5 }),
        );
        let work_dir = std::env::temp_dir().join(format!("ep_mod_json_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&work_dir).unwrap();

        let artifact = execute_module_node(
            &node,
            &[Artifact::Text("hello world".into())],
            &work_dir,
            &single_port_map("faster-whisper", &server),
        )
        .await
        .expect("module JSON call should succeed");
        assert_eq!(artifact, Artifact::Text("你好，世界".into()));

        // 请求形状断言：URL 路径 = /predict/{capability}；body = {params, input}
        let reqs = server.captured();
        assert_eq!(reqs.len(), 1);
        let req = &reqs[0];
        assert!(req.starts_with("POST /predict/transcribe"), "got: {req}");
        let body = req.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(body).expect("request body must be JSON");
        assert_eq!(v["input"], "hello world");
        assert_eq!(v["params"]["language"], "zh");
        assert_eq!(v["params"]["beam_size"], 5);

        let _ = std::fs::remove_dir_all(&work_dir);
    }

    /// 响应 JSON 输出解析：output_type = "json" / 缺省 / 未知 均映射 Artifact::Json；
    /// 上游 Json 产物原样进入请求体 input
    #[tokio::test]
    async fn test_module_node_json_output_type_mapping() {
        let work_dir = std::env::temp_dir().join(format!("ep_mod_jmap_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&work_dir).unwrap();
        let node = module_node_with("m1", "cap", serde_json::json!({}));

        // output_type = "json"
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(Some("json"), serde_json::json!({"segments": ["a", "b"]})),
        })
        .await;
        let artifact = execute_module_node(
            &node,
            &[Artifact::Json(serde_json::json!({"text": "hi"}))],
            &work_dir,
            &single_port_map("m1", &server),
        )
        .await
        .unwrap();
        assert_eq!(
            artifact,
            Artifact::Json(serde_json::json!({"segments": ["a", "b"]}))
        );
        let raw = server.captured().pop().unwrap();
        let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["input"], serde_json::json!({"text": "hi"}));

        // output_type 缺省 → 按 Json
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: serde_json::json!({"status": "completed", "result": {"ok": true}}).to_string(),
        })
        .await;
        let artifact = execute_module_node(
            &node,
            &[],
            &work_dir,
            &single_port_map("m1", &server),
        )
        .await
        .unwrap();
        assert_eq!(artifact, Artifact::Json(serde_json::json!({"ok": true})));

        // 未知 output_type → 回退 Json（不按未知类型失败）
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(Some("weird"), serde_json::json!([1, 2])),
        })
        .await;
        let artifact = execute_module_node(
            &node,
            &[],
            &work_dir,
            &single_port_map("m1", &server),
        )
        .await
        .unwrap();
        assert_eq!(artifact, Artifact::Json(serde_json::json!([1, 2])));

        let _ = std::fs::remove_dir_all(&work_dir);
    }

    /// 文件输入 → multipart 投递契约：字段名 "file" + filename 透传 + 内容原样；
    /// params 字段附加节点参数 JSON（含 output_path 注入）；
    /// 响应 output_type="file" → Artifact::File（模块返回路径，G1-b/d）
    #[tokio::test]
    async fn test_module_node_file_input_multipart_contract() {
        let work_dir = std::env::temp_dir().join(format!("ep_mod_mp_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&work_dir).unwrap();
        let input_file = work_dir.join("audio.wav");
        std::fs::write(&input_file, b"RIFF mock wav payload").unwrap();

        let expected_out = work_dir.join("transcribe_output.srt");
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(
                Some("file"),
                serde_json::json!(expected_out.to_string_lossy().to_string()),
            ),
        })
        .await;

        let node = module_node_with(
            "faster-whisper",
            "transcribe",
            serde_json::json!({ "output_format": "srt", "language": "zh" }),
        );
        let artifact = execute_module_node(
            &node,
            &[Artifact::File(input_file)],
            &work_dir,
            &single_port_map("faster-whisper", &server),
        )
        .await
        .expect("multipart module call should succeed");

        // 产物契约：output_type="file" → Artifact::File(模块返回的路径)
        assert_eq!(artifact, Artifact::File(expected_out.clone()));

        let req = server.captured().pop().unwrap();
        assert!(req.starts_with("POST /predict/transcribe"), "got: {req}");
        let head = req.split_once("\r\n\r\n").map(|(h, _)| h).unwrap_or_default();
        assert!(
            head.to_ascii_lowercase().contains("multipart/form-data"),
            "file input must use multipart: {head}"
        );

        let fields = multipart_fields(&req);
        // 文件字段：name="file"、filename 透传上游文件名、字节内容原样
        let file_field = fields
            .iter()
            .find(|(n, _, _)| n == "file")
            .expect("multipart must carry a 'file' part");
        assert_eq!(file_field.1.as_deref(), Some("audio.wav"));
        assert_eq!(file_field.2, "RIFF mock wav payload");
        // params 字段：节点参数 JSON 化，且注入 output_path 指向任务工作目录
        let params_field = fields
            .iter()
            .find(|(n, _, _)| n == "params")
            .expect("multipart must carry a 'params' part");
        let params: serde_json::Value = serde_json::from_str(&params_field.2)
            .expect("params part must be JSON");
        assert_eq!(params["language"], "zh");
        assert_eq!(params["output_format"], "srt");
        assert_eq!(
            params["output_path"],
            serde_json::json!(expected_out.to_string_lossy().to_string()),
            "output_path must be injected into task work_dir"
        );

        let _ = std::fs::remove_dir_all(&work_dir);
    }

    /// 大文件 multipart 上传回归（P1）：Part::stream_with_length 流式上传，
    /// 约 4 MiB 文件内容应完整送达（替代旧实现的 tokio::fs::read 全量读入）。
    #[tokio::test]
    async fn test_module_node_file_input_multipart_streams_large_file() {
        let work_dir = std::env::temp_dir().join(format!("ep_mod_mp_big_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&work_dir).unwrap();
        // 可打印 ASCII 内容（32..=126），保证 mock 的 lossy 捕获无损比较
        let payload: Vec<u8> = (0..4 * 1024 * 1024)
            .map(|i| 32 + ((i * 7) % 95) as u8)
            .collect();
        let input_file = work_dir.join("big.bin");
        std::fs::write(&input_file, &payload).unwrap();

        let expected_out = work_dir.join("cap_output.bin");
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(
                Some("file"),
                serde_json::json!(expected_out.to_string_lossy().to_string()),
            ),
        })
        .await;

        let node = module_node_with("m1", "cap", serde_json::json!({ "output_format": "bin" }));
        let artifact = execute_module_node(
            &node,
            &[Artifact::File(input_file)],
            &work_dir,
            &single_port_map("m1", &server),
        )
        .await
        .expect("large multipart upload should succeed");
        assert_eq!(artifact, Artifact::File(expected_out));

        // 原始请求完整捕获 → 校验 file part 内容未截断/未损坏
        let req = server.captured().pop().unwrap();
        let fields = multipart_fields(&req);
        let file_field = fields
            .iter()
            .find(|(n, _, _)| n == "file")
            .expect("multipart must carry a 'file' part");
        assert_eq!(file_field.1.as_deref(), Some("big.bin"));
        assert_eq!(
            file_field.2.as_bytes(),
            payload.as_slice(),
            "大文件 multipart 内容应完整无损"
        );

        let _ = std::fs::remove_dir_all(&work_dir);
    }

    /// output_format 声明 → output_path 注入 params（JSON body 路径）；
    /// output_format="json" 不注入
    #[tokio::test]
    async fn test_module_node_output_path_injection_json_body() {
        let work_dir = std::env::temp_dir().join(format!("ep_mod_op_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&work_dir).unwrap();

        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(Some("text"), serde_json::json!("ok")),
        })
        .await;
        let node = module_node_with("m1", "cap", serde_json::json!({"output_format": "srt"}));
        execute_module_node(
            &node,
            &[Artifact::Text("hi".into())],
            &work_dir,
            &single_port_map("m1", &server),
        )
        .await
        .unwrap();
        let raw = server.captured().pop().unwrap();
        let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            v["params"]["output_path"],
            serde_json::json!(work_dir.join("cap_output.srt").to_string_lossy().to_string()),
            "output_path must be derived from node id and point into work_dir"
        );

        // output_format="json" → 不注入 output_path
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(Some("text"), serde_json::json!("ok")),
        })
        .await;
        let node = module_node_with("m1", "cap", serde_json::json!({"output_format": "json"}));
        execute_module_node(
            &node,
            &[Artifact::Text("hi".into())],
            &work_dir,
            &single_port_map("m1", &server),
        )
        .await
        .unwrap();
        let raw = server.captured().pop().unwrap();
        let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(
            v["params"].get("output_path").is_none(),
            "output_format=json must not inject output_path"
        );

        let _ = std::fs::remove_dir_all(&work_dir);
    }

    /// 模块返回 500 → 节点失败，错误信息保留状态码与响应体，且不重试（G1-f）
    #[tokio::test]
    async fn test_module_node_http_500_fails_and_error_preserved() {
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 500,
            body: r#"{"error": "model exploded"}"#.to_string(),
        })
        .await;
        let node = module_node_with("m1", "cap", serde_json::json!({}));

        let err = execute_module_node(
            &node,
            &[Artifact::Text("hi".into())],
            std::path::Path::new("."),
            &single_port_map("m1", &server),
        )
        .await
        .unwrap_err();
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(!mce.retryable, "HTTP status errors must not be retryable");
        assert!(mce.message.contains("500"), "status code preserved: {}", mce.message);
        assert!(
            mce.message.contains("model exploded"),
            "response body preserved: {}",
            mce.message
        );
        assert_eq!(server.captured().len(), 1, "non-retryable error must not retry");
    }

    /// HTTP 4xx → 不可重试：即使配置了 retry_count 也短路（G1-e 分类边界）
    #[tokio::test]
    async fn test_module_node_http_404_not_retried_even_with_retry_budget() {
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 404,
            body: r#"{"error": "no such capability"}"#.to_string(),
        })
        .await;
        let mut node = module_node_with("m1", "cap", serde_json::json!({}));
        node.retry_count = Some(3);

        let err = execute_module_node(
            &node,
            &[Artifact::Text("hi".into())],
            std::path::Path::new("."),
            &single_port_map("m1", &server),
        )
        .await
        .unwrap_err();
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(!mce.retryable);
        assert!(mce.message.contains("404"), "got: {}", mce.message);
        assert_eq!(
            server.captured().len(),
            1,
            "4xx must short-circuit regardless of retry_count"
        );
    }

    /// 模块返回非 completed 状态 → 失败且错误携带原状态值，不重试
    #[tokio::test]
    async fn test_module_node_non_completed_status_fails() {
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: serde_json::json!({"status": "failed", "result": null}).to_string(),
        })
        .await;
        let node = module_node_with("m1", "cap", serde_json::json!({}));

        let err = execute_module_node(
            &node,
            &[],
            std::path::Path::new("."),
            &single_port_map("m1", &server),
        )
        .await
        .unwrap_err();
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(!mce.retryable);
        assert!(
            mce.message.contains("non-completed status") && mce.message.contains("'failed'"),
            "got: {}",
            mce.message
        );
        assert_eq!(server.captured().len(), 1);
    }

    /// 响应体非 JSON → 解析失败（不可重试、不重试）
    #[tokio::test]
    async fn test_module_node_invalid_json_response_fails() {
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: "this is not json".to_string(),
        })
        .await;
        let node = module_node_with("m1", "cap", serde_json::json!({}));

        let err = execute_module_node(
            &node,
            &[],
            std::path::Path::new("."),
            &single_port_map("m1", &server),
        )
        .await
        .unwrap_err();
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(!mce.retryable);
        assert!(
            mce.message.contains("failed to parse response JSON"),
            "got: {}",
            mce.message
        );
        assert_eq!(server.captured().len(), 1);
    }

    /// 连接拒绝 → 可重试错误（G1-e）
    #[tokio::test]
    async fn test_module_node_connect_refused_is_retryable() {
        // 取确定空闲的端口后立即释放监听 → 连接必被拒绝
        let closed_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut node = module_node_with("m1", "cap", serde_json::json!({}));
        // 单尝试，避免重试间隔拉长测试
        node.retry_count = Some(0);

        let mut module_ports = HashMap::new();
        module_ports.insert("m1".to_string(), closed_port);

        let err = execute_module_node(&node, &[], std::path::Path::new("."), &module_ports)
            .await
            .unwrap_err();
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(mce.retryable, "connection refused must be retryable: {mce}");
        assert!(
            mce.message.contains("connection failed"),
            "got: {}",
            mce.message
        );
    }

    /// 超时 → 可重试；缺省重试预算 1 次（共两次尝试）后耗尽（G1-e）
    #[tokio::test]
    async fn test_module_node_timeout_retried_then_exhausted() {
        let server = MockLlmServer::start(MockLlmBehavior::Hang).await;
        let mut node = module_node_with("m1", "cap", serde_json::json!({}));
        node.timeout_secs = Some(1);
        // retry_count 缺省 → MAX_RETRIES=1

        let err = execute_module_node(
            &node,
            &[Artifact::Text("hi".into())],
            std::path::Path::new("."),
            &single_port_map("m1", &server),
        )
        .await
        .unwrap_err();
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(mce.retryable, "timeout must be retryable: {mce}");
        assert!(mce.message.contains("timed out"), "got: {}", mce.message);
        assert_eq!(
            server.captured().len(),
            2,
            "default retry budget means two attempts"
        );
    }

    /// 端口解析：注册表与 params 均缺失 → 本地失败不发 HTTP；
    /// params.port 回退生效；注册表优先于 params.port
    #[tokio::test]
    async fn test_module_node_port_resolution_registry_params_and_missing() {
        // 1) 无注册表条目且 params 无 port → 本地即失败，不可重试
        let node = module_node_with("ghost", "cap", serde_json::json!({}));
        let err = execute_module_node(&node, &[], std::path::Path::new("."), &HashMap::new())
            .await
            .unwrap_err();
        let mce = err.downcast_ref::<ModuleCallError>().expect("ModuleCallError");
        assert!(!mce.retryable);
        assert!(
            mce.message.contains("no port registered"),
            "got: {}",
            mce.message
        );

        // 2) params.port 回退生效
        let server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(Some("text"), serde_json::json!("via params port")),
        })
        .await;
        let node = module_node_with(
            "m1",
            "cap",
            serde_json::json!({ "port": mock_server_port(&server) }),
        );
        let artifact = execute_module_node(&node, &[], std::path::Path::new("."), &HashMap::new())
            .await
            .unwrap();
        assert_eq!(artifact, Artifact::Text("via params port".into()));

        // 3) 注册表优先于 params.port
        let registry_server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(Some("text"), serde_json::json!("from registry")),
        })
        .await;
        let decoy_server = MockLlmServer::start(MockLlmBehavior::Respond {
            status: 200,
            body: module_ok_response(Some("text"), serde_json::json!("from params")),
        })
        .await;
        let mut node = module_node_with(
            "m1",
            "cap",
            serde_json::json!({ "port": mock_server_port(&decoy_server) }),
        );
        node.id = "prio".to_string();
        let artifact = execute_module_node(
            &node,
            &[],
            std::path::Path::new("."),
            &single_port_map("m1", &registry_server),
        )
        .await
        .unwrap();
        assert_eq!(artifact, Artifact::Text("from registry".into()));
        assert_eq!(registry_server.captured().len(), 1);
        assert!(
            decoy_server.captured().is_empty(),
            "params.port must be ignored when registry has the module"
        );
    }
}

// ─── 节点执行辅助函数 ────────────────────────────────────────────────────────

/// 收集指定节点的所有上游产物（按边在 pipeline.edges 中的出现顺序）
pub(crate) fn collect_upstream_artifacts(
    node_id: &str,
    pipeline: &Pipeline,
    task: &PipelineTask,
) -> Vec<Artifact> {
    pipeline
        .edges
        .iter()
        .filter(|e| e.to.0 == node_id)
        .filter_map(|e| {
            if let Some(NodeState::Completed { artifact: Some(a) }) = task.node_state(&e.from.0) {
                Some(a.clone())
            } else {
                None
            }
        })
        .collect()
}

/// 从上游产物中提取第一个文件路径
#[allow(dead_code)]
pub(crate) fn first_upstream_file_path(
    node_id: &str,
    pipeline: &Pipeline,
    task: &PipelineTask,
) -> Option<PathBuf> {
    collect_upstream_artifacts(node_id, pipeline, task)
        .into_iter()
        .find_map(|a| match a {
            Artifact::File(p) => Some(p),
            _ => None,
        })
}

/// 执行单个节点 — 根据 NodeKind 分派到具体执行逻辑
pub(crate) async fn execute_node(
    node: &PipelineNode,
    pipeline: &Pipeline,
    task: &PipelineTask,
    work_dir: &Path,
    module_ports: &HashMap<String, u16>,
) -> anyhow::Result<Artifact> {
    let upstream = collect_upstream_artifacts(&node.id, pipeline, task);

    match &node.kind {
        NodeKind::Builtin { builtin } => {
            execute_builtin_node(builtin, node, &upstream, work_dir).await
        }
        NodeKind::Module { .. } => execute_module_node(node, &upstream, work_dir, module_ports).await,
        // 遗留 `kind = "external_api" | "llm"` 节点：统一走 llm 执行路径（§6.7）。
        // kind 级 endpoint/api_key_env 作为 base_url/api_key_env 的来源之一，
        // 与 params 中的同名字段合并（kind 级非空值优先）。
        NodeKind::ExternalApi {
            endpoint,
            api_key_env,
        } => {
            let endpoint = (!endpoint.is_empty()).then_some(endpoint.as_str());
            execute_llm_node(node, &upstream, endpoint, api_key_env.as_deref()).await
        }
    }
}

/// 执行内置节点
async fn execute_builtin_node(
    builtin: &str,
    node: &PipelineNode,
    upstream: &[Artifact],
    work_dir: &Path,
) -> anyhow::Result<Artifact> {
    match builtin {
        "file_input" => execute_builtin_file_input(node, work_dir).await,
        "file_output" => execute_builtin_file_output(node, upstream, work_dir).await,
        "ffmpeg" => execute_builtin_ffmpeg(node, upstream, work_dir).await,
        // §6.7 LLM builtin：规范名 `llm`；`external_api` 保留为别名，
        // 两名执行完全等价（builtin 形状无 kind 级字段，参数全部来自 params）
        "llm" | "external_api" => execute_llm_node(node, upstream, None, None).await,
        other => Err(anyhow::anyhow!("unknown builtin node type: {other}")),
    }
}

/// FileInput: 验证源文件存在，复制到工作目录，返回 Artifact::File
async fn execute_builtin_file_input(
    node: &PipelineNode,
    work_dir: &Path,
) -> anyhow::Result<Artifact> {
    let source = node
        .params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("file_input node '{}' missing 'path' param", node.id))?;

    let source_path = Path::new(source);
    if !source_path.exists() {
        return Err(anyhow::anyhow!(
            "input file not found: {}",
            source_path.display()
        ));
    }

    let file_name = source_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("input"));
    // P2：目标放入按节点 id 隔离的子目录 —— 同管线多个同名源文件（不同目录）
    // 不再互相覆盖，且产物文件名保持源文件名（`{node_id}_` 前缀命名会改变
    // 产物名，破坏 daemon 任务产物契约）
    let dest_dir = work_dir.join(&node.id);
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(file_name);

    // P2：源与目标是同一文件时跳过复制（避免 Windows 文件锁冲突）。
    // 用 canonicalize 绝对化后比较而非词法比较——相对/绝对路径混用时
    // 词法比较不可靠；dest 尚不存在时 canonicalize 失败即视为不同文件。
    let same_file = source_path
        .canonicalize()
        .ok()
        .zip(dest.canonicalize().ok())
        .is_some_and(|(src, dst)| src == dst);
    if !same_file {
        std::fs::copy(source_path, &dest)?;
    }

    Ok(Artifact::File(dest))
}

/// FileOutput: 从上游获取文件，复制到目标路径，返回 Artifact::File
///
/// 目标路径解析：优先 `path` 参数；缺省时按 `extension` 参数派生
/// `<work_dir>/<node_id>_output.<ext>`（产物归集会把它收进任务产物）。
async fn execute_builtin_file_output(
    node: &PipelineNode,
    upstream: &[Artifact],
    work_dir: &Path,
) -> anyhow::Result<Artifact> {
    let source_file = upstream
        .iter()
        .find_map(|a| match a {
            Artifact::File(p) => Some(p.clone()),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("file_output node '{}' has no file input from upstream", node.id))?;

    let dest_path: PathBuf = match node
        .params
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(dest) => PathBuf::from(dest),
        None => {
            let ext = node
                .params
                .get("extension")
                .and_then(|v| v.as_str())
                .unwrap_or("out");
            let safe: String = ext.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            work_dir.join(format!("{}_output.{safe}", node.id))
        }
    };

    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&source_file, &dest_path)?;

    Ok(Artifact::File(dest_path))
}

/// 解析 ffmpeg 可执行文件路径
///
/// 搜索优先级：
/// 1. `runtime/bin/ffmpeg`（项目内置 portable 版本）
/// 2. 系统 PATH 中的 `ffmpeg`（用户环境变量）
/// 3. `modules/test-ffmpeg/ffmpeg`（fallback，不入 git/发包）
pub(crate) fn resolve_ffmpeg_path() -> PathBuf {
    let ffmpeg_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

    // 1. runtime/bin/ffmpeg
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        while let Some(d) = dir {
            let candidate = d.join("runtime").join("bin").join(ffmpeg_name);
            if candidate.exists() {
                return candidate;
            }
            dir = d.parent();
        }
    }

    // 2. 系统 PATH
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(which_cmd).arg("ffmpeg").output() {
        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                let p = PathBuf::from(line.trim());
                if p.is_file() {
                    return p;
                }
            }
        }
    }

    // 3. modules/test-ffmpeg/ffmpeg (fallback)
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        while let Some(d) = dir {
            let candidate = d.join("modules").join("test-ffmpeg").join(ffmpeg_name);
            if candidate.exists() {
                return candidate;
            }
            dir = d.parent();
        }
    }

    // 最终回退：裸名，让 OS 报错
    PathBuf::from("ffmpeg")
}

/// args 占位符：上游输入文件路径
pub(crate) const INPUT_PLACEHOLDER: &str = "{input}";
/// args 占位符：本节点输出文件路径
pub(crate) const OUTPUT_PLACEHOLDER: &str = "{output}";

/// 替换 ffmpeg 节点 args 中的 `{input}` / `{output}` 占位符（纯函数，可单测）。
///
/// 规则：
/// - `{input}`  → 上游输入文件路径；args 含 `{input}` 但无可用上游文件时返回中文错误
/// - `{output}` → 本节点解析出的输出文件路径
///
/// 返回 `(替换后的 args, {output} 是否被替换过)`。
/// 当 `{output}` 被替换过时，调用方**不得**再追加额外输出参数，否则会形成双输出命令。
pub(crate) fn substitute_ffmpeg_placeholders(
    args: &[String],
    node_id: &str,
    input_file: Option<&Path>,
    output_path: &Path,
) -> anyhow::Result<(Vec<String>, bool)> {
    let mut result = Vec::with_capacity(args.len());
    let mut output_substituted = false;

    for arg in args {
        let mut replaced = arg.clone();

        if replaced.contains(INPUT_PLACEHOLDER) {
            let input = input_file.ok_or_else(|| {
                anyhow::anyhow!(
                    "ffmpeg node '{node_id}' has the {INPUT_PLACEHOLDER} placeholder in its args, \
                     but the node has no usable upstream input file; \
                     ensure the upstream node is connected and has succeeded"
                )
            })?;
            replaced = replaced.replace(INPUT_PLACEHOLDER, &input.to_string_lossy());
        }

        if replaced.contains(OUTPUT_PLACEHOLDER) {
            replaced = replaced.replace(OUTPUT_PLACEHOLDER, &output_path.to_string_lossy());
            output_substituted = true;
        }

        result.push(replaced);
    }

    Ok((result, output_substituted))
}

/// 解析 ffmpeg 节点的输出文件路径
///
/// 优先级：
/// 1. `output` 参数（显式指定的完整路径，原样使用）
/// 2. `work_dir/{node_id}_output`；若声明了 `output_extension` 参数则追加对应扩展名
///    （ffmpeg 依扩展名推断输出容器格式，无扩展名会报
///    "Unable to find a suitable output format"；shipped 管线均依赖此参数）
fn resolve_ffmpeg_output_path(node: &PipelineNode, work_dir: &Path) -> PathBuf {
    if let Some(output) = node.params.get("output").and_then(|v| v.as_str()) {
        return PathBuf::from(output);
    }

    let mut name = format!("{}_output", node.id);
    if let Some(ext) = node
        .params
        .get("output_extension")
        .and_then(|v| v.as_str())
    {
        let ext = ext.trim().trim_start_matches('.');
        if !ext.is_empty() {
            name.push('.');
            name.push_str(ext);
        }
    }
    work_dir.join(name)
}

/// 按 shell 词法将字符串拆分为参数数组（ffmpeg `args` 字符串形状的防御性兼容，P0-2）。
///
/// POSIX 风格规则（无平台分支）：
/// - 空白（空格/制表符/换行）分隔词条
/// - 单引号内原样保留（引号本身移除）
/// - 双引号内 `\` 仅转义 `\` 与 `"`，其余字符原样
/// - 引号外 `\` 转义下一字符
/// - 未闭合引号按到输入末尾处理（宽容，不报错）；空引号对产生空词条
pub(crate) fn split_shell_words(input: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut has_token = false; // 区分"空词条"（""）与"无词条"
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            _ if c.is_whitespace() => {
                if has_token || !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            '\'' => {
                has_token = true;
                while let Some(&sc) = chars.peek() {
                    if sc == '\'' {
                        chars.next();
                        break;
                    }
                    current.push(sc);
                    chars.next();
                }
            }
            '"' => {
                has_token = true;
                while let Some(&sc) = chars.peek() {
                    chars.next();
                    match sc {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"') | Some('\\')) => {
                            current.push(chars.next().expect("peek matched"));
                        }
                        other => current.push(other),
                    }
                }
            }
            '\\' => {
                has_token = true;
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            other => {
                has_token = true;
                current.push(other);
            }
        }
    }
    if has_token || !current.is_empty() {
        words.push(current);
    }
    words
}

/// FFmpeg: 构建并执行 ffmpeg 命令，返回输出文件的 Artifact::File
///
/// args 占位符语义（shipped 管线 video_to_srt / audio_extract 依赖）：
/// - `{input}`  → 第一个上游文件产物；此时 args 视为已自行声明输入，不再前置上游输入
/// - `{output}` → 本节点解析出的输出文件路径；此时不再在末尾追加输出参数
/// - 无占位符   → 完全向后兼容旧行为（args 无 `-i` 时前置上游文件输入，
///   且总在末尾追加输出参数；lavfi 等自包含命令不受影响）
async fn execute_builtin_ffmpeg(
    node: &PipelineNode,
    upstream: &[Artifact],
    work_dir: &Path,
) -> anyhow::Result<Artifact> {
    // args 契约形状为**数组**；若收到字符串（P0-2 前端历史形状），按 shell
    // 词法拆分为数组 —— 防御性兼容，避免参数被静默丢弃
    let args_vec: Vec<String> = match node.params.get("args") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|a| a.as_str().map(String::from))
            .collect(),
        Some(serde_json::Value::String(s)) => {
            tracing::warn!(
                node_id = %node.id,
                "ffmpeg node `args` is a string; splitting into an array (array is the contract shape)"
            );
            split_shell_words(s)
        }
        _ => Vec::new(),
    };

    // 先解析输出路径（占位符替换需要它）
    let output_path = resolve_ffmpeg_output_path(node, work_dir);
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // 第一个上游文件产物 —— `{input}` 占位符的替换来源
    let input_file: Option<PathBuf> = upstream.iter().find_map(|a| match a {
        Artifact::File(p) => Some(p.clone()),
        _ => None,
    });

    // 替换 {input} / {output} 占位符
    let (args, output_substituted) =
        substitute_ffmpeg_placeholders(&args_vec, &node.id, input_file.as_deref(), &output_path)?;

    let ffmpeg_bin = resolve_ffmpeg_path();
    let mut cmd = tokio::process::Command::new(&ffmpeg_bin);
    // 缺陷 #5：取消/超时 abort 执行任务时，future 被丢弃 → Child 被 drop
    // → 子进程一并终止；否则 ffmpeg 会脱离任务继续跑到自然结束。
    cmd.kill_on_drop(true);
    cmd.arg("-y"); // overwrite output

    // args 已通过 -i 或 {input} 自行声明输入时，不再前置上游文件
    let args_declares_input = args_vec.iter().any(|a| a == "-i")
        || args_vec.iter().any(|a| a.contains(INPUT_PLACEHOLDER));
    if !args_declares_input {
        for artifact in upstream {
            if let Artifact::File(path) = artifact {
                cmd.arg("-i").arg(path);
            }
        }
    }

    // 从 params 添加参数（占位符已替换）
    for arg in &args {
        cmd.arg(arg);
    }

    // {output} 占位符已替换时输出参数已在 args 中就位，追加会形成双输出
    if !output_substituted {
        cmd.arg(&output_path);
    }

    let output = cmd.output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("ffmpeg failed: {stderr}"));
    }

    // P3：命令成功但产物未落盘 → 下游将拿到悬空路径，显式报错
    //（如输出到空设备 / 写错位置时 ffmpeg 仍以 0 退出）
    if !output_path.is_file() {
        return Err(anyhow::anyhow!(
            "ffmpeg succeeded but did not produce the expected output file '{}'",
            output_path.display()
        ));
    }

    Ok(Artifact::File(output_path))
}

/// 模块 HTTP 调用错误 — 携带可重试标志
#[derive(Debug, Clone, thiserror::Error)]
#[error("module call to `{module_id}` failed: {message}")]
pub struct ModuleCallError {
    pub module_id: String,
    pub message: String,
    /// 是否为可重试错误（连接失败 / 超时等瞬态故障）
    pub retryable: bool,
}

/// 模块 HTTP 响应体
#[derive(Debug, serde::Deserialize)]
struct ModuleResponse {
    status: String,
    output_type: Option<String>,
    result: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    metadata: Option<serde_json::Value>,
}

/// 最大重试次数（不含首次尝试）— 节点未配置 `retry_count` 时的缺省语义
const MAX_RETRIES: u32 = 1;
/// 重试间隔（秒）
const RETRY_DELAY_SECS: u64 = 2;
/// HTTP 请求超时（秒）— 节点未配置 `timeout_secs` 时的缺省值
const HTTP_TIMEOUT_SECS: u64 = 300;

/// 节点级 HTTP 调用超时解析（P1-11）：`timeout_secs` 显式声明且 >0 时生效。
///
/// `Some(0)` = 显式「无硬超时」，返回 0（调用方据此**不设置**客户端超时，
/// 与 runner 侧 `timeout_secs=0 → 无 wall-clock 包裹` 的语义一致）；
/// 缺省（None）才回退 [`HTTP_TIMEOUT_SECS`]。
fn node_timeout_secs(node: &PipelineNode) -> u64 {
    match node.timeout_secs {
        Some(0) => 0,
        Some(secs) => u64::from(secs),
        None => HTTP_TIMEOUT_SECS,
    }
}

/// 模块节点执行 — 通过 HTTP 调用本地模块服务
///
/// 请求 URL: `http://127.0.0.1:{port}/predict/{capability}`
///
/// - 文件类上游产物（audio/video/image/file）→ multipart/form-data
/// - 文本/JSON 类上游产物 → JSON body
///
/// 可重试错误（连接失败、超时）最多重试 1 次，间隔 2 秒。
async fn execute_module_node(
    node: &PipelineNode,
    upstream: &[Artifact],
    work_dir: &Path,
    module_ports: &HashMap<String, u16>,
) -> anyhow::Result<Artifact> {
    let (module_id, capability) = match &node.kind {
        NodeKind::Module {
            module_id,
            capability,
            ..
        } => (module_id.clone(), capability.clone()),
        _ => unreachable!(),
    };

    // ── 解析端口：优先 module_ports 注册表，其次 node params ──────────────
    let port = module_ports.get(&module_id).copied().or_else(|| {
        node.params
            .get("port")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16)
    });

    let port = port.ok_or_else(|| {
        anyhow::anyhow!(ModuleCallError {
            module_id: module_id.clone(),
            message: format!(
                "no port registered for module '{module_id}' and no 'port' param in node '{}'",
                node.id
            ),
            retryable: false,
        })
    })?;

    let url = format!("http://127.0.0.1:{port}/predict/{capability}");

    // ── 判断上游是否包含文件类产物 ────────────────────────────────────────
    let has_file_input = upstream
        .iter()
        .any(|a| matches!(a, Artifact::File(_)));

    // ── 构建 HTTP 客户端 ──────────────────────────────────────────────────
    // 模块调用永远只打本机地址（127.0.0.1）：显式禁用代理，避免配置的
    // 出口代理（HTTP_PROXY 等）拦截 localhost 流量。
    // 节点级 timeout_secs（P1-11）：配置后作为本次调用的客户端超时，
    // 缺省沿用 HTTP_TIMEOUT_SECS；`timeout_secs=0` = 无硬超时（不设置
    // 客户端超时，与 runner 侧语义一致）。
    let timeout_secs = node_timeout_secs(node);
    let mut client_builder = reqwest::Client::builder().no_proxy();
    if timeout_secs > 0 {
        client_builder = client_builder.timeout(std::time::Duration::from_secs(timeout_secs));
    }
    let client = client_builder
        .build()
        .map_err(|e| {
            anyhow::anyhow!(ModuleCallError {
                module_id: module_id.clone(),
                message: format!("failed to build HTTP client: {e}"),
                retryable: false,
            })
        })?;

    // ── 重试循环 ──────────────────────────────────────────────────────────
    // 节点级 retry_count（P1-11）：配置后覆盖默认重试 1 次语义
    let max_retries = node.retry_count.unwrap_or(MAX_RETRIES);
    let mut last_error: Option<ModuleCallError> = None;

    for attempt in 0..=max_retries {
        if attempt > 0 {
            tracing::info!(
                module_id = %module_id,
                attempt,
                "retrying module HTTP call after {RETRY_DELAY_SECS}s"
            );
            tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS)).await;
        }

        tracing::debug!(
            module_id = %module_id,
            capability = %capability,
            url = %url,
            attempt,
            "executing module HTTP call"
        );

        match send_module_request(&client, &url, &module_id, node, upstream, has_file_input, work_dir).await {
            Ok(artifact) => return Ok(artifact),
            Err(e) => {
                let mce: ModuleCallError = match e.downcast_ref::<ModuleCallError>() {
                    Some(mce) => (*mce).clone(),
                    None => ModuleCallError {
                        module_id: module_id.clone(),
                        message: e.to_string(),
                        retryable: false,
                    },
                };

                tracing::warn!(
                    module_id = %module_id,
                    attempt,
                    retryable = mce.retryable,
                    error = %mce.message,
                    "module HTTP call failed"
                );

                if !mce.retryable {
                    // 不可重试 — 立即返回
                    return Err(anyhow::anyhow!(mce));
                }

                last_error = Some(mce);
            }
        }
    }

    // 所有重试耗尽
    Err(anyhow::anyhow!(
        last_error.unwrap_or_else(|| ModuleCallError {
            module_id: module_id.clone(),
            message: "unknown error after retries".to_string(),
            retryable: true,
        })
    ))
}

/// 发送单次模块 HTTP 请求并解析响应
async fn send_module_request(
    client: &reqwest::Client,
    url: &str,
    module_id: &str,
    node: &PipelineNode,
    upstream: &[Artifact],
    has_file_input: bool,
    work_dir: &Path,
) -> anyhow::Result<Artifact> {
    // ── output_format 声明 → 注入 output_path（模块可据此产出文件产物） ──
    // 约定：节点 params 含 output_format（如 "srt"/"txt"，非 "json"）时，
    // 执行器在 params 中补充 output_path=<work_dir>/<node_id>_output.<fmt>，
    // 模块按该路径写出文件并返回 output_type="file" + result=路径。
    let output_format = node
        .params
        .get("output_format")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|f| !f.is_empty() && *f != "json");
    let mut params_value = node.params.clone();
    if let Some(fmt) = output_format {
        let safe: String = fmt.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let out_path = work_dir.join(format!("{}_output.{safe}", node.id));
        let obj = match params_value.as_object_mut() {
            Some(o) => o,
            None => {
                params_value = serde_json::Value::Object(serde_json::Map::new());
                params_value.as_object_mut().expect("just set")
            }
        };
        obj.insert(
            "output_path".to_string(),
            serde_json::Value::String(out_path.to_string_lossy().to_string()),
        );
    }

    let resp = if has_file_input {
        // ── multipart/form-data：文件上传 ─────────────────────────────────
        let mut form = reqwest::multipart::Form::new();

        for artifact in upstream {
            if let Artifact::File(path) = artifact {
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "input".to_string());

                // P1：流式上传（Part::stream_with_length）替代 tokio::fs::read
                // 全量读入内存 —— GB 级输入不再出现「内存峰值 ≈ 文件大小」；
                // 显式携带 Content-Length，兼容要求声明长度的服务端。
                let file = tokio::fs::File::open(path).await.map_err(|e| {
                    anyhow::anyhow!(ModuleCallError {
                        module_id: module_id.to_string(),
                        message: format!("failed to open input file '{}': {e}", path.display()),
                        retryable: false,
                    })
                })?;
                let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);

                let part =
                    reqwest::multipart::Part::stream_with_length(file, len).file_name(file_name);
                form = form.part("file", part);
            }
        }

        // 将节点 params（含可能的 output_path 注入）作为 JSON 字符串附加到 params 字段
        if params_value.as_object().is_some_and(|o| !o.is_empty()) {
            form = form.text("params", params_value.to_string());
        }

        client
            .post(url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(e, module_id))?
    } else {
        // ── JSON body：文本 / JSON 输入 ───────────────────────────────────
        let mut body = serde_json::Map::new();

        // 合并节点 params（含可能的 output_path 注入）
        if let Some(obj) = params_value.as_object() {
            body.insert("params".to_string(), serde_json::Value::Object(obj.clone()));
        }

        // 从上游提取文本 / JSON 输入
        for artifact in upstream {
            match artifact {
                Artifact::Text(t) => {
                    body.insert(
                        "input".to_string(),
                        serde_json::Value::String(t.clone()),
                    );
                }
                Artifact::Json(j) => {
                    body.insert("input".to_string(), j.clone());
                }
                Artifact::File(p) => {
                    body.insert(
                        "input".to_string(),
                        serde_json::Value::String(p.to_string_lossy().to_string()),
                    );
                }
            }
        }

        client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_reqwest_error(e, module_id))?
    };

    // ── 解析响应 ──────────────────────────────────────────────────────────
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(ModuleCallError {
            module_id: module_id.to_string(),
            message: format!("HTTP {status}: {text}"),
            retryable: false,
        }));
    }

    let module_resp: ModuleResponse = resp.json().await.map_err(|e| {
        anyhow::anyhow!(ModuleCallError {
            module_id: module_id.to_string(),
            message: format!("failed to parse response JSON: {e}"),
            retryable: false,
        })
    })?;

    if module_resp.status != "completed" {
        return Err(anyhow::anyhow!(ModuleCallError {
            module_id: module_id.to_string(),
            message: format!(
                "module returned non-completed status: '{}'",
                module_resp.status
            ),
            retryable: false,
        }));
    }

    // ── 将 result 转为 Artifact ───────────────────────────────────────────
    let artifact = match module_resp.output_type.as_deref() {
        Some("file") => {
            let path_str = module_resp
                .result
                .as_ref()
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!(ModuleCallError {
                        module_id: module_id.to_string(),
                        message: "output_type is 'file' but result is not a string path".to_string(),
                        retryable: false,
                    })
                })?;
            Artifact::File(PathBuf::from(path_str))
        }
        Some("text") => {
            let text = module_resp
                .result
                .as_ref()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            Artifact::Text(text)
        }
        Some("json") | None => {
            // json 或未指定 output_type → 返回完整 result 作为 Json
            Artifact::Json(module_resp.result.unwrap_or(serde_json::Value::Null))
        }
        Some(other) => {
            tracing::warn!(
                module_id = %module_id,
                output_type = other,
                "unknown output_type, treating as JSON"
            );
            Artifact::Json(module_resp.result.unwrap_or(serde_json::Value::Null))
        }
    };

    tracing::info!(
        module_id = %module_id,
        output_type = ?module_resp.output_type,
        "module HTTP call completed successfully"
    );

    Ok(artifact)
}

/// 将 reqwest 错误分类为可重试 / 不可重试
fn classify_reqwest_error(e: reqwest::Error, module_id: &str) -> anyhow::Error {
    let retryable = e.is_connect() || e.is_timeout() || e.is_request();
    let message = if e.is_connect() {
        format!("connection failed: {e}")
    } else if e.is_timeout() {
        format!("request timed out: {e}")
    } else {
        format!("request error: {e}")
    };

    anyhow::anyhow!(ModuleCallError {
        module_id: module_id.to_string(),
        message,
        retryable,
    })
}

// ─── LLM builtin 节点（§6.7：OpenAI 兼容 chat/completions 单一形状） ────────

/// LLM 提示词模板占位符：`system_prompt` 中的 `{input}` 被替换为上游文本输入
pub(crate) const LLM_INPUT_PLACEHOLDER: &str = "{input}";

/// LLM 上游文本输入大小上限（20 MiB）— 防止超大文件撑爆请求体
const LLM_MAX_INPUT_BYTES: u64 = 20 * 1024 * 1024;

/// 视为文本内容的文件扩展名（llm 上游接文件产物时读取内容）
const LLM_TEXT_EXTENSIONS: &[&str] = &[
    "txt", "srt", "vtt", "ass", "ssa", "md", "markdown", "json", "csv", "tsv", "log", "yaml",
    "yml", "xml", "html", "htm",
];

/// LLM 输出格式（`output_format` 参数）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LlmOutputFormat {
    /// 响应内容原样作为文本产物
    Text,
    /// 响应内容必须为合法 JSON（否则报错），产物为解析后的 Json
    Json,
}

/// LLM 调用准备结果（纯解析产物，可单测）
#[derive(Debug)]
struct PreparedLlmCall {
    /// 完整请求 URL：`{base_url}/chat/completions`
    url: String,
    /// 从环境变量解析出的 API Key（仅驻留内存，绝不落盘/入日志）
    api_key: Option<String>,
    /// chat/completions 请求体：`{model, messages, temperature?, max_tokens?}`
    body: serde_json::Value,
    output_format: LlmOutputFormat,
}

/// 从上游产物提取 LLM 文本输入（input_type=text，§6.7）。
///
/// - `Text` → 原样
/// - `Json` → JSON 字符串取内部文本，否则紧凑序列化
/// - `File` → 文本类扩展名读取文件内容（大小上限 [`LLM_MAX_INPUT_BYTES`]），否则报错
/// - 无上游产物 → 空串（纯模板驱动场景）
fn llm_input_text(upstream: &[Artifact], node_id: &str) -> anyhow::Result<String> {
    let Some(artifact) = upstream.first() else {
        return Ok(String::new());
    };
    match artifact {
        Artifact::Text(t) => Ok(t.clone()),
        Artifact::Json(v) => Ok(match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        Artifact::File(path) => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();
            if !LLM_TEXT_EXTENSIONS.contains(&ext.as_str()) {
                return Err(anyhow::anyhow!(
                    "llm node '{node_id}' expects text input, but upstream file '{}' has non-text extension `.{ext}`",
                    path.display()
                ));
            }
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if len > LLM_MAX_INPUT_BYTES {
                return Err(anyhow::anyhow!(
                    "llm node '{node_id}': upstream file '{}' is too large ({len} bytes > {LLM_MAX_INPUT_BYTES} limit)",
                    path.display()
                ));
            }
            std::fs::read_to_string(path).map_err(|e| {
                anyhow::anyhow!(
                    "llm node '{node_id}' failed to read upstream file '{}': {e}",
                    path.display()
                )
            })
        }
    }
}

/// 构建 chat/completions 的 messages（纯函数，可单测）。
///
/// `{input}` 占位符规则（§6.7）：
/// - `system_prompt` 含 `{input}` → 替换后作为唯一 user 消息（不再重复发送上游文本）
/// - `system_prompt` 非空且无占位符 → system 消息 + 上游文本作 user 消息
/// - 无 `system_prompt` → 上游文本作 user 消息
fn build_llm_messages(system_prompt: Option<&str>, input_text: &str) -> Vec<serde_json::Value> {
    let mut messages = Vec::with_capacity(2);
    match system_prompt.map(str::trim).filter(|s| !s.is_empty()) {
        Some(prompt) if prompt.contains(LLM_INPUT_PLACEHOLDER) => {
            let content = prompt.replace(LLM_INPUT_PLACEHOLDER, input_text);
            messages.push(serde_json::json!({ "role": "user", "content": content }));
        }
        Some(prompt) => {
            messages.push(serde_json::json!({ "role": "system", "content": prompt }));
            messages.push(serde_json::json!({ "role": "user", "content": input_text }));
        }
        None => {
            messages.push(serde_json::json!({ "role": "user", "content": input_text }));
        }
    }
    messages
}

/// 解析 LLM 节点参数为一次调用准备结果（纯函数，可单测）。
///
/// `kind_endpoint` / `kind_api_key_env`：遗留 `kind = "external_api" | "llm"`
/// 节点的 kind 级字段；builtin 形状传 `None`，全部参数来自 `params`。
/// 同名时 kind 级非空值优先（遗留节点的权威声明）。
///
/// API Key 语义：`api_key_env` 存**环境变量名**，执行时经 `std::env::var`
/// 读取 —— 绝不落盘明文；声明了变量名但环境中缺失 → 报错（i18n 键需求见报告）。
/// 未声明 → 不携带 Authorization（本地免密钥端点，如 Ollama/vLLM）。
fn parse_llm_request(
    node_id: &str,
    params: &serde_json::Value,
    kind_endpoint: Option<&str>,
    kind_api_key_env: Option<&str>,
    input_text: &str,
) -> anyhow::Result<PreparedLlmCall> {
    let param_str = |key: &str| -> Option<String> {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    // base_url：params 优先声明于 builtin 形状；kind 级 endpoint 为遗留来源
    let base_url = param_str("base_url")
        .or_else(|| kind_endpoint.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "llm node '{node_id}' is missing required param 'base_url' \
                 (OpenAI-compatible endpoint, e.g. https://api.openai.com/v1)"
            )
        })?;
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let model = param_str("model").ok_or_else(|| {
        anyhow::anyhow!("llm node '{node_id}' is missing required param 'model'")
    })?;

    // api_key：从环境变量读取（绝不落盘明文）
    let api_key_env = param_str("api_key_env")
        .or_else(|| kind_api_key_env.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string));
    let api_key = match &api_key_env {
        Some(var) => {
            let key = std::env::var(var).map_err(|_| {
                anyhow::anyhow!(
                    "llm node '{node_id}': api_key_env points to environment variable '{var}', \
                     but it is not set; API keys are only read from the environment and never stored in pipeline files"
                )
            })?;
            if key.trim().is_empty() {
                return Err(anyhow::anyhow!(
                    "llm node '{node_id}': environment variable '{var}' (api_key_env) is set but empty"
                ));
            }
            Some(key)
        }
        None => None,
    };

    // temperature：可选，范围 [0.0, 2.0]（OpenAI 兼容约定）
    let temperature = match params.get("temperature") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let t = v
                .as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "llm node '{node_id}': param 'temperature' must be a number, got {v}"
                    )
                })?;
            if !(0.0..=2.0).contains(&t) {
                return Err(anyhow::anyhow!(
                    "llm node '{node_id}': param 'temperature' must be within [0.0, 2.0], got {t}"
                ));
            }
            Some(t)
        }
    };

    // max_tokens：可选，正整数
    let max_tokens = match params.get("max_tokens") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let n = v
                .as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "llm node '{node_id}': param 'max_tokens' must be a positive integer, got {v}"
                    )
                })?;
            if n == 0 {
                return Err(anyhow::anyhow!(
                    "llm node '{node_id}': param 'max_tokens' must be a positive integer"
                ));
            }
            Some(n)
        }
    };

    // output_format：text（缺省）| json
    let output_format = match param_str("output_format").as_deref() {
        None | Some("text") => LlmOutputFormat::Text,
        Some("json") => LlmOutputFormat::Json,
        Some(other) => {
            return Err(anyhow::anyhow!(
                "llm node '{node_id}': param 'output_format' must be 'text' or 'json', got '{other}'"
            ))
        }
    };

    let messages = build_llm_messages(param_str("system_prompt").as_deref(), input_text);

    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), serde_json::Value::String(model));
    body.insert(
        "messages".to_string(),
        serde_json::Value::Array(messages),
    );
    if let Some(t) = temperature {
        body.insert(
            "temperature".to_string(),
            serde_json::json!(t),
        );
    }
    if let Some(n) = max_tokens {
        body.insert("max_tokens".to_string(), serde_json::json!(n));
    }

    Ok(PreparedLlmCall {
        url,
        api_key,
        body: serde_json::Value::Object(body),
        output_format,
    })
}

/// LLM 端点是否为本机地址 —— 本机地址豁免代理（沿用模块调用的 no_proxy
/// 豁免规则）；外部端点走 reqwest 默认行为（尊重 HTTP(S)_PROXY 环境变量）
fn llm_endpoint_is_local(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .map(|h| {
            matches!(
                h.trim_matches(['[', ']']),
                "localhost" | "127.0.0.1" | "::1"
            )
        })
        .unwrap_or(false)
}

/// 构建 LLM HTTP 客户端：节点 `timeout_secs` 生效（0 = 无客户端超时），
/// 本机端点豁免代理
fn build_llm_http_client(url: &str, timeout_secs: u64) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if timeout_secs > 0 {
        builder = builder.timeout(std::time::Duration::from_secs(timeout_secs));
    }
    if llm_endpoint_is_local(url) {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build LLM HTTP client: {e}"))
}

/// LLM 节点执行 — OpenAI 兼容 chat/completions POST（§6.7）。
///
/// 失败语义与模块节点一致：连接失败/超时等瞬态故障最多重试 1 次
/// （`retry_count` 可覆盖），间隔 2 秒；非 2xx 不重试且错误携带状态码。
async fn execute_llm_node(
    node: &PipelineNode,
    upstream: &[Artifact],
    kind_endpoint: Option<&str>,
    kind_api_key_env: Option<&str>,
) -> anyhow::Result<Artifact> {
    let input_text = llm_input_text(upstream, &node.id)?;
    let prepared = parse_llm_request(
        &node.id,
        &node.params,
        kind_endpoint,
        kind_api_key_env,
        &input_text,
    )?;

    let timeout_secs = node_timeout_secs(node);
    let client = build_llm_http_client(&prepared.url, timeout_secs)?;
    let max_retries = node.retry_count.unwrap_or(MAX_RETRIES);

    let mut last_error: Option<ModuleCallError> = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            tracing::info!(
                node_id = %node.id,
                attempt,
                "retrying LLM call after {RETRY_DELAY_SECS}s"
            );
            tokio::time::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS)).await;
        }

        // 日志只带 url/model，绝不输出 api_key
        let model = prepared
            .body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        tracing::debug!(
            node_id = %node.id,
            url = %prepared.url,
            model = %model,
            attempt,
            "executing LLM call"
        );

        match send_llm_request(&client, &prepared, &node.id).await {
            Ok(artifact) => {
                tracing::info!(node_id = %node.id, model = %model, "LLM call completed");
                return Ok(artifact);
            }
            Err(e) => {
                let mce = match e.downcast_ref::<ModuleCallError>() {
                    Some(mce) => mce.clone(),
                    None => ModuleCallError {
                        module_id: node.id.clone(),
                        message: e.to_string(),
                        retryable: false,
                    },
                };
                tracing::warn!(
                    node_id = %node.id,
                    attempt,
                    retryable = mce.retryable,
                    error = %mce.message,
                    "LLM call failed"
                );
                if !mce.retryable {
                    return Err(anyhow::anyhow!(mce));
                }
                last_error = Some(mce);
            }
        }
    }

    Err(anyhow::anyhow!(
        last_error.unwrap_or_else(|| ModuleCallError {
            module_id: node.id.clone(),
            message: "unknown LLM error after retries".to_string(),
            retryable: true,
        })
    ))
}

/// 发送单次 chat/completions 请求并解析响应为产物
async fn send_llm_request(
    client: &reqwest::Client,
    prepared: &PreparedLlmCall,
    node_id: &str,
) -> anyhow::Result<Artifact> {
    let mut req = client.post(&prepared.url).json(&prepared.body);
    if let Some(key) = &prepared.api_key {
        req = req.bearer_auth(key);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| classify_reqwest_error(e, node_id))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(ModuleCallError {
            module_id: node_id.to_string(),
            message: format!("LLM endpoint returned HTTP {status}: {text}"),
            retryable: false,
        }));
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| {
        anyhow::anyhow!(ModuleCallError {
            module_id: node_id.to_string(),
            message: format!("failed to parse LLM response JSON: {e}"),
            retryable: false,
        })
    })?;

    let content = body
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(ModuleCallError {
                module_id: node_id.to_string(),
                message: "LLM response is missing choices[0].message.content".to_string(),
                retryable: false,
            })
        })?;

    match prepared.output_format {
        LlmOutputFormat::Text => Ok(Artifact::Text(content.to_string())),
        LlmOutputFormat::Json => {
            let value: serde_json::Value = serde_json::from_str(content).map_err(|e| {
                anyhow::anyhow!(ModuleCallError {
                    module_id: node_id.to_string(),
                    message: format!(
                        "output_format is 'json' but LLM response content is not valid JSON: {e}"
                    ),
                    retryable: false,
                })
            })?;
            Ok(Artifact::Json(value))
        }
    }
}
