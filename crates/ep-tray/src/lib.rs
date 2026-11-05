//! ep-tray — EntryPoint Windows 管理台（ep-daemon sidecar 壳：托盘 + GUI 主窗口）。
//!
//! 定位（GT-1，2026-11-05 起）：**壳不承载业务功能**。业务全部在 ep-daemon
//! （HTTP 服务）与 WebUI；本 crate 负责托盘 UI、主窗口骨架（eframe/egui，
//! 页面内容由 GT-2+ 逐票填充）、daemon 子进程托管（拉起/探活/崩溃重启
//! 一次/再失败红标）、单实例锁与开机自启开关。
//!
//! 模块布局：
//! - 平台无关纯逻辑（任意目标编译，单测零 IO 或 IO 注入）：
//!   - [`supervisor`]：daemon 托管状态机（重启一次/再失败红标）
//!   - [`singleton`]：单实例锁抽象（真实实现 = Windows 命名互斥体，可注入）
//!   - [`autostart`]：开机自启开关逻辑（真实实现 = HKCU Run 键，可注入）
//!   - [`icons`]：状态图标 RGBA 生成（绿/黄/红，程序化绘制无资源文件）
//!   - [`net_probe`]：TCP connect 探活
//! - [`platform`]（`#[cfg(windows)]`）：eframe/egui 主循环 + tray-icon 菜单 +
//!   daemon sidecar 线程 + 注册表/互斥体真实实现。
//!
//! 非 Windows 目标：本 crate 编译为空壳（仅纯逻辑模块），`main` 打印提示后
//! 退出——保证 `cargo check --workspace` 在 Linux/CI 无需任何系统依赖通过。

pub mod autostart;
pub mod icons;
pub mod net_probe;
pub mod singleton;
pub mod supervisor;

#[cfg(windows)]
pub mod platform;
