//! 桌面端 i18n 薄封装 — 直接转发 [`ep_core::i18n`]。
//!
//! Wave 1 桌面端代理统一通过 [`tr`] 取用户可见文案，不要在 pages/ui 中手写
//! 字面量。键格式（`"命名空间.键"`）、插值语法（`{{name}}`）、语言归一化规则
//! 与缺失键回退行为见 `ep_core::i18n` 模块文档；翻译资源位于仓库根
//! `i18n/locales/`，经 ep-core 编译期嵌入，桌面端无需运行时文件。
//!
//! 注意边界：日志（tracing 宏）永远英文，不走本模块。

/// 翻译查找：`key` 形如 `"命名空间.键"`，`params` 按 `{{name}}` 插值。
///
/// 键缺失时返回键本身（与 [`ep_core::i18n::t`] 一致）。
pub fn tr(lang: &str, key: &str, params: &[(&str, &str)]) -> String {
    ep_core::i18n::t(lang, key, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tr_forwards_to_ep_core() {
        assert_eq!(tr("zh-CN", "common.action.save", &[]), "保存");
        assert_eq!(tr("en", "common.action.save", &[]), "Save");
        // 归一化与缺失键回退同样生效
        assert_eq!(tr("zh-TW", "common.action.save", &[]), "保存");
        assert_eq!(tr("zh-CN", "common.missing.key", &[]), "common.missing.key");
    }
}
