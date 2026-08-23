//! 任务注册表 — daemon/桌面共用的任务索引（P1-4 下沉 ep-core）
//!
//! ## 职责
//!
//! - [`TaskRecord`]：一条管线/直跑任务的完整记录（身份、状态、节点状态、
//!   产物索引、工作目录）。`pipeline_id` 持久化（§6.8 任务↔管线身份）。
//! - [`TaskRegistry`]：内存索引 + **可选落盘持久化**（`runtime/tasks/{task_id}.json`，
//!   原子写 = 写 `.tmp` 后 rename），daemon 重启不丢索引。
//!
//! ## 持久化语义
//!
//! - 每次 insert/update 成功后立即落盘该任务文件（启用持久化时）；
//! - [`TaskRegistry::load`] / [`TaskRegistry::enable_persistence`] 读回全部
//!   `*.json`：上次进程退出时仍处于 `queued`/`running` 的任务不可能再完成，
//!   加载时一律改判 `failed`（错误信息标注"被进程重启中断"）；
//! - `queue_position` 为运行期瞬态值，落盘前置 `None`。
//!
//! ## 边界
//!
//! 注册表只做**索引与状态存储**：并发闸门、执行调度、回调属于 daemon
//! 执行层（`ep-daemon/src/execution.rs`）；桌面端（C4）直连本模块 +
//! ep-core runner 自行执行。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── 任务状态 ────────────────────────────────────────────────────────────────

/// 任务整体状态（API 序列化为小写字符串，§6.8 新增 `queued`）。
///
/// 序列化形状即前端契约：`"queued" | "running" | "completed" | "failed" | "cancelled"`。
/// `Failed` 的具体错误文本存放在 [`TaskRecord::error`]（状态本身为 unit 变体，
/// 保证 JSON/TOML 双向简单稳定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// 已提交，等待全局 `max_parallel` 或管线级 `max_instances` 闸门（§6.8）
    Queued,
    Running,
    Completed,
    Failed,
    /// 用户取消（排队中取消 = 不再执行；运行中取消 = 逻辑终态，
    /// 引擎线程可能仍在后台收尾，见 execution.rs 文档）
    Cancelled,
}

impl TaskState {
    /// 前端契约：小写状态字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// 是否为终态（不会再变化）
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// 从字符串解析（注册表查询 `?status=` 过滤用；大小写不敏感）
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

// ─── 记录 ────────────────────────────────────────────────────────────────────

/// 单节点状态记录（`state` 为 pending/running/completed/failed/skipped 字符串，
/// 与引擎 [`crate::pipeline::runner::NodeDetail`] 的取值一致）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 一条任务记录（注册表值；daemon 与桌面端共用）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    /// 任务 ID（对外唯一标识）
    pub id: String,
    /// 管线 ID（§6.8 持久化；直跑任务为 `direct:<module>:<capability>` 合成 id）
    pub pipeline_id: String,
    pub status: TaskState,
    /// 失败原因（仅 status=Failed 时有值；技术层英文）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 队列位置（1 起；仅 status=Queued 时有意义）。运行期瞬态值，不落盘。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<usize>,
    /// 提交（入队）时间。命名沿用既有 API 契约（tasks.rs 直接消费）。
    pub started_at: DateTime<Utc>,
    /// 实际开始执行时间（Queued 期间为 None；用于排队耗时统计）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_running_at: Option<DateTime<Utc>>,
    /// 终结时间（completed/failed/cancelled）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// 节点定义顺序（保证详情/产物输出顺序稳定）
    #[serde(default)]
    pub node_order: Vec<String>,
    /// node_id → 节点状态
    #[serde(default)]
    pub nodes: HashMap<String, NodeRecord>,
    /// node_id → 引擎输出的原始产物文件路径
    #[serde(default)]
    pub artifacts: HashMap<String, PathBuf>,
    /// node_id → 归集到任务目录 `files/{node_id}/` 下的产物路径（可下载位置）
    #[serde(default)]
    pub served_artifacts: HashMap<String, PathBuf>,
    /// 任务工作目录（{workspace}/tasks/{task_id}）
    pub work_dir: PathBuf,
    /// 任务暂存目录（RAM 优先，ep-core::staging）：节点中间产物与 adapter
    /// 帧序列落位根；任务终态清算。旧落盘 JSON 无此字段 → default None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging_dir: Option<String>,
}

impl TaskRecord {
    /// 任务是否仍在占用闸门（queued 或 running）
    pub fn is_active(&self) -> bool {
        matches!(self.status, TaskState::Queued | TaskState::Running)
    }
}

// ─── 注册表 ──────────────────────────────────────────────────────────────────

/// 任务注册表（内存索引 + 可选磁盘持久化）
#[derive(Debug, Default)]
pub struct TaskRegistry {
    records: HashMap<String, TaskRecord>,
    /// 持久化目录；None = 纯内存
    persist_dir: Option<PathBuf>,
}

impl TaskRegistry {
    /// 纯内存注册表（无持久化）
    pub fn new() -> Self {
        Self::default()
    }

    /// 从目录加载既有记录并启用持久化。
    ///
    /// 目录不存在视为空；单个文件损坏仅告警跳过。加载时把遗留的
    /// queued/running 任务改判 failed（进程重启中断）。
    pub fn load(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let mut registry = Self {
            records: HashMap::new(),
            persist_dir: Some(dir.clone()),
        };
        registry.load_from_disk(&dir);
        registry
    }

    /// 为已存在的注册表启用持久化：设置目录并回读既有记录。
    ///
    /// 幂等：目录相同则无操作。已有内存记录与磁盘记录冲突时内存优先。
    /// 返回加载到的记录数（不含内存已有）。
    pub fn enable_persistence(&mut self, dir: impl Into<PathBuf>) -> std::io::Result<usize> {
        let dir = dir.into();
        if self.persist_dir.as_deref() == Some(dir.as_path()) {
            return Ok(0);
        }
        std::fs::create_dir_all(&dir)?;
        self.persist_dir = Some(dir.clone());
        let before = self.records.len();
        self.load_from_disk(&dir);
        Ok(self.records.len() - before)
    }

    pub fn persist_dir(&self) -> Option<&Path> {
        self.persist_dir.as_deref()
    }

    fn load_from_disk(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        paths.sort();
        for path in paths {
            let mut record: TaskRecord = match std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
            {
                Some(r) => r,
                None => {
                    tracing::warn!(file = %path.display(), "corrupt task record, skipping");
                    continue;
                }
            };
            // 进程重启后不可能继续执行：非终态一律改判 failed
            if !record.status.is_terminal() {
                record.status = TaskState::Failed;
                record.error = Some(
                    "task interrupted by daemon restart (never finished)".to_string(),
                );
                record.finished_at = Some(Utc::now());
            }
            record.queue_position = None;
            // P2：文件名（`{task_id}.json`）与 record.id 必须一致——不一致时
            // 以文件名 key 为准重写 record.id，否则该记录会丢失（如 x.json 内
            // 声明 id=y 时，查询 get("x") 落空且可能被 y.json 覆盖）
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if !stem.is_empty() && record.id != stem {
                tracing::warn!(
                    file = %path.display(),
                    record_id = %record.id,
                    "task record id does not match its filename; rewriting id to match filename"
                );
                record.id = stem;
            }
            // 内存优先：冲突时不覆盖
            self.records.entry(record.id.clone()).or_insert(record);
        }
    }

    // ── 写路径 ───────────────────────────────────────────────────────────────

    /// 插入新记录（同 id 已存在则覆盖）并落盘
    pub fn insert(&mut self, record: TaskRecord) -> std::io::Result<()> {
        self.persist_one(&record)?;
        self.records.insert(record.id.clone(), record);
        Ok(())
    }

    /// 原地更新记录并落盘；任务不存在返回 `None`
    pub fn update<F>(&mut self, task_id: &str, f: F) -> Option<std::io::Result<()>>
    where
        F: FnOnce(&mut TaskRecord),
    {
        let record = self.records.get_mut(task_id)?;
        f(record);
        let clone = record.clone();
        Some(self.persist_one(&clone))
    }

    fn persist_one(&self, record: &TaskRecord) -> std::io::Result<()> {
        let Some(dir) = &self.persist_dir else {
            return Ok(());
        };
        std::fs::create_dir_all(dir)?;
        // 落盘副本：queue_position 为运行期瞬态值，不持久化
        let mut to_save = record.clone();
        to_save.queue_position = None;
        let json = serde_json::to_vec_pretty(&to_save)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        atomic_write(&dir.join(format!("{}.json", record.id)), &json)
    }

    // ── 读路径 ───────────────────────────────────────────────────────────────

    pub fn get(&self, task_id: &str) -> Option<&TaskRecord> {
        self.records.get(task_id)
    }

    /// 可变借用（调用方自行保证随后经 [`Self::update`] 落盘；
    /// 执行层的节点回调属高频路径，逐次落盘开销过大，终态时统一持久化）
    pub fn get_mut(&mut self, task_id: &str) -> Option<&mut TaskRecord> {
        self.records.get_mut(task_id)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// 全部任务快照（新任务在前：started_at 降序，id 降序兜底）
    pub fn all(&self) -> Vec<TaskRecord> {
        let mut list: Vec<TaskRecord> = self.records.values().cloned().collect();
        list.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        list
    }

    /// 按 pipeline_id 过滤（新任务在前），供 `GET /api/pipelines/{id}/tasks` 使用
    pub fn by_pipeline(&self, pipeline_id: &str) -> Vec<TaskRecord> {
        self.all()
            .into_iter()
            .filter(|r| r.pipeline_id == pipeline_id)
            .collect()
    }
}

/// 原子写：先写同目录 `.tmp` 再 rename 覆盖（双平台同卷 rename 原子）。
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "ep-taskreg-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(id: &str, pipeline_id: &str, status: TaskState) -> TaskRecord {
        TaskRecord {
            id: id.to_string(),
            pipeline_id: pipeline_id.to_string(),
            status,
            error: None,
            queue_position: None,
            started_at: Utc::now(),
            started_running_at: None,
            finished_at: None,
            node_order: vec!["input".into(), "output".into()],
            nodes: HashMap::new(),
            artifacts: HashMap::new(),
            served_artifacts: HashMap::new(),
            work_dir: PathBuf::from(format!("/tmp/tasks/{id}")),
            staging_dir: None,
        }
    }

    // ── TaskState 字符串契约 ─────────────────────────────────────────────────

    #[test]
    fn task_state_string_contract() {
        assert_eq!(TaskState::Queued.as_str(), "queued");
        assert_eq!(TaskState::Running.as_str(), "running");
        assert_eq!(TaskState::Completed.as_str(), "completed");
        assert_eq!(TaskState::Failed.as_str(), "failed");
        assert_eq!(TaskState::Cancelled.as_str(), "cancelled");
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Cancelled.is_terminal());
        assert!(!TaskState::Queued.is_terminal());
        assert!(!TaskState::Running.is_terminal());
        assert_eq!(TaskState::parse("QUEUED"), Some(TaskState::Queued));
        assert_eq!(TaskState::parse("cancelled"), Some(TaskState::Cancelled));
        assert_eq!(TaskState::parse("bogus"), None);
    }

    #[test]
    fn task_state_serde_lowercase() {
        let v = serde_json::to_value(TaskState::Queued).unwrap();
        assert_eq!(v, serde_json::json!("queued"));
        let back: TaskState = serde_json::from_value(serde_json::json!("cancelled")).unwrap();
        assert_eq!(back, TaskState::Cancelled);
    }

    // ── 内存注册表基本操作 ───────────────────────────────────────────────────

    #[test]
    fn insert_get_all_sorted_newest_first() {
        let mut reg = TaskRegistry::new();
        let mut older = record("task-a", "p1", TaskState::Completed);
        older.started_at = Utc::now() - chrono::Duration::seconds(60);
        let newer = record("task-b", "p1", TaskState::Running);
        reg.insert(older).unwrap();
        reg.insert(newer).unwrap();

        let all = reg.all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "task-b", "新任务在前");
        assert_eq!(all[1].id, "task-a");
    }

    #[test]
    fn by_pipeline_filters() {
        let mut reg = TaskRegistry::new();
        reg.insert(record("t1", "pipe-a", TaskState::Completed)).unwrap();
        reg.insert(record("t2", "pipe-b", TaskState::Completed)).unwrap();
        reg.insert(record("t3", "pipe-a", TaskState::Failed)).unwrap();

        let a = reg.by_pipeline("pipe-a");
        assert_eq!(a.len(), 2);
        assert!(a.iter().all(|r| r.pipeline_id == "pipe-a"));
        assert!(reg.by_pipeline("ghost").is_empty());
    }

    #[test]
    fn update_mutates_and_reports_missing() {
        let mut reg = TaskRegistry::new();
        reg.insert(record("t1", "p", TaskState::Running)).unwrap();

        let res = reg.update("t1", |r| {
            r.status = TaskState::Completed;
            r.finished_at = Some(Utc::now());
        });
        assert!(res.is_some());
        assert!(matches!(res.unwrap(), Ok(())));
        assert_eq!(reg.get("t1").unwrap().status, TaskState::Completed);

        assert!(reg.update("ghost", |_| {}).is_none());
    }

    // ── 持久化往返 ───────────────────────────────────────────────────────────

    #[test]
    fn persistence_roundtrip() {
        let dir = temp_dir("roundtrip");
        {
            let mut reg = TaskRegistry::new();
            reg.enable_persistence(&dir).unwrap();
            let mut r = record("task-x", "pipe-x", TaskState::Running);
            r.started_running_at = Some(r.started_at);
            r.nodes.insert(
                "input".into(),
                NodeRecord {
                    state: "completed".into(),
                    error: None,
                },
            );
            r.artifacts
                .insert("input".into(), PathBuf::from("/data/out.txt"));
            reg.insert(r).unwrap();

            // 磁盘文件存在且是合法 JSON
            let file = dir.join("task-x.json");
            assert!(file.exists());
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            assert_eq!(v["pipeline_id"], "pipe-x");
            // queue_position 不落盘
            assert!(v.get("queue_position").is_none());
        }

        // 模拟 daemon 重启：全新注册表从目录恢复索引
        let reg2 = TaskRegistry::load(&dir);
        let r = reg2.get("task-x").expect("重启后索引应恢复");
        assert_eq!(r.pipeline_id, "pipe-x");
        assert_eq!(r.status, TaskState::Failed, "重启时 running 任务应改判 failed");
        assert!(r
            .error
            .as_deref()
            .unwrap()
            .contains("interrupted by daemon restart"));
        assert_eq!(r.nodes["input"].state, "completed");
        assert_eq!(r.artifacts["input"], PathBuf::from("/data/out.txt"));
        assert!(r.finished_at.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_preserves_terminal_records() {
        let dir = temp_dir("terminal");
        {
            let mut reg = TaskRegistry::new();
            reg.enable_persistence(&dir).unwrap();
            let mut done = record("task-ok", "p", TaskState::Completed);
            done.finished_at = Some(done.started_at);
            let mut cancelled = record("task-cancel", "p", TaskState::Cancelled);
            cancelled.finished_at = Some(cancelled.started_at);
            let mut failed = record("task-fail", "p", TaskState::Failed);
            failed.error = Some("boom".into());
            reg.insert(done).unwrap();
            reg.insert(cancelled).unwrap();
            reg.insert(failed).unwrap();
        }

        let reg2 = TaskRegistry::load(&dir);
        assert_eq!(reg2.get("task-ok").unwrap().status, TaskState::Completed);
        assert_eq!(reg2.get("task-cancel").unwrap().status, TaskState::Cancelled);
        let failed = reg2.get("task-fail").unwrap();
        assert_eq!(failed.status, TaskState::Failed);
        assert_eq!(failed.error.as_deref(), Some("boom"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_skips_corrupt_files() {
        let dir = temp_dir("corrupt");
        std::fs::write(dir.join("broken.json"), "not json {{").unwrap();
        std::fs::write(dir.join("not-a-record.txt"), "ignore me").unwrap();
        let good = record("task-good", "p", TaskState::Completed);
        std::fs::write(
            dir.join("task-good.json"),
            serde_json::to_vec_pretty(&good).unwrap(),
        )
        .unwrap();

        let reg = TaskRegistry::load(&dir);
        assert_eq!(reg.len(), 1);
        assert!(reg.get("task-good").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P2：文件名为 `{task_id}.json`，与 record.id 不一致时以文件名 key 为准
    /// 重写 record.id（否则记录丢失：x.json 内声明 id=y 时 get("x") 落空）。
    #[test]
    fn load_rewrites_record_id_to_match_filename() {
        let dir = temp_dir("mismatch");
        let mut r = record("mismatched-id", "p", TaskState::Completed);
        r.finished_at = Some(r.started_at);
        std::fs::write(
            dir.join("file-x.json"),
            serde_json::to_vec_pretty(&r).unwrap(),
        )
        .unwrap();

        let reg = TaskRegistry::load(&dir);
        assert_eq!(reg.len(), 1, "不一致记录不应丢失");
        assert!(reg.get("file-x").is_some(), "应以文件名 key 索引");
        assert!(reg.get("mismatched-id").is_none(), "原 record.id 不应再被索引");
        assert_eq!(reg.get("file-x").unwrap().id, "file-x", "record.id 应重写为文件名");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_replaces_existing() {
        let dir = temp_dir("atomic");
        let target = dir.join("task-a.json");

        let mut reg = TaskRegistry::new();
        reg.enable_persistence(&dir).unwrap();
        reg.insert(record("task-a", "p", TaskState::Running)).unwrap();
        reg.update("task-a", |r| r.status = TaskState::Completed)
            .unwrap()
            .unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(v["status"], "completed", "覆盖写应落盘终态");
        assert!(!dir.join("task-a.json.tmp").exists(), "tmp 文件不应残留");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enable_persistence_idempotent_and_conflict_memory_wins() {
        let dir = temp_dir("idempotent");
        // 先落盘一条旧记录
        {
            let mut reg = TaskRegistry::new();
            reg.enable_persistence(&dir).unwrap();
            let mut old = record("task-c", "p", TaskState::Completed);
            old.error = None;
            reg.insert(old).unwrap();
        }
        // 内存中已有同 id 的更新记录 → 启用持久化时内存优先
        let mut reg = TaskRegistry::new();
        let mut in_memory = record("task-c", "p", TaskState::Completed);
        in_memory.error = Some("memory version".into());
        reg.insert(in_memory).unwrap();
        let loaded = reg.enable_persistence(&dir).unwrap();
        assert_eq!(loaded, 0, "内存已有的记录不计入磁盘加载数");
        assert_eq!(
            reg.get("task-c").unwrap().error.as_deref(),
            Some("memory version")
        );
        // 再次启用同目录 → 无操作
        assert_eq!(reg.enable_persistence(&dir).unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
