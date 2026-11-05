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
pub mod events_jsonl;
pub mod file_tail;
pub mod ws_source;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

// ─── 集成：LogPipe::spawn（三源三线程汇入同一 ring） ───────────────────────

/// LogPipe 启动配置。`root` = daemon 根（`runtime/logs` 所在）；daemon 端口
/// 只喂 WsSource；`ring_cap` = 环形缓冲容量（设置页可调 1000–50000 的接线口，
/// GT-5 接 UiShared 时换成共享配置即可）；`tail_start` = daemon.log 起始策略
/// （默认 End 只跟新增，Beginning 供导出/冒烟）。
#[derive(Debug, Clone)]
pub struct LogPipeConfig {
    pub root: std::path::PathBuf,
    pub daemon_port: u16,
    pub ring_cap: usize,
    pub tail_start: file_tail::TailStart,
}

impl Default for LogPipeConfig {
    fn default() -> Self {
        Self {
            root: std::path::PathBuf::from("."),
            daemon_port: 9800,
            ring_cap: LOG_RING_CAP,
            tail_start: file_tail::TailStart::End,
        }
    }
}

/// 日志引擎命名空间（三源三线程汇流）。线程模式选型：**每源一线程**（而非
/// 单线程 select）——三源节奏各异（tail 300ms / events 500ms / WS 阻塞读），
/// 分线程互不拖累且各自单测已覆盖，汇流点只有一个 `Arc<Mutex<LogRing>>`，
/// 简单可靠；与 GT-1/2 的后台线程先例同模式（IO 全在后台线程，绝不阻塞 UI）。
pub struct LogPipe;

impl LogPipe {
    /// 启动日志引擎：起三个后台线程（tail / events / ws），返回句柄供
    /// UI 侧 drain。与 `UiShared` 解耦（GT-5 日志页自行接线：后台引擎 →
    /// `drain_new()` → UiShared/直接入 UI 缓存）。
    ///
    /// 线程 spawn 失败（极罕见）→ 对应源静默缺席，不影响其余源与句柄。
    pub fn spawn(cfg: LogPipeConfig) -> LogPipeHandle {
        let ring = Arc::new(Mutex::new(LogRing::new(cfg.ring_cap)));
        let ws_status = Arc::new(Mutex::new(ws_source::WsStatus::default()));
        let jsonl_bad = Arc::new(AtomicU64::new(0));

        // 线程 1：daemon.log tail（300ms 轮）
        {
            let path = file_tail::daemon_log_path(&cfg.root);
            let ring = ring.clone();
            let start = cfg.tail_start;
            let _ = std::thread::Builder::new()
                .name("logpipe-tail".into())
                .spawn(move || {
                    let mut tail = file_tail::FileTailSource::new(path, start);
                    loop {
                        tail.poll(&mut |line| {
                            let level = LogRecord::level_from_daemon_line(&line);
                            if let Ok(mut r) = ring.lock() {
                                r.push(SystemTime::now(), LogSource::Daemon, level, line);
                            }
                        });
                        std::thread::sleep(file_tail::TAIL_INTERVAL);
                    }
                });
        }

        // 线程 2：events-*.jsonl（500ms 轮；坏行计数定期同步到共享原子量）
        {
            let dir = events_jsonl::events_dir(&cfg.root);
            let ring = ring.clone();
            let jsonl_bad = jsonl_bad.clone();
            let _ = std::thread::Builder::new()
                .name("logpipe-events".into())
                .spawn(move || {
                    let mut src = events_jsonl::EventsJsonlSource::new(dir);
                    loop {
                        src.poll(&mut |ts, line| {
                            if let Ok(mut r) = ring.lock() {
                                r.push(ts, LogSource::Event, None, line);
                            }
                        });
                        jsonl_bad.store(src.bad_lines(), Ordering::Relaxed);
                        std::thread::sleep(events_jsonl::EVENTS_INTERVAL);
                    }
                });
        }

        // 线程 3：/ws/logs（阻塞读 + 断线指数退避，见 ws_source.rs）
        {
            let url = ws_source::ws_logs_url(cfg.daemon_port);
            let connector = Box::new(ws_source::TungsteniteConnector::new(url));
            let src = ws_source::WsSource::new(connector, ring.clone(), ws_status.clone());
            let _ = std::thread::Builder::new()
                .name("logpipe-ws".into())
                .spawn(move || src.run());
        }

        LogPipeHandle {
            ring,
            ws_status,
            jsonl_bad,
            cursor: 0,
        }
    }
}

/// UI 侧句柄（GT-5 日志页接线面）。每消费者一份（游标独立）；本引擎按
/// 单 UI 消费者设计，多句柄并存时各游标互不干扰（drain 语义见下）。
pub struct LogPipeHandle {
    ring: Arc<Mutex<LogRing>>,
    ws_status: Arc<Mutex<ws_source::WsStatus>>,
    jsonl_bad: Arc<AtomicU64>,
    cursor: u64,
}

impl LogPipeHandle {
    /// 取自上次调用以来的新行（游标式增量；本句柄独占游标，需 `&mut self`）。
    /// 早期行被 ring 挤出时不补历史（缺口经 seq 可检测），由
    /// [`LogPipeHandle::drop_count`] 提示「已丢弃早期 N 行」。
    pub fn drain_new(&mut self) -> Vec<LogRecord> {
        let mut ring = match self.ring.lock() {
            Ok(r) => r,
            Err(_) => return Vec::new(), // 锁中毒：静默空（红线：绝不 panic）
        };
        let (records, cursor) = ring.drain_since(self.cursor);
        self.cursor = cursor;
        records
    }

    /// 累计丢弃行数（ring 超限挤出的最旧行；GT-5 可提示）
    pub fn drop_count(&self) -> u64 {
        self.ring.lock().map(|r| r.dropped()).unwrap_or(0)
    }

    /// 当前 ring 内行数
    pub fn ring_len(&self) -> usize {
        self.ring.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// WsSource 连接状态快照（「已连接/重连中」徽标）
    pub fn ws_status(&self) -> ws_source::WsStatus {
        self.ws_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// events jsonl 坏行累计（解析失败被跳过的行数）
    pub fn jsonl_bad_lines(&self) -> u64 {
        self.jsonl_bad.load(Ordering::Relaxed)
    }
}

// ─── ANSI 剥离（纯函数；daemon.log 为 tracing ANSI 彩色输出） ───────────────

/// 剥离 ANSI 转义序列，返回纯文本行。
///
/// daemon.log 是 tracing 的 ANSI 彩色输出（dim 时间戳/绿色 INFO/斜体字段名），
/// UI 展示前必须剥离。字节级扫描安全：UTF-8 多字节序列的续字节 ≥ 0x80，
/// 不可能与 ASCII `ESC`(0x1b) 混淆，非转义字节原样透传（含 CJK）。
///
/// 覆盖的序列族：
/// - **CSI** `ESC [ <params> <final>`（final ∈ 0x40–0x7E）：`ESC[0m` 重置、
///   `ESC[32m` 前景、`ESC[3m` 斜体、光标移动 `ESC[2J`/`ESC[1;2H` 等；
/// - **OSC** `ESC ] <payload> BEL` 或 `ESC ] <payload> ESC \`（标题等）；
/// - **DCS/APC/PM/SOS**（`ESC P/X/^/_` ... `ESC \`）：同 OSC 终止规则；
/// - 两字节转义（`ESC` + 单字符）与行尾孤 `ESC`：剥弃。
pub fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // bytes[i] == ESC；判断序列族并整体吞掉
        let next = bytes.get(i + 1).copied();
        match next {
            // CSI：吞到 final byte（0x40..=0x7E）
            Some(b'[') => {
                i += 2;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i = (i + 1).min(bytes.len()); // 跳过 final byte
            }
            // OSC / DCS / SOS / PM / APC：吞到 BEL 或 ST(ESC \)
            Some(b']' | b'P' | b'X' | b'^' | b'_') => {
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            // 两字节转义（ESC + 单字符，如 ESC M）或行尾孤 ESC
            Some(_) => i += 2,
            None => i += 1,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

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

    // ── ANSI 剥离：样张 = runtime/logs/daemon.log 真实 tracing 输出（2026-08-23）──
    //
    // fixture 常量逐字节取自真文件（ESC = \x1b）：
    //   \x1b[2m  = dim      \x1b[0m = reset
    //   \x1b[32m = 绿前景   \x1b[3m = italic（字段名）
    const FIXTURE_DAEMON_START: &str = concat!(
        "\x1b[2m2026-08-23T16:56:15.366898Z\x1b[0m ",
        "\x1b[32m INFO\x1b[0m ",
        "\x1b[2mep_daemon\x1b[0m\x1b[2m:\x1b[0m EntryPoint Daemon starting...",
    );
    const FIXTURE_DAEMON_ROOT: &str = concat!(
        "\x1b[2m2026-08-23T16:56:15.366920Z\x1b[0m ",
        "\x1b[32m INFO\x1b[0m ",
        "\x1b[2mep_daemon\x1b[0m\x1b[2m:\x1b[0m project root resolved ",
        "\x1b[3mroot\x1b[0m\x1b[2m=\x1b[0m/home/bob/EntryPoint",
    );

    #[test]
    fn strip_ansi_real_tracing_line_with_timestamp_and_level() {
        assert_eq!(
            strip_ansi(FIXTURE_DAEMON_START),
            "2026-08-23T16:56:15.366898Z  INFO ep_daemon: EntryPoint Daemon starting..."
        );
    }

    #[test]
    fn strip_ansi_real_tracing_line_with_fields() {
        assert_eq!(
            strip_ansi(FIXTURE_DAEMON_ROOT),
            "2026-08-23T16:56:15.366920Z  INFO ep_daemon: project root resolved root=/home/bob/EntryPoint"
        );
    }

    #[test]
    fn strip_ansi_strips_stripped_line_level_parses() {
        // 剥离产物与 level_from_daemon_line 串联（真管线形状）
        let plain = strip_ansi(FIXTURE_DAEMON_START);
        assert_eq!(
            LogRecord::level_from_daemon_line(&plain),
            Some(LogLevel::Info)
        );
    }

    #[test]
    fn strip_ansi_handles_non_color_csi_and_osc() {
        // 非 m 终符的 CSI（光标/清屏）与 OSC 标题
        assert_eq!(strip_ansi("\x1b[2J\x1b[1;2Hreset"), "reset", "CSI 吞到字母终符");
        assert_eq!(strip_ansi("\x1b[38;5;196mred"), "red", "扩展色 SGR");
        assert_eq!(strip_ansi("\x1b]0;title\x07body"), "body", "OSC+BEL");
        assert_eq!(strip_ansi("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"), "link", "OSC+ST");
        assert_eq!(strip_ansi("\x1bP+q544e\x1b\\after"), "after", "DCS+ST");
        assert_eq!(strip_ansi("a\x1bMb"), "ab", "两字节转义 ESC M");
        assert_eq!(strip_ansi("end\x1b"), "end", "行尾孤 ESC 不吞正文");
    }

    #[test]
    fn strip_ansi_plain_text_and_cjk_passthrough() {
        assert_eq!(strip_ansi("plain 123"), "plain 123");
        assert_eq!(strip_ansi("staging 初始化 budget_mb=7967"), "staging 初始化 budget_mb=7967");
        // ESC 看似在 CJK 中间也不破坏多字节序列
        assert_eq!(strip_ansi("中文\x1b[31m红字"), "中文红字");
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

    // ── 集成：LogPipe::spawn 三源汇流（临时 root；WS 指向必然拒绝的端口，
    //    后台退避重试不干扰断言）──

    #[test]
    fn spawn_collects_tail_and_events_into_ring() {
        let dir = temp_root("spawn");
        let logs = dir.join("runtime").join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        // daemon.log：真实 tracing ANSI 形状
        std::fs::write(
            logs.join("daemon.log"),
            "\x1b[2m2026-08-23T16:56:15.366898Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mep_daemon\x1b[0m\x1b[2m:\x1b[0m first line\nsecond plain line\n",
        )
        .unwrap();
        std::fs::write(
            logs.join("events-2026-08.jsonl"),
            "{\"ts\":1788057268,\"type\":\"task_terminal\"}\n{\"ts\":1788057269}\n",
        )
        .unwrap();

        let cfg = LogPipeConfig {
            root: dir,
            daemon_port: 1, // 必然拒绝：WS 线程后台退避，不参与断言
            ring_cap: LOG_RING_CAP,
            tail_start: file_tail::TailStart::Beginning,
        };
        let mut handle = LogPipe::spawn(cfg);
        // 300ms tail 轮 + 500ms events 轮 + 余量
        std::thread::sleep(std::time::Duration::from_millis(1200));

        let records = handle.drain_new();
        let daemon_lines: Vec<&str> = records
            .iter()
            .filter(|r| r.source == LogSource::Daemon)
            .map(|r| r.line.as_str())
            .collect();
        assert_eq!(
            daemon_lines,
            vec![
                "2026-08-23T16:56:15.366898Z  INFO ep_daemon: first line",
                "second plain line",
            ],
            "tail 经 ANSI 剥离入环"
        );
        assert_eq!(
            daemon_lines
                .iter()
                .filter_map(|l| LogRecord::level_from_daemon_line(l))
                .next(),
            Some(LogLevel::Info)
        );
        let event_count = records
            .iter()
            .filter(|r| r.source == LogSource::Event)
            .count();
        assert_eq!(event_count, 2, "events 启动载入 2 行");
        assert_eq!(handle.jsonl_bad_lines(), 0);
        assert_eq!(handle.drop_count(), 0);
        // 游标增量：立刻再 drain → 空
        assert!(handle.drain_new().is_empty(), "drain 游标语义");
    }

    #[test]
    fn spawn_ring_cap_enforced_with_drop_count() {
        let dir = temp_root("spawn-cap");
        let logs = dir.join("runtime").join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let mut content = String::new();
        for i in 0..10 {
            content.push_str(&format!("line-{i}\n"));
        }
        std::fs::write(logs.join("daemon.log"), content).unwrap();

        let cfg = LogPipeConfig {
            root: dir,
            daemon_port: 1,
            ring_cap: 4,
            tail_start: file_tail::TailStart::Beginning,
        };
        let mut handle = LogPipe::spawn(cfg);
        std::thread::sleep(std::time::Duration::from_millis(900));

        assert_eq!(handle.ring_len(), 4, "容量铁律：缓冲有界");
        assert_eq!(handle.drop_count(), 6, "10 行入 4 容量环 → 丢 6 条最旧");
        let records = handle.drain_new();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].line, "line-6", "最旧 6 条已挤出");
        assert_eq!(records[3].line, "line-9");
    }

    /// 临时 root（runtime/logs 由测试自建）
    fn temp_root(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ep-tray-gt4-pipe-{tag}-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    static TMP_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
}
