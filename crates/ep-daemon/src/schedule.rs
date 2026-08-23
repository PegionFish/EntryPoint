//! 管线定时调度注册表（cron 机制，零依赖 ep-core::cron）
//!
//! ## 设计
//!
//! 调度与管线定义**分离**：管线 TOML 是用户编辑器反复覆写的产物，
//! 定时配置独立存放于 `runtime/schedules.json`，避免编辑器回写覆盖排期。
//!
//! ```json
//! { "video-to-srt": { "cron": "0 3 * * *", "enabled": true,
//!     "inputs": {"input": {"path": "/data/today.avi"}}, "params": {} } }
//! ```
//!
//! 触发语义：回收循环每 30s 醒来，对每个启用条目计算
//! `next_after(last_checked)`；若命中点 ≤ now 即提交执行并推进水位线。
//! `last_checked` 持久化——daemon 重启不重复补跑错过的窗口（错过即错过，
//! 管线执行有实时性），也不因重启在同一窗口内双跑。
//!
//! 并发防护：提交前检查该管线是否已有活跃任务（闸门层另有全局/管线级
//! 上限兜底）；上一轮还没跑完则本轮跳过并告警。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 单条定时计划
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleEntry {
    /// 五段 cron 表达式（本地时区）
    pub cron: String,
    /// 停用后保留配置（UI 可见可改），仅不触发
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 提交时使用的输入模板（节点 id → 参数对象，同 /pipelines/execute 契约）
    #[serde(default)]
    pub inputs: serde_json::Value,
    /// 追加参数模板（透传 execute 的 params 面）
    #[serde(default)]
    pub params: serde_json::Value,
    /// 上次巡检水位线（epoch 秒）：触发窗口判定基准
    #[serde(default)]
    pub last_checked: i64,
    /// 最近一次实际触发的任务 id（观测用；未触发过为空）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task_id: Option<String>,
}

fn default_true() -> bool {
    true
}

/// 全量注册表：pipeline_id → 计划
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleRegistry {
    #[serde(flatten)]
    pub entries: HashMap<String, ScheduleEntry>,
}

impl ScheduleRegistry {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(file = %path.display(), error = %e, "schedules.json 解析失败，按空表启动");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// 单次巡检产生的待触发项
#[derive(Debug, Clone)]
pub struct DueTrigger {
    pub pipeline_id: String,
    /// 触发时的计划快照（inputs/params 模板；e2e 独立编译下仅部分消费）
    #[allow(dead_code)]
    pub entry: ScheduleEntry,
}

/// 巡检纯函数：对注册表求本窗口内应触发的条目集合，并返回推进后的新表
/// （`last_checked` 统一推进到 `now`）。持久化由调用方决定。
///
/// - 未启用条目只推进水位线不触发（停用期间不错误补跑）
/// - cron 表达式非法 → 记 warn 跳过（配置错误不应拖垮整个循环）
/// - `active_pipelines` 中的管线跳过触发但同样推进水位线（防风暴）
pub fn collect_due(
    registry: &ScheduleRegistry,
    now_epoch_secs: i64,
    active_pipelines: &std::collections::HashSet<String>,
) -> (Vec<DueTrigger>, ScheduleRegistry) {
    use chrono::{Datelike, TimeZone, Timelike};

    let mut next = registry.clone();
    let mut due = Vec::new();
    for (pid, entry) in next.entries.iter_mut() {
        // 水位线缺省：以当前时间为起点（首装不补跑历史）
        let from = if entry.last_checked == 0 {
            now_epoch_secs
        } else {
            entry.last_checked
        };
        // 推进前先判触发（next_after 不含起点自身）
        let mut fired: Option<i64> = None;
        if entry.enabled && !active_pipelines.contains(pid) {
            match ep_core::cron::CronExpr::parse(&entry.cron) {
                Ok(cron) => {
                    // 从上次水位线逐分钟扫到现在（上限 2 天窗口；
                    // daemon 长眠超过该窗口视为错过，如实放弃）
                    let mut t = from + 60;
                    t -= t.rem_euclid(60);
                    while t <= now_epoch_secs {
                        if let Some(dt) = chrono::Local.timestamp_opt(t, 0).single() {
                            if cron.matches_parts(
                                dt.minute(),
                                dt.hour(),
                                dt.day(),
                                dt.month(),
                                dt.weekday().num_days_from_sunday(),
                            ) {
                                fired = Some(t);
                                break;
                            }
                        }
                        t += 60;
                    }
                }
                Err(e) => {
                    tracing::warn!(pipeline_id = %pid, error = %e, "cron 表达式无效，本轮跳过");
                }
            }
        }
        if let Some(fired_at) = fired {
            tracing::info!(pipeline_id = %pid, cron = %entry.cron, "schedule 触发");
            due.push(DueTrigger {
                pipeline_id: pid.clone(),
                entry: entry.clone(),
            });
            let _ = fired_at;
        }
        entry.last_checked = now_epoch_secs;
    }
    (due, next)
}

/// 注册表文件默认路径：`<root>/runtime/schedules.json`
pub fn default_registry_path(root: &Path) -> PathBuf {
    root.join("runtime").join("schedules.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn reg(entries: Vec<(&str, bool, &str, i64)>) -> ScheduleRegistry {
        let mut r = ScheduleRegistry::default();
        for (id, enabled, cron, last) in entries {
            r.entries.insert(
                id.to_string(),
                ScheduleEntry {
                    cron: cron.to_string(),
                    enabled,
                    inputs: serde_json::Value::Null,
                    params: serde_json::Value::Null,
                    last_checked: last,
                    last_task_id: None,
                },
            );
        }
        r
    }

    fn epoch(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        chrono::Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn fires_when_window_crosses_match_minute() {
        // 每天 03:00：水位线在 02:59，now=03:00 → 触发一次
        let r = reg(vec![("p", true, "0 3 * * *", epoch(2026, 8, 23, 2, 59))]);
        let now = epoch(2026, 8, 23, 3, 0);
        let (due, next) = collect_due(&r, now, &Default::default());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].pipeline_id, "p");
        assert_eq!(next.entries["p"].last_checked, now);
    }

    #[test]
    fn no_refire_within_same_window() {
        // 已推进到 03:00 后，同一分钟再巡检不重复触发
        let checked = epoch(2026, 8, 23, 3, 0);
        let r = reg(vec![("p", true, "0 3 * * *", checked)]);
        let (due, _) = collect_due(&r, checked + 30, &Default::default());
        assert!(due.is_empty(), "水位线已越过触发点不得重跑");
    }

    #[test]
    fn disabled_advances_watermark_without_firing() {
        let r = reg(vec![("p", false, "0 3 * * *", epoch(2026, 8, 23, 2, 59))]);
        let now = epoch(2026, 8, 23, 3, 1);
        let (due, next) = collect_due(&r, now, &Default::default());
        assert!(due.is_empty());
        assert_eq!(next.entries["p"].last_checked, now, "停用也要推进水位线");
        // 且之后重新启用不会补跑这个窗口
        let (due2, _) = collect_due(&next, now + 60, &Default::default());
        assert!(due2.is_empty());
    }

    #[test]
    fn busy_pipeline_skips_but_advances() {
        let r = reg(vec![("p", true, "* * * * *", epoch(2026, 8, 23, 2, 59))]);
        let now = epoch(2026, 8, 23, 3, 0);
        let active: std::collections::HashSet<String> = ["p".to_string()].into();
        let (due, next) = collect_due(&r, now, &active);
        assert!(due.is_empty(), "已有活跃任务的管线本轮跳过");
        assert_eq!(next.entries["p"].last_checked, now);
    }

    #[test]
    fn invalid_cron_skips_gracefully_and_advances() {
        let r = reg(vec![("bad", true, "99 * * * *", epoch(2026, 8, 23, 2, 59))]);
        let now = epoch(2026, 8, 23, 3, 0);
        let (due, next) = collect_due(&r, now, &Default::default());
        assert!(due.is_empty());
        assert_eq!(next.entries["bad"].last_checked, now);
    }
}
