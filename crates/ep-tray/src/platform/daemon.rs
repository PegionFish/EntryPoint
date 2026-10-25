//! ep-daemon sidecar 托管线程（Windows）：把真实 IO（spawn/kill/探活）
//! 翻译成 [`crate::supervisor`] 状态机事件，执行其输出的 [`Action`]。
//!
//! 壳纪律：本模块只做进程生命周期与端口探活的接线，不实现任何业务功能。

use std::io;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use crate::supervisor::{status_label, Action, Event, Phase, Supervisor, TrayColor};

/// CREATE_NO_WINDOW：daemon 是控制台程序，托盘拉起时不得弹黑色控制台窗口
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 子进程退出/命令轮询节拍
const POLL_INTERVAL: Duration = Duration::from_millis(400);
/// 回环端口 connect 探活超时
const PROBE_TIMEOUT_MS: u64 = 400;
/// 崩溃重启前的缓速（daemon 秒级 crash-loop 时不打满 CPU）
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// UI → 托管线程命令。
#[derive(Debug)]
pub enum DaemonCommand {
    /// 重启 daemon（杀现有 + 重拉，尝试额度重置）
    Restart,
    /// 停止 daemon（杀现有，进入 Stopped）
    Stop,
    /// 退出托盘：托管中的 daemon 一并停止后结束线程
    Shutdown,
}

/// 托管线程 → UI 状态更新。
#[derive(Debug, Clone)]
pub struct StateUpdate {
    pub color: TrayColor,
    pub label: String,
}

/// sidecar 配置：daemon 可执行文件路径 + WebUI 端口。
pub struct DaemonSidecar {
    /// 与 ep-tray.exe 同目录的 ep-daemon.exe（sidecar 约定）
    pub daemon_path: PathBuf,
    /// daemon HTTP 端口（探活 + 打开 WebUI）
    pub port: u16,
}

impl DaemonSidecar {
    /// 拉起 daemon 子进程（无窗口、不继承 stdin）。
    pub fn spawn_child(&self) -> io::Result<Child> {
        Command::new(&self.daemon_path)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .spawn()
    }
}

/// 托管线程主循环。退出条件：收到 [`DaemonCommand::Shutdown`] 或命令通道断开。
pub fn run_supervisor_thread(
    sidecar: DaemonSidecar,
    commands: Receiver<DaemonCommand>,
    ui: Sender<StateUpdate>,
) {
    let mut sup = Supervisor::new();
    let mut child: Option<Child> = None;

    let push = |sup: &Supervisor| {
        let _ = ui.send(StateUpdate {
            color: sup.color(),
            label: status_label(sup.phase()),
        });
    };

    // 0. 先探端口：外部已在跑的 daemon（如 start-daemon.bat 拉起的实例）
    //    → 采纳为运行中，绝不重复 spawn（端口冲突会立刻崩溃并污染状态机）
    let adopted = crate::net_probe::probe_tcp_loopback(sidecar.port, PROBE_TIMEOUT_MS);
    if adopted {
        sup.handle(Event::ProbeOk);
        push(&sup);
    }

    // 1. 启动即自动托管 daemon（无外部实例时）；--minimized 只影响是否
    //    自动打开浏览器（app.rs），daemon 托管两种形态一致
    if !adopted {
        sup.handle(Event::LaunchRequested);
        apply_action(&mut child, &sidecar, Action::KillAndSpawn, &mut sup);
        push(&sup);
    }

    loop {
        // ── 1) UI 命令 ──
        match commands.try_recv() {
            Ok(cmd) => match cmd {
                DaemonCommand::Restart => {
                    let (action, _) = sup.handle(Event::RestartRequested);
                    apply_action(&mut child, &sidecar, action, &mut sup);
                    push(&sup);
                }
                DaemonCommand::Stop => {
                    let (action, _) = sup.handle(Event::StopRequested);
                    apply_action(&mut child, &sidecar, action, &mut sup);
                    push(&sup);
                }
                DaemonCommand::Shutdown => {
                    if let Some(mut c) = child.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                    return;
                }
            },
            Err(TryRecvError::Disconnected) => {
                // UI 线程已消亡（异常退出路径）：不留孤儿 daemon
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                return;
            }
            Err(TryRecvError::Empty) => {}
        }

        // ── 2) 子进程意外退出检测 ──
        if let Some(c) = child.as_mut() {
            match c.try_wait() {
                Ok(Some(_status)) => {
                    child = None;
                    eprintln!("ep-tray: daemon 进程退出，尝试自动恢复");
                    let (action, _) = sup.handle(Event::ChildExited);
                    apply_action(&mut child, &sidecar, action, &mut sup);
                    push(&sup);
                }
                Ok(None) => {}
                Err(_) => {}
            }
        }

        // ── 3) 端口探活：有子进程或处于 Starting/Running 才探（Stopped/Failed 静默）──
        let probing =
            child.is_some() || matches!(sup.phase(), Phase::Starting { .. } | Phase::Running);
        if probing {
            if crate::net_probe::probe_tcp_loopback(sidecar.port, PROBE_TIMEOUT_MS) {
                if sup.phase() != Phase::Running {
                    sup.handle(Event::ProbeOk);
                    push(&sup);
                }
            } else if sup.phase() == Phase::Running && child.is_none() {
                // 采纳的外部实例消失 → 走状态机（会尝试自行拉起一次）
                let (action, _) = sup.handle(Event::ChildExited);
                apply_action(&mut child, &sidecar, action, &mut sup);
                push(&sup);
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 执行状态机输出的动作：杀子进程（若有）、（重）拉起并回填 SpawnOk/SpawnFailed。
fn apply_action(
    child: &mut Option<Child>,
    sidecar: &DaemonSidecar,
    action: Action,
    sup: &mut Supervisor,
) {
    match action {
        Action::KillChild | Action::KillAndSpawn => {
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            if action == Action::KillAndSpawn {
                spawn_supervised(child, sidecar, sup);
            }
        }
        Action::Spawn => {
            std::thread::sleep(RESTART_DELAY);
            spawn_supervised(child, sidecar, sup);
        }
        Action::None => {}
    }
}

fn spawn_supervised(child: &mut Option<Child>, sidecar: &DaemonSidecar, sup: &mut Supervisor) {
    match sidecar.spawn_child() {
        Ok(c) => {
            *child = Some(c);
            sup.handle(Event::SpawnOk);
        }
        Err(e) => {
            eprintln!("ep-tray: daemon 拉起失败: {e}");
            sup.handle(Event::SpawnFailed);
        }
    }
}
