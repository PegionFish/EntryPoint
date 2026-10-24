//! 模块故障自愈监督器（T-14）。
//!
//! 每 1 秒采样一次模块进程状态，只处理“曾 Running → Error（进程消失 /
//! 健康失败）”这一类运行中死亡：随后调用既有拉起入口
//! [`crate::api::autostart::ensure_module_running`] 自动重启。
//!
//! 调度规则：
//! - 首次死亡立即重启；重启未恢复（启动失败或 10 分钟内再次死亡）后按
//!   指数退避重试——2 秒起、每次 ×2，封顶 `max_backoff_secs`；
//! - 模块稳定运行 10 分钟后失败计数清零；
//! - 连续失败达 `circuit_breaker_after` 次后熔断：停止自动重启并写事件
//!   日志，需用户手动启动模块解除（观察到非 supervisor 发起的 Starting
//!   即复位）；`circuit_breaker_after = 0` 表示不熔断；
//! - 开关关闭时不重启、不写事件，并丢弃全部调度状态。
//!
//! 任务前按需拉起（§6.5）仍走既有 autostart 路径，不退避；本模块只管
//! 运行中死亡。每次重启尝试/结果与熔断都写统一事件日志（eventlog）：
//! `module_auto_restart {module_id, attempt, stage, backoff_secs, error?}`、
//! `module_circuit_breaker {module_id, failures, threshold}`。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use tracing::{debug, info, warn};

use ep_core::config::ModulesAutoRestartConfig;
use ep_core::types::ServiceStatus;

use crate::api::autostart;
use crate::eventlog;
use crate::state::AppState;

/// 事件日志类型：自愈重启尝试 / 结果
pub(crate) const RESTART_EVENT_TYPE: &str = "module_auto_restart";
/// 事件日志类型：连续失败熔断
pub(crate) const BREAKER_EVENT_TYPE: &str = "module_circuit_breaker";

/// 状态采样轮询间隔
const TICK: Duration = Duration::from_secs(1);
/// 视为“稳定运行”的时长：满该时长后重置失败计数与退避
const STABLE_RUNNING_SECS: i64 = 10 * 60;
/// 退避基数（秒）：第 2 次起的重试间隔为 2、4、8…（首次立即重启）
const BASE_BACKOFF_SECS: u64 = 2;

// ─── 事件（纯状态机输出，由运行循环落盘）────────────────────────────────

/// 重启阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartStage {
    /// 已发起一次重启尝试（已交给拉起入口）
    Starting,
    /// 拉起入口返回成功：进程已通过健康检查
    Succeeded,
    /// 本轮重启失败，将按退避重试或熔断
    Failed,
}

impl RestartStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

/// 一次自愈重启尝试 / 结果事件
#[derive(Debug, Clone, PartialEq, Eq)]
struct RestartEvent {
    module_id: String,
    /// 第几次连续恢复尝试（从 1 开始）
    attempt: u32,
    /// 本轮启动前等待的退避时长（秒；首次为 0）
    backoff_secs: u64,
    stage: RestartStage,
    error: Option<String>,
    at: DateTime<Utc>,
}

/// 连续失败熔断事件
#[derive(Debug, Clone, PartialEq, Eq)]
struct BreakerEvent {
    module_id: String,
    failures: u32,
    threshold: u32,
    at: DateTime<Utc>,
}

/// 需要写事件日志的动作
#[derive(Debug, Clone, PartialEq, Eq)]
enum SupervisorEvent {
    Restart(RestartEvent),
    CircuitBreaker(BreakerEvent),
}

/// 到期需要执行的重启（由运行循环交给 autostart）
#[derive(Debug, Clone, PartialEq, Eq)]
struct RestartRequest {
    module_id: String,
    attempt: u32,
    backoff_secs: u64,
}

// ─── 纯状态机（可注入时间，便于单测）────────────────────────────────────

/// 单个模块的自愈调度状态
#[derive(Debug, Default)]
struct ModuleRuntime {
    /// 上一轮采样到的状态（用于判定“曾 Running → Error”跃迁）
    last_status: Option<ServiceStatus>,
    /// 本轮连续 Running 的起点
    running_since: Option<DateTime<Utc>>,
    /// 连续失败计数（含导致本轮重启的死亡）
    failures: u32,
    /// 下一次重试的到期时间
    restart_at: Option<DateTime<Utc>>,
    /// supervisor 发起的重启正在执行（避免重复调度）
    in_flight: bool,
    /// 已熔断：本轮不再自动重启
    circuit_open: bool,
}

/// 模块自愈调度状态机。
#[derive(Debug, Default)]
struct Supervisor {
    modules: HashMap<String, ModuleRuntime>,
}

impl Supervisor {
    fn new() -> Self {
        Self::default()
    }

    /// 当前已跟踪的模块 id（用于下一轮采样，覆盖已死亡不再 Running 的模块）
    fn tracked_ids(&self) -> Vec<String> {
        self.modules.keys().cloned().collect()
    }

    /// 丢弃全部调度状态（开关关闭时调用）
    fn reset(&mut self) {
        self.modules.clear();
    }

    /// 观察一轮模块状态快照，返回需要落盘的事件。
    ///
    /// 只有 `last_status == Running` 的模块首次进入 Error 才判定为“运行中死亡”；
    /// 手动启动、手动停止、空闲释放都不会被误判为需要自愈的故障。
    fn observe(
        &mut self,
        samples: &[(String, ServiceStatus)],
        now: DateTime<Utc>,
        policy: &ModulesAutoRestartConfig,
    ) -> Vec<SupervisorEvent> {
        let mut events = Vec::new();
        if !policy.enabled {
            self.reset();
            return events;
        }

        // 已卸载模块不再跟踪，避免状态泄漏
        let present: std::collections::HashSet<&str> =
            samples.iter().map(|(id, _)| id.as_str()).collect();
        self.modules
            .retain(|id, _| present.contains(id.as_str()));

        for (id, status) in samples {
            let rt = self.modules.entry(id.clone()).or_default();
            let prev = rt.last_status.replace(status.clone());

            match status {
                ServiceStatus::Running => {
                    if rt.running_since.is_none() {
                        rt.running_since = Some(now);
                    }
                    // 稳定运行满 10 分钟：重置失败计数与熔断（退避随之归零）
                    if let Some(since) = rt.running_since {
                        let stable = (now - since).num_seconds() >= STABLE_RUNNING_SECS;
                        if stable && (rt.failures > 0 || rt.circuit_open) {
                            rt.failures = 0;
                            rt.restart_at = None;
                            rt.circuit_open = false;
                            debug!(module_id = %id, "模块已稳定运行，自愈退避计数重置");
                        }
                    }
                }
                ServiceStatus::Starting | ServiceStatus::Preparing => {
                    rt.running_since = None;
                    // 非 supervisor 发起的启动 = 用户手动启动：解除熔断/退避。
                    // 任务前按需拉起同样会走到这里，按手动启动同样处理（无关自愈）。
                    if !rt.in_flight
                        && (rt.circuit_open || rt.failures > 0 || rt.restart_at.is_some())
                    {
                        rt.circuit_open = false;
                        rt.failures = 0;
                        rt.restart_at = None;
                        debug!(module_id = %id, "检测到手动启动，自愈状态已复位");
                    }
                }
                ServiceStatus::Error(_) => {
                    rt.running_since = None;
                    if rt.in_flight || rt.circuit_open {
                        continue;
                    }
                    // 仅在“曾 Running → Error”的首次跃迁判定为运行中死亡
                    if !matches!(prev, Some(ServiceStatus::Running)) {
                        continue;
                    }
                    rt.failures = rt.failures.saturating_add(1);
                    if let Some(threshold) = breaker_threshold(policy) {
                        if rt.failures >= threshold {
                            rt.circuit_open = true;
                            rt.restart_at = None;
                            warn!(
                                module_id = %id,
                                failures = rt.failures,
                                threshold,
                                "模块连续故障达到阈值，已熔断自动重启（需手动启动解除）"
                            );
                            events.push(SupervisorEvent::CircuitBreaker(BreakerEvent {
                                module_id: id.clone(),
                                failures: rt.failures,
                                threshold,
                                at: now,
                            }));
                            continue;
                        }
                    }
                    let delay = backoff_secs(rt.failures, policy);
                    rt.restart_at =
                        Some(now + chrono::Duration::seconds(delay.min(i64::MAX as u64) as i64));
                }
                ServiceStatus::Stopped | ServiceStatus::NotReady => {
                    rt.running_since = None;
                    // 用户手动停止 / 空闲释放：无待执行重试且未熔断时复位失败计数。
                    // 失败重启后的清理也会短暂经过 Stopped，此时 restart_at 已排定或有
                    // 熔断标记，必须保留退避进度；熔断只能由手动启动或稳定运行解除。
                    if !rt.in_flight && rt.restart_at.is_none() && !rt.circuit_open {
                        rt.failures = 0;
                    }
                }
            }
        }
        events
    }

    /// 取出到期应执行的重启，并标记为 in_flight（结果经 `on_restart_result` 回填）。
    fn take_due_restarts(
        &mut self,
        now: DateTime<Utc>,
        policy: &ModulesAutoRestartConfig,
    ) -> Vec<RestartRequest> {
        let mut due = Vec::new();
        for (id, rt) in self.modules.iter_mut() {
            if rt.circuit_open || rt.in_flight {
                continue;
            }
            let Some(at) = rt.restart_at else {
                continue;
            };
            if at > now {
                continue;
            }
            rt.in_flight = true;
            rt.restart_at = None;
            due.push(RestartRequest {
                module_id: id.clone(),
                attempt: rt.failures.max(1),
                backoff_secs: backoff_secs(rt.failures, policy),
            });
        }
        // 固定顺序，便于日志与测试断言
        due.sort_by(|a, b| a.module_id.cmp(&b.module_id));
        due
    }

    /// 回填一次重启的结果，返回需要落盘的事件（可能同时触发熔断）。
    fn on_restart_result(
        &mut self,
        module_id: &str,
        result: Result<(), String>,
        now: DateTime<Utc>,
        policy: &ModulesAutoRestartConfig,
    ) -> Vec<SupervisorEvent> {
        let mut events = Vec::new();
        let Some(rt) = self.modules.get_mut(module_id) else {
            return events;
        };
        // 非 supervisor 发起（已被复位或模块已卸载）的结果直接忽略
        if !rt.in_flight {
            return events;
        }
        rt.in_flight = false;
        let attempt = rt.failures.max(1);

        match result {
            Ok(()) => {
                rt.restart_at = None;
                info!(module_id = %module_id, attempt, "模块自愈重启成功");
                events.push(SupervisorEvent::Restart(RestartEvent {
                    module_id: module_id.to_string(),
                    attempt,
                    backoff_secs: 0,
                    stage: RestartStage::Succeeded,
                    error: None,
                    at: now,
                }));
            }
            Err(err) => {
                rt.failures = rt.failures.saturating_add(1);
                let delay = backoff_secs(rt.failures, policy);
                warn!(
                    module_id = %module_id,
                    attempt,
                    failures = rt.failures,
                    error = %err,
                    "模块自愈重启失败"
                );
                events.push(SupervisorEvent::Restart(RestartEvent {
                    module_id: module_id.to_string(),
                    attempt,
                    backoff_secs: delay,
                    stage: RestartStage::Failed,
                    error: Some(err),
                    at: now,
                }));
                if let Some(threshold) = breaker_threshold(policy) {
                    if rt.failures >= threshold {
                        rt.circuit_open = true;
                        rt.restart_at = None;
                        warn!(
                            module_id = %module_id,
                            failures = rt.failures,
                            threshold,
                            "模块连续重启失败达到阈值，已熔断自动重启（需手动启动解除）"
                        );
                        events.push(SupervisorEvent::CircuitBreaker(BreakerEvent {
                            module_id: module_id.to_string(),
                            failures: rt.failures,
                            threshold,
                            at: now,
                        }));
                        return events;
                    }
                }
                rt.restart_at = Some(
                    now + chrono::Duration::seconds(delay.min(i64::MAX as u64) as i64),
                );
            }
        }
        events
    }
}

/// 熔断阈值：0 = 不熔断
fn breaker_threshold(policy: &ModulesAutoRestartConfig) -> Option<u32> {
    (policy.circuit_breaker_after > 0).then_some(policy.circuit_breaker_after)
}

/// 第 `failures` 次连续失败后、下一轮重启前的退避时长（秒）。
///
/// `failures == 1`（运行中死亡）返回 0：首次立即重启，满足“故障后 2 秒内
/// 恢复”的可用性目标；其后的失败重试为 2、4、8…，封顶 `max_backoff_secs`
/// （小于 2 时按 2 秒处理）。
fn backoff_secs(failures: u32, policy: &ModulesAutoRestartConfig) -> u64 {
    if failures <= 1 {
        return 0;
    }
    let cap = u64::from(policy.max_backoff_secs).max(BASE_BACKOFF_SECS);
    let exp = (failures - 2).min(20);
    BASE_BACKOFF_SECS
        .saturating_mul(2u64.saturating_pow(exp))
        .min(cap)
}

// ─── 事件落盘 ────────────────────────────────────────────────────────────

/// 将自愈事件写入统一事件日志（复用 eventlog，不新造格式）。
fn write_event(root: &Path, event: &SupervisorEvent) {
    let payload = match event {
        SupervisorEvent::Restart(e) => {
            let mut value = json!({
                "ts": e.at.timestamp(),
                "type": RESTART_EVENT_TYPE,
                "module_id": e.module_id.as_str(),
                "attempt": e.attempt,
                "backoff_secs": e.backoff_secs,
                "stage": e.stage.as_str(),
            });
            if let Some(err) = &e.error {
                value["error"] = json!(err);
            }
            value
        }
        SupervisorEvent::CircuitBreaker(e) => json!({
            "ts": e.at.timestamp(),
            "type": BREAKER_EVENT_TYPE,
            "module_id": e.module_id.as_str(),
            "failures": e.failures,
            "threshold": e.threshold,
        }),
    };
    eventlog::append_event(root, &payload);
}

// ─── 运行循环 ────────────────────────────────────────────────────────────

/// 启动模块故障自愈监督循环（daemon 启动时调用一次）。
///
/// 每 [`TICK`] 采样一次状态：先回填上一轮重启结果，再观察状态并执行到期
/// 重启。重启本身在独立 task 中执行（内部会等待健康检查），不阻塞采样。
pub fn spawn_health_supervisor(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut supervisor = Supervisor::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, Result<(), String>)>();
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;

            let policy = {
                let cfg = state.config.read().await;
                cfg.modules.auto_restart.clone()
            };
            if !policy.enabled {
                supervisor.reset();
                continue;
            }

            // 1) 回填上一轮重启结果（可能触发退避重试或熔断事件）
            while let Ok((module_id, result)) = rx.try_recv() {
                let events =
                    supervisor.on_restart_result(&module_id, result, Utc::now(), &policy);
                for event in &events {
                    write_event(&state.root, event);
                }
            }

            // 2) 采样：已跟踪模块（可能已 Error/Stopped）+ 当前运行中模块
            let samples: Vec<(String, ServiceStatus)> = {
                let pm = state.process_manager.read().await;
                let mut ids = supervisor.tracked_ids();
                ids.extend(
                    pm.list_running()
                        .into_iter()
                        .map(|inst| inst.module_id.clone()),
                );
                ids.sort();
                ids.dedup();
                ids.into_iter()
                    .filter_map(|id| pm.get_status(&id).cloned().map(|status| (id, status)))
                    .collect()
            };

            // 3) 观察状态，写熔断/恢复事件
            let now = Utc::now();
            let events = supervisor.observe(&samples, now, &policy);
            for event in &events {
                write_event(&state.root, event);
            }

            // 4) 执行到期重启（每次尝试写“starting”事件，结果异步回填）
            for request in supervisor.take_due_restarts(now, &policy) {
                let event = SupervisorEvent::Restart(RestartEvent {
                    module_id: request.module_id.clone(),
                    attempt: request.attempt,
                    backoff_secs: request.backoff_secs,
                    stage: RestartStage::Starting,
                    error: None,
                    at: now,
                });
                write_event(&state.root, &event);
                info!(
                    module_id = %request.module_id,
                    attempt = request.attempt,
                    backoff_secs = request.backoff_secs,
                    "检测到模块进程异常退出，开始自动重启"
                );

                let state = state.clone();
                let tx = tx.clone();
                let module_id = request.module_id.clone();
                tokio::spawn(async move {
                    let result = autostart::ensure_module_running(&state, &module_id)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send((module_id, result));
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(max_backoff_secs: u32, circuit_breaker_after: u32) -> ModulesAutoRestartConfig {
        ModulesAutoRestartConfig {
            enabled: true,
            max_backoff_secs,
            circuit_breaker_after,
        }
    }

    fn sample(module_id: &str, status: ServiceStatus) -> Vec<(String, ServiceStatus)> {
        vec![(module_id.to_string(), status)]
    }

    fn died() -> ServiceStatus {
        ServiceStatus::Error("process exited with status: signal: 9 (SIGKILL)".to_string())
    }

    /// 1) 开关关闭：运行中死亡不重启、不产生事件
    #[test]
    fn disabled_policy_never_restarts_or_logs() {
        let mut supervisor = Supervisor::new();
        let off = ModulesAutoRestartConfig::default();
        assert!(!off.enabled);

        let t0 = Utc::now();
        supervisor.observe(&sample("asr", ServiceStatus::Running), t0, &off);
        let events = supervisor.observe(
            &sample("asr", died()),
            t0 + chrono::Duration::seconds(1),
            &off,
        );
        assert!(events.is_empty());
        assert!(supervisor
            .take_due_restarts(t0 + chrono::Duration::seconds(60), &off)
            .is_empty());
    }

    /// 2) 运行中死亡：首次立即重启（退避 0），重启成功后写 succeeded 事件；
    ///    稳定运行 10 分钟后失败计数重置，下一次死亡仍立即重启
    #[test]
    fn first_death_restarts_immediately_then_stable_resets() {
        let mut supervisor = Supervisor::new();
        let p = policy(30, 5);
        let t0 = Utc::now();

        // 运行中 → 死亡（取快照跃迁）
        supervisor.observe(&sample("asr", ServiceStatus::Running), t0, &p);
        let t1 = t0 + chrono::Duration::seconds(1);
        assert!(supervisor.observe(&sample("asr", died()), t1, &p).is_empty());

        // 到期 → 立即重启，attempt=1、退避 0
        let due = supervisor.take_due_restarts(t1, &p);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].module_id, "asr");
        assert_eq!(due[0].attempt, 1);
        assert_eq!(due[0].backoff_secs, 0);
        // 已 in_flight，不会重复调度
        assert!(supervisor
            .take_due_restarts(t1 + chrono::Duration::seconds(5), &p)
            .is_empty());

        // 重启成功事件
        let t2 = t1 + chrono::Duration::seconds(2);
        let events = supervisor.on_restart_result("asr", Ok(()), t2, &p);
        assert_eq!(events.len(), 1);
        match &events[0] {
            SupervisorEvent::Restart(e) => {
                assert_eq!(e.stage, RestartStage::Succeeded);
                assert_eq!(e.attempt, 1);
                assert_eq!(e.error, None);
            }
            other => panic!("expected restart event, got {other:?}"),
        }

        // 稳定运行满 10 分钟 → 计数清零
        supervisor.observe(&sample("asr", ServiceStatus::Running), t2, &p);
        supervisor.observe(
            &sample("asr", ServiceStatus::Running),
            t2 + chrono::Duration::seconds(STABLE_RUNNING_SECS + 1),
            &p,
        );
        assert_eq!(supervisor.modules["asr"].failures, 0);

        // 再次死亡仍是首次：立即重启
        let t3 = t2 + chrono::Duration::seconds(STABLE_RUNNING_SECS + 2);
        supervisor.observe(&sample("asr", died()), t3, &p);
        let due = supervisor.take_due_restarts(t3, &p);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].backoff_secs, 0);
    }

    /// 3) 重启失败后的指数退避：2s → 4s → 8s，封顶 8s（max_backoff_secs=8）
    #[test]
    fn failed_restarts_back_off_exponentially_with_cap() {
        let mut supervisor = Supervisor::new();
        let p = policy(8, 0); // 0 = 不熔断，便于验证退避序列
        let t0 = Utc::now();
        supervisor.observe(&sample("asr", ServiceStatus::Running), t0, &p);
        supervisor.observe(
            &sample("asr", died()),
            t0 + chrono::Duration::seconds(1),
            &p,
        );
        let mut now = t0 + chrono::Duration::seconds(1);
        assert_eq!(supervisor.take_due_restarts(now, &p).len(), 1);

        // 失败 → 等待 2s
        let events = supervisor.on_restart_result("asr", Err("spawn failed".to_string()), now, &p);
        let backoff = match &events[0] {
            SupervisorEvent::Restart(e) => {
                assert_eq!(e.stage, RestartStage::Failed);
                e.backoff_secs
            }
            other => panic!("expected restart event, got {other:?}"),
        };
        assert_eq!(backoff, 2);
        now += chrono::Duration::seconds(1); // 到期前 1s
        assert!(supervisor.take_due_restarts(now, &p).is_empty());
        now += chrono::Duration::seconds(1); // 到期
        assert_eq!(supervisor.take_due_restarts(now, &p).len(), 1);

        // 连续失败：4s → 8s → 8s（封顶）
        for expected in [4u64, 8, 8] {
            let events =
                supervisor.on_restart_result("asr", Err("health timed out".to_string()), now, &p);
            let backoff = match &events[0] {
                SupervisorEvent::Restart(e) => e.backoff_secs,
                other => panic!("expected restart event, got {other:?}"),
            };
            assert_eq!(backoff, expected, "backoff cap/ladder mismatch");
            // 到期前不可调度
            assert!(supervisor
                .take_due_restarts(
                    now + chrono::Duration::seconds(expected as i64 - 1),
                    &p
                )
                .is_empty());
            now += chrono::Duration::seconds(expected as i64);
            assert_eq!(supervisor.take_due_restarts(now, &p).len(), 1);
        }
    }

    /// 4) 连续失败达阈值 → 熔断：停止调度并写 module_circuit_breaker 事件
    #[test]
    fn circuit_breaker_opens_after_threshold() {
        let mut supervisor = Supervisor::new();
        let p = policy(30, 3);
        let t0 = Utc::now();
        supervisor.observe(&sample("asr", ServiceStatus::Running), t0, &p);
        supervisor.observe(
            &sample("asr", died()),
            t0 + chrono::Duration::seconds(1),
            &p,
        );
        let mut now = t0 + chrono::Duration::seconds(1);
        assert_eq!(supervisor.take_due_restarts(now, &p).len(), 1);

        // 第 2、3 次连续失败：第 3 次达到阈值 → 熔断
        let mut breaker_events = 0;
        for _ in 0..2 {
            let events =
                supervisor.on_restart_result("asr", Err("spawn failed".to_string()), now, &p);
            breaker_events += events
                .iter()
                .filter(|e| matches!(e, SupervisorEvent::CircuitBreaker(_)))
                .count();
            if breaker_events == 0 {
                let delay = match &events[0] {
                    SupervisorEvent::Restart(e) => e.backoff_secs,
                    other => panic!("expected restart event, got {other:?}"),
                };
                now += chrono::Duration::seconds(delay as i64);
                assert_eq!(supervisor.take_due_restarts(now, &p).len(), 1);
            }
        }
        assert_eq!(breaker_events, 1);
        assert!(supervisor.modules["asr"].circuit_open);
        assert!(supervisor
            .take_due_restarts(now + chrono::Duration::seconds(3600), &p)
            .is_empty());

        // 熔断后再次 Error 也不会重新调度（不重复写熔断事件）
        let events = supervisor.observe(
            &sample("asr", died()),
            now + chrono::Duration::seconds(3601),
            &p,
        );
        assert!(events.is_empty());
    }

    /// 5) 熔断后失败重启清理（Stopped）不清除熔断，手动启动（Starting）才解除
    #[test]
    fn manual_start_clears_circuit_breaker() {
        let mut supervisor = Supervisor::new();
        let p = policy(30, 2);
        let t0 = Utc::now();
        supervisor.observe(&sample("asr", ServiceStatus::Running), t0, &p);
        supervisor.observe(
            &sample("asr", died()),
            t0 + chrono::Duration::seconds(1),
            &p,
        );
        let mut now = t0 + chrono::Duration::seconds(1);
        supervisor.take_due_restarts(now, &p);
        // 一次失败即达阈值 2
        let events = supervisor.on_restart_result("asr", Err("spawn failed".to_string()), now, &p);
        assert!(events
            .iter()
            .any(|e| matches!(e, SupervisorEvent::CircuitBreaker(_))));
        assert!(supervisor.modules["asr"].circuit_open);

        // autostart 失败清理会把状态置为 Stopped：熔断必须保留，等待手动启动
        now += chrono::Duration::seconds(30);
        supervisor.observe(&sample("asr", ServiceStatus::Stopped), now, &p);
        let rt = &supervisor.modules["asr"];
        assert!(rt.circuit_open);
        assert_eq!(rt.failures, 2);
        assert!(supervisor
            .take_due_restarts(now + chrono::Duration::seconds(3600), &p)
            .is_empty());

        // 手动启动
        now += chrono::Duration::seconds(1);
        let events = supervisor.observe(&sample("asr", ServiceStatus::Starting), now, &p);
        assert!(events.is_empty());
        let rt = &supervisor.modules["asr"];
        assert!(!rt.circuit_open);
        assert_eq!(rt.failures, 0);
        assert!(rt.restart_at.is_none());

        // 手动启动随后 Running，自愈恢复正常
        supervisor.observe(&sample("asr", ServiceStatus::Running), now, &p);
        assert_eq!(supervisor.modules["asr"].failures, 0);
    }

    /// 6) 失败重启后的清理（Stopped）不会丢弃已排定的退避重试
    #[test]
    fn stopped_after_failed_restart_keeps_pending_retry() {
        let mut supervisor = Supervisor::new();
        let p = policy(30, 5);
        let t0 = Utc::now();
        supervisor.observe(&sample("asr", ServiceStatus::Running), t0, &p);
        supervisor.observe(
            &sample("asr", died()),
            t0 + chrono::Duration::seconds(1),
            &p,
        );
        let now = t0 + chrono::Duration::seconds(1);
        supervisor.take_due_restarts(now, &p);
        supervisor.on_restart_result("asr", Err("spawn failed".to_string()), now, &p);
        assert!(supervisor.modules["asr"].restart_at.is_some());

        // autostart 失败清理后状态为 Stopped：应保留重试计划
        supervisor.observe(
            &sample("asr", ServiceStatus::Stopped),
            now + chrono::Duration::seconds(1),
            &p,
        );
        assert!(supervisor.modules["asr"].restart_at.is_some());
        assert_eq!(supervisor.modules["asr"].failures, 2);
    }

    /// 7) 事件真正写入 eventlog：重启开始/失败/熔断可被 read_events 检索
    #[test]
    fn supervisor_events_are_written_to_eventlog() {
        let root = temp_root("events");
        let at = Utc::now();
        write_event(
            &root,
            &SupervisorEvent::Restart(RestartEvent {
                module_id: "asr".to_string(),
                attempt: 1,
                backoff_secs: 0,
                stage: RestartStage::Starting,
                error: None,
                at,
            }),
        );
        write_event(
            &root,
            &SupervisorEvent::Restart(RestartEvent {
                module_id: "asr".to_string(),
                attempt: 2,
                backoff_secs: 2,
                stage: RestartStage::Failed,
                error: Some("spawn failed".to_string()),
                at,
            }),
        );
        write_event(
            &root,
            &SupervisorEvent::CircuitBreaker(BreakerEvent {
                module_id: "asr".to_string(),
                failures: 5,
                threshold: 5,
                at,
            }),
        );

        let restarts = eventlog::read_events(&root, None, Some(RESTART_EVENT_TYPE), 10);
        assert_eq!(restarts.len(), 2);
        assert_eq!(restarts[0]["stage"], "failed");
        assert_eq!(restarts[0]["backoff_secs"], 2);
        assert_eq!(restarts[0]["error"], "spawn failed");
        assert_eq!(restarts[1]["stage"], "starting");
        assert_eq!(restarts[1]["module_id"], "asr");

        let breakers = eventlog::read_events(&root, None, Some(BREAKER_EVENT_TYPE), 10);
        assert_eq!(breakers.len(), 1);
        assert_eq!(breakers[0]["failures"], 5);
        assert_eq!(breakers[0]["threshold"], 5);

        let _ = std::fs::remove_dir_all(&root);
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ep-health-supervisor-{tag}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp root");
        dir
    }
}
