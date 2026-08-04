//! 全限定模型 ID — 冻结契约见 `docs/PACK_UNIFY_PLAN.md` §4.3。
//!
//! - 语法：`<publisher>.<vendor>.<model>`，各段 `^[a-z0-9][a-z0-9-]*$`；
//!   变体是**独立维度** `@<variant>`，不参与 ID 身份。
//! - 保留发布者 `ep`（仓库内置模块）；现有 manifest 简单 id 自动归一为
//!   `ep.<vendor>.<model>`（向后兼容层，见 [`normalize_legacy`]）。
//! - manifest / meta / pack / 管线节点统一消费本模块类型。
//!
//! 当前为 Wave S 骨架（S1 预注册）：类型与函数签名冻结，
//! 解析/校验/归一实现由 Wave 1 **A3 (PackSchema)** 填入（体内暂为 `todo!()`）。

/// 全限定模型 ID（三段式，不含变体维度）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedId {
    /// 发布者（保留值 `ep` = 仓库内置模块）
    pub publisher: String,
    /// 模型厂商（如 `systran`）
    pub vendor: String,
    /// 模型名（如 `faster-whisper`）
    pub model: String,
}

/// 全限定 ID + 变体 pin（`<publisher>.<vendor>.<model>@<variant>`）。
///
/// 用于管线节点变体 pin（§6.2）与整合包构建选择（§4.5）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PinnedModelId {
    pub id: QualifiedId,
    /// 变体维度独立；None = 跟随激活变体（§5.2 单槽位语义）
    pub variant: Option<String>,
}

/// 解析/校验错误。技术层消息为英文；面向用户的文案由 API handler 层
/// 经 i18n 映射（错误消息纪律）。
///
/// 骨架占位：变体细分由 A3 按校验规则补全。
#[derive(Debug, thiserror::Error)]
pub enum ModelIdError {
    /// 语法非法（段数不符 / 非法字符 / 大写 / 空段等）
    #[error("invalid qualified model id: {0}")]
    Invalid(String),
}

impl QualifiedId {
    /// 解析 `publisher.vendor.model` 形式的全限定 ID。
    ///
    /// Wave 1 A3 实现：各段 `^[a-z0-9][a-z0-9-]*$` 校验 + 错误定位。
    pub fn parse(s: &str) -> Result<Self, ModelIdError> {
        let _ = s;
        todo!("Wave 1 A3: parse `publisher.vendor.model`")
    }

    /// 格式化为规范形式 `publisher.vendor.model`。Wave 1 A3 实现。
    pub fn to_canonical(&self) -> String {
        todo!("Wave 1 A3: canonical formatting")
    }
}

impl PinnedModelId {
    /// 解析 `publisher.vendor.model[@variant]`（变体可选）。Wave 1 A3 实现。
    pub fn parse(s: &str) -> Result<Self, ModelIdError> {
        let _ = s;
        todo!("Wave 1 A3: parse qualified id with optional @variant")
    }

    /// 格式化为规范形式（含变体时追加 `@variant`）。Wave 1 A3 实现。
    pub fn to_canonical(&self) -> String {
        todo!("Wave 1 A3: canonical formatting with variant")
    }
}

/// 旧 manifest 简单 id 归一：`(vendor, model)` → `ep.<vendor>.<model>`
/// （向后兼容层，§4.3）。Wave 1 A3 实现。
pub fn normalize_legacy(vendor: &str, model: &str) -> QualifiedId {
    let _ = (vendor, model);
    todo!("Wave 1 A3: legacy id normalization to reserved publisher `ep`")
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! 测试占位：Wave 1 A3 补全 parse / to_canonical / normalize_legacy 的
    //! 正例与非法输入用例（含变体维度独立性与 serde 往返）。
}
