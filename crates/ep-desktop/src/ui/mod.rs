//! 桌面端 UI 基础 — 设计令牌与共享组件。
//!
//! 所有页面必须从 [`Palette`] 获取语义色，禁止散落硬编码 RGB，
//! 保证深色/浅色主题下视觉一致（色板对齐 docs/DESIGN_SYSTEM.md）。

pub mod components;
pub mod palette;

pub use components::*;
pub use palette::{service_status, Palette, StatusMeta};
