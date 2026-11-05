//! Windows 平台实现（`#[cfg(windows)]` 门控，非 Windows 目标整体不编译）。
//!
//! - [`app`]：壳组装（eframe/egui 事件循环 + tray-icon 共存接线；GT-1 起替代 tao）
//! - [`ui`]：主窗口本体（TrayApp：四页导航骨架/托盘事件消费/CJK 字体/关窗缩托盘）
//! - [`daemon`]：ep-daemon sidecar 托管线程（spawn/探活/重启接线）
//! - [`probe_ext`]：daemon HTTP API 轮询线程（GT-2 状态页快照源）
//! - [`mutex`]：单实例锁的真实实现（命名互斥体）
//! - [`registry`]：开机自启的真实实现（HKCU Run 键）

pub mod app;
pub mod daemon;
pub mod mutex;
pub mod probe_ext;
pub mod registry;
pub mod ui;
