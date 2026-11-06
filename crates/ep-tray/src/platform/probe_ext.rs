//! daemon HTTP API 轮询线程（GT-2 状态页数据源）。
//!
//! 单线程 tick 循环：按三档节奏抓 daemon 既有 API 快照，写入 [`UiShared`]
//! 的 [`ProbeSlot`]（快照 struct + 最后成功时刻 + 连续失败计数），每次写入后
//! `ctx.request_repaint()` 唤醒 egui logic——模式照抄 GT-1 的 state-fwd 线程。
//!
//! 快照均为**自有 struct**（serde 反序列化，字段形状与 daemon 侧 handler
//! 一一对照，见各快照的测试 fixture），绝不透传 raw `serde_json::Value`。
//!
//! 红线：
//! - **绝不 panic**：共享锁中毒 / 请求失败全部走静默降级（字段置 None + 失败
//!   计数），线程永不崩；
//! - **绝不阻塞超 3s**：请求级总超时 [`REQUEST_TIMEOUT`]（含连接与响应体），
//!   回环 connect 拒绝即时返回，不额外挂起；
//! - 线程生命周期 = 进程生命周期（eframe 主循环退出 → main 返回 → 线程随
//!   进程终止），无独立退出通道。
//!
//! 轮询节奏为 `pub const`（设置页可调版后续票接线时改读共享配置即可）。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use serde::Deserialize;

use super::ui::UiShared;

// ─── 轮询节奏与阈值（pub const：设置页接电线，GT-2 先只读展示） ─────────────

/// `/api/health` 轮询间隔
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(2);
/// `/api/queue` 轮询间隔
pub const QUEUE_INTERVAL: Duration = Duration::from_secs(5);
/// `/api/devices` 轮询间隔
pub const DEVICES_INTERVAL: Duration = Duration::from_secs(10);
/// 单次 HTTP 请求总超时（含连接与响应体读取）——红线「绝不阻塞超 3s」
pub const REQUEST_TIMEOUT: Duration = Duration::from_millis(2500);
/// TCP 连接超时（回环 connect 应即时；兜底防半开连接）
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(1000);
/// 数据新鲜度阈值：最后一次成功轮询距今超过该值 → 状态页标灰「探询失联」
pub const STALE_AFTER: Duration = Duration::from_secs(10);
/// 单次 tick 的 sleep 上限（把长间隔切成片，未来接可调节奏时无需改循环结构）
const MAX_SLEEP: Duration = Duration::from_millis(500);

// ─── 快照 struct（字段形状 = daemon handler 响应体，2026-11-06 对照） ───────

/// `GET /api/health` 快照（crates/ep-daemon/src/api/health.rs）
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HealthSnapshot {
    pub status: String,
    pub version: String,
}

/// `GET /api/queue` 快照（crates/ep-daemon/src/api/scheduler.rs `queue_status`；
/// daemon 侧 usize 序列化为 JSON number，此处按 u64 接）
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QueueSnapshot {
    pub paused: bool,
    pub running: u64,
    pub queued: u64,
}

/// `GET /api/devices` 条目快照（crates/ep-daemon/src/api/devices.rs
/// `DeviceResponse`：一条 = 一个物理设备，跨栈归并后 `stacks` 列全量覆盖栈；
/// 指标未探测到时为 null → Option）
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceSnapshot {
    pub id: String,
    pub backend: String,
    pub name: String,
    pub stacks: Vec<String>,
    pub total_memory_mb: Option<u32>,
    pub used_memory_mb: Option<u32>,
    pub utilization: Option<u8>,
    pub temperature: Option<u8>,
}

// ─── 探询槽位（UiShared 扩展模式：GT-4 日志接收口照此模式加字段） ───────────

/// 通用探询槽位：最新快照 + 最后成功时刻 + 连续失败计数。
///
/// 失败（daemon 宕机/拒绝/超时/解析错）→ `data` 置 None、`failures` +1；
/// 成功 → 写快照、刷新 `at`、清零 `failures`。UI 侧用 [`ProbeSlot::is_fresh`]
/// 判定展示真数据还是「—（探询失联/未就绪）」。
#[derive(Debug)]
pub struct ProbeSlot<T> {
    /// 最新快照（None = 未就绪或探询失联）
    pub data: Option<T>,
    /// 最后一次成功轮询时刻
    pub at: Option<Instant>,
    /// 连续失败次数（成功清零）
    pub failures: u64,
}

impl<T> Default for ProbeSlot<T> {
    fn default() -> Self {
        Self {
            data: None,
            at: None,
            failures: 0,
        }
    }
}

impl<T> ProbeSlot<T> {
    /// 成功记录（GT-3 scm_watch 复用本槽写服务快照）
    pub(crate) fn record_ok(&mut self, data: T) {
        self.data = Some(data);
        self.at = Some(Instant::now());
        self.failures = 0;
    }

    /// 失败记录（data 置 None + 计数）
    pub(crate) fn record_fail(&mut self) {
        self.data = None;
        self.failures += 1;
    }

    /// 新鲜度：最后一次成功距今 ≤ [`STALE_AFTER`]。
    pub fn is_fresh(&self) -> bool {
        self.at
            .map(|at| at.elapsed() <= STALE_AFTER)
            .unwrap_or(false)
    }
}

// ─── 轮询线程 ───────────────────────────────────────────────────────────────

/// 轮询线程主循环（app.rs creator 闭包内 spawn；cc.egui_ctx 可用）。
pub(crate) fn run_probe_thread(port: u16, shared: Arc<Mutex<UiShared>>, ctx: egui::Context) {
    let agent = ureq::AgentBuilder::new()
        .timeout(REQUEST_TIMEOUT)
        .timeout_connect(CONNECT_TIMEOUT)
        .build();
    let health_url = format!("http://127.0.0.1:{port}/api/health");
    let queue_url = format!("http://127.0.0.1:{port}/api/queue");
    let devices_url = format!("http://127.0.0.1:{port}/api/devices");

    // 三个「下次到期时刻」初始为 now：线程起步即各抓一轮，UI 无空窗
    let mut next_health = Instant::now();
    let mut next_queue = Instant::now();
    let mut next_devices = Instant::now();

    loop {
        let now = Instant::now();

        if now >= next_health {
            next_health = now + HEALTH_INTERVAL;
            let result = get_json::<HealthSnapshot>(&agent, &health_url);
            if let Ok(mut s) = shared.lock() {
                match result {
                    Ok(snap) => s.health.record_ok(snap),
                    Err(e) => fail_throttled("health", &e, &mut s.health),
                }
                ctx.request_repaint();
            }
        }

        if now >= next_queue {
            next_queue = now + QUEUE_INTERVAL;
            let result = get_json::<QueueSnapshot>(&agent, &queue_url);
            if let Ok(mut s) = shared.lock() {
                match result {
                    Ok(snap) => s.queue.record_ok(snap),
                    Err(e) => fail_throttled("queue", &e, &mut s.queue),
                }
                ctx.request_repaint();
            }
        }

        if now >= next_devices {
            next_devices = now + DEVICES_INTERVAL;
            let result = get_json::<Vec<DeviceSnapshot>>(&agent, &devices_url);
            if let Ok(mut s) = shared.lock() {
                match result {
                    Ok(snap) => s.devices.record_ok(snap),
                    Err(e) => fail_throttled("devices", &e, &mut s.devices),
                }
                ctx.request_repaint();
            }
        }

        // 睡到三个档位中最近的下一个到期点（切片上限见 MAX_SLEEP）
        let wake = next_health.min(next_queue).min(next_devices);
        std::thread::sleep(wake.saturating_duration_since(Instant::now()).min(MAX_SLEEP));
    }
}

/// 单次 GET + JSON 反序列化。任何一步失败都归一为 String 错误（失败日志节流用）。
fn get_json<T: for<'de> Deserialize<'de>>(agent: &ureq::Agent, url: &str) -> Result<T, String> {
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

/// 失败日志节流：进入失败（首次）与之后每 30 次失败打一条（2s 档 ≈ 每分钟
/// 一条），daemon 宕机期间不刷屏；锁中毒时本函数不会被调（调用方已跳过）。
fn fail_throttled<T>(endpoint: &str, err: &str, slot: &mut ProbeSlot<T>) {
    let n = slot.failures;
    slot.record_fail();
    if n == 0 || (n + 1).is_multiple_of(30) {
        eprintln!("ep-tray: 探询 {endpoint} 连续失败 {} 次: {err}", n + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 快照形状守护：fixture 对照 daemon handler 实际响应（防接线漂移）──

    #[test]
    fn health_snapshot_parses_daemon_shape() {
        let snap: HealthSnapshot = serde_json::from_str(
            r#"{"status":"ok","version":"0.1.0"}"#,
        )
        .expect("health 形状应可解析");
        assert_eq!(snap.status, "ok");
        assert_eq!(snap.version, "0.1.0");
    }

    #[test]
    fn queue_snapshot_parses_daemon_shape() {
        let snap: QueueSnapshot =
            serde_json::from_str(r#"{"paused":true,"running":2,"queued":5}"#)
                .expect("queue 形状应可解析");
        assert!(snap.paused);
        assert_eq!(snap.running, 2);
        assert_eq!(snap.queued, 5);
    }

    #[test]
    fn device_snapshot_parses_daemon_shape_with_null_metrics() {
        // 对照 devices.rs 测试：指标未探测 → null；跨栈归并 → stacks 全量
        let snap: Vec<DeviceSnapshot> = serde_json::from_str(
            r#"[
            {"id":"rocm:0","backend":"rocm",
             "name":"AMD Radeon RX 7900 XTX (RADV NAVI31)",
             "stacks":["rocm","vulkan"],
             "total_memory_mb":24563,"used_memory_mb":536,
             "utilization":23,"temperature":41},
            {"id":"cpu","backend":"cpu","name":"Test CPU","stacks":["cpu"],
             "total_memory_mb":16384,"used_memory_mb":null,
             "utilization":null,"temperature":null}
        ]"#,
        )
        .expect("devices 形状应可解析");
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].stacks, vec!["rocm", "vulkan"]);
        assert_eq!(snap[0].temperature, Some(41));
        assert_eq!(snap[1].used_memory_mb, None);
    }

    #[test]
    fn snapshot_ignores_unknown_fields() {
        // daemon 未来加字段不破坏 tray 解析（serde 默认忽略未知键）
        let snap: HealthSnapshot = serde_json::from_str(
            r#"{"status":"ok","version":"0.1.0","uptime_s":999}"#,
        )
        .expect("未知字段应被忽略");
        assert_eq!(snap.status, "ok");
    }

    // ── ProbeSlot 状态机：成功/失败/新鲜度（降级判定，GT-2 验收点）──

    #[test]
    fn slot_starts_empty_and_stale() {
        let slot: ProbeSlot<HealthSnapshot> = ProbeSlot::default();
        assert!(slot.data.is_none());
        assert!(!slot.is_fresh(), "从未成功过 → 永远不新鲜");
        assert_eq!(slot.failures, 0);
    }

    #[test]
    fn slot_success_clears_failures_and_marks_fresh() {
        let mut slot: ProbeSlot<QueueSnapshot> = ProbeSlot::default();
        slot.record_fail();
        slot.record_fail();
        assert_eq!(slot.failures, 2);
        assert!(slot.data.is_none());
        slot.record_ok(QueueSnapshot {
            paused: false,
            running: 1,
            queued: 0,
        });
        assert_eq!(slot.failures, 0, "成功清零失败计数");
        assert!(slot.is_fresh());
        assert_eq!(slot.data.unwrap().running, 1);
    }

    #[test]
    fn slot_failure_drops_data() {
        let mut slot: ProbeSlot<HealthSnapshot> = ProbeSlot::default();
        slot.record_ok(HealthSnapshot {
            status: "ok".into(),
            version: "0.1.0".into(),
        });
        slot.record_fail();
        assert!(slot.data.is_none(), "失败即置 None（宕机降级）");
        assert_eq!(slot.failures, 1);
    }

    #[test]
    fn slot_freshness_expires_after_stale_after() {
        let mut slot: ProbeSlot<HealthSnapshot> = ProbeSlot::default();
        slot.record_ok(HealthSnapshot {
            status: "ok".into(),
            version: "0.1.0".into(),
        });
        // 手工把成功时刻拨回 STALE_AFTER 之前（模拟 daemon 停止响应后超时）
        slot.at = Some(Instant::now() - STALE_AFTER - Duration::from_millis(1));
        assert!(!slot.is_fresh(), "超 STALE_AFTER → 失联判定");
    }
}
