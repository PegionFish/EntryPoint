//! 托盘 UI 壳（Windows）：tao 事件循环 + tray-icon 菜单 + 托管线程接线。
//!
//! 红线：**壳不承载业务功能**——所有业务能力经 WebUI（默认浏览器打开
//! `http://127.0.0.1:<port>`）。本模块只做托盘三色图标、菜单项与
//! daemon 托管线程（`super::daemon`）之间的状态/命令搬运。

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::autostart;
use crate::icons;
use crate::platform::daemon::{run_supervisor_thread, DaemonCommand, DaemonSidecar, StateUpdate};
use crate::platform::mutex::{NamedMutex, TRAY_MUTEX_NAME};
use crate::platform::registry::HkuRunKey;
use crate::singleton::{acquire_single_instance, SingletonOutcome};
use crate::supervisor::TrayColor;

/// daemon 缺省端口（与 ep-core `config::default_port` 同源约定；`--port` 可覆盖）
pub const DEFAULT_PORT: u16 = 9800;

/// CREATE_NO_WINDOW：隐藏 cmd 子进程的控制台窗口
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 托管线程 → UI 的事件（经 tao EventLoopProxy 唤醒事件循环）
enum UserEvent {
    Menu(MenuEvent),
    State(StateUpdate),
}

/// ep-tray 主入口（Windows）：参数解析 → 单实例 → 托管线程 → 事件循环。
///
/// 参数：
/// - `--minimized`：静默启动（开机自启 Run 键使用；不自动打开 WebUI）
/// - `--port=<n>` / `--port <n>`：daemon 端口（缺省 9800）
/// - `--help` / `-h`：用法
pub fn run() -> Result<(), String> {
    let (minimized, port) = parse_args()?;

    // ── 单实例：已有实例在运行 → 静默退出（退出码 0，供自启场景复用）──
    let mutex = NamedMutex::acquire(TRAY_MUTEX_NAME)?;
    match acquire_single_instance(&mutex) {
        SingletonOutcome::Acquired => {}
        SingletonOutcome::AlreadyRunning => {
            eprintln!("ep-tray: 已有实例在运行，本实例退出");
            return Ok(());
        }
        SingletonOutcome::Error(e) => {
            return Err(format!("单实例检测失败（保守不启动）: {e}"));
        }
    }

    // ── daemon sidecar：与 ep-tray.exe 同目录的 ep-daemon.exe ──
    let exe_dir: PathBuf = std::env::current_exe()
        .map_err(|e| format!("定位 ep-tray.exe 失败: {e}"))?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "ep-tray.exe 无父目录".to_string())?;
    let daemon_path = exe_dir.join("ep-daemon.exe");
    if !daemon_path.is_file() {
        return Err(format!(
            "未找到 daemon sidecar: {}（ep-daemon.exe 须与 ep-tray.exe 同目录）",
            daemon_path.display()
        ));
    }

    // ── 托管线程（spawn/探活/崩溃重启的真实执行者）──
    let (cmd_tx, cmd_rx) = mpsc::channel::<DaemonCommand>();
    let (state_tx, state_rx) = mpsc::channel::<StateUpdate>();
    let supervisor_handle = std::thread::Builder::new()
        .name("ep-tray-supervisor".into())
        .spawn(move || {
            run_supervisor_thread(DaemonSidecar { daemon_path, port }, cmd_rx, state_tx);
        })
        .map_err(|e| format!("托管线程启动失败: {e}"))?;

    // ── 开机自启初始勾选态（读真实注册表；失败按关处理，不阻断启动）──
    let autostart_on = autostart::is_autostart_enabled(&HkuRunKey).unwrap_or(false);

    // ── 事件循环 + 托盘 ──
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    // 菜单事件 → 事件循环
    let menu_proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    // 托管线程状态 → 事件循环（转发线程阻塞 recv，UI 不轮询）
    let state_proxy = event_loop.create_proxy();
    std::thread::Builder::new()
        .name("ep-tray-state-fwd".into())
        .spawn(move || {
            while let Ok(update) = state_rx.recv() {
                if state_proxy.send_event(UserEvent::State(update)).is_err() {
                    break; // 事件循环已退出
                }
            }
        })
        .map_err(|e| format!("状态转发线程启动失败: {e}"))?;

    // 菜单项（句柄留在闭包里更新文字/勾选态）
    let status_item = MenuItem::new("● EntryPoint：启动中…", false, None);
    let open_item = MenuItem::new("打开 WebUI", true, None);
    let restart_item = MenuItem::new("重启 daemon", true, None);
    let stop_item = MenuItem::new("停止 daemon", true, None);
    let autostart_item = CheckMenuItem::new("开机自启", true, autostart_on, None);
    let quit_item = MenuItem::new("退出（并停止 daemon）", true, None);

    let menu = Menu::new();
    menu.append_items(&[
            &status_item,
            &PredefinedMenuItem::separator(),
            &open_item,
            &restart_item,
            &stop_item,
            &autostart_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ])
        .map_err(|e| format!("构建托盘菜单失败: {e}"))?;

    // 手动启动（无 --minimized）：daemon 首次就绪后自动打开 WebUI 一次
    let mut pending_open = !minimized;
    let mut tray: Option<TrayIcon> = None;
    let mut current_color: Option<TrayColor> = None;

    // run_return（而非 run）：tao 0.37 的 run 签名为 `-> !`（内部直接结束
    // 进程，不跑析构）——用 run_return 拿回控制权，保证退出路径上能 join
    // 托管线程、停掉 daemon 子进程后再收尸。
    let mut event_loop = event_loop;
    let exit_code = event_loop.run_return(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            // 托盘必须在事件循环实际运行后创建（tray-icon#90）
            Event::NewEvents(StartCause::Init) => match build_tray(&menu) {
                Ok(t) => tray = Some(t),
                Err(e) => {
                    eprintln!("ep-tray: {e}");
                    let _ = cmd_tx.send(DaemonCommand::Shutdown);
                    *control_flow = ControlFlow::Exit;
                }
            },

            // 托管线程状态 → 图标三色 + 状态行文字
            Event::UserEvent(UserEvent::State(update)) => {
                if current_color != Some(update.color) {
                    match icon_for(update.color) {
                        Ok(icon) => {
                            if let Some(t) = tray.as_mut() {
                                if t.set_icon(Some(icon)).is_ok() {
                                    current_color = Some(update.color);
                                }
                            }
                        }
                        Err(e) => eprintln!("ep-tray: {e}"),
                    }
                }
                status_item.set_text(update.label);

                if pending_open && update.color == TrayColor::Green {
                    open_webui(port);
                    pending_open = false;
                }
            }

            // 菜单点击
            Event::UserEvent(UserEvent::Menu(ev)) => {
                if ev.id == open_item.id() {
                    open_webui(port);
                } else if ev.id == restart_item.id() {
                    let _ = cmd_tx.send(DaemonCommand::Restart);
                } else if ev.id == stop_item.id() {
                    let _ = cmd_tx.send(DaemonCommand::Stop);
                } else if ev.id == autostart_item.id() {
                    let enabled = autostart_item.is_checked();
                    match std::env::current_exe()
                        .map_err(|e| format!("定位 exe: {e}"))
                        .and_then(|exe| autostart::set_autostart(&HkuRunKey, &exe, enabled))
                    {
                        Ok(()) => {}
                        Err(e) => {
                            eprintln!("ep-tray: 自启开关写入失败: {e}");
                            autostart_item.set_checked(!enabled); // 回滚 UI
                        }
                    }
                } else if ev.id == quit_item.id() {
                    let _ = cmd_tx.send(DaemonCommand::Shutdown);
                    *control_flow = ControlFlow::Exit;
                }
            }

            _ => {}
        }
    });

    // 事件循环结束（无论正常/异常）：确保托管线程停掉 daemon 再收尸
    let _ = supervisor_handle.join();
    if exit_code == 0 {
        Ok(())
    } else {
        Err(format!("事件循环异常退出（代码 {exit_code}）"))
    }
}

/// 参数解析（纯解析，无副作用；错误信息面向终端用户）。
fn parse_args() -> Result<(bool, u16), String> {
    let mut minimized = false;
    let mut port = DEFAULT_PORT;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--minimized" => minimized = true,
            "--help" | "-h" => {
                println!(
                    "ep-tray — EntryPoint 托盘管理器\n\
                     用法: ep-tray [--minimized] [--port=<n>]\n\
                     \x20 --minimized   静默启动（开机自启使用，不自动打开 WebUI）\n\
                     \x20 --port=<n>    daemon 端口（缺省 {DEFAULT_PORT}）"
                );
                std::process::exit(0);
            }
            other => {
                if let Some(v) = other.strip_prefix("--port=") {
                    port = v.parse().map_err(|_| format!("--port 值无效: {v}"))?;
                } else if other == "--port" {
                    let v = args.next().ok_or("--port 缺少值")?;
                    port = v.parse().map_err(|_| format!("--port 值无效: {v}"))?;
                } else {
                    return Err(format!(
                        "未知参数: {other}（支持 --minimized / --port=<n> / --help）"
                    ));
                }
            }
        }
    }
    Ok((minimized, port))
}

/// 构建托盘（黄色初始图标 + 菜单）。
fn build_tray(menu: &Menu) -> Result<TrayIcon, String> {
    let icon = icon_for(TrayColor::Yellow)?;
    TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_tooltip("EntryPoint")
        .with_icon(icon)
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| format!("托盘创建失败: {e}"))
}

fn icon_for(color: TrayColor) -> Result<Icon, String> {
    let (rgba, w, h) = icons::status_icon_rgba(color);
    Icon::from_rgba(rgba, w, h).map_err(|e| format!("图标生成失败: {e}"))
}

/// 用系统默认浏览器打开 WebUI（cmd start，空标题占位避免 URL 被当窗口标题）。
fn open_webui(port: u16) {
    let url = format!("http://127.0.0.1:{port}");
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}
