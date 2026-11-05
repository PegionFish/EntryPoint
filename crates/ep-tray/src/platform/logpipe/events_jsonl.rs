//! 统一事件日志源（GT-4 第三源之一）：`runtime/logs/events-*.jsonl`。
//!
//! 语义（契约 §2.3）：
//! - **启动载入最近 N 行**（[`STARTUP_LINES`] = 200）——事件是结构化历史
//!   （任务终态等），与 daemon.log/WS 的「接入后开始」不同，UI 要有底；
//! - **随后跟随最新月文件追加**（同 byte offset + 半行 carry 手法，见
//!   file_tail.rs）；
//! - **按文件名取最新月文件**：`events-YYYY-MM.jsonl` 零填充月份，字典序 =
//!   时间序（沿 daemon eventlog.rs 既有注释约定），每次 poll 重扫目录——
//!   月滚动自动切换，切换按「新文件启动」重新载入最近 N 行；
//! - **坏行容错**：JSON 解析失败跳过 + 计数（[`EventsJsonlSource::bad_lines`]），
//!   空行静默跳过不计数。
//!
//! 行内容：原始 JSON 串原样入 ring（`line` 字段），`ts` 取 JSON 的 `ts`
//! 字段（Unix 秒，daemon eventlog.rs 写入格式）缺省用当前时刻；级别不
//! 解析（事件无级别语义）→ `None`，UI 层可按 JSON 字段再加工。

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::file_tail::MAX_CARRY;

/// 启动载入的最近行数（契约 §2.3：N=200）
pub const STARTUP_LINES: usize = 200;

/// 事件文件跟随轮询间隔（调度在集成层 LogPipe::spawn；与 tail 同量级）
pub const EVENTS_INTERVAL: Duration = Duration::from_millis(500);

/// 事件日志目录在 root 下的固定位置（沿 daemon 语义：`runtime/logs/`）
pub fn events_dir(root: &Path) -> PathBuf {
    root.join("runtime").join("logs")
}

/// 扫描目录取最新月文件（`events-*.jsonl`，文件名字典序最大 = 最新月）。
pub fn latest_events_file(dir: &Path) -> Option<PathBuf> {
    let mut names: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("events-") && n.ends_with(".jsonl"))
        })
        .collect();
    names.sort();
    names.pop()
}

/// 事件日志源。`poll` 单轮逻辑独立于线程调度，注入目录可测。
#[derive(Debug)]
pub struct EventsJsonlSource {
    dir: PathBuf,
    /// 当前跟随的文件（None = 尚未发现任何 events-*.jsonl）
    current: Option<CurrentFile>,
    /// JSON 解析失败行累计（pub 只读接口 [`EventsJsonlSource::bad_lines`]）
    bad_lines: u64,
}

#[derive(Debug)]
struct CurrentFile {
    path: PathBuf,
    /// 已消费到的字节位置
    offset: u64,
    /// 半行 carry（字节，同 file_tail）
    carry: Vec<u8>,
}

impl EventsJsonlSource {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            current: None,
            bad_lines: 0,
        }
    }

    /// 坏行（JSON 解析失败）累计计数
    pub fn bad_lines(&self) -> u64 {
        self.bad_lines
    }

    /// 单轮：发现/切换最新月文件（切换 = 重新载入最近 N 行）+ 跟随追加。
    /// 产出 (ts, 原始 JSON 行)。目录不存在/无事件文件 → 静默 0（红线：
    /// daemon 未起属常态）。
    pub fn poll(&mut self, out: &mut dyn FnMut(SystemTime, String)) -> usize {
        let latest = match latest_events_file(&self.dir) {
            Some(p) => p,
            None => {
                self.current = None;
                return 0;
            }
        };
        match &self.current {
            Some(c) if c.path == latest => {} // 同文件继续跟随
            _ => {
                // 首次发现或月滚动切换 → 按「启动」语义载入最近 N 行
                self.current = Some(CurrentFile {
                    path: latest,
                    offset: 0,
                    carry: Vec::new(),
                });
                return self.load_startup(out);
            }
        }
        self.follow(out)
    }

    /// 启动/切换：整读当前文件，产出最近 [`STARTUP_LINES`] 行（offset 推进
    /// 到当前文件尾，随后进入跟随态）。整读失败（文件正被重建等竞态）→
    /// 保留 current 静默返回，下轮重试。
    fn load_startup(&mut self, out: &mut dyn FnMut(SystemTime, String)) -> usize {
        let Some(cur) = &self.current else { return 0 };
        let data = match fs::read(&cur.path) {
            Ok(d) => d,
            Err(_) => {
                self.current = None; // 文件消失（竞态）→ 下轮重扫
                return 0;
            }
        };
        let Some(cur) = self.current.as_mut() else { return 0 };
        cur.offset = data.len() as u64;
        cur.carry.clear();

        // 完整行集合：尾部无换行的半行是正在写入的行——启动整读后 offset 已到
        // 尾，留 carry 会在下轮 len==offset 快路径被跳过而永久丢，故直接产出
        // （事件行完整性优先）；取最近 STARTUP_LINES 行（早期行不产出）
        let lines: Vec<&[u8]> = data.split(|&b| b == b'\n').filter(|l| !l.is_empty()).collect();
        let recent_start = lines.len().saturating_sub(STARTUP_LINES);
        let mut count = 0;
        for raw in &lines[recent_start..] {
            if let Some((ts, line)) = parse_event_line(raw, &mut self.bad_lines) {
                out(ts, line);
                count += 1;
            }
        }
        count
    }

    /// 跟随态：同文件增量读（同 file_tail 手法：offset + carry + 完整行产出）
    fn follow(&mut self, out: &mut dyn FnMut(SystemTime, String)) -> usize {
        let Some(cur) = self.current.as_mut() else { return 0 };
        let mut file = match File::open(&cur.path) {
            Ok(f) => f,
            Err(_) => {
                self.current = None; // 文件消失 → 下轮重扫（可能已滚月）
                return 0;
            }
        };
        let len = match file.metadata() {
            Ok(m) => m.len(),
            Err(_) => return 0,
        };
        if len < cur.offset {
            // 文件变小 = 被重建/截断 → 重新按启动语义载入
            cur.offset = 0;
            cur.carry.clear();
            return self.load_startup(out);
        }
        if len == cur.offset {
            return 0;
        }
        if file.seek(SeekFrom::Start(cur.offset)).is_err() {
            return 0;
        }
        let mut chunk = Vec::new();
        if file.read_to_end(&mut chunk).is_err() {
            return 0;
        }
        cur.offset += chunk.len() as u64;

        let mut data = std::mem::take(&mut cur.carry);
        data.extend_from_slice(&chunk);
        let last_nl = match data.iter().rposition(|&b| b == b'\n') {
            Some(p) => p,
            None => {
                // 整段无换行 → 留 carry（上限防御同 file_tail）
                if data.len() <= MAX_CARRY {
                    cur.carry = data;
                } else {
                    cur.carry.clear();
                }
                return 0;
            }
        };
        cur.carry = data[last_nl + 1..].to_vec();

        let mut count = 0;
        for raw in data[..last_nl].split(|&b| b == b'\n') {
            if raw.is_empty() {
                continue;
            }
            if let Some((ts, line)) = parse_event_line(raw, &mut self.bad_lines) {
                out(ts, line);
                count += 1;
            }
        }
        count
    }
}

/// 解析一行事件：JSON 合法 → (ts, 原始行)；坏行 → 计数返回 None。
/// `ts` 字段（Unix 秒）缺省回退当前时刻。
fn parse_event_line(raw: &[u8], bad: &mut u64) -> Option<(SystemTime, String)> {
    let line = String::from_utf8_lossy(raw);
    let line = line.trim();
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => {
            let ts = v
                .get("ts")
                .and_then(|t| t.as_u64())
                .map(|secs| UNIX_EPOCH + std::time::Duration::from_secs(secs))
                .unwrap_or_else(SystemTime::now);
            Some((ts, line.to_string()))
        }
        Err(_) => {
            *bad += 1;
            None
        }
    }
}

// ─── 单测（临时目录注入，零 GUI / 零网络） ─────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ep-tray-gt4-ev-{}-{}-{}",
            tag,
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("临时目录应可创建");
        d
    }

    fn collect(src: &mut EventsJsonlSource) -> Vec<String> {
        let mut lines = Vec::new();
        src.poll(&mut |_, l| lines.push(l));
        lines
    }

    #[test]
    fn missing_dir_and_no_files_are_silent() {
        let mut src = EventsJsonlSource::new(temp_dir("none").join("nope"));
        assert_eq!(src.poll(&mut |_, _| {}), 0, "目录不存在：静默 0");
        let dir = temp_dir("empty");
        let mut src = EventsJsonlSource::new(dir);
        assert_eq!(src.poll(&mut |_, _| {}), 0, "无 events 文件：静默 0");
    }

    #[test]
    fn startup_reads_most_recent_200_lines() {
        let dir = temp_dir("startup");
        let mut content = String::new();
        for i in 0..250 {
            content.push_str(&format!(
                "{{\"type\":\"task_terminal\",\"ts\":{},\"task_id\":\"t{i}\"}}\n",
                1_700_000_000 + i
            ));
        }
        fs::write(dir.join("events-2026-08.jsonl"), content).unwrap();
        let mut src = EventsJsonlSource::new(dir);
        let lines = collect(&mut src);
        assert_eq!(lines.len(), STARTUP_LINES, "启动只载入最近 N 行");
        assert!(lines[0].contains("t50"), "最早 50 行被截去");
        assert!(lines.last().unwrap().contains("t249"), "最新行在末尾");
    }

    #[test]
    fn startup_then_follows_append() {
        let dir = temp_dir("follow");
        fs::write(dir.join("events-2026-09.jsonl"), "{\"ts\":1}\n{\"ts\":2}\n").unwrap();
        let mut src = EventsJsonlSource::new(dir.clone());
        assert_eq!(collect(&mut src).len(), 2);
        // 追加（模拟 daemon 写事件）
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(dir.join("events-2026-09.jsonl"))
            .unwrap();
        use std::io::Write;
        writeln!(f, "{{\"ts\":3}}").unwrap();
        let lines = collect(&mut src);
        assert_eq!(lines, vec!["{\"ts\":3}"], "跟随追加只出新行");
        assert_eq!(src.bad_lines(), 0);
    }

    #[test]
    fn bad_lines_skipped_and_counted() {
        let dir = temp_dir("bad");
        fs::write(
            dir.join("events-2026-08.jsonl"),
            concat!(
                "{\"ts\":10,\"type\":\"a\"}\n",
                "not-json-at-all\n",
                "\n", // 空行：静默跳过不计数
                "{\"ts\":11,\"type\":\"b\"}\n",
                "{\"ts\": broken\n",
                "{\"ts\":12,\"type\":\"c\"}\n",
            ),
        )
        .unwrap();
        let mut src = EventsJsonlSource::new(dir);
        let lines = collect(&mut src);
        assert_eq!(lines.len(), 3, "坏行跳过，好行全收");
        assert!(lines[0].contains("\"a\""));
        assert!(lines[2].contains("\"c\""));
        assert_eq!(src.bad_lines(), 2, "坏行计数（空行不计）");
    }

    #[test]
    fn month_roll_switches_to_new_file_with_startup() {
        let dir = temp_dir("roll");
        fs::write(dir.join("events-2026-08.jsonl"), "{\"ts\":1,\"old\":true}\n").unwrap();
        let mut src = EventsJsonlSource::new(dir.clone());
        assert_eq!(collect(&mut src).len(), 1);
        // 跨月：daemon 创建新月份文件
        fs::write(
            dir.join("events-2026-09.jsonl"),
            "{\"ts\":2,\"new\":true}\n{\"ts\":3,\"new2\":true}\n",
        )
        .unwrap();
        let lines = collect(&mut src);
        assert_eq!(lines.len(), 2, "月滚动 → 切换并按启动语义载入新文件");
        assert!(lines[0].contains("\"new\""));
        // 旧文件不再产出
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(dir.join("events-2026-08.jsonl"))
            .unwrap();
        use std::io::Write;
        writeln!(f, "{{\"ts\":99}}").unwrap();
        assert_eq!(collect(&mut src).len(), 0, "已滚月的旧文件不再跟随");
    }

    #[test]
    fn ts_extracted_from_field_and_latest_file_picked() {
        let dir = temp_dir("ts");
        // 两个月文件并存 → 只取字典序最新的 09
        fs::write(dir.join("events-2026-08.jsonl"), "{\"ts\":100}\n").unwrap();
        fs::write(dir.join("events-2026-09.jsonl"), "{\"ts\":1788057268}\n").unwrap();
        let mut src = EventsJsonlSource::new(dir);
        let mut got_ts = None;
        src.poll(&mut |ts, l| {
            got_ts = Some((ts, l));
        });
        let (ts, line) = got_ts.expect("应有产出");
        assert_eq!(ts, UNIX_EPOCH + std::time::Duration::from_secs(1_788_057_268));
        assert_eq!(line, "{\"ts\":1788057268}", "原始 JSON 原样保留");
        // 无 ts 字段 → 回退当前时刻
        let dir2 = temp_dir("ts2");
        fs::write(dir2.join("events-2026-09.jsonl"), "{\"type\":\"x\"}\n").unwrap();
        let mut src2 = EventsJsonlSource::new(dir2);
        let mut ts2 = UNIX_EPOCH;
        src2.poll(&mut |ts, _| ts2 = ts);
        assert!(ts2 > UNIX_EPOCH, "无 ts 字段回退 now");
    }
}
