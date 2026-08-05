//! 页面模块 — 各页面渲染入口 + 跨页共享的页面层基础设施。
//!
//! ## 页面层数据桥（Wave 3 C5）
//!
//! 桌面端页面由 `app.rs` 按骨架签名分发。部分跨页数据（设备列表/任务列表）
//! 在页面签名下不直接可达，本模块提供 **ctx-data 快照桥**：
//!
//! - dashboard/tasks 页每帧渲染时发布权威快照（它们从 app.rs 收到
//!   `&[ComputeDevice]` / `&[TaskSummary]`）；
//! - pipeline_editor 页消费快照。仪表盘为默认首页，应用启动后快照即存在；
//!   快照过期/缺失时消费侧按"未知"降级渲染。
//!
//! 信息架构终稿（协调记录 #47）后「模型就是模块」：模块卡直接消费 app.rs
//! 传入的 `&[ModuleEntry]`（运行状态/日志权威数据），不再经模块快照桥。
//!
//! 快照只做展示辅助，写入全部经 [`crate::app::AppCmd`] 走后台线程，
//! 页面不直接改动后台状态。

pub mod dashboard;
pub mod modules;
pub mod pipeline_editor;
pub mod settings;
pub mod tasks;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::model::{ModelManager, ModelMeta};
use ep_core::module::{CapabilityDecl, DiscoveredModule, ModuleManifest};
use ep_core::pipeline::runner::TaskSummary;
use ep_core::types::ComputeDevice;

use crate::i18n::tr;

/// 模块清单缓存的 TTL：页面层本地读取 modules/ 目录的复用窗口。
/// 清单为少量小 TOML 文件，TTL 内的复用避免每帧 IO；过期后下一帧重建。
const MODULE_DATA_TTL: Duration = Duration::from_secs(30);

// ─── i18n 兜底 ──────────────────────────────────────────────────────────────

/// 带兜底文案的翻译查找：键尚未落盘（C8 之前）时返回兜底文案而非键本身。
///
/// 与 `app.rs` NAV_ITEMS 的"键缺失回退兜底"过渡期策略一致；C8 落盘后
/// 兜底自动失效（tr 命中即返回译文）。兜底文案同样支持 `{{name}}` 插值
/// （ep-core 的缺失键路径返回键本身、不做插值，故兜底分支自行插值）。
pub(crate) fn trfb(lang: &str, key: &str, fallback: &str, params: &[(&str, &str)]) -> String {
    let translated = tr(lang, key, params);
    if translated != key {
        return translated;
    }
    let mut out = fallback.to_string();
    for (name, value) in params {
        let pattern = format!("{{{{{name}}}}}");
        out = out.replace(&pattern, value);
    }
    out
}

// ─── 设备快照（dashboard 发布 → pipeline_editor VRAM 账本消费） ────────────

/// 计算设备快照（来自后台周期性检测的权威列表）
#[derive(Debug, Clone, Default)]
pub(crate) struct DeviceSnapshot {
    pub devices: Vec<ComputeDevice>,
}

fn device_snapshot_id() -> egui::Id {
    egui::Id::new("ep_pages_device_snapshot")
}

/// 发布设备快照（dashboard 页每帧调用）
pub(crate) fn publish_device_snapshot(ctx: &egui::Context, devices: &[ComputeDevice]) {
    let snapshot = DeviceSnapshot {
        devices: devices.to_vec(),
    };
    ctx.data_mut(|d| d.insert_temp(device_snapshot_id(), snapshot));
}

/// 读取设备快照；None = 尚未检测/发布（VRAM 账本按"容量未知"降级）
pub(crate) fn device_snapshot(ctx: &egui::Context) -> Option<DeviceSnapshot> {
    ctx.data(|d| d.get_temp::<DeviceSnapshot>(device_snapshot_id()))
}

// ─── 任务快照（tasks 发布 → pipeline_editor 节点状态回显消费） ─────────────

/// 任务列表快照（来自后台任务拉取的权威列表）
#[derive(Debug, Clone, Default)]
pub(crate) struct TasksSnapshot {
    pub tasks: Vec<TaskSummary>,
}

fn tasks_snapshot_id() -> egui::Id {
    egui::Id::new("ep_pages_tasks_snapshot")
}

/// 发布任务快照（tasks 页每帧调用）
pub(crate) fn publish_tasks_snapshot(ctx: &egui::Context, tasks: &[TaskSummary]) {
    let snapshot = TasksSnapshot {
        tasks: tasks.to_vec(),
    };
    ctx.data_mut(|d| d.insert_temp(tasks_snapshot_id(), snapshot));
}

/// 读取任务快照；None = 尚未发布（节点状态回显不渲染）
pub(crate) fn tasks_snapshot(ctx: &egui::Context) -> Option<TasksSnapshot> {
    ctx.data(|d| d.get_temp::<TasksSnapshot>(tasks_snapshot_id()))
}

// ─── 模块清单缓存（页面层本地，TTL 复用） ───────────────────────────────────

/// 模块发现结果的页面层缓存：统一页卡片（能力/变体/VRAM）与管线编辑器
/// （节点 palette / 参数 schema / 端口类型）共用。
///
/// 数据来源与后台 `background_loop` 相同（`discover_modules(root/modules)`），
/// 但独立缓存于 UI 侧：S2 骨架的 `AppMsg::ModulesDiscovered` 只保留
/// [`ModuleEntry`] 的概要字段（不含 manifest），页面层富数据（capabilities /
/// models / vram）需自行读取清单。TTL 复用保证非每帧 IO。
/// 清单数据经 app.rs 增补传递后可移除本缓存（见 C5 仲裁请求）。
pub(crate) struct ModuleData {
    pub discovered: Vec<DiscoveredModule>,
    pub loaded_at: Instant,
}

impl ModuleData {
    /// 按 module_id 查找有效清单
    pub fn manifest(&self, module_id: &str) -> Option<&ModuleManifest> {
        self.discovered
            .iter()
            .filter_map(|dm| dm.manifest.as_ref())
            .find(|mf| mf.module.id == module_id)
    }

    /// 查找模块的 capability 声明
    pub fn capability(&self, module_id: &str, capability: &str) -> Option<&CapabilityDecl> {
        self.manifest(module_id)?
            .interface
            .capabilities
            .iter()
            .find(|c| c.name == capability)
    }

    /// 全部有效清单（保持发现顺序）
    pub fn manifests(&self) -> impl Iterator<Item = &ModuleManifest> {
        self.discovered.iter().filter_map(|dm| dm.manifest.as_ref())
    }
}

fn module_data_id() -> egui::Id {
    egui::Id::new("ep_pages_module_data")
}

/// 获取模块清单缓存；`force` = 忽略 TTL 强制重读（"刷新"按钮用）。
pub(crate) fn module_data(ctx: &egui::Context, force: bool) -> Arc<ModuleData> {
    if !force {
        if let Some(cached) = ctx.data(|d| d.get_temp::<Arc<ModuleData>>(module_data_id())) {
            if cached.loaded_at.elapsed() < MODULE_DATA_TTL {
                return cached;
            }
        }
    }
    let root = ep_core::config::resolve_root();
    let discovered = ep_core::module::discover_modules(&root.join("modules"));
    let fresh = Arc::new(ModuleData {
        discovered,
        loaded_at: Instant::now(),
    });
    ctx.data_mut(|d| d.insert_temp(module_data_id(), fresh.clone()));
    fresh
}

// ─── 模型 meta（tags 等）读写 ────────────────────────────────────────────────

/// 构建与后台同口径的 ModelManager（cache_dir 相对路径按 root 解析）
fn model_manager(config: &AppConfig) -> ModelManager {
    let root = ep_core::config::resolve_root();
    ModelManager::new(&config.models, &root)
}

/// 读取模型 meta（不存在/损坏 → None）
pub(crate) fn read_model_meta(config: &AppConfig, target_dir: &str) -> Option<ModelMeta> {
    model_manager(config).read_meta(target_dir)
}

/// 覆写模型 tags（read-modify-write；meta 不存在返回错误，调用方降级提示）
pub(crate) fn write_model_tags(
    config: &AppConfig,
    target_dir: &str,
    tags: Vec<String>,
) -> anyhow::Result<()> {
    let mgr = model_manager(config);
    let mut meta = mgr
        .read_meta(target_dir)
        .ok_or_else(|| anyhow::anyhow!("model meta not found for '{target_dir}'"))?;
    meta.tags = tags;
    mgr.write_meta(target_dir, &meta)
}

// ─── 参数草稿（manifest schema 驱动表单，models 直跑 + 管线编辑器共用） ────

/// 参数编辑草稿：schema 类型化的中间态，提交时按目标序列化
/// （直跑 → 字符串参数对；管线节点 → params JSON 值）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ParamDraft {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl ParamDraft {
    /// 直跑提交的字符串形式（C4 按 schema 强类型化）
    pub fn to_arg(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Int(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Bool(b) => b.to_string(),
        }
    }
}

/// schema 默认值 → 草稿（无默认值按类型给中性初值；enum 取首项）
pub(crate) fn draft_default(schema: &ep_core::module::ParamSchema) -> ParamDraft {
    let t = schema.param_type.to_ascii_lowercase();
    match schema.default.as_ref() {
        Some(v) => {
            if t == "boolean" || t == "bool" {
                ParamDraft::Bool(v.as_bool().unwrap_or(false))
            } else if t == "integer" || t == "int" {
                ParamDraft::Int(v.as_i64().unwrap_or(0))
            } else if t == "number" || t == "float" || t == "double" {
                ParamDraft::Float(v.as_f64().unwrap_or(0.0))
            } else {
                ParamDraft::Str(
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string()),
                )
            }
        }
        None => {
            if t == "boolean" || t == "bool" {
                ParamDraft::Bool(false)
            } else if t == "integer" || t == "int" {
                ParamDraft::Int(0)
            } else if t == "number" || t == "float" || t == "double" {
                ParamDraft::Float(0.0)
            } else if let Some(first) = schema
                .enum_values
                .as_ref()
                .and_then(|e| e.first())
                .or_else(|| schema.options.as_ref().and_then(|o| o.first()))
            {
                ParamDraft::Str(first.clone())
            } else {
                ParamDraft::Str(String::new())
            }
        }
    }
}

// ─── 通用格式化 ──────────────────────────────────────────────────────────────

/// 格式化文件大小：B / KB / MB / GB（1 位小数）。models/tasks 页共用。
pub(crate) fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{b:.0} B")
    }
}

// ─── 平台文件打开 ─────────────────────────────────────────────────────────────

/// 跨平台打开文件/目录（系统默认程序）。
///
/// 平台分支（§15.3 同款纪律）：
/// - Windows：`cmd /C start "" <path>`（start 同时支持文件与目录）
/// - Linux：`xdg-open <path>`
/// - macOS：`open <path>`（桌面端目标平台为 Windows/Linux，此分支仅防御）
///
/// 返回 spawn 是否成功（进程退出码不追踪——系统打开器自身负责报错）。
pub(crate) fn open_path(path: &std::path::Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.to_string_lossy()])
            .spawn()
            .is_ok()
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .is_ok()
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn().is_ok()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        false
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trfb_falls_back_when_key_missing() {
        // 已落盘键 → 译文
        assert_eq!(trfb("zh-CN", "common.action.save", "兜底", &[]), "保存");
        assert_eq!(trfb("en", "common.action.save", "兜底", &[]), "Save");
        // 未落盘键 → 兜底文案（而非键本身）
        assert_eq!(
            trfb("zh-CN", "desktopPages.models.notYetLanded", "兜底文案", &[]),
            "兜底文案"
        );
        // 插值在兜底路径同样生效
        assert_eq!(
            trfb(
                "zh-CN",
                "desktopPages.models.notYetLanded2",
                "共 {{count}} 个",
                &[("count", "3")]
            ),
            "共 3 个"
        );
    }

    // 注：open_path 不做单测——Windows `cmd /C start` 对不存在的目标会弹
    // 系统对话框阻塞测试进程；其行为（spawn 成功与否）由集成使用验证。

    #[test]
    fn format_size_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
