//! EntryPoint UI — egui 前端

pub mod app;
pub mod pages;
pub mod theme;
pub mod toast;

pub use app::{App, AppCmd, AppMsg, AppState, ModuleEntry, Page};
