//! 整合包导入编排 — 冻结流程见计划 §4.4（暂存 → 解包 → checksum → 清单校验 →
//! 模型 bundle/reference 落位 → 管线冲突处理 → 注册表 → WS pack_import 进度）。
//!
//! 实现所有者：Wave 2 **B1 (PackImport)**。当前为 Wave S 骨架占位。
//!
//! daemon 侧路由（`ep-daemon/src/api/packs.rs`）与注册表、WS 事件由 B2 接线；
//! 本模块只提供 daemon / CLI 共用的编排核心。

/// 导入来源（对应 §8.1 三个入口：local / url / upload）。
///
/// 骨架占位：serde 反序列化形状（`{source:"local",path}` / `{source:"url",url}`）
/// 由 B1 对齐 API 请求体契约实现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportSource {
    /// 本地 `.epzip` 路径
    Local(std::path::PathBuf),
    /// 远程 URL（下载后进暂存目录，大小上限复用上传约束）
    Url(String),
    /// 浏览器上传后暂存于 workspace/uploads 的路径
    Upload(std::path::PathBuf),
}
