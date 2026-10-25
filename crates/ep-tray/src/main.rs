//! ep-tray 可执行入口（Windows 托盘管理器壳）。

#[cfg(windows)]
fn main() {
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
