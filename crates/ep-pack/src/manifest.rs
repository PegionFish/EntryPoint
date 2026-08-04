//! 整合包清单 `ep-pack.toml` 类型定义与校验 — 冻结契约见计划 §4.2。
//!
//! 实现所有者：Wave 1 **A3 (PackSchema)**。
//! serde 惯例对齐 module.toml：lowercase 枚举、Option 可选、default 缺省。
//! 当前为 Wave S 骨架占位，勿在此前实现业务逻辑。

/// 整合包清单顶层结构（对应 `ep-pack.toml` 全文）。
///
/// 骨架占位：`[pack]` / `[compute]` / `[[models]]` / `[[pipelines]]` 四段字段
/// 由 A3 按 §4.2 冻结格式补全并 serde 化（含 semver 版本比较与 min_ep_version 校验）。
#[derive(Debug, Clone, Default)]
pub struct PackManifest {
    // A3 实现
}

/// 清单中的单个模型条目（`[[models]]`）。
///
/// 骨架占位：`qualified_id`（§4.3 全限定 ID）/ `variant` / `mode` / `tags`
/// 由 A3 补全。
#[derive(Debug, Clone, Default)]
pub struct PackModelEntry {
    // A3 实现
}

/// 模型权重携带模式（冻结，§4.2）：`reference` = 仅描述符（导入时按模块声明下载），
/// `bundle` = 权重随包（落位 `models/<target_dir>`）。
///
/// 骨架占位：serde lowercase 命名由 A3 接线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelMode {
    Reference,
    Bundle,
}
