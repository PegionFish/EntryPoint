//! 整合包清单 `ep-pack.toml` 类型定义与校验 — 冻结契约见计划 §4.2。
//!
//! 实现所有者：Wave 1 **A3 (PackSchema)**。
//! serde 惯例对齐 module.toml：lowercase 枚举、Option 可选、default 缺省。
//!
//! 字段与 §4.2 TOML 示例逐一对应：
//!
//! ```toml
//! [pack]
//! id = "pigeonfish.subtitle-kit"     # <publisher>.<pack-name>，全局唯一键
//! version = "1.0.0"                  # semver（正式版本比较，见 [`semver`]）
//! name = "字幕制作整合包"
//! description = "视频转字幕 + 降噪一体化"
//! authors = ["pigeonfish"]
//! license = "MIT"
//! homepage = "https://github.com/pigeonfish/subtitle-kit"
//! min_ep_version = "0.1.0"
//! tags = ["字幕", "视频"]
//!
//! [compute]
//! backends = ["cuda", "cpu"]
//! notes = { rocm = "需 torch-rocm wheel" }
//!
//! [[models]]
//! qualified_id = "ep.systran.faster-whisper"   # 全限定 ID（§4.3）
//! variant = "large-v3"
//! mode = "reference"                 # reference | bundle
//! tags = ["字幕"]
//!
//! [[pipelines]]
//! file = "pipelines/video_to_srt.toml"         # 包内相对路径
//! ```

use std::collections::HashMap;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use ep_core::model_id::{ModelIdError, PinnedModelId, QualifiedId};
use ep_core::types::ComputeBackend;

/// 清单加载/校验错误（形态对齐 `ep_core::module::manifest::ModuleError`）。
#[derive(Debug, Error)]
pub enum PackManifestError {
    #[error("failed to read pack manifest file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse pack manifest TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("pack manifest validation failed:\n{}", .0.join("\n"))]
    Validation(Vec<String>),
}

/// 整合包清单顶层结构（对应 `ep-pack.toml` 全文）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackManifest {
    pub pack: PackInfo,
    pub compute: PackCompute,
    /// 模型条目（可空：纯管线包合法）
    #[serde(default)]
    pub models: Vec<PackModelEntry>,
    /// 管线引用（可空：纯模型包合法）
    #[serde(default)]
    pub pipelines: Vec<PackPipelineRef>,
}

/// `[pack]` 段 — 包身份与元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackInfo {
    /// `<publisher>.<pack-name>`，全局唯一键（各段小写字母数字连字符）
    pub id: String,
    /// semver（见 [`semver`]）
    pub version: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    /// 最低兼容 EntryPoint 版本（semver）；缺省 = 不设下限。
    /// 实际"当前版本 ≥ 下限"门禁由导入编排（B1）结合运行版本比对。
    pub min_ep_version: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `[compute]` 段 — 包声明可利用的后端（导入时与本机设备比对，§4.6）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackCompute {
    pub backends: Vec<ComputeBackend>,
    /// 每后端运行备注（自由文本，展示用）；键为后端名（lowercase 枚举序列化）
    #[serde(default)]
    pub notes: HashMap<ComputeBackend, String>,
}

/// 模型权重携带模式（冻结，§4.2）：`reference` = 仅描述符（导入时按模块声明下载），
/// `bundle` = 权重随包（落位 `models/<target_dir>`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelMode {
    Reference,
    Bundle,
}

impl ModelMode {
    /// 模式的字符串形式（用于错误信息、报告与 API 输出）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Bundle => "bundle",
        }
    }
}

impl std::fmt::Display for ModelMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[[models]]` 段 — 清单中的单个模型条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackModelEntry {
    /// 全限定 ID `publisher.vendor.model`（§4.3；validate() 校验可解析性）
    pub qualified_id: String,
    /// 变体维度（与 qualified_id 组合成 pin：`<qualified_id>@<variant>`）
    pub variant: String,
    pub mode: ModelMode,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl PackModelEntry {
    /// 解析全限定 ID（validate() 通过后调用不会失败）。
    pub fn parsed_qualified_id(&self) -> Result<QualifiedId, ModelIdError> {
        QualifiedId::parse(&self.qualified_id)
    }

    /// pin 形式 `<qualified_id>@<variant>`（§6.2 节点 schema 同款表示）。
    pub fn pinned_id(&self) -> Result<PinnedModelId, ModelIdError> {
        PinnedModelId::parse(&format!("{}@{}", self.qualified_id, self.variant))
    }
}

/// `[[pipelines]]` 段 — 包内管线文件引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackPipelineRef {
    /// 包内相对路径（`/` 分隔，禁止 `..`，见 validate()）
    pub file: String,
}

// ─── 校验 ───────────────────────────────────────────────────────────────────

/// pack.id 段语法：`^[a-z0-9][a-z0-9-]*$`（与 §4.3 模型 ID 段同规则）
fn is_pack_id_segment(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `[[pipelines]].file` 路径校验：包内相对路径、`/` 分隔、无 `..`。
///
/// 双平台同语义：显式拒绝反斜杠（Windows 分隔符不入归档路径，
/// 归档条目名一律 `/`，与 A4 打包纪律一致）；绝对/带根路径拒绝。
fn check_pipeline_file(file: &str) -> Result<(), String> {
    if file.is_empty() {
        return Err("must not be empty".to_string());
    }
    if file.contains('\\') {
        return Err(format!(
            "'{file}' uses backslash separators; pack-internal paths must use '/'"
        ));
    }
    // Windows 盘符前缀（如 `C:/...`）：跨平台显式拒绝。Linux/macOS 上
    // Path 不识别盘符，`C:` 会被当作普通相对目录名而漏放；此处统一拦截，
    // 与 Windows 上 is_absolute() 的判定保持同语义。
    let bytes = file.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(format!(
            "'{file}' uses a Windows drive prefix; pack-internal paths must be relative"
        ));
    }
    let path = Path::new(file);
    if path.is_absolute() || path.has_root() {
        return Err(format!("'{file}' must be a relative path inside the pack"));
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(format!("'{file}' must not contain '..' components"));
    }
    Ok(())
}

impl PackManifest {
    /// 从文件加载清单（形态对齐 `ModuleManifest::from_file`）。
    pub fn from_file(path: &Path) -> Result<Self, PackManifestError> {
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&content)?;
        Ok(manifest)
    }

    /// schema 校验（契约 §4.2 + 任务冻结清单）：
    ///
    /// 1. `pack.id` 语法 — `<publisher>.<pack-name>`，各段小写字母数字连字符；
    /// 2. semver 合法性 — `pack.version` 必填合法；`min_ep_version` 若存在须合法；
    /// 3. `models` 非空时 `qualified_id` 可解析（§4.3），变体符合变体字符集；
    /// 4. `pipelines.file` 为包内相对路径且无 `..`（`/` 分隔）。
    ///
    /// 返回**全部**错误（对齐 `ModuleManifest::validate` 的 Vec<String> 惯例）。
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // 1. pack.id = <publisher>.<pack-name>
        let segments: Vec<&str> = self.pack.id.split('.').collect();
        if segments.len() != 2 || !segments.iter().all(|s| is_pack_id_segment(s)) {
            errors.push(format!(
                "pack.id '{}' is invalid: expected `<publisher>.<pack-name>` with each \
                 segment matching ^[a-z0-9][a-z0-9-]*$",
                self.pack.id
            ));
        }

        // 2. semver 合法性
        if let Err(e) = semver::Version::parse(&self.pack.version) {
            errors.push(format!("pack.version is not a valid semver: {e}"));
        }
        if let Some(min) = &self.pack.min_ep_version {
            if let Err(e) = semver::Version::parse(min) {
                errors.push(format!("pack.min_ep_version is not a valid semver: {e}"));
            }
        }

        // 3. models：qualified_id 可解析 + 变体语法
        for (i, model) in self.models.iter().enumerate() {
            if let Err(e) = QualifiedId::parse(&model.qualified_id) {
                errors.push(format!("models[{i}].qualified_id: {e}"));
                continue; // qualified_id 非法时不再叠加 pin 解析错误
            }
            if model.variant.is_empty() {
                errors.push(format!("models[{i}].variant must not be empty"));
            } else if let Err(e) = model.pinned_id() {
                errors.push(format!("models[{i}].variant: {e}"));
            }
        }

        // 4. pipelines.file：包内相对路径、无 ..
        for (i, pipeline) in self.pipelines.iter().enumerate() {
            if let Err(e) = check_pipeline_file(&pipeline.file) {
                errors.push(format!("pipelines[{i}].file: {e}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

// ─── 最小 semver（§4.2 冻结：正式版本比较，不引入新 crate）───────────────────

/// 手写最小 semver 实现 — `major.minor.patch[-预发布][+构建元数据]`。
///
/// 优先级比较按 semver 规范 §11 的简化语义：
/// - 主/次/修订号按数值比较；
/// - 同版本号下，**有**预发布 < **无**预发布；
/// - 预发布标识逐段比较：纯数字按数值、含字母按 ASCII 字典序、数字 < 非数字；
///   前缀相同则标识多者大；
/// - 构建元数据（`+...`）接受但**不参与**比较。
pub mod semver {
    use std::cmp::Ordering;

    /// 解析后的 semver 版本。构建元数据在解析时丢弃（不参与优先级）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Version {
        pub major: u64,
        pub minor: u64,
        pub patch: u64,
        /// 预发布标识（`-` 后按 `.` 切分）；空 = 正式版
        pre: Vec<String>,
    }

    impl Version {
        /// 解析 `major.minor.patch[-pre][+build]`。
        ///
        /// 拒绝：段数不为三、空段、数字段前导零、空预发布段、非法字符。
        pub fn parse(s: &str) -> Result<Self, String> {
            // 构建元数据：接受但忽略（semver §10 不参与优先级）
            let (core, build) = match s.split_once('+') {
                Some((c, b)) => (c, Some(b)),
                None => (s, None),
            };
            if build.is_some_and(str::is_empty) {
                return Err(format!("'{s}': empty build metadata after '+'"));
            }

            // 预发布段
            let (numbers, pre) = match core.split_once('-') {
                None => (core, Vec::new()),
                Some((n, p)) => {
                    if p.is_empty() {
                        return Err(format!("'{s}': empty pre-release section after '-'"));
                    }
                    let ids: Vec<String> = p.split('.').map(str::to_string).collect();
                    for id in &ids {
                        if id.is_empty() {
                            return Err(format!("'{s}': empty pre-release identifier"));
                        }
                        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                            return Err(format!(
                                "'{s}': invalid pre-release identifier '{id}' \
                                 (allowed: A-Z, a-z, 0-9, hyphen)"
                            ));
                        }
                    }
                    (n, ids)
                }
            };

            // 主.次.修订
            let parts: Vec<&str> = numbers.split('.').collect();
            if parts.len() != 3 {
                return Err(format!(
                    "'{s}': expected `major.minor.patch`, found {} numeric segment(s)",
                    parts.len()
                ));
            }
            let mut nums = [0u64; 3];
            for (slot, part) in nums.iter_mut().zip(parts.iter()) {
                if part.is_empty() {
                    return Err(format!("'{s}': empty version segment"));
                }
                if part.len() > 1 && part.starts_with('0') {
                    return Err(format!("'{s}': leading zeros are not allowed in '{part}'"));
                }
                *slot = part
                    .parse()
                    .map_err(|_| format!("'{s}': invalid numeric segment '{part}'"))?;
            }

            Ok(Self {
                major: nums[0],
                minor: nums[1],
                patch: nums[2],
                pre,
            })
        }

        /// 是否预发布版本（带 `-xxx` 段）
        pub fn is_prerelease(&self) -> bool {
            !self.pre.is_empty()
        }
    }

    /// 单个预发布标识比较（semver §11.4）：
    /// 纯数字按数值比，数字 < 非数字，非数字按 ASCII 字典序。
    fn cmp_pre_id(a: &str, b: &str) -> Ordering {
        match (is_numeric_id(a), is_numeric_id(b)) {
            (true, true) => a.parse::<u64>().unwrap().cmp(&b.parse::<u64>().unwrap()),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => a.cmp(b),
        }
    }

    fn is_numeric_id(id: &str) -> bool {
        !id.is_empty() && id.chars().all(|c| c.is_ascii_digit())
    }

    /// 预发布序列比较（semver §11.3/§11.4）：正式版 > 预发布版；
    /// 逐段比较后，前缀相同则段多者大。
    fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
        match (a.is_empty(), b.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater, // 正式版 > 预发布版
            (false, true) => Ordering::Less,
            (false, false) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let ord = cmp_pre_id(x, y);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                a.len().cmp(&b.len())
            }
        }
    }

    impl Ord for Version {
        fn cmp(&self, other: &Self) -> Ordering {
            (self.major, self.minor, self.patch)
                .cmp(&(other.major, other.minor, other.patch))
                .then_with(|| cmp_pre(&self.pre, &other.pre))
        }
    }

    impl PartialOrd for Version {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    /// 便捷比较：两个版本字符串的优先级。
    pub fn compare(a: &str, b: &str) -> Result<Ordering, String> {
        Ok(Version::parse(a)?.cmp(&Version::parse(b)?))
    }

    /// min_ep_version 门禁：当前版本 `current` 是否 ≥ 最低要求 `min`。
    pub fn satisfies_min(current: &str, min: &str) -> Result<bool, String> {
        Ok(Version::parse(current)? >= Version::parse(min)?)
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// §4.2 冻结示例（逐字段）
    const EXAMPLE_TOML: &str = r#"
[pack]
id = "pigeonfish.subtitle-kit"
version = "1.0.0"
name = "字幕制作整合包"
description = "视频转字幕 + 降噪一体化"
authors = ["pigeonfish"]
license = "MIT"
homepage = "https://github.com/pigeonfish/subtitle-kit"
min_ep_version = "0.1.0"
tags = ["字幕", "视频"]

[compute]
backends = ["cuda", "cpu"]
notes = { rocm = "需 torch-rocm wheel" }

[[models]]
qualified_id = "ep.systran.faster-whisper"
variant = "large-v3"
mode = "reference"
tags = ["字幕"]

[[pipelines]]
file = "pipelines/video_to_srt.toml"
"#;

    fn example_manifest() -> PackManifest {
        toml::from_str(EXAMPLE_TOML).unwrap()
    }

    // ── TOML → 结构（§4.2 逐字段）──────────────────────────────────────

    #[test]
    fn parse_example_manifest_all_fields() {
        let m = example_manifest();

        assert_eq!(m.pack.id, "pigeonfish.subtitle-kit");
        assert_eq!(m.pack.version, "1.0.0");
        assert_eq!(m.pack.name, "字幕制作整合包");
        assert_eq!(m.pack.description, "视频转字幕 + 降噪一体化");
        assert_eq!(m.pack.authors, vec!["pigeonfish"]);
        assert_eq!(m.pack.license.as_deref(), Some("MIT"));
        assert_eq!(
            m.pack.homepage.as_deref(),
            Some("https://github.com/pigeonfish/subtitle-kit")
        );
        assert_eq!(m.pack.min_ep_version.as_deref(), Some("0.1.0"));
        assert_eq!(m.pack.tags, vec!["字幕", "视频"]);

        assert_eq!(
            m.compute.backends,
            vec![ComputeBackend::Cuda, ComputeBackend::Cpu]
        );
        assert_eq!(
            m.compute.notes.get(&ComputeBackend::Rocm).map(String::as_str),
            Some("需 torch-rocm wheel")
        );

        assert_eq!(m.models.len(), 1);
        assert_eq!(m.models[0].qualified_id, "ep.systran.faster-whisper");
        assert_eq!(m.models[0].variant, "large-v3");
        assert_eq!(m.models[0].mode, ModelMode::Reference);
        assert_eq!(m.models[0].tags, vec!["字幕"]);

        assert_eq!(m.pipelines.len(), 1);
        assert_eq!(m.pipelines[0].file, "pipelines/video_to_srt.toml");

        assert!(m.validate().is_ok());
    }

    #[test]
    fn model_mode_serde_lowercase() {
        let toml_ref = r#"
[pack]
id = "a.b"
version = "1.0.0"
name = "n"
description = "d"

[compute]
backends = ["cpu"]

[[models]]
qualified_id = "ep.a.b"
variant = "v1"
mode = "bundle"
"#;
        let m: PackManifest = toml::from_str(toml_ref).unwrap();
        assert_eq!(m.models[0].mode, ModelMode::Bundle);
        assert_eq!(m.models[0].mode.as_str(), "bundle");
        assert_eq!(ModelMode::Reference.to_string(), "reference");

        // 非法 mode → 反序列化失败
        let bad = toml_ref.replace("bundle", "linked");
        assert!(toml::from_str::<PackManifest>(&bad).is_err());
    }

    #[test]
    fn backends_are_typed_enum_and_reject_unknown() {
        let m = example_manifest();
        assert_eq!(m.compute.backends[0], ComputeBackend::Cuda);

        let bad = EXAMPLE_TOML.replace(
            "backends = [\"cuda\", \"cpu\"]",
            "backends = [\"cuda\", \"warp9\"]",
        );
        assert!(toml::from_str::<PackManifest>(&bad).is_err());
    }

    #[test]
    fn notes_reject_unknown_backend_key() {
        // M4 后词表含 vulkan（合法键），改用真正不存在的后端名验证拒绝
        let bad = EXAMPLE_TOML.replace("rocm = ", "warp9 = ");
        assert!(toml::from_str::<PackManifest>(&bad).is_err());
    }

    // ── serde 往返一致 ─────────────────────────────────────────────────

    #[test]
    fn serde_toml_roundtrip_full_example() {
        let m1 = example_manifest();
        let serialized = toml::to_string(&m1).unwrap();
        let m2: PackManifest = toml::from_str(&serialized).unwrap();
        assert_eq!(m1, m2, "roundtrip mismatch:\n{serialized}");
    }

    #[test]
    fn serde_json_roundtrip() {
        let m1 = example_manifest();
        let json = serde_json::to_string(&m1).unwrap();
        let m2: PackManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m1, m2);
    }

    // ── 缺省字段默认值 ─────────────────────────────────────────────────

    #[test]
    fn defaults_for_optional_fields() {
        let minimal = r#"
[pack]
id = "alice.mini"
version = "0.1.0"
name = "Minimal"
description = "minimal pack"

[compute]
backends = ["cpu"]
"#;
        let m: PackManifest = toml::from_str(minimal).unwrap();
        assert!(m.pack.authors.is_empty());
        assert!(m.pack.license.is_none());
        assert!(m.pack.homepage.is_none());
        assert!(m.pack.min_ep_version.is_none());
        assert!(m.pack.tags.is_empty());
        assert!(m.compute.notes.is_empty());
        assert!(m.models.is_empty());
        assert!(m.pipelines.is_empty());
        assert!(m.validate().is_ok());

        // 往返同样稳定
        let serialized = toml::to_string(&m).unwrap();
        assert_eq!(toml::from_str::<PackManifest>(&serialized).unwrap(), m);
    }

    #[test]
    fn missing_required_field_fails_parse() {
        let no_version = r#"
[pack]
id = "a.b"
name = "n"
description = "d"

[compute]
backends = ["cpu"]
"#;
        assert!(toml::from_str::<PackManifest>(no_version).is_err());
    }

    // ── validate：pack.id 语法 ─────────────────────────────────────────

    fn manifest_with_id(id: &str) -> PackManifest {
        let mut m = example_manifest();
        m.pack.id = id.to_string();
        m
    }

    #[test]
    fn validate_pack_id_valid_forms() {
        for id in ["pigeonfish.subtitle-kit", "a.b", "pub-1.pack-2-x"] {
            assert!(
                manifest_with_id(id).validate().is_ok(),
                "id {id:?} should be valid"
            );
        }
    }

    #[test]
    fn validate_pack_id_rejects_bad_syntax() {
        for id in [
            "",               // 空
            "nopublisher",    // 缺点
            "a.b.c",          // 三段
            "Pigeon.fish",    // 大写
            "a.-b",           // 段以连字符开头
            "a.b_",           // 下划线
            ".b",             // 空 publisher
            "a.",             // 空 pack-name
        ] {
            let errors = manifest_with_id(id).validate().unwrap_err();
            assert!(
                errors.iter().any(|e| e.contains("pack.id")),
                "id {id:?} → {errors:?}"
            );
        }
    }

    // ── validate：semver ───────────────────────────────────────────────

    #[test]
    fn validate_rejects_bad_version() {
        let mut m = example_manifest();
        m.pack.version = "1.0".to_string();
        let errors = m.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("pack.version")), "{errors:?}");
    }

    #[test]
    fn validate_rejects_bad_min_ep_version() {
        let mut m = example_manifest();
        m.pack.min_ep_version = Some("not-semver".to_string());
        let errors = m.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("pack.min_ep_version")),
            "{errors:?}"
        );
    }

    // ── validate：models ───────────────────────────────────────────────

    #[test]
    fn validate_rejects_unparseable_qualified_id() {
        for bad_id in ["Ep.systran.faster-whisper", "not-qualified", "a.b", "ep.a.b@c"] {
            let mut m = example_manifest();
            m.models[0].qualified_id = bad_id.to_string();
            let errors = m.validate().unwrap_err();
            assert!(
                errors.iter().any(|e| e.contains("qualified_id")),
                "id {bad_id:?} → {errors:?}"
            );
        }
    }

    #[test]
    fn validate_rejects_bad_variant() {
        let mut m = example_manifest();
        m.models[0].variant = "large_v3".to_string(); // 下划线非法
        let errors = m.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("variant")), "{errors:?}");

        let mut m2 = example_manifest();
        m2.models[0].variant = String::new(); // 空变体
        let errors = m2.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("variant")), "{errors:?}");
    }

    #[test]
    fn validate_empty_models_ok() {
        let mut m = example_manifest();
        m.models.clear();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn model_entry_helpers() {
        let m = example_manifest();
        let entry = &m.models[0];
        assert_eq!(
            entry.parsed_qualified_id().unwrap().to_canonical(),
            "ep.systran.faster-whisper"
        );
        assert_eq!(
            entry.pinned_id().unwrap().to_canonical(),
            "ep.systran.faster-whisper@large-v3"
        );
    }

    // ── validate：pipelines.file ───────────────────────────────────────

    fn manifest_with_pipeline_file(file: &str) -> PackManifest {
        let mut m = example_manifest();
        m.pipelines[0].file = file.to_string();
        m
    }

    #[test]
    fn validate_pipeline_file_valid_forms() {
        for f in ["pipelines/video_to_srt.toml", "a/b/c.toml", "single.toml", "./x.toml"] {
            assert!(
                manifest_with_pipeline_file(f).validate().is_ok(),
                "file {f:?} should be valid"
            );
        }
    }

    #[test]
    fn validate_pipeline_file_rejects_escape_attempts() {
        for f in [
            "",                                // 空
            "../outside.toml",                 // 上溯逃逸
            "pipelines/../../etc/passwd",      // 中段 ..
            "/abs/path.toml",                  // 绝对路径
            "pipelines\\video.toml",           // 反斜杠分隔符
            "C:/abs/windows.toml",             // Windows 绝对路径
        ] {
            let errors = manifest_with_pipeline_file(f).validate().unwrap_err();
            assert!(
                errors.iter().any(|e| e.contains("pipelines[0].file")),
                "file {f:?} → {errors:?}"
            );
        }
    }

    #[test]
    fn validate_collects_all_errors() {
        let mut m = example_manifest();
        m.pack.id = "Bad Id".to_string();
        m.pack.version = "1".to_string();
        m.models[0].qualified_id = "nope".to_string();
        m.pipelines[0].file = "../x.toml".to_string();
        let errors = m.validate().unwrap_err();
        assert_eq!(errors.len(), 4, "{errors:?}");
    }

    // ── from_file ──────────────────────────────────────────────────────

    #[test]
    fn from_file_loads_and_validates() {
        let dir = std::env::temp_dir().join("ep_pack_test_manifest");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("ep-pack.toml");
        std::fs::write(&file_path, EXAMPLE_TOML).unwrap();

        let m = PackManifest::from_file(&file_path).unwrap();
        assert_eq!(m.pack.id, "pigeonfish.subtitle-kit");
        assert!(m.validate().is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── semver：解析 ───────────────────────────────────────────────────

    #[test]
    fn semver_parse_basic() {
        let v = semver::Version::parse("1.2.3").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
        assert!(!v.is_prerelease());
    }

    #[test]
    fn semver_parse_prerelease_and_build() {
        let v = semver::Version::parse("1.0.0-alpha.1").unwrap();
        assert!(v.is_prerelease());

        // 构建元数据接受且不参与比较
        let with_build = semver::Version::parse("1.0.0+build.5").unwrap();
        assert_eq!(with_build, semver::Version::parse("1.0.0").unwrap());
    }

    #[test]
    fn semver_parse_rejects_malformed() {
        for s in [
            "", "1", "1.0", "1.0.0.0", "v1.0.0", "1.x.0", "01.0.0", "1.0.00",
            "1.0.0-", "1.0.0-.", "1.0.0-alpha..1", "1.0.0-al_pha", "1.0.0+",
        ] {
            assert!(semver::Version::parse(s).is_err(), "should reject {s:?}");
        }
    }

    // ── semver：比较（规范 §11 序）─────────────────────────────────────

    #[test]
    fn semver_numeric_ordering_not_lexical() {
        assert!(semver::compare("1.2.3", "1.10.0").unwrap().is_lt());
        assert!(semver::compare("2.0.0", "1.9.9").unwrap().is_gt());
        assert!(semver::compare("0.1.10", "0.1.9").unwrap().is_gt());
        assert!(semver::compare("1.0.0", "1.0.0").unwrap().is_eq());
    }

    #[test]
    fn semver_spec_section_11_ordering_chain() {
        // semver 规范 §11 的经典排序链
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(
                semver::compare(pair[0], pair[1]).unwrap().is_lt(),
                "{} should be < {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn semver_numeric_pre_id_less_than_alphanumeric() {
        assert!(semver::compare("1.0.0-1", "1.0.0-alpha").unwrap().is_lt());
        assert!(semver::compare("1.0.0-2", "1.0.0-11").unwrap().is_lt()); // 数值比较非字典序
    }

    #[test]
    fn semver_satisfies_min_gate() {
        assert!(semver::satisfies_min("0.2.0", "0.1.0").unwrap());
        assert!(semver::satisfies_min("0.1.0", "0.1.0").unwrap());
        assert!(!semver::satisfies_min("0.0.9", "0.1.0").unwrap());
        assert!(!semver::satisfies_min("1.0.0-rc.1", "1.0.0").unwrap()); // 预发布 < 正式版
        assert!(semver::satisfies_min("bad", "0.1.0").is_err());
    }
}
