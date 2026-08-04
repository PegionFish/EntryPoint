//! 三端共享 i18n 基础设施 — 嵌入式翻译加载器（ep-core / ep-daemon / ep-desktop 共用）。
//!
//! # 共享契约（冻结，勿单方面修改）
//!
//! - 翻译文件目录：`<repo>/i18n/locales/{zh-CN,en}/`，每语言 14 个命名空间文件：
//!   `common, dashboard, modules, models, pipeline, tasks, settings, components,
//!   desktopPages, desktopApp, apiCore, apiModels, apiPipelines, packs`（`.json`）。
//! - **格式：扁平键**（如 `"upload.title": "上传模型"`），禁止嵌套对象；
//!   值必须是字符串。zh-CN / en 两语言键集必须完全一致
//!   （`tests::zh_en_keysets_identical_for_all_namespaces` 是长期门禁）。
//! - 语言码：`zh-CN` / `en`；归一化规则见 [`normalize_language`]
//!   （zh* → zh-CN，en* → en，其余 → zh-CN）。
//! - 键引用格式：`"命名空间.键"`（**首个** `.` 之前是命名空间，键本身可再含 `.`）。
//! - 插值语法：`{{name}}`，由 [`t`] 的 `params` 逐个替换。
//!
//! # 文案与日志的边界
//!
//! - **用户可见文案**（API 错误响应、桌面端 UI、toast 等）一律走本模块，
//!   跟随 `config.general.language`。
//! - **日志永远英文**：`tracing` 宏（info!/warn!/error!/debug!）中的消息
//!   不经过 i18n，始终使用英文字面量，不随语言配置变化。
//!
//! # 嵌入方式
//!
//! 全部 14×2 个 JSON 经 `include_str!` 编译期嵌入二进制（daemon 与桌面端
//! 无需在运行时携带 i18n/ 目录），首次调用时经 `OnceLock` 惰性解析为
//! `lang → ns → key → String`。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 契约规定的 14 个命名空间（与 `i18n/locales/*/` 下的文件名一一对应）。
/// `packs` 为 Wave S 新增（整合包管理区，键由 Wave 3 C8 统一落盘）。
pub const NAMESPACES: &[&str] = &[
    "common",
    "dashboard",
    "modules",
    "models",
    "pipeline",
    "tasks",
    "settings",
    "components",
    "desktopPages",
    "desktopApp",
    "apiCore",
    "apiModels",
    "apiPipelines",
    "packs",
];

type KeyMap = HashMap<String, String>;
type NsMap = HashMap<&'static str, KeyMap>;
type LangMap = HashMap<&'static str, NsMap>;

static TABLES: OnceLock<LangMap> = OnceLock::new();

/// 解析一种语言的全部命名空间文件。
///
/// 任何格式违规（非法 JSON、顶层不是对象、值不是字符串）都在首次加载时 panic ——
/// 翻译文件是编译期嵌入的契约产物，宁可启动即炸也不静默降级。
fn parse_lang(entries: &[(&'static str, &'static str)]) -> NsMap {
    let mut ns_map = NsMap::with_capacity(entries.len());
    for (ns, raw) in entries {
        let value: serde_json::Value = serde_json::from_str(raw)
            .unwrap_or_else(|e| panic!("i18n locale `{ns}` is not valid JSON: {e}"));
        let obj = value
            .as_object()
            .unwrap_or_else(|| panic!("i18n locale `{ns}` must be a flat JSON object"));
        let mut keys = KeyMap::with_capacity(obj.len());
        for (k, v) in obj {
            let s = v.as_str().unwrap_or_else(|| {
                panic!("i18n locale `{ns}` key `{k}` must map to a string (flat format, no nesting)")
            });
            keys.insert(k.clone(), s.to_string());
        }
        ns_map.insert(ns, keys);
    }
    ns_map
}

/// `lang → ns → key → value` 全局表（惰性构建一次）。
fn tables() -> &'static LangMap {
    TABLES.get_or_init(|| {
        let mut m = LangMap::with_capacity(2);
        m.insert(
            "zh-CN",
            parse_lang(&[
                ("common", include_str!("../../../i18n/locales/zh-CN/common.json")),
                ("dashboard", include_str!("../../../i18n/locales/zh-CN/dashboard.json")),
                ("modules", include_str!("../../../i18n/locales/zh-CN/modules.json")),
                ("models", include_str!("../../../i18n/locales/zh-CN/models.json")),
                ("pipeline", include_str!("../../../i18n/locales/zh-CN/pipeline.json")),
                ("tasks", include_str!("../../../i18n/locales/zh-CN/tasks.json")),
                ("settings", include_str!("../../../i18n/locales/zh-CN/settings.json")),
                ("components", include_str!("../../../i18n/locales/zh-CN/components.json")),
                ("desktopPages", include_str!("../../../i18n/locales/zh-CN/desktopPages.json")),
                ("desktopApp", include_str!("../../../i18n/locales/zh-CN/desktopApp.json")),
                ("apiCore", include_str!("../../../i18n/locales/zh-CN/apiCore.json")),
                ("apiModels", include_str!("../../../i18n/locales/zh-CN/apiModels.json")),
                ("apiPipelines", include_str!("../../../i18n/locales/zh-CN/apiPipelines.json")),
                ("packs", include_str!("../../../i18n/locales/zh-CN/packs.json")),
            ]),
        );
        m.insert(
            "en",
            parse_lang(&[
                ("common", include_str!("../../../i18n/locales/en/common.json")),
                ("dashboard", include_str!("../../../i18n/locales/en/dashboard.json")),
                ("modules", include_str!("../../../i18n/locales/en/modules.json")),
                ("models", include_str!("../../../i18n/locales/en/models.json")),
                ("pipeline", include_str!("../../../i18n/locales/en/pipeline.json")),
                ("tasks", include_str!("../../../i18n/locales/en/tasks.json")),
                ("settings", include_str!("../../../i18n/locales/en/settings.json")),
                ("components", include_str!("../../../i18n/locales/en/components.json")),
                ("desktopPages", include_str!("../../../i18n/locales/en/desktopPages.json")),
                ("desktopApp", include_str!("../../../i18n/locales/en/desktopApp.json")),
                ("apiCore", include_str!("../../../i18n/locales/en/apiCore.json")),
                ("apiModels", include_str!("../../../i18n/locales/en/apiModels.json")),
                ("apiPipelines", include_str!("../../../i18n/locales/en/apiPipelines.json")),
                ("packs", include_str!("../../../i18n/locales/en/packs.json")),
            ]),
        );
        m
    })
}

/// 归一化任意语言码到契约支持的语言码。
///
/// 规则：`zh*` → `"zh-CN"`，`en*` → `"en"`，其余（含空串、未知语言）→ `"zh-CN"`。
/// 大小写不敏感，首尾空白忽略。
pub fn normalize_language(s: &str) -> &'static str {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "zh-CN";
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("zh") {
        "zh-CN"
    } else if lower.starts_with("en") {
        "en"
    } else {
        "zh-CN"
    }
}

/// 翻译查找：`key` 形如 `"命名空间.键"`（首个 `.` 前为命名空间），
/// `params` 中的 `name` 按 `{{name}}` 插值语法替换进模板。
///
/// 键缺失（未知命名空间、未知键、或 `key` 不含 `.`）时**返回键本身**，
/// 让缺失在 UI 上显形而不是静默成空串。`lang` 先经 [`normalize_language`] 归一化。
pub fn t(lang: &str, key: &str, params: &[(&str, &str)]) -> String {
    let lang = normalize_language(lang);
    let Some((ns, rest)) = key.split_once('.') else {
        return key.to_string();
    };
    let Some(template) = tables()
        .get(lang)
        .and_then(|by_ns| by_ns.get(ns))
        .and_then(|by_key| by_key.get(rest))
    else {
        return key.to_string();
    };
    let mut out = template.clone();
    for (name, value) in params {
        let pattern = format!("{{{{{name}}}}}");
        out = out.replace(&pattern, value);
    }
    out
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 长期门禁：14 个命名空间在 zh-CN / en 下的键集必须完全一致。
    #[test]
    fn zh_en_keysets_identical_for_all_namespaces() {
        let tables = tables();
        let zh = &tables["zh-CN"];
        let en = &tables["en"];
        assert_eq!(zh.len(), NAMESPACES.len(), "zh-CN: all 14 namespaces present");
        assert_eq!(en.len(), NAMESPACES.len(), "en: all 14 namespaces present");
        for ns in NAMESPACES {
            let zh_keys: BTreeSet<&String> = zh[*ns].keys().collect();
            let en_keys: BTreeSet<&String> = en[*ns].keys().collect();
            assert_eq!(zh_keys, en_keys, "namespace `{ns}`: zh-CN/en key sets differ");
        }
    }

    #[test]
    fn common_namespace_is_populated() {
        assert!(tables()["zh-CN"]["common"].len() >= 60);
        assert_eq!(
            tables()["zh-CN"]["common"].len(),
            tables()["en"]["common"].len()
        );
    }

    #[test]
    fn normalize_language_rules() {
        assert_eq!(normalize_language("zh"), "zh-CN");
        assert_eq!(normalize_language("zh-CN"), "zh-CN");
        assert_eq!(normalize_language("zh-TW"), "zh-CN");
        assert_eq!(normalize_language("en"), "en");
        assert_eq!(normalize_language("en-US"), "en");
        assert_eq!(normalize_language("EN-gb"), "en");
        assert_eq!(normalize_language(""), "zh-CN");
        assert_eq!(normalize_language("  "), "zh-CN");
        assert_eq!(normalize_language("fr"), "zh-CN");
        assert_eq!(normalize_language("ja-JP"), "zh-CN");
    }

    #[test]
    fn lookup_hits_in_both_languages() {
        assert_eq!(t("zh-CN", "common.action.confirm", &[]), "确认");
        assert_eq!(t("en", "common.action.confirm", &[]), "Confirm");
        // 任意 zh-* / en-* 变体先归一化再查表
        assert_eq!(t("zh-TW", "common.action.cancel", &[]), "取消");
        assert_eq!(t("en-US", "common.action.cancel", &[]), "Cancel");
    }

    #[test]
    fn missing_key_returns_key_itself() {
        assert_eq!(t("zh-CN", "common.nope.notHere", &[]), "common.nope.notHere");
        assert_eq!(t("zh-CN", "noSuchNamespace.key", &[]), "noSuchNamespace.key");
        // 无命名空间分隔符 → 原样返回
        assert_eq!(t("zh-CN", "no_dot_at_all", &[]), "no_dot_at_all");
    }

    #[test]
    fn interpolation_replaces_placeholders() {
        assert_eq!(
            t("zh-CN", "common.tip.confirmDeleteNamed", &[("name", "large-v3")]),
            "确认删除 large-v3？此操作不可撤销"
        );
        assert_eq!(
            t("en", "common.tip.confirmDeleteNamed", &[("name", "large-v3")]),
            "Delete large-v3? This action cannot be undone"
        );
    }

    #[test]
    fn interpolation_with_multiple_params() {
        // 两个不同占位符各替换一次；未提供的占位符保留原样
        let zh = &tables()["zh-CN"]["common"]["tip.confirmDeleteNamed"];
        assert!(zh.contains("{{name}}"));
        let out = t(
            "zh-CN",
            "common.tip.confirmDeleteNamed",
            &[("name", "a"), ("unused", "b")],
        );
        assert!(!out.contains("{{name}}"));
        assert!(!out.contains("{{unused}}"), "unknown placeholders stay literal, no crash");
    }
}
