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

## Wave 2

### B5

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `apiModels:variantMismatch` | 路径与请求体的模型 ID 不一致（路径：{{path_id}}，请求体：{{body_id}}） | model_id in path and body do not match (path: {{path_id}}, body: {{body_id}}) | B5 | 待落盘 |

### B1（packs 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `packs:errorMinEpVersion` | 整合包要求 EntryPoint ≥ {{required}}，当前版本 {{current}} | Pack requires EntryPoint ≥ {{required}}, current is {{current}} | B1 | 待落盘 |
| `packs:errorAlreadyInstalled` | 整合包「{{id}}」已安装，请先卸载再导入 | Pack "{{id}}" is already installed; uninstall it before re-importing | B1 | 待落盘 |
| `packs:errorModelConflict` | 模型 {{id}} 目标目录已存在，导入不会合并进已有目录 | Target directory for model {{id}} already exists; import never merges into existing directories | B1 | 待落盘 |
| `packs:errorBundleMissing` | 整合包声明模型 {{id}} 随包权重，但归档缺少权重文件 | Pack declares bundle weights for model {{id}} but the archive lacks them | B1 | 待落盘 |
| `packs:errorPipelineFileMissing` | 清单声明的管线文件 {{file}} 不在归档中 | Manifest-declared pipeline file {{file}} is missing from the archive | B1 | 待落盘 |
| `packs:errorInvalidPipeline` | 管线文件 {{file}} 无效：{{detail}} | Pipeline file {{file}} is invalid: {{detail}} | B1 | 待落盘 |
| `packs:adaptDevice` | 将运行于 {{device}} | Will run on {{device}} | B1 | 待落盘 |
| `packs:adaptCpuFallback` | CPU 保底（未检测到匹配的加速设备） | CPU fallback (no matching accelerator device detected) | B1 | 待落盘 |
| `packs:adaptUnsupported` | 不支持：{{reason}} | Not supported: {{reason}} | B1 | 待落盘 |

### B4

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `apiPipelines:single.missingModuleId` | 缺少 module_id 字段 | module_id is required | B4 | 待落盘 |
| `apiPipelines:single.missingCapability` | 缺少 capability 字段 | capability is required | B4 | 待落盘 |
| `apiPipelines:single.missingInputPath` | 缺少 input_path 字段 | input_path is required | B4 | 待落盘 |
| `apiPipelines:single.paramsNotObject` | params 必须是对象 | params must be an object | B4 | 待落盘 |
| `apiPipelines:single.capabilityNotFound` | 模块 '{{module_id}}' 不存在能力 '{{capability}}' | Capability '{{capability}}' not found in module '{{module_id}}' | B4 | 待落盘 |
| `apiPipelines:single.paramMissing` | 缺少必填参数 '{{param}}' | Missing required parameter '{{param}}' | B4 | 待落盘 |
| `apiPipelines:single.paramTypeInvalid` | 参数 '{{param}}' 类型无效（期望 {{expected}}） | Parameter '{{param}}' has invalid type (expected {{expected}}) | B4 | 待落盘 |
| `apiPipelines:single.paramEnumInvalid` | 参数 '{{param}}' 取值不在可选列表内 | Value for parameter '{{param}}' is not in the allowed enum values | B4 | 待落盘 |
| `apiPipelines:single.inputNotFound` | 输入文件不存在: {{path}} | Input file does not exist: {{path}} | B4 | 待落盘 |
| `apiPipelines:single.autostartTimeout` | 模块 '{{module_id}}' 自动拉起后 {{secs}}s 内未就绪 | Module '{{module_id}}' did not become healthy within {{secs}}s after autostart | B4 | 待落盘 |
| `apiPipelines:single.submitFailed` | 直跑任务提交失败: {{detail}} | Direct execution submit failed: {{detail}} | B4 | 待落盘 |
| `apiModels:inputUploadMissingFile` | multipart 缺少 'file' 文件字段 | multipart is missing the 'file' field | B4 | 待落盘 |
| `apiModels:inputUploadPlaceFailed` | 输入文件落盘失败: {{detail}} | Failed to place input file: {{detail}} | B4 | 待落盘 |

> ⚠️ C8 落盘注意：B4 现有测试断言"键缺失回退为键本身"，键落盘后需同步把相关 ep-daemon 测试断言切换为真实文案（门禁期编排者协调）。

### B6

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `apiModels:tagsNoMeta` | 模型没有下载元数据（.ep_meta.json），无法设置标签 | Model has no download metadata (.ep_meta.json); cannot set tags | B6 | 待落盘 |
| `apiModels:tagsWriteFailed` | 写入模型标签失败：{{detail}} | Failed to write model tags: {{detail}} | B6 | 待落盘 |
| `apiModels:cancelNotActive` | 该模型没有进行中的下载，无需取消 | This model has no active download to cancel | B6 | 待落盘 |

> ⚠️ C8 落盘注意（B6）：`tagsNoMeta`/`cancelNotActive` 两处 ep-daemon 测试断言当前为键兜底值，落盘后需同步切换为真实文案。

### B7（pipeline 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `pipeline:error.llmMissingBaseUrl` | LLM 节点 {{node}} 缺少必填参数 base_url（OpenAI 兼容端点） | LLM node {{node}} is missing required param 'base_url' (OpenAI-compatible endpoint) | B7 | 待落盘 |
| `pipeline:error.llmMissingModel` | LLM 节点 {{node}} 缺少必填参数 model | LLM node {{node}} is missing required param 'model' | B7 | 待落盘 |
| `pipeline:error.llmApiKeyNotSet` | LLM 节点 {{node}}：环境变量 {{env_var}} 未设置（API Key 只从环境变量读取，不落盘） | LLM node {{node}}: environment variable {{env_var}} is not set (API keys are only read from the environment) | B7 | 待落盘 |
| `pipeline:error.llmApiKeyEmpty` | LLM 节点 {{node}}：环境变量 {{env_var}} 为空 | LLM node {{node}}: environment variable {{env_var}} is empty | B7 | 待落盘 |
| `pipeline:error.llmHttpStatus` | LLM 节点 {{node}} 请求失败（HTTP {{status}}）：{{detail}} | LLM node {{node}} request failed (HTTP {{status}}): {{detail}} | B7 | 待落盘 |
| `pipeline:error.llmResponseShape` | LLM 节点 {{node}}：响应缺少 choices[0].message.content | LLM node {{node}}: response is missing choices[0].message.content | B7 | 待落盘 |
| `pipeline:error.llmInvalidJsonOutput` | LLM 节点 {{node}}：output_format 为 json 但响应不是合法 JSON | LLM node {{node}}: output_format is 'json' but the response is not valid JSON | B7 | 待落盘 |
| `pipeline:error.llmInvalidOutputFormat` | LLM 节点 {{node}}：output_format 仅支持 text/json | LLM node {{node}}: output_format must be 'text' or 'json' | B7 | 待落盘 |
| `pipeline:error.llmParamRange` | LLM 节点 {{node}}：参数 {{param}} 取值非法（{{hint}}） | LLM node {{node}}: invalid value for param {{param}} ({{hint}}) | B7 | 待落盘 |
| `pipeline:error.llmInputNotText` | LLM 节点 {{node}}：上游文件非文本类型（.{{ext}}） | LLM node {{node}}: upstream file is not text (.{{ext}}) | B7 | 待落盘 |
| `pipeline:warn.deviceFallback` | 节点 {{node}} 请求的设备 {{device}} 本机不存在，已回退 auto | Node {{node}}: requested device {{device}} not available; falling back to auto | B7 | 待落盘 |
| `pipeline:warn.ffmpegStringArgs` | ffmpeg 节点 {{node}} 的 args 为字符串，已按词法拆分为数组（契约形状为数组） | ffmpeg node {{node}}: string args split into an array (array is the contract shape) | B7 | 待落盘 |

### B2（packs 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `packs:errorInternal` | 整合包操作失败：{{detail}} | Pack operation failed: {{detail}} | B2 | 待落盘 |
| `packs:errorNotFound` | 整合包不存在：{{id}} | Pack not found: {{id}} | B2 | 待落盘 |
| `packs:errorImportRequestInvalid` | 导入请求无效：{{detail}} | Invalid import request: {{detail}} | B2 | 待落盘 |
| `packs:errorImportFileMissing` | 本地整合包文件不存在：{{path}} | Local pack file not found: {{path}} | B2 | 待落盘 |
| `packs:errorDownloadFailed` | 整合包 URL 下载失败：{{detail}} | Pack URL download failed: {{detail}} | B2 | 待落盘 |
| `packs:errorManifestInvalid` | 整合包清单校验失败：{{detail}} | Pack manifest validation failed: {{detail}} | B2 | 待落盘 |
| `packs:errorMinVersion` | 整合包要求最低版本 {{min}}，当前 {{current}} | Pack requires EntryPoint >= {{min}}, current {{current}} | B2 | 待落盘 |
| `packs:errorAlreadyInstalled` | 整合包已安装：{{id}}（请先卸载再导入） | Pack already installed: {{id}} (uninstall first) | B2 | 待落盘 |
| `packs:errorModelConflict` | 模型目录已存在，拒绝合并：{{target}} | Model directory exists, merge refused: {{target}} | B2 | 待落盘 |
| `packs:errorBundleMissing` | bundle 模型 {{model}} 声明权重但归档缺少 models/{{target_dir}} | Bundle model {{model}} declares weights but archive lacks models/{{target_dir}} | B2 | 待落盘 |
| `packs:errorPipelineInvalid` | 包内管线 {{file}} 无效：{{detail}} | Pack pipeline {{file}} invalid: {{detail}} | B2 | 待落盘 |
| `packs:errorBuildInvalid` | 构建请求无效：{{detail}} | Invalid build request: {{detail}} | B2 | 待落盘 |
| `packs:errorBuildNoModels` | 构建圈选未匹配到模型：{{detail}} | Build selection matched no models: {{detail}} | B2 | 待落盘 |
| `packs:errorExportNotBuilt` | 整合包产物不存在（未构建/未安装）：{{id}} | Pack artifact not found: {{id}} | B2 | 待落盘 |
| `packs:errorUploadNoFile` | 上传缺少 file 文件字段 | Upload missing 'file' field | B2 | 待落盘 |
| `packs:importDone` | 整合包导入完成：{{models}} 个模型落位、{{downloads}} 个待下载、{{pipelines}} 条管线 | Pack imported: {{models}} placed, {{downloads}} pending, {{pipelines}} pipeline(s) | B2 | 待落盘 |
| `packs:buildDone` | 整合包构建完成（{{files}} 个文件） | Pack built ({{files}} file(s)) | B2 | 待落盘 |

> ⚠️ C8 归并注意：B1/B2 各提了 `errorAlreadyInstalled`/`errorModelConflict`/`errorBundleMissing` 同名键（参数与措辞略有差异）——落盘时各取其一（建议 B2 版：API 层实际消费方），并确认双方代码引用兼容。

### B3（apiPipelines 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `apiPipelines:vramBudget.specEmpty` | VRAM 预算请求缺少节点 | VRAM budget request has no nodes | B3 | 待落盘 |
| `apiPipelines:vramBudget.cycleDetected` | 管线存在环，无法计算 VRAM 预算 | Pipeline contains a cycle; VRAM budget cannot be computed | B3 | 待落盘 |
| `apiPipelines:tasks.cancelAlreadyTerminal` | 任务已终结（{{status}}），无法取消 | Task already terminal ({{status}}), cannot cancel | B3 | 待落盘 |
| `apiPipelines:execute.capabilityNotFound` | 模块 {{moduleId}} 不存在能力 {{capability}} | Module {{moduleId}} has no capability {{capability}} | B3 | 待落盘 |
| `apiPipelines:execute.inputMissing` | 输入文件不存在: {{path}} | Input file does not exist: {{path}} | B3 | 待落盘 |
| `apiPipelines:execute.moduleStartFailed` | 模块自动拉起失败: {{detail}} | Failed to auto-start module: {{detail}} | B3 | 待落盘 |
