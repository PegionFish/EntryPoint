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
//! - 淘汰顺序按完成时间（`finished_at`，缺失时回退 `started_at`）升序：
//!   先删超龄任务（`max_age_days`），再按总容量（`max_total_gb`）从最旧开始删，
//!   直至满足上限；两个维度互相独立，`0` = 该维度不限。
//! - 删除即整个任务目录 `remove_dir_all`（**产物一并删除**）；成功写一条
//!   `task_retention_cleanup` 事件到统一事件日志（复用
//!   [`crate::eventlog::append_event`]，不另造日志文件/格式）。
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
use ep_core::task_registry::{TaskRecord, TaskState};
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
            continue;
        };
        if record.id != id {
            tracing::warn!(
                dir = %id,
                record_id = %record.id,
                "任务目录名与注册表记录 id 不一致，跳过该目录"
            );
            continue;
        }
        if !record.status.is_terminal() {
            continue;
        }
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
            tracing::debug!(
                total_bytes,
                limit_bytes,
                "终态任务已清空仍超出容量上限（非终态任务占用或上限过小）"
            );
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

/// 删除整个任务目录（含产物）并写清理事件；失败仅告警返回 false。
fn cleanup_task(root: &Path, candidate: &Candidate, reason: &str, now: DateTime<Utc>) -> bool {
    match std::fs::remove_dir_all(&candidate.path) {
        Ok(()) => {
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

/// 读取任务注册表记录（`runtime/tasks/{task_id}.json`）；缺失/损坏返回 None。
///
/// 注意：**不能**用 [`ep_core::task_registry::TaskRegistry::load`]——它会把
/// 上次进程退出时仍处于 queued/running 的任务改判 failed，而本清理器必须以
/// 磁盘上的原始状态判定终态，运行中任务绝不能被误删。
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
}
