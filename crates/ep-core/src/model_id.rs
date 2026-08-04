//! 全限定模型 ID — 冻结契约见 `docs/PACK_UNIFY_PLAN.md` §4.3。
//!
//! - 语法：`<publisher>.<vendor>.<model>`，各段 `^[a-z0-9][a-z0-9-]*$`；
//!   变体是**独立维度** `@<variant>`，不参与 ID 身份。
//! - 大小写策略（冻结）：三段一律小写，解析遇到大写**拒绝并报错**（不做静默转换）；
//!   变体维度允许大小写字母、数字、连字符、点。
//! - 保留发布者 [`RESERVED_PUBLISHER`]（`ep`，仓库内置模块）；现有 manifest 简单 id
//!   自动归一为 `ep.<vendor>.<model>`（向后兼容层，见 [`normalize_legacy`]）。
//! - manifest / meta / pack / 管线节点统一消费本模块类型。
//!
//! serde：[`QualifiedId`] / [`PinnedModelId`] 序列化为规范字符串形式
//! （如 `"ep.systran.faster-whisper@large-v3"`），反序列化走同一解析校验，
//! 供 `.ep_meta.json`（§4.3）与 API 层直接消费。
//!
//! 实现所有者：Wave 1 **A3 (PackSchema)**。

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 保留发布者 `ep`（仓库内置模块，§4.3）。
pub const RESERVED_PUBLISHER: &str = "ep";

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
#[derive(Debug, thiserror::Error)]
pub enum ModelIdError {
    /// 语法非法（段数不符 / 非法字符 / 大写 / 空段等）
    #[error("invalid qualified model id: {0}")]
    Invalid(String),
}

/// 三段共用的段校验：`^[a-z0-9][a-z0-9-]*$`。
///
/// 大写一律拒绝（§4.3 冻结：段强制小写，解析时拒绝大写并报错），
/// 错误消息指出具体段名与违规原因。
fn check_segment(seg: &str, name: &str, whole: &str) -> Result<(), ModelIdError> {
    if seg.is_empty() {
        return Err(ModelIdError::Invalid(format!(
            "'{whole}': {name} segment must not be empty"
        )));
    }
    if seg.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(ModelIdError::Invalid(format!(
            "'{whole}': {name} segment '{seg}' contains uppercase characters; \
             segments must be lowercase (§4.3)"
        )));
    }
    let mut chars = seg.chars();
    // is_empty 已检查，next() 必有值
    let first = chars.next().unwrap_or(' ');
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(ModelIdError::Invalid(format!(
            "'{whole}': {name} segment '{seg}' must start with a lowercase letter or digit"
        )));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(ModelIdError::Invalid(format!(
            "'{whole}': {name} segment '{seg}' contains invalid characters \
             (allowed: a-z, 0-9, hyphen)"
        )));
    }
    Ok(())
}

/// 变体维度校验：非空，字符集 `[A-Za-z0-9.-]`（大小写、数字、连字符、点）。
fn check_variant(variant: &str, whole: &str) -> Result<(), ModelIdError> {
    if variant.is_empty() {
        return Err(ModelIdError::Invalid(format!(
            "'{whole}': variant after '@' must not be empty"
        )));
    }
    if !variant
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return Err(ModelIdError::Invalid(format!(
            "'{whole}': variant '{variant}' contains invalid characters \
             (allowed: A-Z, a-z, 0-9, hyphen, dot)"
        )));
    }
    Ok(())
}

impl QualifiedId {
    /// 解析 `publisher.vendor.model` 形式的全限定 ID。
    ///
    /// 各段 `^[a-z0-9][a-z0-9-]*$`；段数必须恰好为三；大写拒绝并报错。
    pub fn parse(s: &str) -> Result<Self, ModelIdError> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return Err(ModelIdError::Invalid(format!(
                "'{s}' must have exactly three dot-separated segments \
                 `<publisher>.<vendor>.<model>` (found {})",
                parts.len()
            )));
        }
        const NAMES: [&str; 3] = ["publisher", "vendor", "model"];
        let mut segs = Vec::with_capacity(3);
        for (seg, name) in parts.iter().zip(NAMES) {
            check_segment(seg, name, s)?;
            segs.push((*seg).to_string());
        }
        Ok(Self {
            publisher: segs[0].clone(),
            vendor: segs[1].clone(),
            model: segs[2].clone(),
        })
    }

    /// 格式化为规范形式 `publisher.vendor.model`。
    pub fn to_canonical(&self) -> String {
        format!("{}.{}.{}", self.publisher, self.vendor, self.model)
    }
}

impl fmt::Display for QualifiedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_canonical())
    }
}

impl Serialize for QualifiedId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_canonical())
    }
}

impl<'de> Deserialize<'de> for QualifiedId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(D::Error::custom)
    }
}

impl PinnedModelId {
    /// 解析 `publisher.vendor.model[@variant]`（变体可选）。
    ///
    /// 以**第一个** `@` 切分：左侧走 [`QualifiedId::parse`]，
    /// 右侧走变体字符集校验（大小写字母、数字、连字符、点）。
    pub fn parse(s: &str) -> Result<Self, ModelIdError> {
        let (id_part, variant_part) = match s.split_once('@') {
            Some((id, v)) => (id, Some(v)),
            None => (s, None),
        };
        let id = QualifiedId::parse(id_part)?;
        let variant = match variant_part {
            Some(v) => {
                check_variant(v, s)?;
                Some(v.to_string())
            }
            None => None,
        };
        Ok(Self { id, variant })
    }

    /// 格式化为规范形式（含变体时追加 `@variant`）。
    pub fn to_canonical(&self) -> String {
        match &self.variant {
            Some(v) => format!("{}@{}", self.id.to_canonical(), v),
            None => self.id.to_canonical(),
        }
    }
}

impl fmt::Display for PinnedModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_canonical())
    }
}

impl Serialize for PinnedModelId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_canonical())
    }
}

impl<'de> Deserialize<'de> for PinnedModelId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(D::Error::custom)
    }
}

/// 旧 manifest 简单 id 归一：`(vendor, model)` → `ep.<vendor>.<model>`
/// （向后兼容层，§4.3 保留发布者 `ep`）。
///
/// 大小写策略：legacy 字段按 §4.3 归一为小写（此处是兼容层的主动归一，
/// 与 [`QualifiedId::parse`] 的"拒绝大写"不冲突）。输入若含段字符集之外的
/// 字符，构造结果的 [`QualifiedId::parse`] 往返会失败——legacy 数据来自本机
/// manifest，视为数据损坏而非解析错误。
pub fn normalize_legacy(vendor: &str, model: &str) -> QualifiedId {
    QualifiedId {
        publisher: RESERVED_PUBLISHER.to_string(),
        vendor: vendor.to_ascii_lowercase(),
        model: model.to_ascii_lowercase(),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── QualifiedId::parse 合法语法 ────────────────────────────────────

    #[test]
    fn parse_valid_three_segments() {
        let id = QualifiedId::parse("ep.systran.faster-whisper").unwrap();
        assert_eq!(id.publisher, "ep");
        assert_eq!(id.vendor, "systran");
        assert_eq!(id.model, "faster-whisper");
        assert_eq!(id.to_canonical(), "ep.systran.faster-whisper");
    }

    #[test]
    fn parse_canonical_roundtrip() {
        for s in [
            "ep.systran.faster-whisper",
            "alice.acme-corp.model-x2",
            "a.1.x-y2",
            "0.zero.0",
            "pub.vendor.a-", // 尾连字符符合冻结正则 ^[a-z0-9][a-z0-9-]*$
        ] {
            let id = QualifiedId::parse(s).unwrap();
            assert_eq!(id.to_canonical(), s, "roundtrip: {s}");
            assert_eq!(QualifiedId::parse(&id.to_canonical()).unwrap(), id);
        }
    }

    #[test]
    fn reserved_publisher_parses() {
        let id = QualifiedId::parse("ep.builtin.module-a").unwrap();
        assert_eq!(id.publisher, RESERVED_PUBLISHER);
    }

    // ── QualifiedId::parse 非法语法 ────────────────────────────────────

    #[test]
    fn parse_rejects_wrong_segment_count() {
        for s in ["", "onlyone", "two.parts", "a.b.c.d", "a.b.c."] {
            let err = QualifiedId::parse(s).unwrap_err();
            assert!(
                err.to_string().contains("three dot-separated segments"),
                "input {s:?} → {err}"
            );
        }
    }

    #[test]
    fn parse_rejects_empty_segment() {
        let err = QualifiedId::parse("a..b").unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[test]
    fn parse_rejects_uppercase_in_any_segment() {
        // §4.3 冻结：段强制小写，解析时拒绝大写并报错
        for s in [
            "Ep.systran.faster-whisper",
            "ep.Systran.faster-whisper",
            "ep.systran.Faster-Whisper",
        ] {
            let err = QualifiedId::parse(s).unwrap_err();
            assert!(
                err.to_string().contains("lowercase"),
                "input {s:?} → {err}"
            );
        }
    }

    #[test]
    fn parse_rejects_leading_hyphen() {
        let err = QualifiedId::parse("-a.b.c").unwrap_err();
        assert!(
            err.to_string().contains("must start with a lowercase letter or digit"),
            "{err}"
        );
    }

    #[test]
    fn parse_rejects_invalid_chars() {
        for s in ["a_b.c.d", "a b.c.d", "a.b.c!", "a.b@c.d"] {
            let err = QualifiedId::parse(s).unwrap_err();
            assert!(
                err.to_string().contains("invalid characters"),
                "input {s:?} → {err}"
            );
        }
        // 非 ASCII 段同样拒绝（可能命中起始字符检查或字符集检查）
        assert!(QualifiedId::parse("a.b.中文").is_err());
    }

    // ── PinnedModelId：变体独立维度 ────────────────────────────────────

    #[test]
    fn pinned_parse_without_variant() {
        let pinned = PinnedModelId::parse("ep.systran.faster-whisper").unwrap();
        assert_eq!(pinned.id.to_canonical(), "ep.systran.faster-whisper");
        assert!(pinned.variant.is_none());
        assert_eq!(pinned.to_canonical(), "ep.systran.faster-whisper");
    }

    #[test]
    fn pinned_parse_with_variant() {
        let pinned = PinnedModelId::parse("ep.systran.faster-whisper@large-v3").unwrap();
        assert_eq!(pinned.id.publisher, "ep");
        assert_eq!(pinned.variant.as_deref(), Some("large-v3"));
        assert_eq!(pinned.to_canonical(), "ep.systran.faster-whisper@large-v3");
    }

    #[test]
    fn pinned_variant_allows_uppercase_digits_hyphen_dot() {
        // 变体是独立维度：允许大小写字母、数字、连字符、点（§4.3 小写约束只管三段）
        for v in ["large-v3", "V1.5-rc1", "FP16", "2.0", "a"] {
            let pinned = PinnedModelId::parse(&format!("ep.a.b@{v}")).unwrap();
            assert_eq!(pinned.variant.as_deref(), Some(v));
        }
    }

    #[test]
    fn pinned_rejects_empty_variant() {
        let err = PinnedModelId::parse("ep.a.b@").unwrap_err();
        assert!(err.to_string().contains("variant"), "{err}");
    }

    #[test]
    fn pinned_rejects_invalid_variant_chars() {
        for s in ["ep.a.b@v_1", "ep.a.b@a@b", "ep.a.b@x y"] {
            let err = PinnedModelId::parse(s).unwrap_err();
            assert!(
                err.to_string().contains("invalid characters"),
                "input {s:?} → {err}"
            );
        }
    }

    #[test]
    fn pinned_rejects_invalid_id_part() {
        let err = PinnedModelId::parse("Only.Two@v1").unwrap_err();
        assert!(err.to_string().contains("three dot-separated segments"), "{err}");
    }

    // ── normalize_legacy：归一化往返 ────────────────────────────────────

    #[test]
    fn normalize_legacy_roundtrip() {
        let id = normalize_legacy("systran", "faster-whisper");
        assert_eq!(id.publisher, RESERVED_PUBLISHER);
        let canonical = id.to_canonical();
        assert_eq!(canonical, "ep.systran.faster-whisper");
        assert_eq!(QualifiedId::parse(&canonical).unwrap(), id);
    }

    #[test]
    fn normalize_legacy_lowercases_input() {
        let id = normalize_legacy("Systran", "Faster-Whisper");
        assert_eq!(id.to_canonical(), "ep.systran.faster-whisper");
        assert!(QualifiedId::parse(&id.to_canonical()).is_ok());
    }

    // ── Display 与 serde ───────────────────────────────────────────────

    #[test]
    fn display_matches_canonical() {
        let pinned = PinnedModelId::parse("ep.systran.faster-whisper@large-v3").unwrap();
        assert_eq!(pinned.id.to_string(), "ep.systran.faster-whisper");
        assert_eq!(pinned.to_string(), "ep.systran.faster-whisper@large-v3");
    }

    #[test]
    fn serde_qualified_id_roundtrip_json() {
        let id = QualifiedId::parse("ep.systran.faster-whisper").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"ep.systran.faster-whisper\"");
        assert_eq!(serde_json::from_str::<QualifiedId>(&json).unwrap(), id);
    }

    #[test]
    fn serde_pinned_roundtrip_json() {
        let pinned = PinnedModelId::parse("ep.systran.faster-whisper@large-v3").unwrap();
        let json = serde_json::to_string(&pinned).unwrap();
        assert_eq!(json, "\"ep.systran.faster-whisper@large-v3\"");
        assert_eq!(serde_json::from_str::<PinnedModelId>(&json).unwrap(), pinned);

        let no_variant = PinnedModelId::parse("ep.a.b").unwrap();
        let json = serde_json::to_string(&no_variant).unwrap();
        assert_eq!(serde_json::from_str::<PinnedModelId>(&json).unwrap(), no_variant);
    }

    #[test]
    fn serde_deserialize_rejects_invalid() {
        assert!(serde_json::from_str::<QualifiedId>("\"Bad.Id.x\"").is_err());
        assert!(serde_json::from_str::<QualifiedId>("\"a.b\"").is_err());
        assert!(serde_json::from_str::<PinnedModelId>("\"ep.a.b@v_1\"").is_err());
    }
}
