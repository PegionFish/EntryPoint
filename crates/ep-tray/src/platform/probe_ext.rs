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

use std::path::Path;
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
///
/// `token`：GT-3 探询鉴权修复（GT-2 遗留）——resolved root 下
/// `config/app.toml` 的共享令牌（见 [`read_probe_token`]），非空时所有
/// /api 请求带 `Authorization: Bearer <token>`。启动时读一次（与 log_root
/// 同款一次性解析；daemon 配置无热更新，令牌轮换需重启托盘）。
pub(crate) fn run_probe_thread(
    port: u16,
    shared: Arc<Mutex<UiShared>>,
    ctx: egui::Context,
    token: Option<String>,
) {
    let agent = ureq::AgentBuilder::new()
        .timeout(REQUEST_TIMEOUT)
        .timeout_connect(CONNECT_TIMEOUT)
        .build();
    let health_url = format!("http://127.0.0.1:{port}/api/health");
    let queue_url = format!("http://127.0.0.1:{port}/api/queue");
    let devices_url = format!("http://127.0.0.1:{port}/api/devices");
    if token.is_some() {
        eprintln!("ep-tray: 探询已启用 Bearer 鉴权（config/app.toml 共享令牌）");
    }

    // 三个「下次到期时刻」初始为 now：线程起步即各抓一轮，UI 无空窗
    let mut next_health = Instant::now();
    let mut next_queue = Instant::now();
    let mut next_devices = Instant::now();

    loop {
        let now = Instant::now();

        if now >= next_health {
            next_health = now + HEALTH_INTERVAL;
            let result = get_json::<HealthSnapshot>(&agent, &health_url, token.as_deref());
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
            let result = get_json::<QueueSnapshot>(&agent, &queue_url, token.as_deref());
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
            let result = get_json::<Vec<DeviceSnapshot>>(&agent, &devices_url, token.as_deref());
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
/// `token` 非空 → `Authorization: Bearer` 头（daemon auth.rs：Bearer/X-API-Key/
/// GET ?token= 均可，探询用标准 Bearer）。
fn get_json<T: for<'de> Deserialize<'de>>(
    agent: &ureq::Agent,
    url: &str,
    token: Option<&str>,
) -> Result<T, String> {
    let mut req = agent.get(url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let resp = req.call().map_err(|e| e.to_string())?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

// ─── 探询鉴权令牌读取（GT-3，GT-2 遗留 401 修复） ──────────────────────────

/// 从 resolved root 读 `config/app.toml` 的共享令牌（纯文件读 + 行扫描）。
/// 读取/解析失败一律 None（等价「无令牌」，探询退回匿名——access_protection
/// =none 的默认部署不受影响）。
pub(crate) fn read_probe_token(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("config").join("app.toml")).ok()?;
    scan_api_token(&text)
}

/// app.toml 行扫描取鉴权令牌（**不引 toml crate** 的 naive 实现）。
///
/// 段感知：跟踪 `[section]` 行，只认 `[server]` 与 `[api]` 段内的
/// `token = "..."` 键；**优先 `[server].token`，兜底 `[api].token**。
///
/// **与票面口径的偏差说明**：派发单写「读 [api].token」，但 daemon 真实校验
/// 面（crates/ep-daemon/src/api/auth.rs:108-119）是 `/api/*` 全局访问保护
/// 校验 **`[server].token`**（`[api].token` 只管 `/api/v1/*` 推理面；全局
/// token 模式下 v1 回退 server token）——只扫 [api] 会拿错令牌、401 修不掉，
/// 故按真实鉴权面优先 [server]，兜底 [api] 兼容票面字面（多发一个头无副作用）。
///
/// 局限（注释留档）：不做完整 TOML 解析——不支持多行字符串/转义/内联表；
/// 每段取第一个合法 token（重复键不覆盖）；键名精确匹配 `token`（不含
/// `xxx_token`）；值支持双引号/单引号/裸值（剥行内 `#` 注释）。daemon 侧
/// 以 serde toml 为准，本扫描目标仅是「常见配置形状下取到正确 Bearer 头」。
pub(crate) fn scan_api_token(text: &str) -> Option<String> {
    let mut section: Option<String> = None;
    let mut server_token: Option<String> = None;
    let mut api_token: Option<String> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // 段头：取 [ 与 ] 之间
            if let Some(end) = line.find(']') {
                let name = line[1..end].trim();
                section = Some(name.to_owned());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "token" {
            continue;
        }
        let Some(sec) = section.as_deref() else {
            continue; // 段外 token 不认（防御：非本约定面）
        };
        let value = strip_toml_value(value);
        if value.is_empty() {
            continue;
        }
        // 每段首个合法值生效（不覆盖）
        match sec {
            "server" if server_token.is_none() => server_token = Some(value),
            "api" if api_token.is_none() => api_token = Some(value),
            _ => {}
        }
    }
    server_token.or(api_token)
}

/// 取键值文本：双引号/单引号取引号内子串；裸值剥行内 `#` 注释后 trim；
/// 非法 header 字符（非可见 ASCII）整体拒绝（ureq 头值仅接受可见 ASCII，
/// 防御性处理——常规令牌皆为 ASCII）。
fn strip_toml_value(value: &str) -> String {
    let v = value.trim();
    let extracted = if let Some(rest) = v.strip_prefix('"') {
        match rest.find('"') {
            Some(end) => &rest[..end],
            None => return String::new(), // 引号不闭合 → 视为无效
        }
    } else if let Some(rest) = v.strip_prefix('\'') {
        match rest.find('\'') {
            Some(end) => &rest[..end],
            None => return String::new(),
        }
    } else {
        // 裸值（TOML 合法形态仅限部分类型；防御性接受并剥注释）
        v.split('#').next().unwrap_or("").trim()
    };
    let s = extracted.trim().to_owned();
    // 可见 ASCII 才可作 HTTP 头值（空格允许；控制字符拒绝）
    if s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        s
    } else {
        String::new()
    }
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

    // ── 探询鉴权令牌扫描（GT-3，GT-2 遗留 401 修复）──

    #[test]
    fn scan_prefers_server_token() {
        let text = "[server]\naccess_protection = \"token\"\ntoken = \"srv-secret\"\n\n[api]\ntoken = \"api-secret\"\n";
        assert_eq!(scan_api_token(text).as_deref(), Some("srv-secret"),
            "/api/* 全局门校验 [server].token（auth.rs 实面），优先级高于 [api]");
    }

    #[test]
    fn scan_falls_back_to_api_token() {
        let text = "[server]\naccess_protection = \"token\"\n\n[api]\ntoken = \"api-secret\"\n";
        assert_eq!(scan_api_token(text).as_deref(), Some("api-secret"),
            "票面字面兜底：[api].token 在 [server] 缺失时生效");
    }

    #[test]
    fn scan_ignores_other_sections_and_bare_keys() {
        let text = "[llm]\ntoken = \"should-not-match\"\n\n[server]\napi_key = \"x\"\ntokens = \"y\"\n";
        assert_eq!(scan_api_token(text), None, "键名精确 token、段限 server/api");
    }

    #[test]
    fn scan_handles_quotes_comments_and_bare_values() {
        // 行内注释（引号内 # 不截断）
        assert_eq!(
            scan_api_token("[server]\ntoken = \"a#b\" # comment\n").as_deref(),
            Some("a#b")
        );
        // 单引号字面串
        assert_eq!(scan_api_token("[server]\ntoken = 'lit-tok'\n").as_deref(), Some("lit-tok"));
        // 裸值剥注释
        assert_eq!(scan_api_token("[server]\ntoken = bare-tok # note\n").as_deref(), Some("bare-tok"));
        // 空串 → None（fail-closed 场景本就 401，不发无效头）
        assert_eq!(scan_api_token("[server]\ntoken = \"\"\n"), None);
        // 注释与空行跳过；段外键不认
        assert_eq!(scan_api_token("# c\n\n[server]\n# token = commented\n").as_deref(), None);
    }

    #[test]
    fn scan_rejects_non_ascii_header_value() {
        // 非 ASCII（HTTP 头值非法）→ 拒绝，不发无效头
        assert_eq!(scan_api_token("[server]\ntoken = \"令牌-中文\"\n"), None);
    }

    #[test]
    fn scan_first_token_per_section_wins() {
        let text = "[server]\ntoken = \"first\"\ntoken = \"second\"\n";
        assert_eq!(scan_api_token(text).as_deref(), Some("first"));
    }

    /// 文件级：临时 root/config/app.toml fixture → read_probe_token。
    #[test]
    fn read_probe_token_reads_fixture_file() {
        static TMP_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ep-tray-gt3-token-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let cfg = dir.join("config").join("app.toml");
        std::fs::write(
            &cfg,
            "[server]\nhost = \"127.0.0.1\"\nport = 9800\naccess_protection = \"token\"\ntoken = \"file-fixture-token\"\n",
        )
        .unwrap();
        assert_eq!(
            read_probe_token(&dir).as_deref(),
            Some("file-fixture-token")
        );
        // 无 config/app.toml → None（探询退回匿名）
        let empty = std::env::temp_dir().join(format!("ep-tray-gt3-token-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(read_probe_token(&empty), None);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
