//! ep-tray 可执行入口（Windows 托盘管理器壳）。

#[cfg(windows)]
fn main() {
    // ── GT-3：--sc-action 提权一次性动作进程分流（必须最先判——UAC 后由
    //    主进程 ShellExecuteExW(runas) 拉起自身）──
    // 命中即执行 SCM 写操作后按退出码退出（0=ok / 2=拒绝 / 3=失败，见
    // elevate.rs）——**绝不进 GUI 循环**，不碰托盘/单实例锁/sidecar；
    // 未命中 → 常规托盘 + GUI 启动。
    let args: Vec<String> = std::env::args().skip(1).collect();
    match ep_tray::platform::elevate::parse_sc_action(&args) {
        Ok(Some(op)) => {
            let code = ep_tray::platform::elevate::execute_sc_action_real(&op);
            std::process::exit(code);
        }
        Ok(None) => {} // 常规 GUI 启动
        Err(e) => {
            eprintln!("ep-tray: {e}");
            std::process::exit(3); // EXIT_FAILED（参数族损坏）
        }
    }

    if let Err(e) = ep_tray::platform::app::run() {
        eprintln!("ep-tray: {e}");
        std::process::exit(1);
    }
}

/// 非 Windows 空壳（T-33 编译策略）：ep-tray 仅支持 Windows；
/// 非 Windows 目标编译通过即可，运行期显式退出。
#[cfg(not(windows))]
fn main() {
    eprintln!("ep-tray is Windows-only; this platform build is an empty shell.");
    std::process::exit(1);
}
