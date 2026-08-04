//! `.epzip` 解包与路径安全 — 冻结契约见计划 §4.4。
//!
//! 实现所有者：Wave 1 **A4 (PackIO)**。当前为 Wave S 骨架占位。
//!
//! 安全要求（实现时必须满足）：
//! - 路径清洗：归档条目相对路径逐组件校验，拒绝 `..` / 绝对路径（防 zip-slip）；
//! - symlink 逃逸防护：解包目标不得经符号链接逃出暂存目录；
//! - 模式参照 ep-daemon `src/api/upload.rs` 的既有防护（共享同一安全基线）；
//! - 落盘前一律 `Path::join` 组装并校验结果仍在目标根内，双平台（Windows/Linux）同语义。

/// 解包结果摘要。
///
/// 骨架占位：暂存目录、条目计数等字段由 A4 补全。
#[derive(Debug, Clone, Default)]
pub struct ExtractSummary {
    // A4 实现
}
