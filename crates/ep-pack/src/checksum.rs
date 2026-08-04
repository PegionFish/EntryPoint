//! `CHECKSUMS.toml` 校验和表 — 包内所有文件 sha256，导入先验后落盘（§4.2/§4.4）。
//!
//! 实现所有者：Wave 1 **A4 (PackIO)**。当前为 Wave S 骨架占位。

/// 包内校验和表（文件相对路径 → sha256 hex）。
///
/// 骨架占位：条目表 + TOML 序列化/反序列化 + 全量校验由 A4 实现
/// （sha2 + hex 依赖已在 Cargo.toml 就位）。
#[derive(Debug, Clone, Default)]
pub struct ChecksumTable {
    // A4 实现
}
