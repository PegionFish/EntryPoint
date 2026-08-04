# i18n 键需求汇总（各波次代理提交，C8 统一落盘）

> 规则 8：i18n/locales/** 由编排者/C8 独占写入。各代理在交付物附键需求，本文件累计汇总，Wave 3 由 C8 落盘 zh/en 双份并通过键集门禁。
> 格式：`命名空间:键` | zh-CN | en | 提交方 | 状态（待落盘/已落盘）

## Wave S

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `components:sidebar.nav.packs` | 整合包 | Packs | S2 | 待落盘 |
| `desktopApp:nav.packs` | 整合包 | Packs | S2 | 待落盘 |
| `packs:page.title` | 整合包 | Packs | S2 | 待落盘 |
| `packs:page.description` | 导入、构建与管理模型整合包（.epzip） | Import, build and manage model packs (.epzip) | S2 | 待落盘 |
| `packs:empty.title` | 暂无整合包 | No packs yet | S2 | 待落盘 |
| `packs:empty.description` | 导入或构建整合包后将在此显示 | Imported or built packs will appear here | S2 | 待落盘 |
| `desktopApp:toast.packImportComplete` | 整合包「{{id}}」导入完成 | Pack "{{id}}" imported | S2 | 待落盘 |
| `desktopApp:toast.packImportFailed` | 整合包「{{id}}」导入失败：{{detail}} | Pack "{{id}}" import failed: {{detail}} | S2 | 待落盘 |

注：S1 零新增键（501 stub 复用 `common.tip.comingSoon`）。
