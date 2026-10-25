//! daemon 托管状态机（T-33）——**纯逻辑，零 IO**。
//!
//! 状态转移表：
//!
//! ```text
//!                    LaunchRequested
//!        ┌────────────────────────────────────┐
//!        │                                    ▼
//!   ┌─────────┐  StopRequested   ┌──────────────┐   ProbeOk   ┌─────────┐
//!   │ Stopped │◄────────────────►│  Starting    │────────────►│ Running │
//!   └─────────┘                  │ {attempt:N}  │             └─────────┘
//!        ▲                       └──────────────┘                  │
//!        │                              │ ChildExited/SpawnFailed     │ ChildExited/
//!        │ RestartRequested             │ (attempt < MAX → 重启一次)   │ SpawnFailed
//!        │                              ▼                             │
//!        │                       ┌──────────────┐                     │
//!        │                       │   Failed     │◄────────────────────┘
//!        │                       │ (红标，不再   │  (attempt >= MAX)
//!        └──────────────────────►│  自动重启)    │
//!                                └──────────────┘
//! ```
//!
//! 纪律：本模块不 spawn、不探针、不知晓进程句柄——IO 由 platform 层执行，
//! 单测直接喂 [`Event`] 断言 [`Action`]，覆盖"崩溃重启一次/再失败红标"全链。

/// 托管尝试上限：首次拉起 + **一次**崩溃重启（再失败进入 Failed 红标）。
pub const MAX_ATTEMPTS: u32 = 2;

/// daemon 的健康三色（托盘图标与状态文字共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayColor {
    /// 运行中（端口探活通过）
    Green,
    /// 启动中 / 重启中 / 已停止（非异常的中间态）
    Yellow,
    /// 连续失败，已放弃自动重启（需用户手动介入）
    Red,
}

/// 托管阶段（状态机的状态层，与颜色解耦）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 未托管（初始 / 用户停止后）
    Stopped,
    /// 已 spawn、等待端口探活通过；`attempt` 从 1 计
    Starting { attempt: u32 },
    /// 运行中（探活通过）
    Running,
    /// 连续失败达上限，红标待手动处理
    Failed,
}

/// 输入事件（由 platform 层从真实 IO 翻译而来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// 要求拉起 daemon（托盘启动时自动 / Failed 后用户重试）
    LaunchRequested,
    /// 子进程 spawn 成功
    SpawnOk,
    /// spawn 失败（可执行文件缺失等，语义等同一次崩溃）
    SpawnFailed,
    /// 端口探活成功
    ProbeOk,
    /// 子进程意外退出（非用户停止；含外部采纳实例消失）
    ChildExited,
    /// 用户要求停止（菜单"停止 daemon"）
    StopRequested,
    /// 用户要求重启（菜单"重启 daemon"，尝试计数归零）
    RestartRequested,
}

/// 输出动作（platform 层执行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 拉起子进程
    Spawn,
    /// 终止现有子进程（若有）
    KillChild,
    /// 先终止现有子进程（若有）再拉起（重启语义）
    KillAndSpawn,
    /// 无动作
    None,
}

/// 托管状态机：`handle` 必须是唯一入口，转移全表驱动、可穷举单测。
#[derive(Debug, Clone)]
pub struct Supervisor {
    phase: Phase,
    attempts_used: u32,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            phase: Phase::Stopped,
            attempts_used: 0,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn attempts_used(&self) -> u32 {
        self.attempts_used
    }

    /// 处理一个事件，返回（动作，新状态颜色）。状态与颜色同步推进。
    pub fn handle(&mut self, ev: Event) -> (Action, TrayColor) {
        match ev {
            // （重新）开始托管：尝试计数归一；先杀残留子进程再拉起（无子进程时等价 Spawn）
            Event::LaunchRequested | Event::RestartRequested => {
                self.attempts_used = 1;
                self.phase = Phase::Starting { attempt: 1 };
                (Action::KillAndSpawn, TrayColor::Yellow)
            }

            // 用户停止：杀子进程、停止探活（platform 层据 Stopped 停探）
            Event::StopRequested => {
                self.phase = Phase::Stopped;
                (Action::KillChild, TrayColor::Yellow)
            }

            // spawn 成功：等待探活/退出事件，状态不变
            Event::SpawnOk => (Action::None, self.color()),

            // spawn 失败：等同一次崩溃
            Event::SpawnFailed => self.collapse(),

            // 意外退出：Stopped（用户刚停止后的迟到事件）忽略；其余走崩溃归宿
            Event::ChildExited if self.phase == Phase::Stopped => (Action::None, TrayColor::Yellow),
            Event::ChildExited => self.collapse(),

            // 探活通过：任何状态即运行中（含外部实例采纳 / 红标解除）
            Event::ProbeOk => {
                self.phase = Phase::Running;
                (Action::None, TrayColor::Green)
            }
        }
    }

    /// 崩溃类事件（SpawnFailed / ChildExited）的统一归宿：
    /// 未达 [`MAX_ATTEMPTS`] → 尝试数 +1 自动重启（黄）；
    /// 达上限 → [`Phase::Failed`] 红标，不再自动拉起。
    fn collapse(&mut self) -> (Action, TrayColor) {
        if self.attempts_used < MAX_ATTEMPTS {
            self.attempts_used += 1;
            self.phase = Phase::Starting {
                attempt: self.attempts_used,
            };
            (Action::Spawn, TrayColor::Yellow)
        } else {
            self.phase = Phase::Failed;
            (Action::None, TrayColor::Red)
        }
    }

    /// 当前阶段对应的托盘颜色。
    pub fn color(&self) -> TrayColor {
        match self.phase {
            Phase::Running => TrayColor::Green,
            Phase::Failed => TrayColor::Red,
            Phase::Stopped | Phase::Starting { .. } => TrayColor::Yellow,
        }
    }
}

/// 托盘状态行文字（菜单首行只读项；含 emoji 圆点，纯函数便于单测锁定文案）。
pub fn status_label(phase: Phase) -> String {
    match phase {
        Phase::Stopped => "● EntryPoint：已停止".to_string(),
        Phase::Starting { attempt } => {
            if attempt <= 1 {
                "● EntryPoint：启动中…".to_string()
            } else {
                format!("● EntryPoint：崩溃，第 {attempt} 次尝试…")
            }
        }
        Phase::Running => "● EntryPoint：运行中".to_string(),
        Phase::Failed => "● EntryPoint：异常（查看日志）".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;
    use Event::*;
    use Phase::{Failed, Running, Starting, Stopped};
    use TrayColor::{Green, Red, Yellow};

    #[test]
    fn launch_starts_first_attempt_and_spawns() {
        let mut sup = Supervisor::new();
        assert_eq!(sup.phase(), Stopped);
        assert_eq!(sup.handle(LaunchRequested), (KillAndSpawn, Yellow));
        assert_eq!(sup.phase(), Starting { attempt: 1 });
        assert_eq!(sup.attempts_used(), 1);
    }

    #[test]
    fn probe_ok_turns_running_green_from_any_phase() {
        // Stopped → 采纳外部实例
        let mut sup = Supervisor::new();
        assert_eq!(sup.handle(ProbeOk), (None, Green));
        assert_eq!(sup.phase(), Running);

        // Starting → 探活通过转绿
        let mut sup = Supervisor::new();
        sup.handle(LaunchRequested);
        assert_eq!(sup.handle(ProbeOk), (None, Green));
        assert_eq!(sup.phase(), Running);

        // Failed → 用户手工拉起外部 daemon 后红标解除
        let mut sup = Supervisor::new();
        sup.handle(LaunchRequested);
        sup.handle(ChildExited);
        sup.handle(ChildExited); // 达上限 → Failed
        assert_eq!(sup.phase(), Failed);
        assert_eq!(sup.handle(ProbeOk), (None, Green));
        assert_eq!(sup.phase(), Running);
    }

    #[test]
    fn crash_restarts_once_then_marks_failed_red() {
        // 核心规格：崩溃重启一次 / 再失败红标
        let mut sup = Supervisor::new();
        sup.handle(LaunchRequested); // attempt 1
        sup.handle(SpawnOk);
        sup.handle(ProbeOk); // Running / 绿
        assert_eq!(sup.color(), Green);

        // 第一次崩溃 → 自动重启（attempt 2）
        assert_eq!(sup.handle(ChildExited), (Spawn, Yellow));
        assert_eq!(sup.phase(), Starting { attempt: 2 });
        assert_eq!(sup.attempts_used(), 2);

        sup.handle(SpawnOk);
        sup.handle(ProbeOk);
        assert_eq!(sup.color(), Green);

        // 第二次崩溃 → 红标，不再自动拉起
        assert_eq!(sup.handle(ChildExited), (None, Red));
        assert_eq!(sup.phase(), Failed);
        // 后续再退出也不动作
        assert_eq!(sup.handle(ChildExited), (None, Red));
        assert_eq!(sup.phase(), Failed);
    }

    #[test]
    fn spawn_failure_counts_as_crash_and_exhausts_to_red() {
        let mut sup = Supervisor::new();
        sup.handle(LaunchRequested); // attempts=1
        assert_eq!(sup.handle(SpawnFailed), (Spawn, Yellow)); // attempts=2 重试
        assert_eq!(sup.phase(), Starting { attempt: 2 });
        assert_eq!(sup.handle(SpawnFailed), (None, Red)); // 达上限
        assert_eq!(sup.phase(), Failed);
    }

    #[test]
    fn stop_kills_child_and_enters_stopped() {
        let mut sup = Supervisor::new();
        sup.handle(LaunchRequested);
        sup.handle(ProbeOk);
        assert_eq!(sup.handle(StopRequested), (KillChild, Yellow));
        assert_eq!(sup.phase(), Stopped);
        // 停止后的迟到退出事件不得触发重启
        assert_eq!(sup.handle(ChildExited), (None, Yellow));
        assert_eq!(sup.phase(), Stopped);
    }

    #[test]
    fn restart_resets_attempt_budget() {
        let mut sup = Supervisor::new();
        sup.handle(LaunchRequested);
        sup.handle(ChildExited); // attempts=2
        sup.handle(ChildExited); // Failed
        assert_eq!(sup.phase(), Failed);

        // 用户重启 → 计数归零（重新获得一次崩溃重启额度）
        assert_eq!(sup.handle(RestartRequested), (KillAndSpawn, Yellow));
        assert_eq!(sup.phase(), Starting { attempt: 1 });
        assert_eq!(sup.attempts_used(), 1);

        sup.handle(ChildExited);
        assert_eq!(sup.phase(), Starting { attempt: 2 });
        sup.handle(ChildExited);
        assert_eq!(sup.phase(), Failed);
    }

    #[test]
    fn launch_from_failed_recovers_budget() {
        let mut sup = Supervisor::new();
        sup.handle(LaunchRequested);
        sup.handle(ChildExited);
        sup.handle(ChildExited);
        assert_eq!(sup.phase(), Failed);

        assert_eq!(sup.handle(LaunchRequested), (KillAndSpawn, Yellow));
        assert_eq!(sup.attempts_used(), 1);
    }

    #[test]
    fn status_label_covers_all_phases() {
        assert!(status_label(Stopped).contains("已停止"));
        assert!(status_label(Starting { attempt: 1 }).contains("启动中"));
        assert!(status_label(Starting { attempt: 2 }).contains("第 2 次"));
        assert!(status_label(Running).contains("运行中"));
        assert!(status_label(Failed).contains("异常"));
    }

    #[test]
    fn spawn_ok_is_noop_in_transient_phases() {
        let mut sup = Supervisor::new();
        sup.handle(LaunchRequested);
        assert_eq!(sup.handle(SpawnOk), (None, Yellow));
        assert_eq!(sup.phase(), Starting { attempt: 1 });

        // Running 下迟到的 SpawnOk 不改变状态与颜色
        sup.handle(ProbeOk);
        assert_eq!(sup.handle(SpawnOk), (None, Green));
        assert_eq!(sup.phase(), Running);
    }
}
