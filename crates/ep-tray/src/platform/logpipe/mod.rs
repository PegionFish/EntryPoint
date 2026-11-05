//! 日志引擎 LogPipe（GT-4）：三源汇流 + 环形缓冲，纯逻辑可测。
//!
//! 契约（reports/plan/2026-11-05-ep-tray-gui-dispatch.md §2）：
//! 1. [`ws_source`]：订阅 `ws://127.0.0.1:<port>/ws/logs`（模块进程日志，
//!    LogMessage{module_id,line}；断线指数退避重连；无历史回放——接入后开始）；
//! 2. [`file_tail`]：tail `runtime/logs/daemon.log`（增量读 + byte offset +
//!    ANSI 剥离 + 轮转/重建重置）；
//! 3. [`events_jsonl`]：读 `runtime/logs/events-*.jsonl`（最新月文件跟随追加
//!    + 启动载入最近 [`events_jsonl::STARTUP_LINES`] 行）。
//!
//! 全源汇入进程内 [`LogRing`]（容量 [`LOG_RING_CAP`] 起步，超限丢最旧 + 丢弃
//! 计数；过滤在 UI 层做，引擎只做环形存储与背压丢弃）。线程模式沿 GT-1/2
//! 既有先例：**IO 全在 logpipe 后台线程**（[`LogPipe::spawn`] 每源一线程），
//! UI 经 [`LogPipeHandle`] drain——本模块不接 `UiShared`（GT-5 日志页自行接线）。
//!
//! 红线：
//! - 绝不阻塞 UI 线程（全部文件/网络 IO 都在后台线程）；
//! - ring 容量铁律（防膨胀，沿 processed_registry 教训——Vec 只能有界增长）；
//! - daemon.log / events 文件不存在时静默等待（daemon 未起属常态，不报错）。

// 子模块接线（逐 commit 加入）：file_tail / events_jsonl / ws_source
// 各自实现 + 单测独立落地，本文件在集成 commit 提供 LogPipe::spawn 汇流。

use std::time::SystemTime;

// ─── 核心类型 ───────────────────────────────────────────────────────────────

/// ring 容量起步值（契约：默认 5000 行；设置页可调 1000–50000 属 GT-5 接线面，
/// [`LogRing::new`] 的容量参数已留好注入口）。
pub const LOG_RING_CAP: usize = 5000;

/// 日志级别（daemon.log 的 tracing 级别词；WS 模块行与事件行无可判级别 →
/// `None`）。有序：Error > Warn > Info > Debug > Trace（GT-5 级别过滤可用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// 从 tracing 格式化输出的级别词解析（如 `" INFO"` / `"ERROR"`）。
    /// tracing 右对齐 5 字符宽度 → 行内常见 ` INFO`（前导空格），先 trim。
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim() {
            "TRACE" => Some(Self::Trace),
            "DEBUG" => Some(Self::Debug),
            "INFO" => Some(Self::Info),
            "WARN" => Some(Self::Warn),
            "ERROR" => Some(Self::Error),
            _ => None,
        }
    }
}

/// 三源标签（契约 §2：`source: Daemon | Module(id) | Event`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LogSource {
    /// daemon 自身日志（daemon.log tail）
    Daemon,
    /// 模块进程日志（/ws/logs 推送，id = module_id）
    Module(String),
    /// 统一事件日志（events-*.jsonl）
    Event,
}

/// 统一日志行。`ts` 用 [`SystemTime`] 存原始值（不引 chrono，显示层格式化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// 全局单调序号（ring 分配；UI 端 drain 游标与丢行检测依据）
    pub seq: u64,
    pub ts: SystemTime,
    pub source: LogSource,
    pub level: Option<LogLevel>,
    pub line: String,
}

impl LogRecord {
    /// 从 daemon.log 一行剥 ANSI 后的明文解析级别：级别词位于时间戳之后
    /// （如 `2026-08-23T16:56:15Z  INFO ep_daemon: ...`），取前 3 个空白
    /// 分词里第一个可识别词（时间戳 1 词 + 级别 1 词，余量 1 词防格式漂移）。
    pub fn level_from_daemon_line(line: &str) -> Option<LogLevel> {
        line.split_whitespace()
            .take(3)
            .find_map(LogLevel::from_token)
    }
}

// ─── 环形缓冲（背压语义：超限丢最旧 + 计数，绝不无界膨胀） ─────────────────

/// 进程内环形日志缓冲。多源后台线程经 `Arc<Mutex<LogRing>>` 汇入；
/// UI 消费端用 [`LogRing::drain_since`] 游标式增量取新行。
///
/// 背压语义：写满后再 push → **丢最旧行**（`dropped` 计数 +1），新行必进——
/// 保近期现场、绝不阻塞写入方（源线程永不因 ring 满而卡死）。
#[derive(Debug)]
pub struct LogRing {
    buf: std::collections::VecDeque<LogRecord>,
    cap: usize,
    /// 因容量超限被挤出的最旧行总数（pub 只读接口 [`LogRing::dropped`]）
    dropped: u64,
    /// 已分配的最大 seq（下一条 = next_seq）
    next_seq: u64,
}

impl LogRing {
    /// 容量下限保护：cap 传 0 视为 1（容量铁律——缓冲永远有界）。
    pub fn new(cap: usize) -> Self {
        Self {
            buf: std::collections::VecDeque::with_capacity(cap.max(1)),
            cap: cap.max(1),
            dropped: 0,
            next_seq: 0,
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 超限丢最旧的累计条数（UI 端提示「已丢弃早期 N 行」）。
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 写入一条（seq 由 ring 分配）。满容量 → 丢最旧、`dropped` +1。
    pub fn push(&mut self, ts: SystemTime, source: LogSource, level: Option<LogLevel>, line: String) {
        if self.buf.len() >= self.cap {
            self.buf.pop_front();
            self.dropped += 1;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.buf.push_back(LogRecord {
            seq,
            ts,
            source,
            level,
            line,
        });
    }

    /// 游标式增量读取：返回 seq ≥ cursor 的全部现存行 + 新游标（= 下一条
    /// 将分配的 seq，直接回填下次调用）。
    ///
    /// 若消费端长期不取导致早期行已被挤出，返回行首条 seq 会 > cursor
    /// （缺口可检测；UI 只展示现存行，属预期降级）。
    pub fn drain_since(&mut self, cursor: u64) -> (Vec<LogRecord>, u64) {
        let new_cursor = self.next_seq;
        let records: Vec<LogRecord> = self
            .buf
            .iter()
            .filter(|r| r.seq >= cursor)
            .cloned()
            .collect();
        (records, new_cursor)
    }
}

// ─── 单测（纯逻辑，零 IO） ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn ts(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn rec(ring: &mut LogRing, i: usize) -> LogRecord {
        let (records, _) = ring.drain_since(0);
        records.into_iter().find(|r| r.seq == i as u64).expect("seq 应存在")
    }

    #[test]
    fn level_from_token_parses_tracing_words_with_padding() {
        assert_eq!(LogLevel::from_token(" INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::from_token("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::from_token(" WARN"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_token("ERROR"), Some(LogLevel::Error));
        assert_eq!(LogLevel::from_token("DEBUG"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::from_token("TRACE"), Some(LogLevel::Trace));
        assert_eq!(LogLevel::from_token("info"), None, "tracing 级别词全大写，小写不识别");
        assert_eq!(LogLevel::from_token("ep_daemon:"), None);
    }

    #[test]
    fn level_from_daemon_line_stripped_sample() {
        // 对照 daemon.log 真实行剥 ANSI 后的形状（见 ansi 剥离样张单测）
        let line = "2026-08-23T16:56:15.366898Z  INFO ep_daemon: EntryPoint Daemon starting...";
        assert_eq!(LogRecord::level_from_daemon_line(line), Some(LogLevel::Info));
        let err_line = "2026-08-24T10:00:00.000000Z ERROR ep_daemon::api: bad request";
        assert_eq!(LogRecord::level_from_daemon_line(err_line), Some(LogLevel::Error));
        let no_level = "{\"task_id\":\"x\",\"ts\":1}";
        assert_eq!(LogRecord::level_from_daemon_line(no_level), None);
    }

    #[test]
    fn ring_push_order_and_seq_monotonic() {
        let mut ring = LogRing::new(8);
        ring.push(ts(1), LogSource::Daemon, Some(LogLevel::Info), "a".into());
        ring.push(ts(2), LogSource::Module("asr".into()), None, "b".into());
        ring.push(ts(3), LogSource::Event, None, "c".into());
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.dropped(), 0);
        assert_eq!(rec(&mut ring, 0).line, "a");
        assert_eq!(rec(&mut ring, 0).source, LogSource::Daemon);
        assert_eq!(rec(&mut ring, 1).source, LogSource::Module("asr".into()));
        assert_eq!(rec(&mut ring, 2).source, LogSource::Event);
        assert_eq!(rec(&mut ring, 1).ts, ts(2));
    }

    #[test]
    fn ring_overflow_evicts_oldest_and_counts_dropped() {
        let mut ring = LogRing::new(3);
        for i in 0..5u64 {
            ring.push(ts(i), LogSource::Daemon, None, format!("line-{i}"));
        }
        assert_eq!(ring.len(), 3, "容量铁律：缓冲有界");
        assert_eq!(ring.dropped(), 2, "超限 2 条被挤出");
        assert_eq!(rec(&mut ring, 2).line, "line-2", "最旧两条已丢");
        assert_eq!(rec(&mut ring, 4).line, "line-4", "新行必进");
    }

    #[test]
    fn ring_cap_zero_is_clamped_to_one() {
        let mut ring = LogRing::new(0);
        ring.push(ts(1), LogSource::Daemon, None, "only".into());
        ring.push(ts(2), LogSource::Daemon, None, "replaced".into());
        assert_eq!(ring.cap(), 1);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.dropped(), 1);
    }

    #[test]
    fn ring_drain_since_cursor_incremental() {
        let mut ring = LogRing::new(16);
        for i in 0..3u64 {
            ring.push(ts(i), LogSource::Daemon, None, format!("m{i}"));
        }
        let (first, cursor) = ring.drain_since(0);
        assert_eq!(first.len(), 3);
        assert_eq!(cursor, 3, "新游标 = 下一条 seq");

        ring.push(ts(9), LogSource::Daemon, None, "m9".into());
        let (second, cursor) = ring.drain_since(cursor);
        assert_eq!(second.len(), 1, "游标增量：只取新行");
        assert_eq!(second[0].line, "m9");
        assert_eq!(cursor, 4);
    }

    #[test]
    fn ring_drain_detects_gap_after_eviction() {
        let mut ring = LogRing::new(2);
        for i in 0..6u64 {
            ring.push(ts(i), LogSource::Daemon, None, format!("x{i}"));
        }
        // 消费端从 seq 0 开始取，但 0..2 已被挤出 → 首条 seq=4 > 游标 0（缺口可检测）
        let (records, _) = ring.drain_since(0);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].seq, 4);
        assert!(records[0].seq > 0, "缺口：现存首条 seq 大于消费游标");
    }
}
