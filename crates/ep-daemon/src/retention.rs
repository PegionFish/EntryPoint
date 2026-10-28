//! 任务产物保留策略（T-12）——无人值守清盘，防止 `workspace/tasks/` 撑爆磁盘。
//!
//! ## 清理语义
//!
//! - 配置来源：[`ep_core::config::TasksRetentionConfig`]（`[tasks.retention]`）；
//!   每轮巡检实时读取 `state.config`，运行期经 `PUT /api/config` 改动**下一轮生效**。
//! - 扫描范围：`<workspace_dir>/tasks/*`，每个任务一个目录（含 `files/` 产物）。
//! - **只清理终态任务**：以 `runtime/tasks/{task_id}.json` 注册表记录判定
//!   completed / failed / cancelled；queued / running 永不删除。注册表缺失、
//!   损坏或 id 不匹配的目录一律跳过（无法证明终态 → 保守不动）。
//! - 崩溃遗留豁免：磁盘记录仍是非终态、但内存注册表已在启动加载时改判 failed
//!   且错误文本为 [`ep_core::task_registry::INTERRUPTED_BY_RESTART_ERROR`] 的
//!   任务，按 failed 终态处理（无人值守场景下断电重启的遗留目录不会永远占盘）；
//!   真正运行中 / 排队中的任务内存状态未变，仍绝不触碰。
//! - 不可判定终态的目录（运行中 / 排队中、注册表缺失或损坏）计入总容量，
//!   但不参与清理；若因此无法满足 `max_total_gb`，本轮输出一条 `warn` 日志。
//! - 淘汰顺序按完成时间（`finished_at`，缺失时回退 `started_at`）升序：
//!   先删超龄任务（`max_age_days`），再按总容量（`max_total_gb`）从最旧开始删，
//!   直至满足上限；两个维度互相独立，`0` = 该维度不限。
//! - 删除即整个任务目录 `remove_dir_all`（**产物一并删除**）；成功后同步删除
//!   `runtime/tasks/{id}.json` 注册表记录（F5：防幽灵任务残留，删除失败仅
//!   告警不回滚），并写一条 `task_retention_cleanup` 事件到统一事件日志
//!   （复用 [`crate::eventlog::append_event`]，不另造日志文件/格式）。
//! - 单个目录删除失败仅告警并继续本轮，不阻断其它任务清理。
//!
//! ## 调度
//!
//! [`spawn_retention_loop`] 启动后延迟 [`INITIAL_DELAY_SECS`] 首跑（避开启动期
//! I/O 高峰），之后每 [`INTERVAL_SECS`]（6 小时）巡检一轮。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ep_core::config::TasksRetentionConfig;
use ep_core::task_registry::{TaskRecord, TaskState, INTERRUPTED_BY_RESTART_ERROR};
use serde_json::json;

use crate::eventlog;
use crate::state::AppState;

/// 巡检周期：6 小时（无人值守清盘，无需更频繁）。
const INTERVAL_SECS: u64 = 6 * 60 * 60;

/// 启动后首跑延迟：3 分钟（避开 daemon 启动期 I/O 高峰）。
const INITIAL_DELAY_SECS: u64 = 3 * 60;

/// 清理事件类型（统一事件日志 `events-YYYY-MM.jsonl` 的 `type` 字段）。
pub(crate) const CLEANUP_EVENT_TYPE: &str = "task_retention_cleanup";

/// 淘汰原因：按年龄超限。
const REASON_MAX_AGE: &str = "max_age_days";

/// 淘汰原因：按总容量超限。
const REASON_MAX_TOTAL: &str = "max_total_gb";

/// 单轮巡检统计（供日志与测试断言）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetentionReport {
    /// 扫描到的任务目录数（含非终态 / 无注册表记录的目录）
    pub(crate) scanned: usize,
    /// 其中可参与淘汰的终态任务数
    pub(crate) terminal: usize,
    /// 本轮实际删除的任务目录数
    pub(crate) deleted: usize,
    /// 本轮释放的字节数（按删除前目录内容统计）
    pub(crate) freed_bytes: u64,
}

/// 后台巡检循环：启动后延迟 [`INITIAL_DELAY_SECS`] 首跑，之后每
/// [`INTERVAL_SECS`] 一轮；每轮实时读取配置，`PUT /api/config` 改动下一轮生效。
pub async fn spawn_retention_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(INITIAL_DELAY_SECS)).await;
        let mut interval = tokio::time::interval(Duration::from_secs(INTERVAL_SECS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let (policy, workspace_dir) = {
                let cfg = state.config.read().await;
                (
                    cfg.tasks.retention.clone(),
                    cfg.resolve_workspace_dir(&state.root),
                )
            };
            if !policy.enabled {
                tracing::debug!("任务产物保留策略未启用，跳过本轮清理");
                continue;
            }
            // 目录遍历与 remove_dir_all 均为阻塞 IO，放进 spawn_blocking，避免
            // 在大目录场景下长时间占用 async worker 线程。
            let root = state.root.clone();
            let report = match tokio::task::spawn_blocking(move || {
                run_retention_once(&root, &workspace_dir, &policy, Utc::now())
            })
            .await
            {
                Ok(report) => report,
                Err(e) => {
                    tracing::warn!(error = %e, "任务产物保留巡检任务执行失败，跳过本轮");
                    continue;
                }
            };
            if report.deleted > 0 {
                tracing::info!(
                    scanned = report.scanned,
                    terminal = report.terminal,
                    deleted = report.deleted,
                    freed_bytes = report.freed_bytes,
                    "任务产物保留巡检完成"
                );
            }
        }
    });
}

/// 执行单轮清理（同步、可注入 `now`，便于单测覆盖）。
///
/// 返回本轮统计；`enabled == false` 或两个上限均为 `0`（不限）时直接返回
/// 零值报告，不扫描、不删除。
pub(crate) fn run_retention_once(
    root: &Path,
    workspace_dir: &Path,
    policy: &TasksRetentionConfig,
    now: DateTime<Utc>,
) -> RetentionReport {
    run_retention_once_with(root, workspace_dir, policy, now, |task_id| {
        crate::api::execute::execution::snapshot(task_id)
    })
}

/// [`run_retention_once`] 的可注入实现。
///
/// `memory_record` 查询内存注册表状态：生产路径固定使用
/// [`crate::api::execute::execution::snapshot`]；单测可注入构造的记录，覆盖
/// 「内存已改判 / 仍在运行」两类场景，无需改动进程级全局注册表。
fn run_retention_once_with(
    root: &Path,
    workspace_dir: &Path,
    policy: &TasksRetentionConfig,
    now: DateTime<Utc>,
    memory_record: impl Fn(&str) -> Option<TaskRecord>,
) -> RetentionReport {
    let mut report = RetentionReport::default();
    if !policy.enabled || (policy.max_age_days == 0 && policy.max_total_gb == 0) {
        return report;
    }

    let tasks_dir = workspace_dir.join("tasks");
    let entries = match std::fs::read_dir(&tasks_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return report,
        Err(e) => {
            tracing::warn!(path = %tasks_dir.display(), error = %e, "任务产物目录读取失败，跳过本轮清理");
            return report;
        }
    };

    // 候选：终态任务目录；total_bytes 统计全部任务目录（含运行中 / 孤儿目录），
    // 因为容量上限约束的是整个 workspace/tasks 的磁盘占用。
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut total_bytes: u64 = 0;
    // 不可判定终态的目录（运行中 / 排队中、注册表缺失或损坏）：计入 total_bytes，
    // 但不参与清理；容量超限时用于告警，避免「清完仍超限」静默失败。
    let mut undeletable_count: usize = 0;
    let mut undeletable_bytes: u64 = 0;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // 只处理真实子目录；符号链接/联接点一律跳过（避免误删链接目标）
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let id = id.to_owned();
        report.scanned += 1;

        let size_bytes = dir_size(&path);
        total_bytes = total_bytes.saturating_add(size_bytes);

        let Some(record) = load_record(root, &id) else {
            tracing::debug!(task_id = %id, "无任务注册表记录，跳过该目录（无法证明终态）");
            undeletable_count += 1;
            undeletable_bytes = undeletable_bytes.saturating_add(size_bytes);
            continue;
        };
        if record.id != id {
            tracing::warn!(
                dir = %id,
                record_id = %record.id,
                "任务目录名与注册表记录 id 不一致，跳过该目录"
            );
            undeletable_count += 1;
            undeletable_bytes = undeletable_bytes.saturating_add(size_bytes);
            continue;
        }
        // 磁盘记录非终态时，仅当内存注册表已把它改判为「重启中断的 failed」
        // 才视为终态候选；真正 running/queued 的任务内存状态未变，不会命中。
        let memory = if record.status.is_terminal() {
            None
        } else {
            memory_record(&id)
        };
        let Some(record) = terminal_candidate(record, memory) else {
            undeletable_count += 1;
            undeletable_bytes = undeletable_bytes.saturating_add(size_bytes);
            continue;
        };
        report.terminal += 1;
        candidates.push(Candidate {
            id,
            pipeline_id: record.pipeline_id,
            status: record.status,
            path,
            completed_at: record.finished_at.unwrap_or(record.started_at),
            size_bytes,
        });
    }

    // 最旧优先：完成时间升序（缺失时回退 started_at，见上）
    candidates.sort_by_key(|candidate| candidate.completed_at);

    // 1) 超龄淘汰
    let mut kept: Vec<Candidate> = Vec::new();
    if policy.max_age_days > 0 {
        let max_age = chrono::Duration::seconds(i64::from(policy.max_age_days) * 86_400);
        let cutoff = now - max_age;
        for candidate in candidates {
            if candidate.completed_at < cutoff {
                if cleanup_task(root, &candidate, REASON_MAX_AGE, now) {
                    total_bytes = total_bytes.saturating_sub(candidate.size_bytes);
                    report.deleted += 1;
                    report.freed_bytes = report.freed_bytes.saturating_add(candidate.size_bytes);
                }
            } else {
                kept.push(candidate);
            }
        }
    } else {
        kept = candidates;
    }

    // 2) 总容量淘汰：从最旧开始删，直至满足上限或无可删任务
    if policy.max_total_gb > 0 {
        let limit_bytes = u64::from(policy.max_total_gb) * 1024 * 1024 * 1024;
        let mut index = 0;
        while total_bytes > limit_bytes && index < kept.len() {
            let candidate = &kept[index];
            if cleanup_task(root, candidate, REASON_MAX_TOTAL, now) {
                total_bytes = total_bytes.saturating_sub(candidate.size_bytes);
                report.deleted += 1;
                report.freed_bytes = report.freed_bytes.saturating_add(candidate.size_bytes);
                kept.remove(index);
            } else {
                index += 1;
            }
        }
        if total_bytes > limit_bytes {
            if undeletable_count > 0 {
                tracing::warn!(
                    total_bytes,
                    limit_bytes,
                    undeletable_count,
                    undeletable_bytes,
                    "容量上限未达成：存在不可删除目录（非终态、注册表缺失或损坏），其占用计入总量但不参与清理"
                );
            } else {
                tracing::debug!(
                    total_bytes,
                    limit_bytes,
                    "终态任务已清空仍超出容量上限（上限过小或删除失败）"
                );
            }
        }
    }

    report
}

/// 待淘汰的终态任务目录。
struct Candidate {
    id: String,
    pipeline_id: String,
    status: TaskState,
    path: PathBuf,
    /// 完成时间（`finished_at` 缺失回退 `started_at`）
    completed_at: DateTime<Utc>,
    size_bytes: u64,
}

/// 判定任务目录是否可作为终态候选。
///
/// 磁盘记录本身终态时直接候选；非终态时仅接受「内存已改判」的情况：启动加载
/// 把上次退出时仍 queued/running 的任务改判为 `Failed`，并写入
/// [`INTERRUPTED_BY_RESTART_ERROR`] 作为特征。该改判只发生在内存（不覆盖磁盘
/// 原始状态），所以必须同时查内存注册表；真正运行中 / 排队中的任务内存状态仍
/// 是 Running/Queued，不会被当作终态。
fn terminal_candidate(
    record: TaskRecord,
    memory_record: Option<TaskRecord>,
) -> Option<TaskRecord> {
    if record.status.is_terminal() {
        return Some(record);
    }
    let memory = memory_record?;
    if memory.status == TaskState::Failed
        && memory.error.as_deref() == Some(INTERRUPTED_BY_RESTART_ERROR)
    {
        Some(memory)
    } else {
        None
    }
}

/// 删除整个任务目录（含产物）并写清理事件；失败仅告警返回 false。
///
/// F5（审计遗留合并项）：目录删除成功后**同步删除** `runtime/tasks/{id}.json`
/// 注册表记录——注册表是任务历史的唯一索引，目录已删而记录残留会让
/// GET /api/tasks 列出指向不存在目录的幽灵任务；删除记录失败仅告警，
/// **不回滚目录删除**（目录删除是磁盘回收的主语义，记录残留仅是可观测
/// 面的陈旧项，下一轮巡检不再触碰该 id——目录不存在时 `load_record`
/// 对应目录根本不会被扫描）。
fn cleanup_task(root: &Path, candidate: &Candidate, reason: &str, now: DateTime<Utc>) -> bool {
    match std::fs::remove_dir_all(&candidate.path) {
        Ok(()) => {
            // 演进 follow-up P2-1：内存注册表同步移除（此前只删盘上 JSON，
            // GET /api/tasks 与 metrics 在重启前仍见幽灵条目）
            crate::api::execute::execution::remove_task_record(&candidate.id);
            if let Err(e) = delete_registry_record(root, &candidate.id) {
                tracing::warn!(
                    task_id = %candidate.id,
                    error = %e,
                    "任务注册表记录删除失败（目录已删除，不回滚）"
                );
            }
            eventlog::append_event(
                root,
                &json!({
                    "ts": now.timestamp(),
                    "type": CLEANUP_EVENT_TYPE,
                    "task_id": candidate.id.as_str(),
                    "pipeline_id": candidate.pipeline_id.as_str(),
                    "status": candidate.status.as_str(),
                    "reason": reason,
                    "size_bytes": candidate.size_bytes,
                }),
            );
            tracing::info!(
                task_id = %candidate.id,
                reason,
                size_bytes = candidate.size_bytes,
                "已删除超限任务产物目录"
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                task_id = %candidate.id,
                path = %candidate.path.display(),
                error = %e,
                "任务产物目录删除失败，跳过"
            );
            false
        }
    }
}

/// 删除任务注册表记录（`runtime/tasks/{task_id}.json`）。
/// 记录本就不存在视为成功（幂等）；其余 IO 错误上抛由调用方告警。
fn delete_registry_record(root: &Path, task_id: &str) -> std::io::Result<()> {
    let path = root
        .join("runtime")
        .join("tasks")
        .join(format!("{task_id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// 读取任务注册表记录（`runtime/tasks/{task_id}.json`）；缺失/损坏返回 None。
///
/// 注意：**不能**用 [`ep_core::task_registry::TaskRegistry::load`]——它会把
/// 上次进程退出时仍处于 queued/running 的任务改判 failed，而本清理器必须以
/// 磁盘上的原始状态判定终态，运行中任务绝不能被误删。启动改判只发生在内存
/// 注册表，故非终态记录由 [`terminal_candidate`] 结合内存快照二次确认。
fn load_record(root: &Path, task_id: &str) -> Option<TaskRecord> {
    let path = root
        .join("runtime")
        .join("tasks")
        .join(format!("{task_id}.json"));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "任务注册表记录读取失败，跳过该目录");
            return None;
        }
    };
    match serde_json::from_str::<TaskRecord>(&text) {
        Ok(record) => Some(record),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "任务注册表记录解析失败，跳过该目录");
            None
        }
    }
}

/// 递归统计目录内容大小（只计普通文件；读取失败的条目跳过）。
fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                if let Ok(metadata) = entry.metadata() {
                    total = total.saturating_add(metadata.len());
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MB: u64 = 1024 * 1024;

    /// 唯一临时 root（时间戳 + seq 双保险，避免并行测试互扰）
    fn unique_root(tag: &str) -> PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "ep-retention-{tag}-{}-{seq}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// 创建任务目录（含 `files/` 产物文件，逻辑大小 `artifact_bytes`；
    /// `set_len` 不实际写盘，测试大容量淘汰时仍快速且省磁盘）。
    fn create_task_dir(workspace: &Path, id: &str, artifact_bytes: u64) -> PathBuf {
        let dir = workspace.join("tasks").join(id);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let file = std::fs::File::create(dir.join("files").join("output.bin")).unwrap();
        file.set_len(artifact_bytes).unwrap();
        dir
    }

    /// 写入任务注册表记录（形状与 `TaskRegistry::persist_one` 落盘一致）。
    fn write_record(
        root: &Path,
        id: &str,
        pipeline_id: &str,
        status: TaskState,
        started_at: DateTime<Utc>,
        finished_at: Option<DateTime<Utc>>,
        work_dir: &Path,
    ) {
        let dir = root.join("runtime").join("tasks");
        std::fs::create_dir_all(&dir).unwrap();
        let mut record = json!({
            "id": id,
            "pipeline_id": pipeline_id,
            "status": status,
            "started_at": started_at,
            "work_dir": work_dir,
            "node_order": [],
            "nodes": {},
            "artifacts": {},
            "served_artifacts": {},
        });
        if let Some(finished_at) = finished_at {
            record["finished_at"] = json!(finished_at);
        }
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_string_pretty(&record).unwrap(),
        )
        .unwrap();
    }

    fn cleanup_events(root: &Path) -> Vec<serde_json::Value> {
        eventlog::read_events(root, None, Some(CLEANUP_EVENT_TYPE), 100)
    }

    /// 构造内存注册表记录（不落盘），用于向 `run_retention_once_with` 注入
    /// 「启动改判 / 运行中」两类状态。
    fn memory_record(
        id: &str,
        status: TaskState,
        started_at: DateTime<Utc>,
        finished_at: Option<DateTime<Utc>>,
        error: Option<&str>,
    ) -> TaskRecord {
        let mut value = json!({
            "id": id,
            "pipeline_id": "pipeline-retention-test",
            "status": status,
            "started_at": started_at,
            "work_dir": std::env::temp_dir(),
            "node_order": [],
            "nodes": {},
            "artifacts": {},
            "served_artifacts": {},
        });
        if let Some(finished_at) = finished_at {
            value["finished_at"] = json!(finished_at);
        }
        if let Some(error) = error {
            value["error"] = json!(error);
        }
        serde_json::from_value(value).expect("构造内存任务记录应成功")
    }

    /// 验收 ①：3 个超龄终态任务 + 1 个 running，`max_age_days = 1` →
    /// 扫后仅 running 存活，且事件日志恰好 3 条。
    #[test]
    fn age_sweep_deletes_only_old_terminal_tasks() {
        let root = unique_root("age");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let three_days_ago = now - chrono::Duration::days(3);

        for id in ["old-a", "old-b", "old-c"] {
            let dir = create_task_dir(&workspace, id, 1024);
            write_record(
                &root,
                id,
                "pipeline-age",
                TaskState::Completed,
                now - chrono::Duration::days(4),
                Some(three_days_ago),
                &dir,
            );
        }
        let running_dir = create_task_dir(&workspace, "running-task", 2048);
        write_record(
            &root,
            "running-task",
            "pipeline-running",
            TaskState::Running,
            three_days_ago,
            None,
            &running_dir,
        );

        let policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 1,
            max_total_gb: 0,
        };
        let report = run_retention_once(&root, &workspace, &policy, now);

        assert_eq!(report.scanned, 4);
        assert_eq!(report.terminal, 3);
        assert_eq!(report.deleted, 3);
        assert_eq!(report.freed_bytes, 3 * 1024);
        for id in ["old-a", "old-b", "old-c"] {
            assert!(
                !workspace.join("tasks").join(id).exists(),
                "超龄终态任务目录应被删除（含产物）"
            );
        }
        assert!(running_dir.exists(), "运行中任务目录绝不能被删除");

        let events = cleanup_events(&root);
        assert_eq!(events.len(), 3, "每个被删任务应写一条清理事件");
        assert!(events.iter().all(|e| e["reason"] == REASON_MAX_AGE));
        assert!(events.iter().all(|e| e["type"] == CLEANUP_EVENT_TYPE));
        assert!(events.iter().all(|e| e["status"] == "completed"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 验收 ②：`max_total_gb` 超限时按完成时间从最旧开始淘汰。
    #[test]
    fn total_limit_evicts_oldest_terminal_tasks_first() {
        let root = unique_root("total");
        let workspace = root.join("workspace");
        let now = Utc::now();
        // 3 × 400 MiB = 1.2 GiB > 1 GiB 上限：删最旧一个后剩 800 MiB，满足上限
        let tasks = [
            ("oldest", now - chrono::Duration::days(3)),
            ("middle", now - chrono::Duration::days(2)),
            ("newest", now - chrono::Duration::days(1)),
        ];
        for (id, finished_at) in tasks {
            let dir = create_task_dir(&workspace, id, 400 * MB);
            write_record(
                &root,
                id,
                "pipeline-total",
                TaskState::Completed,
                finished_at,
                Some(finished_at),
                &dir,
            );
        }

        let policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 0,
            max_total_gb: 1,
        };
        let report = run_retention_once(&root, &workspace, &policy, now);

        assert_eq!(report.deleted, 1, "删除最旧一个即满足 1 GiB 上限");
        assert_eq!(report.freed_bytes, 400 * MB);
        assert!(!workspace.join("tasks").join("oldest").exists());
        assert!(workspace.join("tasks").join("middle").exists());
        assert!(workspace.join("tasks").join("newest").exists());

        let events = cleanup_events(&root);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["task_id"], "oldest");
        assert_eq!(events[0]["reason"], REASON_MAX_TOTAL);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 验收 ③：`enabled = false` 时零删除、零事件。
    #[test]
    fn disabled_policy_deletes_nothing() {
        let root = unique_root("disabled");
        let workspace = root.join("workspace");
        let now = Utc::now();
        for id in ["stale-a", "stale-b"] {
            let dir = create_task_dir(&workspace, id, 4096);
            write_record(
                &root,
                id,
                "pipeline-disabled",
                TaskState::Completed,
                now - chrono::Duration::days(30),
                Some(now - chrono::Duration::days(30)),
                &dir,
            );
        }

        let policy = TasksRetentionConfig {
            enabled: false,
            max_age_days: 1,
            max_total_gb: 1,
        };
        let report = run_retention_once(&root, &workspace, &policy, now);

        assert_eq!(report, RetentionReport::default());
        assert!(workspace.join("tasks").join("stale-a").exists());
        assert!(workspace.join("tasks").join("stale-b").exists());
        assert!(cleanup_events(&root).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 安全边界：queued 任务与无注册表记录的孤儿目录永不删除。
    #[test]
    fn queued_and_unknown_task_dirs_are_never_deleted() {
        let root = unique_root("safe");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let long_ago = now - chrono::Duration::days(30);

        let queued_dir = create_task_dir(&workspace, "queued-task", 1024);
        write_record(
            &root,
            "queued-task",
            "pipeline-queued",
            TaskState::Queued,
            long_ago,
            None,
            &queued_dir,
        );
        let orphan_dir = create_task_dir(&workspace, "orphan-task", 1024);

        let policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 1,
            max_total_gb: 1,
        };
        let report = run_retention_once(&root, &workspace, &policy, now);

        assert_eq!(report.scanned, 2);
        assert_eq!(report.terminal, 0);
        assert_eq!(report.deleted, 0);
        assert!(queued_dir.exists(), "排队任务目录绝不能被删除");
        assert!(orphan_dir.exists(), "无注册表记录的目录必须保守跳过");
        assert!(cleanup_events(&root).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// P1 回归：磁盘记录仍为 running（模拟崩溃遗留），但内存注册表已在启动
    /// 加载时改判 failed（错误文本为重启中断特征）→ 应纳入终态候选并按年龄清理。
    #[test]
    fn restart_interrupted_task_is_cleaned_by_age() {
        let root = unique_root("interrupted");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let started_at = now - chrono::Duration::days(10);
        let interrupted_at = now - chrono::Duration::days(2);
        let dir = create_task_dir(&workspace, "interrupted-task", 1024);
        write_record(
            &root,
            "interrupted-task",
            "pipeline-interrupted",
            TaskState::Running,
            started_at,
            None,
            &dir,
        );

        let policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 1,
            max_total_gb: 0,
        };
        let report = run_retention_once_with(&root, &workspace, &policy, now, |task_id| {
            match task_id {
                "interrupted-task" => Some(memory_record(
                    "interrupted-task",
                    TaskState::Failed,
                    started_at,
                    Some(interrupted_at),
                    Some(INTERRUPTED_BY_RESTART_ERROR),
                )),
                _ => None,
            }
        });

        assert_eq!(report.scanned, 1);
        assert_eq!(report.terminal, 1);
        assert_eq!(report.deleted, 1);
        assert!(!dir.exists(), "重启中断遗留任务应按 failed 终态被清理");

        let events = cleanup_events(&root);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["status"], "failed");
        assert_eq!(events[0]["reason"], REASON_MAX_AGE);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 安全边界：内存注册表仍是 running / queued 的任务，即使磁盘记录较旧、
    /// 占用超过容量上限，也绝不能被删除。
    #[test]
    fn memory_running_and_queued_tasks_are_never_deleted() {
        let root = unique_root("live");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let started_at = now - chrono::Duration::days(30);
        let running_dir = create_task_dir(&workspace, "live-running", 1024);
        write_record(
            &root,
            "live-running",
            "pipeline-live",
            TaskState::Running,
            started_at,
            None,
            &running_dir,
        );
        let queued_dir = create_task_dir(&workspace, "live-queued", 1024);
        write_record(
            &root,
            "live-queued",
            "pipeline-live",
            TaskState::Queued,
            started_at,
            None,
            &queued_dir,
        );

        let policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 1,
            max_total_gb: 1,
        };
        let report = run_retention_once_with(&root, &workspace, &policy, now, |task_id| {
            match task_id {
                "live-running" => Some(memory_record(
                    "live-running",
                    TaskState::Running,
                    started_at,
                    None,
                    None,
                )),
                "live-queued" => Some(memory_record(
                    "live-queued",
                    TaskState::Queued,
                    started_at,
                    None,
                    None,
                )),
                _ => None,
            }
        });

        assert_eq!(report.scanned, 2);
        assert_eq!(report.terminal, 0);
        assert_eq!(report.deleted, 0);
        assert!(running_dir.exists(), "运行中任务目录绝不能被删除");
        assert!(queued_dir.exists(), "排队任务目录绝不能被删除");
        assert!(cleanup_events(&root).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 改配置后下一轮按新约束收敛：先按容量删最旧，再把年龄上限调严继续删。
    #[test]
    fn changed_policy_converges_on_next_run() {
        let root = unique_root("reconfig");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let tasks = [
            ("oldest", now - chrono::Duration::days(10)),
            ("middle", now - chrono::Duration::days(8)),
            ("newest", now - chrono::Duration::hours(1)),
        ];
        for (id, finished_at) in tasks {
            let dir = create_task_dir(&workspace, id, 400 * MB);
            write_record(
                &root,
                id,
                "pipeline-reconfig",
                TaskState::Completed,
                finished_at,
                Some(finished_at),
                &dir,
            );
        }

        // 第一轮：只按容量（1 GiB）→ 3 × 400 MiB 超限，删最旧一个即满足
        let capacity_policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 0,
            max_total_gb: 1,
        };
        let first = run_retention_once(&root, &workspace, &capacity_policy, now);
        assert_eq!(first.deleted, 1);
        assert!(!workspace.join("tasks").join("oldest").exists());
        assert!(workspace.join("tasks").join("middle").exists());

        // 第二轮：只按年龄（1 天）→ 8 天前的 middle 被删，1 小时前的 newest 保留
        let age_policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 1,
            max_total_gb: 0,
        };
        let second = run_retention_once(&root, &workspace, &age_policy, now);
        assert_eq!(second.deleted, 1);
        assert!(!workspace.join("tasks").join("middle").exists());
        assert!(workspace.join("tasks").join("newest").exists());

        let events = cleanup_events(&root);
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| e["reason"] == REASON_MAX_TOTAL));
        assert!(events.iter().any(|e| e["reason"] == REASON_MAX_AGE));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `finished_at` 缺失时按 `started_at` 回退计算年龄。
    #[test]
    fn missing_finished_at_falls_back_to_started_at() {
        let root = unique_root("no-finished");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let started_at = now - chrono::Duration::days(5);
        let dir = create_task_dir(&workspace, "no-finished", 1024);
        write_record(
            &root,
            "no-finished",
            "pipeline-no-finished",
            TaskState::Completed,
            started_at,
            None,
            &dir,
        );

        let policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 1,
            max_total_gb: 0,
        };
        let report = run_retention_once(&root, &workspace, &policy, now);

        assert_eq!(report.terminal, 1);
        assert_eq!(report.deleted, 1);
        assert!(!dir.exists(), "缺失 finished_at 时应按 started_at 判定超龄");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 注册表记录损坏时保守跳过（不删除、不写事件）。
    #[test]
    fn corrupt_registry_record_is_skipped() {
        let root = unique_root("corrupt");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let dir = create_task_dir(&workspace, "corrupt-task", 1024);
        let record_dir = root.join("runtime").join("tasks");
        std::fs::create_dir_all(&record_dir).unwrap();
        std::fs::write(record_dir.join("corrupt-task.json"), "{not-json").unwrap();

        let policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 1,
            max_total_gb: 1,
        };
        let report = run_retention_once(&root, &workspace, &policy, now);

        assert_eq!(report.scanned, 1);
        assert_eq!(report.terminal, 0);
        assert_eq!(report.deleted, 0);
        assert!(dir.exists(), "注册表损坏的目录必须保守跳过");
        assert!(cleanup_events(&root).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── F5：目录删除成功后同步删除注册表记录 ──────────────────────────────

    /// F5 验收 ①：超龄任务目录删除成功后，`runtime/tasks/{id}.json` 记录
    /// 一并消失（防幽灵任务残留），其余任务的记录不受影响。
    #[test]
    fn cleanup_removes_registry_record_with_task_dir() {
        let root = unique_root("f5-record");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let long_ago = now - chrono::Duration::days(30);

        let old_dir = create_task_dir(&workspace, "old-task", 1024);
        write_record(
            &root,
            "old-task",
            "pipeline-f5",
            TaskState::Completed,
            long_ago,
            Some(long_ago),
            &old_dir,
        );
        let keep_dir = create_task_dir(&workspace, "keep-task", 1024);
        write_record(
            &root,
            "keep-task",
            "pipeline-f5",
            TaskState::Completed,
            now,
            Some(now),
            &keep_dir,
        );

        let policy = TasksRetentionConfig {
            enabled: true,
            max_age_days: 1,
            max_total_gb: 0,
        };
        let report = run_retention_once(&root, &workspace, &policy, now);
        assert_eq!(report.deleted, 1);

        // 目录删除成功 → 注册表记录同步消失
        assert!(!old_dir.exists());
        assert!(
            !root.join("runtime").join("tasks").join("old-task.json").exists(),
            "F5：任务目录删除后注册表记录必须一并删除"
        );
        // 未被清理的任务记录原样保留
        assert!(keep_dir.exists());
        assert!(
            root.join("runtime").join("tasks").join("keep-task.json").exists(),
            "未清理任务的注册表记录不得误删"
        );
        // 清理事件照常
        let events = cleanup_events(&root);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["task_id"], "old-task");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// F5 验收 ②：注册表记录删除失败仅告警，**不回滚目录删除**——
    /// cleanup_task 仍返回成功语义（目录已删、事件照写）。
    ///
    /// 手法：把 `runtime/tasks/{id}.json` 造成**目录**——`remove_file` 对
    /// 目录必然失败（Windows ACCESS_DENIED / POSIX EISDIR），而 cleanup_task
    /// 只删记录不读内容，构造不影响候选判定。
    #[test]
    fn registry_record_delete_failure_does_not_block_dir_deletion() {
        let root = unique_root("f5-stuck");
        let workspace = root.join("workspace");
        let now = Utc::now();
        let long_ago = now - chrono::Duration::days(30);

        let dir = create_task_dir(&workspace, "stuck-task", 1024);
        // 注册表"记录"造成目录 → 删除必失败
        let record_path = root.join("runtime").join("tasks").join("stuck-task.json");
        std::fs::create_dir_all(&record_path).unwrap();

        let candidate = Candidate {
            id: "stuck-task".to_string(),
            pipeline_id: "pipeline-f5".to_string(),
            status: TaskState::Completed,
            path: dir.clone(),
            completed_at: long_ago,
            size_bytes: 1024,
        };

        // cleanup_task 直接调用（私有函数，模块内测试可达）
        let ok = cleanup_task(&root, &candidate, REASON_MAX_AGE, now);
        assert!(ok, "记录删除失败不得改变目录删除的成功语义");
        assert!(!dir.exists(), "任务目录必须已被删除");
        assert!(
            record_path.is_dir(),
            "删不掉的记录仍留在原地（仅告警，不回滚）"
        );
        // 清理事件照常写入
        let events = cleanup_events(&root);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["task_id"], "stuck-task");

        std::fs::remove_dir_all(&record_path).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}
