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

## Wave 1

### A4（packs 命名空间，错误在 API 层由 B1/B2 映射）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `packs:errorArchiveOpen` | 无法打开整合包归档：{{detail}} | Failed to open pack archive: {{detail}} | A4 | 待落盘 |
| `packs:errorArchiveInvalid` | 整合包归档无效或不可读：{{detail}} | Invalid or unreadable pack archive: {{detail}} | A4 | 待落盘 |
| `packs:errorUnsafePath` | 归档含非法条目路径（绝对路径/.. /反斜杠/保留名）：{{entry}} | Unsafe archive entry path: {{entry}} | A4 | 待落盘 |
| `packs:errorSymlinkEntry` | 归档含符号链接条目（整合包禁止）：{{entry}} | Archive contains a forbidden symlink entry: {{entry}} | A4 | 待落盘 |
| `packs:errorSymlinkEscape` | 解包路径经符号链接逃出暂存目录：{{entry}} | Extraction would escape staging via symlink: {{entry}} | A4 | 待落盘 |
| `packs:errorSpecialFile` | 归档含特殊文件条目（模式 {{mode}}）：{{entry}} | Archive contains special-file entry: {{entry}} | A4 | 待落盘 |
| `packs:errorDuplicateEntry` | 归档含重复条目（大小写冲突）：{{entry}} | Duplicate archive entry (case collision): {{entry}} | A4 | 待落盘 |
| `packs:errorMissingManifest` | 归档缺少清单 ep-pack.toml | Archive lacks manifest ep-pack.toml | A4 | 待落盘 |
| `packs:errorSizeLimit` | 整合包内容超过大小上限（{{limit}} 字节） | Pack content exceeds size limit ({{limit}} bytes) | A4 | 待落盘 |
| `packs:errorChecksumMissing` | 未找到 CHECKSUMS.toml | CHECKSUMS.toml not found | A4 | 待落盘 |
| `packs:errorChecksumParse` | CHECKSUMS.toml 解析失败：{{detail}} | Failed to parse CHECKSUMS.toml: {{detail}} | A4 | 待落盘 |
| `packs:errorChecksumIntegrity` | 校验和验证失败：{{missing}} 缺失、{{unexpected}} 多余、{{mismatched}} 篡改 | Checksum verification failed: {{missing}} missing, {{unexpected}} unexpected, {{mismatched}} mismatched | A4 | 待落盘 |
| `packs:errorBuildSourceMissing` | 包源目录不存在或不是目录：{{path}} | Pack source dir missing: {{path}} | A4 | 待落盘 |
| `packs:errorBuildManifestMissing` | 包源目录缺少 ep-pack.toml：{{path}} | Pack source lacks ep-pack.toml: {{path}} | A4 | 待落盘 |
| `packs:errorBuildOutputInsideSource` | 输出路径不得位于包源目录内 | Output path must not live inside pack source dir | A4 | 待落盘 |

注：A1/A2/A3/A6 零新增键（技术层英文纪律，用户可见文案由 B/C 波消费侧提需求）。
