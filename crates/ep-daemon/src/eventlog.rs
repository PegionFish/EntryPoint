//! 统一事件日志设施（PLAN_TRIGGER_UNIFIED_LOG §4.2/§5.7）。
//!
//! ## 存储
//!
//! `runtime/logs/events-YYYY-MM.jsonl`，**单行 JSON 追加**（`write_all` + 换行 +
//! flush），按月滚动（本地时区）；目录自动创建。std::fs 同步实现——调用方
//! （watcher 巡检循环 / 任务终态收尾 / 日志清理循环）均在阻塞不敏感路径。
//!
//! ## 事件形状（公共字段 `ts`（epoch 秒）、`type`）
//!
//! | type | 字段 | 写入方 |
//! |---|---|---|
//! | `watcher_trigger` | rule, file, task_id?, status, detail? | watcher 巡检循环 |
//! | `task_terminal` | task_id, pipeline_id, status, error? | [`write_task_terminal`]（execution.rs finalize 统一出口接入） |
//!
//! ## 读取
//!
//! [`read_events`] 从最新月份文件向前读（文件名字典序即时间序），文件内倒序；
//! 容忍尾行不完整 / 损坏行（解析失败跳过）；按 `rule` / `type` 过滤、截断 limit。
//!
//! ## 保留策略
//!
//! [`cleanup_expired_logs`] 删除 `runtime/logs/` 下 mtime 超期的 `events-*.jsonl`
//! 与模块日志（`*.log`，与既有 runtime/logs 文件形状一致）；`retention_days == 0`
//! 表示永久保留（跳过，返回 0）。巡检循环（main.rs，1h tick）按
//! `config.general.log_retention_days` 调用。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// 事件日志目录：`<root>/runtime/logs/`（事件日志与模块日志共用）
pub(crate) fn logs_dir(root: &Path) -> PathBuf {
    root.join("runtime").join("logs")
}

/// 本地时区按月滚动的事件文件名：`events-YYYY-MM.jsonl`
fn month_file_name(ts: i64) -> String {
    use chrono::{Datelike, TimeZone};
    let dt = chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .unwrap_or_else(chrono::Local::now);
    format!("events-{:04}-{:02}.jsonl", dt.year(), dt.month())
}

/// 追加一条事件（单行 JSON）。`event` 须含公共字段 `ts`（epoch 秒）/`type`；
/// 缺 `ts` 时按当前时间归档到当月文件。失败仅告警（best-effort，不阻断调用方）。
pub(crate) fn append_event(root: &Path, event: &Value) {
    let ts = event
        .get("ts")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let dir = logs_dir(root);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(root = %root.display(), error = %e, "事件日志目录创建失败");
        return;
    }
    let path = dir.join(month_file_name(ts));
    let line = event.to_string();
    let open = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    let mut file = match open {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "事件日志文件打开失败");
            return;
        }
    };
    // 行与换行符合并为单次 write_all：两次写入之间可被并发追加方插入
    // 完整行，导致两行粘连成非法 JSON（评审 R2-5）
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(&line);
    buf.push('\n');
    if let Err(e) = file.write_all(buf.as_bytes()).and_then(|_| file.flush()) {
        tracing::warn!(path = %path.display(), error = %e, "事件日志写入失败");
    }
}

/// 倒序读取事件（最新在前）。
///
/// 从最新月份文件向前读（文件名字典序倒序），文件内逐行倒序；解析失败的行
/// （尾行不完整 / 损坏）跳过；`rule` / `type` 为 Some（非空）时按事件对应字段
/// 精确过滤；累计 `limit` 条即返回。目录不存在 / 文件不可读按空处理。
pub(crate) fn read_events(
    root: &Path,
    rule: Option<&str>,
    type_: Option<&str>,
    limit: usize,
) -> Vec<Value> {
    if limit == 0 {
        return Vec::new();
    }
    let rule = rule.filter(|r| !r.is_empty());
    let type_ = type_.filter(|t| !t.is_empty());

    let dir = logs_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_event_log_file(p))
        .collect();
    // 文件名 events-YYYY-MM.jsonl：零填充月份字典序即时间序，倒序 = 最新在前
    files.sort();
    files.reverse();

    let mut out: Vec<Value> = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines().rev() {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue; // 尾行不完整 / 损坏行：跳过
            };
            if type_.is_some_and(|t| event.get("type").and_then(Value::as_str) != Some(t)) {
                continue;
            }
            if rule.is_some_and(|r| event.get("rule").and_then(Value::as_str) != Some(r)) {
                continue;
            }
            out.push(event);
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

/// 是否为事件日志文件（`events-*.jsonl`；不含普通模块日志）
fn is_event_log_file(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("events-") && n.ends_with(".jsonl"))
}

/// 清理超期日志：删除 `runtime/logs/` 下 mtime 早于 `now - retention_days`
/// 的 `events-*.jsonl` 与模块日志文件（`*.log`，与既有 runtime/logs 文件形状
/// 一致，如 daemon.log）。`retention_days == 0` = 永久保留，跳过返回 0。
/// 返回删除的文件数。
pub(crate) fn cleanup_expired_logs(root: &Path, retention_days: u64) -> usize {
    if retention_days == 0 {
        return 0;
    }
    let dir = logs_dir(root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(retention_days.saturating_mul(86_400));
    let mut removed = 0;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_target = (name.starts_with("events-") && name.ends_with(".jsonl"))
            || name.ends_with(".log");
        if !is_target {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let expired = meta.modified().is_ok_and(|mtime| mtime < cutoff);
        if expired && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 写入 `watcher_trigger` 事件（§5.7：watcher 巡检循环消费）。
///
/// `status ∈ {submitted, rejected, archive_done, archive_skipped}`（字符串透传，
/// 不在本层枚举校验——写入方与前端契约以计划文档为准）。
pub(crate) fn write_watcher_trigger(
    root: &Path,
    rule: &str,
    file: &str,
    task_id: Option<&str>,
    status: &str,
    detail: Option<&str>,
) {
    let mut event = json!({
        "ts": chrono::Utc::now().timestamp(),
        "type": "watcher_trigger",
        "rule": rule,
        "file": file,
        "status": status,
    });
    if let Some(id) = task_id {
        event["task_id"] = json!(id);
    }
    if let Some(d) = detail {
        event["detail"] = json!(d);
    }
    append_event(root, &event);
}

/// 写入 `task_terminal` 事件（§5.7：任务终态统一出口，execution.rs
/// finalize_task 单点接入）。
///
/// `status ∈ {completed, failed, cancelled}`（由 [`ep_core::task_registry::TaskState::as_str`]
/// 产出）；`error` 为错误摘要（None / 空串时省略该字段）。
pub(crate) fn write_task_terminal(
    root: &Path,
    task_id: &str,
    pipeline_id: &str,
    status: &str,
    error: Option<&str>,
) {
    let mut event = json!({
        "ts": chrono::Utc::now().timestamp(),
        "type": "task_terminal",
        "task_id": task_id,
        "pipeline_id": pipeline_id,
        "status": status,
    });
    if let Some(err) = error.filter(|e| !e.is_empty()) {
        event["error"] = json!(err);
    }
    append_event(root, &event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// 唯一临时 root（时间戳 + seq 双保险，避免并行测试互扰）
    fn unique_root(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "ep-eventlog-{tag}-{}-{seq}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn local_epoch(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        chrono::Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    fn watcher_event(ts: i64, rule: &str, file: &str, status: &str) -> Value {
        json!({
            "ts": ts,
            "type": "watcher_trigger",
            "rule": rule,
            "file": file,
            "status": status,
        })
    }

    // ── 1. 按月文件名（本地时区） ─────────────────────────────────────────

    #[test]
    fn month_file_name_uses_local_month() {
        assert_eq!(
            month_file_name(local_epoch(2026, 8, 15, 12, 0)),
            "events-2026-08.jsonl"
        );
        assert_eq!(
            month_file_name(local_epoch(2027, 1, 1, 0, 5)),
            "events-2027-01.jsonl"
        );
    }

    // ── 2. 追加格式：单行 JSON + 换行，目录自动创建 ────────────────────────

    #[test]
    fn append_creates_dir_and_writes_single_line_json() {
        let root = unique_root("append-format");
        let ts = local_epoch(2026, 8, 15, 12, 0);
        append_event(&root, &watcher_event(ts, "r1", "/a.mkv", "submitted"));
        append_event(&root, &watcher_event(ts + 5, "r1", "/b.mkv", "archive_done"));

        let path = logs_dir(&root).join("events-2026-08.jsonl");
        assert!(path.exists(), "目录与文件应自动创建");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "每事件恰占一行");
        for line in &lines {
            let v: Value = serde_json::from_str(line).expect("每行必须是合法单行 JSON");
            assert_eq!(v["type"], "watcher_trigger");
            assert!(v.get("ts").and_then(Value::as_i64).is_some());
        }
        assert_eq!(lines[0], lines[0].trim(), "行内不得有换行");
        assert!(text.ends_with('\n'), "以换行结尾");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 3. 按月滚动：不同月份 ts 落入不同文件 ─────────────────────────────

    #[test]
    fn append_rolls_by_month() {
        let root = unique_root("roll");
        // 两个 ts 按本地时区构造，必然分属 8 月 / 9 月
        append_event(&root, &watcher_event(local_epoch(2026, 8, 15, 12, 0), "r1", "/a", "submitted"));
        append_event(&root, &watcher_event(local_epoch(2026, 9, 15, 12, 0), "r1", "/b", "submitted"));

        assert!(logs_dir(&root).join("events-2026-08.jsonl").exists());
        assert!(logs_dir(&root).join("events-2026-09.jsonl").exists());
        assert_eq!(
            std::fs::read_to_string(logs_dir(&root).join("events-2026-08.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 4. 倒序读取（最新在前） ───────────────────────────────────────────

    #[test]
    fn read_events_returns_newest_first() {
        let root = unique_root("desc");
        let ts = local_epoch(2026, 8, 15, 12, 0);
        for (i, file) in ["a", "b", "c"].iter().enumerate() {
            append_event(&root, &watcher_event(ts + i as i64, "r1", file, "submitted"));
        }
        let events = read_events(&root, None, None, 100);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["file"], "c", "最新事件在前");
        assert_eq!(events[2]["file"], "a");

        // 跨月文件也按「新月份文件优先」读取
        append_event(&root, &watcher_event(local_epoch(2026, 9, 15, 12, 0), "r1", "/sep", "submitted"));
        let events = read_events(&root, None, None, 100);
        assert_eq!(events[0]["file"], "/sep", "跨月文件字典序倒序（新月份在前）");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 5. rule / type 过滤 ──────────────────────────────────────────────

    #[test]
    fn read_events_filters_rule_and_type() {
        let root = unique_root("filter");
        let ts = local_epoch(2026, 8, 15, 12, 0);
        append_event(&root, &watcher_event(ts, "r1", "/a", "submitted"));
        append_event(&root, &watcher_event(ts + 1, "r2", "/b", "submitted"));
        write_task_terminal(&root, "t1", "p1", "completed", None);

        // rule 过滤
        let by_rule = read_events(&root, Some("r1"), None, 100);
        assert_eq!(by_rule.len(), 1);
        assert_eq!(by_rule[0]["rule"], "r1");

        // type 过滤（task_terminal 无 rule 字段，rule 过滤下被排除）
        let terminals = read_events(&root, None, Some("task_terminal"), 100);
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["task_id"], "t1");

        // 组合过滤无命中
        assert!(read_events(&root, Some("r1"), Some("task_terminal"), 100).is_empty());

        // 空串过滤参数视为未过滤
        assert_eq!(read_events(&root, Some(""), Some(""), 100).len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 6. limit 截断（取最新 N 条） ─────────────────────────────────────

    #[test]
    fn read_events_respects_limit() {
        let root = unique_root("limit");
        let ts = local_epoch(2026, 8, 15, 12, 0);
        for i in 0..5 {
            append_event(&root, &watcher_event(ts + i, "r1", &format!("/f{i}"), "submitted"));
        }
        let events = read_events(&root, None, None, 2);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["file"], "/f4", "limit 取最新的 N 条");
        assert_eq!(events[1]["file"], "/f3");

        assert!(read_events(&root, None, None, 0).is_empty(), "limit=0 → 空");
        assert_eq!(read_events(&root, None, None, 100).len(), 5);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 7. 尾行不完整 / 损坏行容忍 ────────────────────────────────────────

    #[test]
    fn read_events_tolerates_corrupt_and_partial_lines() {
        let root = unique_root("corrupt");
        let ts = local_epoch(2026, 8, 15, 12, 0);
        append_event(&root, &watcher_event(ts, "r1", "/a", "submitted"));
        append_event(&root, &watcher_event(ts + 1, "r1", "/b", "submitted"));

        // 模拟写一半崩溃：追加一段带换行的损坏行 + 一条无换行的不完整尾行
        let path = logs_dir(&root).join("events-2026-08.jsonl");
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("{broken json\n");
        content.push_str(&format!("{{\"ts\": {}, \"type\": \"watcher_tr", ts + 2));
        std::fs::write(&path, content).unwrap();

        let events = read_events(&root, None, None, 100);
        assert_eq!(events.len(), 2, "损坏行必须被跳过，完好行全部可读");
        assert_eq!(events[0]["file"], "/b");
        assert_eq!(events[1]["file"], "/a");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 8. 清理超期（mtime 早于 now - days 的 events-*.jsonl 与 *.log） ──

    #[test]
    fn cleanup_removes_expired_event_and_module_logs() {
        let root = unique_root("cleanup");
        let logs = logs_dir(&root);
        std::fs::create_dir_all(&logs).unwrap();

        // 两个超期文件（事件日志 + 模块日志）+ 两个新鲜文件 + 一个不相关文件
        let old_event = logs.join("events-2026-01.jsonl");
        let old_module = logs.join("daemon.log");
        let fresh_event = logs.join("events-2026-08.jsonl");
        let fresh_module = logs.join("module.log");
        let unrelated = logs.join("fanout.json");
        for p in [&old_event, &old_module, &fresh_event, &fresh_module, &unrelated] {
            std::fs::write(p, b"x").unwrap();
        }

        // std::fs::FileTimes（Rust 1.75+）回写 mtime：老文件设为 200 天前
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(200 * 86_400);
        for p in [&old_event, &old_module] {
            let f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        }

        // 90 天保留：两个老文件被删，其余保留
        assert_eq!(cleanup_expired_logs(&root, 90), 2);
        assert!(!old_event.exists(), "超期事件日志应被删除");
        assert!(!old_module.exists(), "超期模块日志应被删除");
        assert!(fresh_event.exists(), "新鲜事件日志保留");
        assert!(fresh_module.exists(), "新鲜模块日志保留");
        assert!(unrelated.exists(), "非日志文件不受清理影响");

        // 再次清理（已无可删）→ 0
        assert_eq!(cleanup_expired_logs(&root, 90), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 9. retention_days == 0 = 永久保留 ────────────────────────────────

    #[test]
    fn cleanup_zero_retention_is_permanent() {
        let root = unique_root("zero");
        let logs = logs_dir(&root);
        std::fs::create_dir_all(&logs).unwrap();
        let event = logs.join("events-2026-01.jsonl");
        let module = logs.join("daemon.log");
        std::fs::write(&event, b"x").unwrap();
        std::fs::write(&module, b"x").unwrap();
        // mtime 极旧（一年前）——若 0 天仍参与比较将全部命中，此处断言不删除
        let ancient = std::time::SystemTime::now() - std::time::Duration::from_secs(365 * 86_400);
        for p in [&event, &module] {
            let f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(ancient)).unwrap();
        }

        assert_eq!(cleanup_expired_logs(&root, 0), 0, "0 = 永久保留，直接跳过");
        assert!(event.exists() && module.exists(), "任何文件都不得被删除");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 10. 目录缺失 / 空目录容错 ─────────────────────────────────────────

    #[test]
    fn read_and_cleanup_tolerate_missing_dir() {
        let root = unique_root("missing");
        assert!(read_events(&root, None, None, 10).is_empty(), "目录不存在 → 空列表");
        assert_eq!(cleanup_expired_logs(&root, 90), 0, "目录不存在 → 0");
    }

    // ── 11. write_task_terminal 形状（error 缺省省略 / 空串省略） ─────────

    #[test]
    fn task_terminal_event_shape() {
        let root = unique_root("terminal-shape");

        write_task_terminal(&root, "t1", "p1", "completed", None);
        write_task_terminal(&root, "t2", "p1", "failed", Some("node boom"));
        write_task_terminal(&root, "t3", "p1", "cancelled", Some(""));

        let events = read_events(&root, None, Some("task_terminal"), 100);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["task_id"], "t3", "倒序：最新在前");
        assert_eq!(events[0]["status"], "cancelled");
        assert!(events[0].get("error").is_none(), "空串 error 省略");
        assert_eq!(events[1]["task_id"], "t2");
        assert_eq!(events[1]["error"], "node boom");
        assert_eq!(events[2]["status"], "completed");
        assert!(events[2].get("error").is_none());
        assert_eq!(events[2]["pipeline_id"], "p1");
        let _ = std::fs::remove_dir_all(&root);
    }
}
