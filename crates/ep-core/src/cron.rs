//! 极简 5 段 cron 解析与匹配（零依赖）
//!
//! 支持管线定时执行（`[modules]`-style 用户配置：`0 3 * * *` 等）。
//!
//! 语法（标准 cron 五段）：分 时 日 月 周
//! - `*` 任意
//! - `*/n` 步进
//! - `a-b` 范围
//! - `a,b,c` 列表（可与范围/步进组合，如 `1-5,10/2`）
//!
//! 语义要点：
//! - `day_or` 简化语义：日与周**都**受限时取「或」（vixie cron 经典行为），
//!   任一为 `*` 时取「且」——日常配置（只限日或只限周）直觉一致
//! - 匹配粒度 = 分钟；秒恒为 0

use std::fmt;

use chrono::{Datelike, Timelike};

/// 单字段集合：位集语义用 sorted Vec 承载（域小、可读性好）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSet {
    min: u32,
    max: u32,
    values: Vec<u32>,
}

impl FieldSet {
    /// 是否包含该值
    pub fn contains(&self, v: u32) -> bool {
        self.values.binary_search(&v).is_ok()
    }
}

impl fmt::Display for FieldSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let joined = self
            .values
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",");
        write!(f, "[{joined}]")
    }
}

/// 解析错误（含人读原因）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronError(pub String);

impl fmt::Display for CronError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cron 表达式无效: {}", self.0)
    }
}
impl std::error::Error for CronError {}

/// 五段 cron 表达式
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    pub minute: FieldSet,
    pub hour: FieldSet,
    pub day: FieldSet,
    pub month: FieldSet,
    pub weekday: FieldSet, // 0=周日 … 6=周六（7 视作 0）
}

impl CronExpr {
    /// 解析五段表达式；空白分隔，容忍多余空白
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(CronError(format!(
                "需要 5 段（分 时 日 月 周），实际 {} 段",
                parts.len()
            )));
        }
        Ok(Self {
            minute: parse_field(parts[0], 0, 59)?,
            hour: parse_field(parts[1], 0, 23)?,
            day: parse_field(parts[2], 1, 31)?,
            month: parse_field(parts[3], 1, 12)?,
            weekday: parse_field(&normalize_weekday(parts[4]), 0, 6)?,
        })
    }

    /// 给定本地时间分量是否命中（由调用方提供分解值，纯函数可测）
    pub fn matches_parts(
        &self,
        minute: u32,
        hour: u32,
        day: u32,
        month: u32,
        weekday: u32, // chrono.weekday().num_days_from_sunday()
    ) -> bool {
        if !self.minute.contains(minute)
            || !self.hour.contains(hour)
            || !self.month.contains(month)
        {
            return false;
        }
        let day_restricted = self.day.values.len() != (self.day.max - self.day.min + 1) as usize;
        let wd_restricted =
            self.weekday.values.len() != (self.weekday.max - self.weekday.min + 1) as usize;
        match (day_restricted, wd_restricted) {
            // vixie cron 经典 OR 语义：日与周均显式受限时任一命中即可
            (true, true) => self.day.contains(day) || self.weekday.contains(weekday),
            _ => self.day.contains(day) && self.weekday.contains(weekday),
        }
    }

    /// 从时间戳扫描下一次触发点（不含 now 自身）。上限 `max_lookahead_min`
    /// 分钟（防无解表达式死扫，如 2 月 30 日）；None = 窗口内无触发。
    pub fn next_after(&self, from_epoch_secs: i64, max_lookahead_min: u32) -> Option<i64> {
        use chrono::TimeZone;
        let tz = chrono::Local;
        let mut t = from_epoch_secs + 60;
        // 对齐到分钟边界
        t -= t.rem_euclid(60);
        for _ in 0..max_lookahead_min {
            let dt = tz.timestamp_opt(t, 0).single()?;
            if self.matches_parts(
                dt.minute(),
                dt.hour(),
                dt.day(),
                dt.month(),
                dt.weekday().num_days_from_sunday(),
            ) {
                return Some(t);
            }
            t += 60;
        }
        None
    }
}

fn normalize_weekday(field: &str) -> String {
    // 7 → 0（周日双写兼容）；逐字符替换会误伤 17/27，按 token 处理
    field
        .split(',')
        .map(|tok| {
            let tok = tok.trim();
            if tok == "7" { "0" } else { tok }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_field(raw: &str, min: u32, max: u32) -> Result<FieldSet, CronError> {
    let mut values = std::collections::BTreeSet::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(CronError(format!("空列表项: '{raw}'")));
        }
        // 步进：base/n
        let (base, step) = match part.split_once('/') {
            Some((b, s)) => {
                let n: u32 = s.parse().map_err(|_| CronError(format!("步进非法: '{s}'")))?;
                if n == 0 {
                    return Err(CronError("步进不能为 0".into()));
                }
                (b, Some(n))
            }
            None => (part, None),
        };
        let (lo, hi) = match base {
            "*" => (min, max),
            range if range.contains('-') => {
                let Some((a, b)) = range.split_once('-') else {
                    return Err(CronError(format!("范围非法: '{range}'")));
                };
                let lo: u32 = a.trim().parse().map_err(|_| CronError(format!("数值非法: '{a}'")))?;
                let hi: u32 = b.trim().parse().map_err(|_| CronError(format!("数值非法: '{b}'")))?;
                if lo > hi {
                    return Err(CronError(format!("范围上下界颠倒: '{range}'")));
                }
                (lo, hi)
            }
            single => {
                let v: u32 = single.parse().map_err(|_| CronError(format!("数值非法: '{single}'")))?;
                (v, v)
            }
        };
        if !(min..=max).contains(&lo) || !(min..=max).contains(&hi) {
            return Err(CronError(format!("值越界 [{min}-{max}]: '{part}'")));
        }
        let step_n = step.unwrap_or(1);
        let mut v = lo;
        while v <= hi {
            values.insert(v);
            v += step_n;
        }
    }
    let vals: Vec<u32> = values.into_iter().collect();
    if vals.is_empty() {
        return Err(CronError(format!("字段为空: '{raw}'")));
    }
    Ok(FieldSet { min, max, values: vals })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parse_and_match_daily_3am() {
        let c = CronExpr::parse("0 3 * * *").unwrap();
        assert!(c.matches_parts(0, 3, 15, 6, 1));
        assert!(!c.matches_parts(1, 3, 15, 6, 1));
        assert!(!c.matches_parts(0, 4, 15, 6, 1));
    }

    #[test]
    fn step_list_range_combinations() {
        let c = CronExpr::parse("*/15 8-18 1-5,10 * *").unwrap();
        assert!(c.matches_parts(0, 9, 3, 7, 3));
        assert!(c.matches_parts(45, 18, 10, 7, 3));
        assert!(!c.matches_parts(10, 9, 3, 7, 3)); // 分钟不在 */15 上
        assert!(!c.matches_parts(0, 7, 3, 7, 3)); // 小时越下界
        assert!(!c.matches_parts(0, 9, 7, 7, 3)); // 日不在集合
    }

    #[test]
    fn weekday_7_is_sunday() {
        let c = CronExpr::parse("0 12 * * 7").unwrap();
        assert_eq!(c.weekday.values, vec![0]);
    }

    #[test]
    fn day_week_or_semantics() {
        // 日+周同时受限 → OR（vixie 语义）：13 号周五 或 任意周五? —— 13 号或周五任一命中
        let c = CronExpr::parse("0 0 13 * 5").unwrap();
        assert!(c.matches_parts(0, 0, 13, 7, 1), "13 号即便非周五也命中");
        assert!(c.matches_parts(0, 0, 20, 7, 5), "周五即便非 13 号也命中");
        assert!(!c.matches_parts(0, 0, 20, 7, 3));
        // 仅日受限 → 与周通配求交
        let c2 = CronExpr::parse("0 0 1 * *").unwrap();
        assert!(c2.matches_parts(0, 0, 1, 9, 4));
        assert!(!c2.matches_parts(0, 0, 2, 9, 4));
    }

    #[test]
    fn rejects_malformed() {
        assert!(CronExpr::parse("* * * *").is_err(), "段数不足");
        assert!(CronExpr::parse("* * * * * *").is_err(), "段数超出");
        assert!(CronExpr::parse("60 * * * *").is_err(), "分钟越界");
        assert!(CronExpr::parse("*/0 * * * *").is_err(), "步进为零");
        assert!(CronExpr::parse("5-2 * * * *").is_err(), "范围颠倒");
        assert!(CronExpr::parse("a * * * *").is_err(), "非数值");
    }

    #[test]
    fn next_after_scans_forward() {
        // 每 15 分钟：从 10:07 起，下次应为 10:15
        let c = CronExpr::parse("*/15 * * * *").unwrap();
        let base = chrono::Local
            .with_ymd_and_hms(2026, 8, 23, 10, 7, 30)
            .single()
            .unwrap()
            .timestamp();
        let next = c.next_after(base, 60).unwrap();
        let dt = chrono::Local.timestamp_opt(next, 0).single().unwrap();
        assert_eq!((dt.hour(), dt.minute()), (10, 15));
    }
}
