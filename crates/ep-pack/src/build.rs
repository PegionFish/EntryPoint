//! 整合包构建（`POST /api/packs/build` 与 CLI `ep-pack build` 共用）—
//! 生成 `ep-pack.toml` + CHECKSUMS 并打包为 `.epzip`（§4.5）。
//!
//! 实现所有者：Wave 1 **A4 (PackIO)**。当前为 Wave S 骨架占位。
//!
//! 跨平台纪律：归档内条目名一律使用 `/` 分隔的相对路径（zip 规范），
//! 本地文件系统遍历时经 `Path` 组装、写归档前转换为正斜杠形式。

/// 打包请求描述（模型圈选 + 管线列表 + bundle/reference 逐模型选择）。
///
/// 骨架占位：字段由 A4 冻结实现（models / pipelines / bundle 列表 / 输出路径）。
#[derive(Debug, Clone, Default)]
pub struct BuildPlan {
    // A4 实现
}
