//! Windows 平台实现（`#[cfg(windows)]` 门控，非 Windows 目标整体不编译）。
//!
//! - [`app`]：托盘 UI 壳（tao 事件循环 + tray-icon 菜单）
//! - [`daemon`]：ep-daemon sidecar 托管线程（spawn/探活/重启接线）
//! - [`mutex`]：单实例锁的真实实现（命名互斥体）
//! - [`registry`]：开机自启的真实实现（HKCU Run 键）

pub mod app;
pub mod daemon;
pub mod mutex;
pub mod registry;
