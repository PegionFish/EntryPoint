//! WsSource（GT-4 第一源）：订阅 `ws://127.0.0.1:<port>/ws/logs` 模块日志流。
//!
//! daemon 侧（crates/ep-daemon/src/ws/logs.rs）广播 [`LogMessage`]（JSON
//! `{"module_id":..,"line":..}`），30s 心跳 Ping。本源：
//! - **消息解析**：`LogMessage` 形状 JSON → [`LogRecord`]（source =
//!   [`LogSource::Module`]，级别不解析 → None，行剥 ANSI）；
//! - **断线指数退避重连**：1s 起步 ×2、上限 30s、**连接成功归零**（契约 §2.1；
//!   无历史回放——daemon 不回放 log_tx 历史，「接入后开始」）；
//! - **状态可查**：[`WsStatus`]（Connected/Connecting/Disconnected + 重连
//!   次数 + 最近错误），供 GT-5 日志页「已连接/重连中」徽标；
//! - **IO 抽象**：连接交互抽成 [`WsConnector`]/[`WsConn`] trait——真实实现
//!   [`TungsteniteConnector`] 是 tungstenite blocking 客户端的薄封装；单测用
//!   假连接器注入帧序列，零网络。
//!
//! 不设读超时的理由：对端恒为本机回环 daemon（进程死亡 = 内核即时 RST，
//! `read` 立刻返回错误，不存在网络半开场景）；daemon 30s Ping 经
//! `recv` 内自动 Pong 应答。线程阻塞在 `recv` 属预期（线程生命周期 =
//! 进程生命周期，沿 probe_ext 先例）。
//!
//! [`LogMessage`]: https://docs.rs/ep-daemon （crates/ep-daemon/src/state.rs）
//! [`LogSource::Module`]: crate::platform::logpipe::LogSource::Module

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::{LogRing, LogSource};

/// 重连退避起步值
pub const WS_RECONNECT_BASE: Duration = Duration::from_secs(1);
/// 重连退避上限
pub const WS_RECONNECT_CAP: Duration = Duration::from_secs(30);

/// ws/logs 端点 URL（daemon 默认端口 9800；端口随 LogPipeConfig 传入）
pub fn ws_logs_url(port: u16) -> String {
    format!("ws://127.0.0.1:{port}/ws/logs")
}

// ─── 状态快照（GT-5 日志页连接徽标数据源） ─────────────────────────────────

/// 连接状态机：`Connecting →（成功）→ Connected →（断/错）→ Connecting…`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WsConnState {
    /// 未连接（从未连上或等待重连计时中）
    #[default]
    Disconnected,
    /// 正在握手（含退避到期后重连中）
    Connecting,
    /// 已连接（recv 通道建立，等待/接收推送中）
    Connected,
}

/// WsSource 状态快照（[`LogPipeHandle::ws_status`] 读走）。
#[derive(Debug, Clone, Default)]
pub struct WsStatus {
    pub state: WsConnState,
    /// 当前退避周期内累计失败次数（连接成功归零）
    pub attempts: u64,
    /// 最近一次错误（连接失败/断线原因；成功连接后清空）
    pub last_error: Option<String>,
    /// 最近一次成功连接时刻（None = 从未连上）
    pub last_connected: Option<Instant>,
    /// 收到的无法解析帧累计（module_id/line 缺失、非 JSON）
    pub bad_frames: u64,
}

// ─── 连接抽象（真实实现薄封装 + 离线测试注入口） ───────────────────────────

/// 一条已建立的连接上的消息
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsFrame {
    /// 文本帧（daemon LogMessage JSON；二进制帧按 lossy UTF-8 兜底转入）
    Text(String),
    /// 对端优雅关闭
    Closed,
}

/// 已建立连接的读侧抽象（阻塞读一条；错误 = 连接已坏，原因入 Err）
pub trait WsConn: Send {
    fn recv(&mut self) -> Result<WsFrame, String>;
}

/// 连接工厂抽象（重连循环每次调用）
pub trait WsConnector: Send {
    fn connect(&self) -> Result<Box<dyn WsConn>, String>;
}

/// tungstenite blocking 客户端薄封装（真实实现）
pub struct TungsteniteConnector {
    url: String,
}

impl TungsteniteConnector {
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

impl WsConnector for TungsteniteConnector {
    fn connect(&self) -> Result<Box<dyn WsConn>, String> {
        let (ws, _resp) = tungstenite::client::connect(self.url.as_str())
            .map_err(|e| format!("ws 握手失败: {e}"))?;
        Ok(Box::new(TungsteniteConn { ws }))
    }
}

/// 流类型泛型化：不点名 tungstenite 内部流别名（随特性面变化），
/// 只要求 Read+Write+Send（MaybeTlsStream/TcpStream 皆满足）
struct TungsteniteConn<S> {
    ws: tungstenite::WebSocket<S>,
}

impl<S: std::io::Read + std::io::Write + Send> WsConn for TungsteniteConn<S> {
    fn recv(&mut self) -> Result<WsFrame, String> {
        loop {
            match self.ws.read().map_err(|e| e.to_string())? {
                tungstenite::Message::Text(s) => return Ok(WsFrame::Text(s.to_string())),
                // daemon 只发 Text；Binary 按 lossy UTF-8 兜底，不静默丢
                tungstenite::Message::Binary(b) => {
                    return Ok(WsFrame::Text(String::from_utf8_lossy(&b).into_owned()))
                }
                // daemon 30s 心跳 Ping → 必须回 Pong（否则被服务端断开）
                tungstenite::Message::Ping(p) => {
                    let _ = self.ws.send(tungstenite::Message::Pong(p));
                }
                tungstenite::Message::Pong(_) | tungstenite::Message::Frame(_) => continue,
                tungstenite::Message::Close(_) => return Ok(WsFrame::Closed),
            }
        }
    }
}

// ─── 源主体（重连循环；单轮 run_cycle 可测） ───────────────────────────────

/// 单轮循环结果（测试/诊断用；run 循环内部消费）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleOutcome {
    /// 本轮是否成功建立过连接（成功 → 退避归零）
    pub connected: bool,
    /// 本轮消费的合法消息条数
    pub frames: usize,
    /// 本轮结束后的休眠时长（指数退避；连接成功也按 1s 起步重试）
    pub backoff: Duration,
}

/// WsSource：持连接工厂 + 共享 ring + 状态槽，跑重连循环。
pub struct WsSource {
    connector: Box<dyn WsConnector>,
    sink: Arc<Mutex<LogRing>>,
    status: Arc<Mutex<WsStatus>>,
    backoff: Duration,
}

impl WsSource {
    pub fn new(
        connector: Box<dyn WsConnector>,
        sink: Arc<Mutex<LogRing>>,
        status: Arc<Mutex<WsStatus>>,
    ) -> Self {
        Self {
            connector,
            sink,
            status,
            backoff: WS_RECONNECT_BASE,
        }
    }

    /// 主循环（调用方在独立线程执行；生命周期 = 进程，沿 probe_ext 先例）
    pub fn run(mut self) {
        loop {
            let outcome = self.run_cycle();
            std::thread::sleep(outcome.backoff);
        }
    }

    /// 单轮：尝试连接 →（成功）消费帧直到断/错 →（失败/断开）返回退避时长。
    /// 幂等可重入，测试直接逐轮驱动。
    pub fn run_cycle(&mut self) -> CycleOutcome {
        self.set_state(WsConnState::Connecting);
        match self.connector.connect() {
            Ok(mut conn) => {
                self.record_connected();
                let mut frames = 0usize;
                // Text 帧 → 解析入环；Closed / Err（连接坏）→ 出循环走重连
                while let Ok(WsFrame::Text(text)) = conn.recv() {
                    if self.push_message(&text) {
                        frames += 1;
                    }
                }
                // 断线（daemon 重启/优雅关闭）→ 状态回 Disconnected，
                // 退避已在本轮连接成功时归零，下轮从 1s 起步
                self.set_state(WsConnState::Disconnected);
                let outcome = CycleOutcome {
                    connected: true,
                    frames,
                    backoff: self.backoff,
                };
                self.backoff = WS_RECONNECT_BASE;
                outcome
            }
            Err(err) => {
                self.record_error(err);
                let outcome = CycleOutcome {
                    connected: false,
                    frames: 0,
                    backoff: self.backoff,
                };
                // 指数退避：×2 封顶 30s
                self.backoff = (self.backoff * 2).min(WS_RECONNECT_CAP);
                outcome
            }
        }
    }

    /// 解析一条 LogMessage JSON → push ring。返回是否为合法消息。
    /// 形状：`{"module_id": String, "line": String}`（daemon state.rs）。
    fn push_message(&self, text: &str) -> bool {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(text);
        let value = match parsed {
            Ok(v) => v,
            Err(_) => {
                self.bump_bad_frames();
                return false;
            }
        };
        let module_id = value.get("module_id").and_then(|m| m.as_str());
        let line = value.get("line").and_then(|l| l.as_str());
        match (module_id, line) {
            (Some(id), Some(line)) => {
                let plain = super::strip_ansi(line);
                let mut ring = match self.sink.lock() {
                    Ok(r) => r,
                    Err(_) => return true, // 锁中毒：行已算合法但不入环（静默降级）
                };
                ring.push(SystemTime::now(), LogSource::Module(id.to_string()), None, plain);
                true
            }
            _ => {
                self.bump_bad_frames();
                false
            }
        }
    }

    fn set_state(&self, state: WsConnState) {
        if let Ok(mut s) = self.status.lock() {
            s.state = state;
        }
    }

    fn record_connected(&self) {
        if let Ok(mut s) = self.status.lock() {
            s.state = WsConnState::Connected;
            s.attempts = 0;
            s.last_error = None;
            s.last_connected = Some(Instant::now());
        }
    }

    fn record_error(&self, err: String) {
        if let Ok(mut s) = self.status.lock() {
            s.state = WsConnState::Disconnected;
            s.attempts += 1;
            s.last_error = Some(err);
        }
    }

    fn bump_bad_frames(&self) {
        if let Ok(mut s) = self.status.lock() {
            s.bad_frames += 1;
        }
    }
}

// ─── 单测（假连接器注入帧序列，零网络） ───────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    /// 假连接：预置帧队列，取尽即 Closed
    struct FakeConn {
        frames: VecDeque<Result<WsFrame, String>>,
    }

    impl WsConn for FakeConn {
        fn recv(&mut self) -> Result<WsFrame, String> {
            self.frames
                .pop_front()
                .unwrap_or(Ok(WsFrame::Closed))
        }
    }

    /// 假连接器：按序返回预置连接；`failures_first` 次先失败模拟 daemon 未起
    struct FakeConnector {
        connections: Vec<FakeConn>,
        failures_first: usize,
        error: String,
    }

    impl FakeConnector {
        fn with_frames(frames: Vec<Result<WsFrame, String>>) -> Self {
            Self {
                connections: vec![FakeConn {
                    frames: frames.into(),
                }],
                failures_first: 0,
                error: "connect refused".into(),
            }
        }
    }

    impl WsConnector for FakeConnector {
        fn connect(&self) -> Result<Box<dyn WsConn>, String> {
            if self.failures_first > 0 {
                return Err(self.error.clone());
            }
            self.connections
                .first()
                .map(|c| Box::new(FakeConn { frames: c.frames.clone() }) as Box<dyn WsConn>)
                .ok_or_else(|| self.error.clone())
        }
    }

    /// 连接器在首次成功后置失败（模拟「连上又断，重连失败」）
    struct FlakyConnector {
        first: FakeConn,
        error: String,
        first_used: std::sync::atomic::AtomicBool,
    }

    impl WsConnector for FlakyConnector {
        fn connect(&self) -> Result<Box<dyn WsConn>, String> {
            if self
                .first_used
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
            {
                return Ok(Box::new(FakeConn {
                    frames: self.first.frames.clone(),
                }));
            }
            Err(self.error.clone())
        }
    }

    fn setup(
        connector: Box<dyn WsConnector>,
    ) -> (WsSource, Arc<Mutex<LogRing>>, Arc<Mutex<WsStatus>>) {
        let ring = Arc::new(Mutex::new(LogRing::new(64)));
        let status = Arc::new(Mutex::new(WsStatus::default()));
        let src = WsSource::new(connector, ring.clone(), status.clone());
        (src, ring, status)
    }

    fn msg(module: &str, line: &str) -> Result<WsFrame, String> {
        Ok(WsFrame::Text(format!(
            "{{\"module_id\":\"{module}\",\"line\":\"{line}\"}}"
        )))
    }

    #[test]
    fn url_points_at_daemon_ws_logs() {
        assert_eq!(ws_logs_url(9800), "ws://127.0.0.1:9800/ws/logs");
    }

    #[test]
    fn frames_parse_into_module_records_with_ansi_stripped() {
        let connector = Box::new(FakeConnector::with_frames(vec![
            msg("asr", "hello plain"),
            // JSON 字符串内的控制字符必须 \u 转义（raw ESC 是非法 JSON）
            msg("tts", "\\u001b[31mred text\\u001b[0m done"),
        ]));
        let (mut src, ring, _status) = setup(connector);
        let outcome = src.run_cycle();
        assert!(outcome.connected);
        assert_eq!(outcome.frames, 2);
        let (records, _) = ring.lock().unwrap().drain_since(0);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].source, LogSource::Module("asr".into()));
        assert_eq!(records[0].line, "hello plain");
        assert_eq!(records[0].level, None, "模块行无级别语义");
        assert_eq!(records[1].source, LogSource::Module("tts".into()));
        assert_eq!(records[1].line, "red text done", "ANSI 已剥离");
    }

    #[test]
    fn bad_frames_skipped_and_counted() {
        let connector = Box::new(FakeConnector::with_frames(vec![
            Ok(WsFrame::Text("not json at all".into())),
            Ok(WsFrame::Text("{\"module_id\":\"asr\"}".into())), // 缺 line
            Ok(WsFrame::Text("{\"line\":\"x\"}".into())),        // 缺 module_id
            msg("ocr", "good one"),
        ]));
        let (mut src, ring, status) = setup(connector);
        let outcome = src.run_cycle();
        assert_eq!(outcome.frames, 1, "三条坏帧只收一条好帧");
        assert_eq!(status.lock().unwrap().bad_frames, 3);
        let (records, _) = ring.lock().unwrap().drain_since(0);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, LogSource::Module("ocr".into()));
    }

    #[test]
    fn closed_frame_ends_connection_cleanly() {
        let connector = Box::new(FakeConnector::with_frames(vec![
            msg("asr", "line-1"),
            Ok(WsFrame::Closed),
            msg("asr", "never-arrives"), // Closed 后不再消费
        ]));
        let (mut src, ring, status) = setup(connector);
        let outcome = src.run_cycle();
        assert!(outcome.connected);
        assert_eq!(outcome.frames, 1);
        assert_eq!(status.lock().unwrap().state, WsConnState::Disconnected);
        let (records, _) = ring.lock().unwrap().drain_since(0);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn connect_failure_backs_off_exponentially_capped() {
        let connector = Box::new(FakeConnector {
            connections: vec![],
            failures_first: usize::MAX, // 永远失败
            error: "daemon down".into(),
        });
        let (mut src, _ring, status) = setup(connector);
        for i in 1..=7u32 {
            let outcome = src.run_cycle();
            assert!(!outcome.connected);
            // 1s, 2s, 4s, 8s, 16s, 30s(cap), 30s...
            let expect = Duration::from_secs(1 << (i - 1)).min(WS_RECONNECT_CAP);
            assert_eq!(outcome.backoff, expect, "第 {i} 次失败后的退避");
        }
        let s = status.lock().unwrap();
        assert_eq!(s.state, WsConnState::Disconnected);
        assert_eq!(s.attempts, 7, "失败累计计数");
        assert_eq!(s.last_error.as_deref(), Some("daemon down"));
        assert!(s.last_connected.is_none(), "从未连上");
    }

    #[test]
    fn success_resets_backoff_and_attempts() {
        let connector = Box::new(FlakyConnector {
            first: FakeConn {
                frames: vec![msg("asr", "brief life")].into(),
            },
            error: "reconnect refused".into(),
            first_used: std::sync::atomic::AtomicBool::new(false),
        });
        let (mut src, _ring, status) = setup(connector);
        // 第一轮：连接成功（假连接送达一条后 Closed → 干净断开收尾）
        let ok = src.run_cycle();
        assert!(ok.connected);
        assert_eq!(ok.frames, 1);
        {
            let s = status.lock().unwrap();
            // 周期结束态 = Disconnected（Closed 已断开）；连接期曾转 Connected
            // 的证据是 last_connected 被 record_connected 写入
            assert_eq!(s.attempts, 0, "成功清零失败计数");
            assert!(s.last_connected.is_some());
        }
        // 第二轮：连接失败 → 退避从 1s 起步（成功归零语义）
        let again = src.run_cycle();
        assert!(!again.connected);
        assert_eq!(again.backoff, Duration::from_secs(1), "成功后退避重置");
        assert_eq!(
            status.lock().unwrap().last_error.as_deref(),
            Some("reconnect refused")
        );
        assert_eq!(status.lock().unwrap().attempts, 1);
    }

    #[test]
    fn recv_error_treated_as_disconnect() {
        let connector = Box::new(FakeConnector::with_frames(vec![Err("socket dead".into())]));
        let (mut src, _ring, status) = setup(connector);
        let outcome = src.run_cycle();
        assert!(outcome.connected, "连接本身成功过");
        assert_eq!(status.lock().unwrap().state, WsConnState::Disconnected);
    }
}
