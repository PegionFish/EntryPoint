//! Windows 平台实现（`#[cfg(windows)]` 门控，非 Windows 目标整体不编译）。
//!
//! - [`app`]：壳组装（eframe/egui 事件循环 + tray-icon 共存接线；GT-1 起替代 tao）
//! - [`ui`]：主窗口本体（TrayApp：四页导航骨架/托盘事件消费/CJK 字体/关窗缩托盘）
//! - [`logs`]：日志页（GT-5：显示缓冲 + 过滤 + 虚拟滚动，消费 [`logpipe`] 句柄）
//! - [`daemon`]：ep-daemon sidecar 托管线程（spawn/探活/重启接线）
//! - [`probe_ext`]：daemon HTTP API 轮询线程（GT-2 状态页快照源）
//! - [`logpipe`]：日志引擎（GT-4，三源汇流 + 环形缓冲；GT-5 日志页接线）
//! - [`scm`]：Windows 服务控制管理器封装（GT-3：模式检测 + 双模式管控读/写面）
//! - [`elevate`]：SCM 写操作自提权面（GT-3：ShellExecuteExW runas + --sc-action
//!   一次性动作进程；主进程绝不提权）
//! - [`mutex`]：单实例锁的真实实现（命名互斥体）
//! - [`registry`]：开机自启的真实实现（HKCU Run 键）

pub mod app;
pub mod daemon;
pub mod elevate;
pub mod logpipe;
pub mod logs;
pub mod mutex;
pub mod probe_ext;
pub mod registry;
pub mod scm;
pub mod ui;
