# i18n 键需求汇总（各波次代理提交，C8 统一落盘）

> 规则 8：i18n/locales/** 由编排者/C8 独占写入。各代理在交付物附键需求，本文件累计汇总，Wave 3 由 C8 落盘 zh/en 双份并通过键集门禁。
> 格式：`命名空间:键` | zh-CN | en | 提交方 | 状态（待落盘/已落盘）

## Wave S

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `components:sidebar.nav.packs` | 整合包 | Packs | S2 | 已落盘 |
| `desktopApp:nav.packs` | 整合包 | Packs | S2 | 已落盘 |
| `packs:page.title` | 整合包 | Packs | S2 | 已落盘 |
| `packs:page.description` | 导入、构建与管理模型整合包（.epzip） | Import, build and manage model packs (.epzip) | S2 | 已落盘 |
| `packs:empty.title` | 暂无整合包 | No packs yet | S2 | 已落盘 |
| `packs:empty.description` | 导入或构建整合包后将在此显示 | Imported or built packs will appear here | S2 | 已落盘 |
| `desktopApp:toast.packImportComplete` | 整合包「{{id}}」导入完成 | Pack "{{id}}" imported | S2 | 已落盘 |
| `desktopApp:toast.packImportFailed` | 整合包「{{id}}」导入失败：{{detail}} | Pack "{{id}}" import failed: {{detail}} | S2 | 已落盘 |

注：S1 零新增键（501 stub 复用 `common.tip.comingSoon`）。

## Wave 1

### A4（packs 命名空间，错误在 API 层由 B1/B2 映射）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `packs:errorArchiveOpen` | 无法打开整合包归档：{{detail}} | Failed to open pack archive: {{detail}} | A4 | 已落盘 |
| `packs:errorArchiveInvalid` | 整合包归档无效或不可读：{{detail}} | Invalid or unreadable pack archive: {{detail}} | A4 | 已落盘 |
| `packs:errorUnsafePath` | 归档含非法条目路径（绝对路径/.. /反斜杠/保留名）：{{entry}} | Unsafe archive entry path: {{entry}} | A4 | 已落盘 |
| `packs:errorSymlinkEntry` | 归档含符号链接条目（整合包禁止）：{{entry}} | Archive contains a forbidden symlink entry: {{entry}} | A4 | 已落盘 |
| `packs:errorSymlinkEscape` | 解包路径经符号链接逃出暂存目录：{{entry}} | Extraction would escape staging via symlink: {{entry}} | A4 | 已落盘 |
| `packs:errorSpecialFile` | 归档含特殊文件条目（模式 {{mode}}）：{{entry}} | Archive contains special-file entry: {{entry}} | A4 | 已落盘 |
| `packs:errorDuplicateEntry` | 归档含重复条目（大小写冲突）：{{entry}} | Duplicate archive entry (case collision): {{entry}} | A4 | 已落盘 |
| `packs:errorMissingManifest` | 归档缺少清单 ep-pack.toml | Archive lacks manifest ep-pack.toml | A4 | 已落盘 |
| `packs:errorSizeLimit` | 整合包内容超过大小上限（{{limit}} 字节） | Pack content exceeds size limit ({{limit}} bytes) | A4 | 已落盘 |
| `packs:errorChecksumMissing` | 未找到 CHECKSUMS.toml | CHECKSUMS.toml not found | A4 | 已落盘 |
| `packs:errorChecksumParse` | CHECKSUMS.toml 解析失败：{{detail}} | Failed to parse CHECKSUMS.toml: {{detail}} | A4 | 已落盘 |
| `packs:errorChecksumIntegrity` | 校验和验证失败：{{missing}} 缺失、{{unexpected}} 多余、{{mismatched}} 篡改 | Checksum verification failed: {{missing}} missing, {{unexpected}} unexpected, {{mismatched}} mismatched | A4 | 已落盘 |
| `packs:errorBuildSourceMissing` | 包源目录不存在或不是目录：{{path}} | Pack source dir missing: {{path}} | A4 | 已落盘 |
| `packs:errorBuildManifestMissing` | 包源目录缺少 ep-pack.toml：{{path}} | Pack source lacks ep-pack.toml: {{path}} | A4 | 已落盘 |
| `packs:errorBuildOutputInsideSource` | 输出路径不得位于包源目录内 | Output path must not live inside pack source dir | A4 | 已落盘 |

注：A1/A2/A3/A6 零新增键（技术层英文纪律，用户可见文案由 B/C 波消费侧提需求）。

## Wave 2

### B5

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `apiModels:variantMismatch` | 路径与请求体的模型 ID 不一致（路径：{{path_id}}，请求体：{{body_id}}） | model_id in path and body do not match (path: {{path_id}}, body: {{body_id}}) | B5 | 已落盘 |

### B1（packs 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `packs:errorMinEpVersion` | 整合包要求 EntryPoint ≥ {{required}}，当前版本 {{current}} | Pack requires EntryPoint ≥ {{required}}, current is {{current}} | B1 | 已落盘 |
| `packs:errorAlreadyInstalled` | 整合包「{{id}}」已安装，请先卸载再导入 | Pack "{{id}}" is already installed; uninstall it before re-importing | B1 | 已落盘 |
| `packs:errorModelConflict` | 模型 {{id}} 目标目录已存在，导入不会合并进已有目录 | Target directory for model {{id}} already exists; import never merges into existing directories | B1 | 已落盘 |
| `packs:errorBundleMissing` | 整合包声明模型 {{id}} 随包权重，但归档缺少权重文件 | Pack declares bundle weights for model {{id}} but the archive lacks them | B1 | 已落盘 |
| `packs:errorPipelineFileMissing` | 清单声明的管线文件 {{file}} 不在归档中 | Manifest-declared pipeline file {{file}} is missing from the archive | B1 | 已落盘 |
| `packs:errorInvalidPipeline` | 管线文件 {{file}} 无效：{{detail}} | Pipeline file {{file}} is invalid: {{detail}} | B1 | 已落盘 |
| `packs:adaptDevice` | 将运行于 {{device}} | Will run on {{device}} | B1 | 已落盘 |
| `packs:adaptCpuFallback` | CPU 保底（未检测到匹配的加速设备） | CPU fallback (no matching accelerator device detected) | B1 | 已落盘 |
| `packs:adaptUnsupported` | 不支持：{{reason}} | Not supported: {{reason}} | B1 | 已落盘 |

### B4

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `apiPipelines:single.missingModuleId` | 缺少 module_id 字段 | module_id is required | B4 | 已落盘 |
| `apiPipelines:single.missingCapability` | 缺少 capability 字段 | capability is required | B4 | 已落盘 |
| `apiPipelines:single.missingInputPath` | 缺少 input_path 字段 | input_path is required | B4 | 已落盘 |
| `apiPipelines:single.paramsNotObject` | params 必须是对象 | params must be an object | B4 | 已落盘 |
| `apiPipelines:single.capabilityNotFound` | 模块 '{{module_id}}' 不存在能力 '{{capability}}' | Capability '{{capability}}' not found in module '{{module_id}}' | B4 | 已落盘 |
| `apiPipelines:single.paramMissing` | 缺少必填参数 '{{param}}' | Missing required parameter '{{param}}' | B4 | 已落盘 |
| `apiPipelines:single.paramTypeInvalid` | 参数 '{{param}}' 类型无效（期望 {{expected}}） | Parameter '{{param}}' has invalid type (expected {{expected}}) | B4 | 已落盘 |
| `apiPipelines:single.paramEnumInvalid` | 参数 '{{param}}' 取值不在可选列表内 | Value for parameter '{{param}}' is not in the allowed enum values | B4 | 已落盘 |
| `apiPipelines:single.inputNotFound` | 输入文件不存在: {{path}} | Input file does not exist: {{path}} | B4 | 已落盘 |
| `apiPipelines:single.autostartTimeout` | 模块 '{{module_id}}' 自动拉起后 {{secs}}s 内未就绪 | Module '{{module_id}}' did not become healthy within {{secs}}s after autostart | B4 | 已落盘 |
| `apiPipelines:single.submitFailed` | 直跑任务提交失败: {{detail}} | Direct execution submit failed: {{detail}} | B4 | 已落盘 |
| `apiModels:inputUploadMissingFile` | multipart 缺少 'file' 文件字段 | multipart is missing the 'file' field | B4 | 已落盘 |
| `apiModels:inputUploadPlaceFailed` | 输入文件落盘失败: {{detail}} | Failed to place input file: {{detail}} | B4 | 已落盘 |

> ⚠️ C8 落盘注意：B4 现有测试断言"键缺失回退为键本身"，键落盘后需同步把相关 ep-daemon 测试断言切换为真实文案（门禁期编排者协调）。

### B6

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `apiModels:tagsNoMeta` | 模型没有下载元数据（.ep_meta.json），无法设置标签 | Model has no download metadata (.ep_meta.json); cannot set tags | B6 | 已落盘 |
| `apiModels:tagsWriteFailed` | 写入模型标签失败：{{detail}} | Failed to write model tags: {{detail}} | B6 | 已落盘 |
| `apiModels:cancelNotActive` | 该模型没有进行中的下载，无需取消 | This model has no active download to cancel | B6 | 已落盘 |

> ⚠️ C8 落盘注意（B6）：`tagsNoMeta`/`cancelNotActive` 两处 ep-daemon 测试断言当前为键兜底值，落盘后需同步切换为真实文案。

### B7（pipeline 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `pipeline:error.llmMissingBaseUrl` | LLM 节点 {{node}} 缺少必填参数 base_url（OpenAI 兼容端点） | LLM node {{node}} is missing required param 'base_url' (OpenAI-compatible endpoint) | B7 | 已落盘 |
| `pipeline:error.llmMissingModel` | LLM 节点 {{node}} 缺少必填参数 model | LLM node {{node}} is missing required param 'model' | B7 | 已落盘 |
| `pipeline:error.llmApiKeyNotSet` | LLM 节点 {{node}}：环境变量 {{env_var}} 未设置（API Key 只从环境变量读取，不落盘） | LLM node {{node}}: environment variable {{env_var}} is not set (API keys are only read from the environment) | B7 | 已落盘 |
| `pipeline:error.llmApiKeyEmpty` | LLM 节点 {{node}}：环境变量 {{env_var}} 为空 | LLM node {{node}}: environment variable {{env_var}} is empty | B7 | 已落盘 |
| `pipeline:error.llmHttpStatus` | LLM 节点 {{node}} 请求失败（HTTP {{status}}）：{{detail}} | LLM node {{node}} request failed (HTTP {{status}}): {{detail}} | B7 | 已落盘 |
| `pipeline:error.llmResponseShape` | LLM 节点 {{node}}：响应缺少 choices[0].message.content | LLM node {{node}}: response is missing choices[0].message.content | B7 | 已落盘 |
| `pipeline:error.llmInvalidJsonOutput` | LLM 节点 {{node}}：output_format 为 json 但响应不是合法 JSON | LLM node {{node}}: output_format is 'json' but the response is not valid JSON | B7 | 已落盘 |
| `pipeline:error.llmInvalidOutputFormat` | LLM 节点 {{node}}：output_format 仅支持 text/json | LLM node {{node}}: output_format must be 'text' or 'json' | B7 | 已落盘 |
| `pipeline:error.llmParamRange` | LLM 节点 {{node}}：参数 {{param}} 取值非法（{{hint}}） | LLM node {{node}}: invalid value for param {{param}} ({{hint}}) | B7 | 已落盘 |
| `pipeline:error.llmInputNotText` | LLM 节点 {{node}}：上游文件非文本类型（.{{ext}}） | LLM node {{node}}: upstream file is not text (.{{ext}}) | B7 | 已落盘 |
| `pipeline:warn.deviceFallback` | 节点 {{node}} 请求的设备 {{device}} 本机不存在，已回退 auto | Node {{node}}: requested device {{device}} not available; falling back to auto | B7 | 已落盘 |
| `pipeline:warn.ffmpegStringArgs` | ffmpeg 节点 {{node}} 的 args 为字符串，已按词法拆分为数组（契约形状为数组） | ffmpeg node {{node}}: string args split into an array (array is the contract shape) | B7 | 已落盘 |

### B2（packs 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `packs:errorInternal` | 整合包操作失败：{{detail}} | Pack operation failed: {{detail}} | B2 | 已落盘 |
| `packs:errorNotFound` | 整合包不存在：{{id}} | Pack not found: {{id}} | B2 | 已落盘 |
| `packs:errorImportRequestInvalid` | 导入请求无效：{{detail}} | Invalid import request: {{detail}} | B2 | 已落盘 |
| `packs:errorImportFileMissing` | 本地整合包文件不存在：{{path}} | Local pack file not found: {{path}} | B2 | 已落盘 |
| `packs:errorDownloadFailed` | 整合包 URL 下载失败：{{detail}} | Pack URL download failed: {{detail}} | B2 | 已落盘 |
| `packs:errorManifestInvalid` | 整合包清单校验失败：{{detail}} | Pack manifest validation failed: {{detail}} | B2 | 已落盘 |
| `packs:errorMinVersion` | 整合包要求最低版本 {{min}}，当前 {{current}} | Pack requires EntryPoint >= {{min}}, current {{current}} | B2 | 已落盘 |
| `packs:errorAlreadyInstalled` | 整合包已安装：{{id}}（请先卸载再导入） | Pack already installed: {{id}} (uninstall first) | B2 | 已落盘 |
| `packs:errorModelConflict` | 模型目录已存在，拒绝合并：{{target}} | Model directory exists, merge refused: {{target}} | B2 | 已落盘 |
| `packs:errorBundleMissing` | bundle 模型 {{model}} 声明权重但归档缺少 models/{{target_dir}} | Bundle model {{model}} declares weights but archive lacks models/{{target_dir}} | B2 | 已落盘 |
| `packs:errorPipelineInvalid` | 包内管线 {{file}} 无效：{{detail}} | Pack pipeline {{file}} invalid: {{detail}} | B2 | 已落盘 |
| `packs:errorBuildInvalid` | 构建请求无效：{{detail}} | Invalid build request: {{detail}} | B2 | 已落盘 |
| `packs:errorBuildNoModels` | 构建圈选未匹配到模型：{{detail}} | Build selection matched no models: {{detail}} | B2 | 已落盘 |
| `packs:errorExportNotBuilt` | 整合包产物不存在（未构建/未安装）：{{id}} | Pack artifact not found: {{id}} | B2 | 已落盘 |
| `packs:errorUploadNoFile` | 上传缺少 file 文件字段 | Upload missing 'file' field | B2 | 已落盘 |
| `packs:importDone` | 整合包导入完成：{{models}} 个模型落位、{{downloads}} 个待下载、{{pipelines}} 条管线 | Pack imported: {{models}} placed, {{downloads}} pending, {{pipelines}} pipeline(s) | B2 | 已落盘 |
| `packs:buildDone` | 整合包构建完成（{{files}} 个文件） | Pack built ({{files}} file(s)) | B2 | 已落盘 |

> ⚠️ C8 归并注意：B1/B2 各提了 `errorAlreadyInstalled`/`errorModelConflict`/`errorBundleMissing` 同名键（参数与措辞略有差异）——落盘时各取其一（建议 B2 版：API 层实际消费方），并确认双方代码引用兼容。

### B3（apiPipelines 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `apiPipelines:vramBudget.specEmpty` | VRAM 预算请求缺少节点 | VRAM budget request has no nodes | B3 | 已落盘 |
| `apiPipelines:vramBudget.cycleDetected` | 管线存在环，无法计算 VRAM 预算 | Pipeline contains a cycle; VRAM budget cannot be computed | B3 | 已落盘 |
| `apiPipelines:tasks.cancelAlreadyTerminal` | 任务已终结（{{status}}），无法取消 | Task already terminal ({{status}}), cannot cancel | B3 | 已落盘 |
| `apiPipelines:execute.capabilityNotFound` | 模块 {{moduleId}} 不存在能力 {{capability}} | Module {{moduleId}} has no capability {{capability}} | B3 | 已落盘 |
| `apiPipelines:execute.inputMissing` | 输入文件不存在: {{path}} | Input file does not exist: {{path}} | B3 | 已落盘 |
| `apiPipelines:execute.moduleStartFailed` | 模块自动拉起失败: {{detail}} | Failed to auto-start module: {{detail}} | B3 | 已落盘 |

## 待落盘-迟到（C8 首轮回填后到达，门禁期批量补）

### C2（components 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `components:pipeline.builtin.llm.label` | LLM（OpenAI 兼容） | LLM (OpenAI-compatible) | C2 | 已落盘 |
| `components:pipeline.builtin.llm.description` | 翻译 / 摘要 / 润色（chat/completions） | Translate / summarize / refine text via chat/completions | C2 | 已落盘 |
| `components:pipeline.param.baseUrl` | 接口地址 (base_url) | Endpoint (base_url) | C2 | 已落盘 |
| `components:pipeline.param.llmModel` | 模型名称 (model) | Model name (model) | C2 | 已落盘 |
| `components:pipeline.param.apiKeyEnv` | API Key 环境变量名 | API key environment variable name | C2 | 已落盘 |
| `components:pipeline.param.apiKeyEnv.hint` | 只填环境变量名，切勿填写密钥本身；执行时从环境变量读取，绝不收集或落盘明文密钥 | Env var name only — never the secret itself; the key is read from the environment at runtime and never collected or stored | C2 | 已落盘 |
| `components:pipeline.param.systemPrompt` | 系统提示词 | System prompt | C2 | 已落盘 |
| `components:pipeline.param.systemPrompt.placeholder` | 把 {input} 翻译成中文（{input} 占位符引用上游输入） | Translate {input} into Chinese ({input} placeholder references upstream input) | C2 | 已落盘 |
| `components:pipeline.param.temperature` | 温度 (temperature) | Temperature | C2 | 已落盘 |
| `components:pipeline.param.outputFormat` | 输出格式 | Output format | C2 | 已落盘 |
| `components:pipeline.param.args.hintArray` | 每行一个参数（数组）；{input}/{output} 为占位符 | One argument per entry (array); {input}/{output} are placeholders | C2 | 已落盘 |
| `components:pipeline.param.args.itemPlaceholder` | 单个参数，如 -c:v libx264 | Single argument, e.g. -c:v libx264 | C2 | 已落盘 |
| `components:pipeline.param.args.empty` | 尚无参数，点击下方按钮逐条添加 | No arguments yet; add them one by one with the button below | C2 | 已落盘 |
| `components:pipeline.param.args.add` | 添加参数 | Add argument | C2 | 已落盘 |
| `components:pipeline.param.args.removeAria` | 删除该参数 | Remove this argument | C2 | 已落盘 |
| `components:pipeline.param.outputExtension` | 输出扩展名 (output_extension) | Output extension (output_extension) | C2 | 已落盘 |
| `components:pipeline.param.outputExtension.hint` | 本节点输出产物的扩展名 | Extension of this node's output artifact | C2 | 已落盘 |
| `components:pipeline.module.noCapabilities` | 该模块未声明任何能力（manifest capabilities 缺失） | This module declares no capabilities (manifest capabilities missing) | C2 | 已落盘 |
| `components:pipeline.module.noCapabilitySelected` | 未选择能力 | No capability selected | C2 | 已落盘 |
| `components:pipeline.module.capabilityLabel` | 能力 | Capability | C2 | 已落盘 |
| `components:pipeline.module.pickCapability` | 选择能力 | Pick a capability | C2 | 已落盘 |
| `components:pipeline.module.variantPin` | 变体 pin (model) | Variant pin (model) | C2 | 已落盘 |
| `components:pipeline.module.variantPin.hint` | 缺省跟随激活变体；执行前校验 pin 与激活是否一致 | Defaults to the active variant; pin vs. active consistency is validated before execution | C2 | 已落盘 |
| `components:pipeline.module.variantFollowActive` | 跟随激活变体 | Follow active variant | C2 | 已落盘 |
| `components:pipeline.module.deviceAuto` | auto（调度器自动分配） | auto (scheduler assigns) | C2 | 已落盘 |
| `components:pipeline.module.deviceUnknown` | 本机未检测到该设备；执行时将警告并回退 auto（软约束，不阻断） | Device not detected locally; execution will warn and fall back to auto (soft constraint, non-blocking) | C2 | 已落盘 |
| `components:pipelineSidebar.noCapabilities` | 未声明能力 | No capabilities declared | C2 | 已落盘 |

### C1（约 100 键，文案以源码 defaultValue 为准，en 由 C8 翻译）

- `common:status.queued` | 排队中 | Queued
- `models:*` 约 55 键：source.pack / module.{startStarted,startFailed,stopSucceeded,stopFailed,stopConfirmTitle,stopConfirmDescription,logs,detail,run} / card.{activeVariant,selectVariant,assumedActive,assumedActiveHint,serviceFallback,vramEstimate,fromPack,tagFilterHint,activeMissingHint} / tags.{action,addFirst,editTitle,editDescription,empty,placeholder,add,saved,saveFailed} / filter.{clear,empty,emptyDescription} / variant.{switchSuccess,switchFailed,needsDownload,downloadNow,needsRestart,restartNow} / download.{cancelRequested,cancelFailed} / detail.{expand,drawerSuffix,capabilities,noCapabilities,paramName,paramType,paramDefault,paramConstraint} / logs.{title,description} / run.{title,description,noCapabilities,capability,selectCapability,params,input,inputPlaceholder,upload,uploadDone,uploadFailed,inputHint,submit,startingModule,startingModuleHint,accepted,submitFailed,submitTimeout,submitTimeoutDesc,nodeInput,nodeRun,nodeOutput,progressHint,artifacts,preview,previewFailed,previewBinary}
- `packs:*` 约 45 键：page/empty（S2 已提）/ action.{import,build,export,uninstall} / toast.{importCompleted,buildCompleted,buildAccepted,buildFailed,importFailed,accepted,uploadCancelled,failed,uninstalled,uninstallFailed} / stage.{accepted,extracting,verifying,manifest,models,pipelines,registering,done,build} / card.{modelsCount,pipelinesCount} / import.{title,description,tabLocal,tabUpload,localPlaceholder,urlHint,pickFile,uploading,startUpload} / build.{title,description,identity,idPlaceholder,namePlaceholder,versionPlaceholder,descriptionPlaceholder,tagSelect,models,noCandidates,bundle,bundleHint,pipelines,noPipelines,submit,invalidId,invalidIdHint} / uninstall.{title,description,keepModels} / detail.{installedAt,contents,noModels,modeHint,adaptation,noAdaptation}

> 提取来源：`frontend/src/pages/models.tsx` 与 `pages/packs.tsx` 中全部 `defaultValue` 用法（C1 已定稿 zh 文案）。状态：已落盘（C8-R2：models 76 键 + packs 60 键 + common.status.queued；`run.nodeInput/nodeRun/nodeOutput` 无 defaultValue，zh 由 C8 拟写=输入/运行/输出；`stage.*` 为动态键，zh 参照 C4 desktopApp:packs.stage.* 口径）。

### C7（settings + apiCore 命名空间）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `settings:toast.savedRestart` | 配置已保存，部分改动需重启服务生效 | Settings saved — restart required for some changes | C7 | 已落盘 |
| `settings:toast.savedRestartDescription` | 本次改动包含重启敏感项（监听地址/端口、端口范围、工作区目录、CUDA 库目录、日志级别、代理等），需重启 daemon 后生效 | This change touches restart-sensitive items (listen host/port, port range, workspace dir, CUDA libs dir, log level, proxy) — restart the daemon to apply. | C7 | 已落盘 |
| `settings:general.logLevelDescription` | daemon 日志输出级别。保存后需重启生效；当前 daemon 启动时读取 RUST_LOG 环境变量，本项后端接线待完成 | Daemon log level. Requires restart; the daemon currently reads RUST_LOG at startup — backend wiring pending. | C7 | 已落盘 |
| `settings:general.checkUpdatesPending` | 启动时自动检测新版本并发出提示（daemon 更新检查后端尚未接线，开关已保存但暂不生效） | Check for updates at startup (backend not wired yet — saved but inactive for now). | C7 | 已落盘 |
| `settings:compute.cudaLibsDir` | 共享 CUDA 库目录 | Shared CUDA libraries directory | C7 | 已落盘 |
| `settings:compute.cudaLibsDirDescription` | 启动模块时注入的共享 CUDA 库路径（Linux LD_LIBRARY_PATH / Windows PATH 前置），相对应用根目录 | Shared CUDA libs path injected at module start (LD_LIBRARY_PATH on Linux / PATH prepend on Windows), relative to app root | C7 | 已落盘 |
| `settings:python.uvCacheDir` | uv 缓存目录 | uv cache directory | C7 | 已落盘 |
| `settings:python.uvCacheDirDescription` | uv 依赖缓存目录（与 venv 同盘时硬链接去重），相对应用根目录 | uv dependency cache dir (hardlink dedup when same volume as venvs), relative to app root | C7 | 已落盘 |
| `settings:python.constraints` | 全局 constraints 文件 | Global constraints file | C7 | 已落盘 |
| `settings:python.constraintsDescription` | 锁定依赖版本的全局 constraints 文件（如 torch 全家桶统一版本）；留空 = 停用 | Global constraints file pinning dependency versions (e.g. unified torch stack); empty = disabled | C7 | 已落盘 |
| `settings:packs.title` | 整合包 | Model packs | C7 | 已落盘 |
| `settings:packs.description` | 模型整合包导入相关配置 | Settings for model pack import | C7 | 已落盘 |
| `settings:packs.stagingDir` | 导入暂存目录 | Import staging directory | C7 | 已落盘 |
| `settings:packs.stagingDirDescription` | 整合包导入解包与校验的隔离暂存区，相对应用根目录 | Isolated staging area for pack extraction and verification, relative to app root | C7 | 已落盘 |
| `settings:advanced.title` | 高级 | Advanced | C7 | 已落盘 |
| `settings:advanced.description` | 面向进阶用户的配置，变更前请确认理解其作用 | Advanced settings; make sure you understand each item before changing | C7 | 已落盘 |
| `settings:advanced.activeModels` | 激活模型变体（active_models） | Active model variants (active_models) | C7 | 已落盘 |
| `settings:advanced.activeModelsHint` | 每模块同一时间一个激活变体（单槽位）。已有键只读、可新增；受合并语义限制，删除键请手动编辑 config/app.toml | One active variant per module (single slot). Existing keys are read-only; to remove a key edit config/app.toml manually | C7 | 已落盘 |
| `settings:advanced.empty` | 暂无激活变体覆盖（各模块使用清单默认变体） | No active-variant overrides (modules use manifest defaults) | C7 | 已落盘 |
| `settings:advanced.keyReadOnly` | 已有键只读（合并语义不支持删键/改键） | Existing keys are read-only (merge semantics cannot delete/rename keys) | C7 | 已落盘 |
| `settings:advanced.addEntry` | 新增映射 | Add mapping | C7 | 已落盘 |
| `settings:advanced.moduleIdPlaceholder` | 模块 ID | Module ID | C7 | 已落盘 |
| `settings:advanced.modelIdPlaceholder` | 模型 ID | Model ID | C7 | 已落盘 |
| `settings:pipeline.keepWorkspaceNote` | 实验性预留：任务结束后的工作区清理尚未实现，本开关当前不影响行为 | Experimental: post-task workspace cleanup is not implemented yet; this toggle has no effect | C7 | 已落盘 |
| `apiCore:config.invalidPatch` | 配置补丁无效：{{detail}} | Invalid config patch: {{detail}} | C7 | 已落盘 |

> C7 另注：`settings.toast.savedRestartNote` 已无代码引用，落盘时可跳过/删除。

### C4（desktopApp 命名空间，约 30 键）

| 键 | zh-CN | en | 提交方 | 状态 |
|---|---|---|---|---|
| `desktopApp:info.venvPreparing` | 正在为模块 {{id}} 准备 Python 环境（首次可能需数分钟） | Preparing Python environment for {{id}} (may take minutes on first run) | C4 | 已落盘 |
| `desktopApp:info.packImportDone` | 整合包导入完成：{{models}} 个模型落位，{{downloads}} 个开始下载，{{pipelines}} 条管线注册 | Pack import completed: {{models}} model(s) installed, {{downloads}} download(s) started, {{pipelines}} pipeline(s) registered | C4 | 已落盘 |
| `desktopApp:info.taskSubmitted` | 管线任务已提交：{{id}} | Pipeline task submitted: {{id}} | C4 | 已落盘 |
| `desktopApp:toast.directExecSubmitted` | 直跑任务已提交：{{task}} | Direct exec task submitted: {{task}} | C4 | 已落盘 |
| `desktopApp:error.venvPrepFailed` | Python 环境准备失败：{{detail}} | Failed to prepare Python environment: {{detail}} | C4 | 已落盘 |
| `desktopApp:error.noCompatibleDevice` | 模块 {{id}} 无兼容计算设备 | No compatible device for module {{id}} | C4 | 已落盘 |
| `desktopApp:error.pipelineInvalid` | 管线校验失败：{{detail}} | Pipeline validation failed: {{detail}} | C4 | 已落盘 |
| `desktopApp:error.moduleAutoStartFailed` | 模块 {{id}} 自动拉起失败：{{detail}} | Failed to auto-start module {{id}}: {{detail}} | C4 | 已落盘 |
| `desktopApp:error.taskSubmitFailed` | 任务提交失败：{{detail}} | Task submission failed: {{detail}} | C4 | 已落盘 |
| `desktopApp:error.taskNotFound` | 任务 {{id}} 不存在 | Task {{id}} not found | C4 | 已落盘 |
| `desktopApp:error.packListFailed` | 读取整合包注册表失败：{{detail}} | Failed to read pack registry: {{detail}} | C4 | 已落盘 |
| `desktopApp:error.packImportFailed` | 整合包导入失败：{{detail}} | Pack import failed: {{detail}} | C4 | 已落盘 |
| `desktopApp:error.capabilityNotFound` | 模块 {{module}} 没有能力 {{capability}} | Module {{module}} has no capability {{capability}} | C4 | 已落盘 |
| `desktopApp:error.inputFileMissing` | 输入文件不存在：{{path}} | Input file does not exist: {{path}} | C4 | 已落盘 |
| `desktopApp:error.paramInvalid` | 直跑参数无效：{{detail}} | Invalid direct-exec parameters: {{detail}} | C4 | 已落盘 |
| `desktopApp:packs.title` | 整合包管理 | Pack Manager | C4 | 已落盘 |
| `desktopApp:packs.refresh` | 刷新 | Refresh | C4 | 已落盘 |
| `desktopApp:packs.refreshTip` | 重新读取已装包注册表 | Re-read installed pack registry | C4 | 已落盘 |
| `desktopApp:packs.import` | 导入整合包 | Import Pack | C4 | 已落盘 |
| `desktopApp:packs.importing` | 导入中 | Importing | C4 | 已落盘 |
| `desktopApp:packs.emptyTitle` | 尚未安装整合包 | No packs installed | C4 | 已落盘 |
| `desktopApp:packs.emptyHint` | 点击右上角「导入整合包」选择 .epzip 包文件导入 | Click "Import Pack" (top right) and choose a .epzip file | C4 | 已落盘 |
| `desktopApp:packs.stage.extracting` | 解包中 | Extracting | C4 | 已落盘 |
| `desktopApp:packs.stage.verifying` | 校验校验和 | Verifying checksums | C4 | 已落盘 |
| `desktopApp:packs.stage.manifest` | 校验清单 | Validating manifest | C4 | 已落盘 |
| `desktopApp:packs.stage.models` | 处理模型 | Processing models | C4 | 已落盘 |
| `desktopApp:packs.stage.pipelines` | 注册管线 | Registering pipelines | C4 | 已落盘 |
| `desktopApp:packs.stage.registering` | 写入注册表 | Registering pack | C4 | 已落盘 |

> C4 另注：`desktopApp:nav.packs` 与 `toast.packImportComplete/Failed` 已由 S2 提且 C8 首轮已落盘，无需重复。

### C3（pipeline 命名空间，约 70 键，文案以 pipeline.tsx defaultValue 为准，en 由 C8 翻译）

- `pipeline:vram.*`（19 键）：title、refresh、closeAria、toggleTitle、error、empty、pending、over、unknownCapacity、usageLine、adviceVariant、adviceDevice、adviceStop、unassignedTitle、unassignedHint、blocked、overcommitAllowed、overcommitDenied、blockedToast、blockedReason
- `pipeline:nodePanel.*`（5 键）：bindingTitle、activeVariant、activeVariantDefault、pinMismatchHint、pinUnknownVariantHint
- `pipeline:pinCheck.*`（16 键）：title、description、pinnedTo、activeIs、resolved、switch、switching、allResolved、invalidPin、unknownVariant、goModels、needsDownload、needsRestart、switched、switchFailed
- `pipeline:ptasks.*`（13 键）：title、shortLabel、description、statusQueued、queuePosition、progress、timeDetail、artifacts、artifactsEmpty、artifactsFailed、loadFailed、empty
- `pipeline:io.*`（20 键）：exportHint、importHint、exportEmpty、exportSuccess、exportStats、exportFailed、invalidFile、importParseFailed、importTitle、importDescription、importStats、issuesTitle、issuesHint、missingModule、missingVariant、invalidPin、builtinSkipped、register、registering、registerSuccess、registerFailed
- `pipeline:execute.*`（5 键）：advancedTitle、wait、waitHint、callbackUrl、callbackHint；`pipeline:exec.*`（4 键）：pollLost、pollLostHint、waitDone、waitTerminal；`pipeline:template.*`（2 键）：exampleDenoiseNode、exampleAsrNodeFw

> 提取来源：`frontend/src/pages/pipeline.tsx` 全部 defaultValue 用法（zh 已定稿）。状态：已落盘（C8-R2：共 84 键；`ptasks.statusQueued` 取自状态徽章 fallback；`template.exampleDenoiseNode/exampleAsrNodeFw` 为 tp() 调用形态）。

### C5（desktopPages/desktopApp/common 命名空间，en 由 C8 翻译）

- `desktopPages:models.*`（38 键，zh 文案以 ep-desktop/src/pages/models.rs 的 trfb 兜底为准）：filter.label / filter.clear / active / vram / run / runTip / config / snapshotMissing / logs / logs.staleHint / tags.editTip / tags.title / tags.noMeta / tags.removeTip / tags.inputHint / tags.add / tags.hint / tags.saved / tags.saveFailed / run.title / run.noManifest / run.noCapability / run.capability / run.browse / run.pickFile / run.inputHint / run.submit / run.inputMissing / run.submitted / config.title / config.noVariants / config.apply / config.saved / config.saveFailed / config.current / service.title / service.native / service.noModel
- `desktopApp:pipeline.*` + `desktopApp:palette.*`（53 键，以 pipeline_editor.rs trfb 兜底为准）：emptyHintEdit / browse / openTitle / new / newTip / saveToml2 / saveTip / run / runTip / refreshTasks / refreshTasksTip / palette.addTip / palette.llmTip / palette.noModules / helpTextEdit / props / edgeSelected / deleteEdge / paramLabel2 / timeoutEdit / retryEdit / builtinNoParams / manifestMissing / modelPin / followActive / deviceBind / addArg / argsTip / apiKeyTip / promptTip / temperature / maxTokens / deleteNode / vram.title / vram.over / vram.unknownCap / vram.unassigned / vram.schedulerNote / vram.noDevices / vram.suggestion / vram.overcommitOff / vram.cycle / connSelf / connDup / connCycle / connType / connOk / pickInput / saveTitle / saved / saveFailed / saveSerializeFailed / execPendingWire（保留，见下注）
- `desktopApp:tasks.*`（5 键）：queuePosition / cancelPending（保留，见下注） / artifacts.title / artifacts.dirMissing / artifacts.openDir
- `common:*`（3 键）：status.queued（与 C1 归并取一份） / label.name2 / label.description2

> 旧键清理（C5 报告）：`desktopApp.pipeline.saveToml`/`saveTomlTip`/`saveNotImplemented` 已被 saveToml2/saveTip 取代——C8-R2 grep 确认全仓库零引用，**已删除**。
> ⚠️ C8-R2 执行偏差（grep 优先原则）：`desktopApp.pipeline.execPendingWire`（pipeline_editor.rs:2810/2819 仍在用）与 `desktopApp.tasks.cancelPending`（tasks.rs:260 仍在用）在 master 合并态**仍有代码引用**，故未删除、已落盘；C5 清单中的 `desktopApp.tasks.cancel` 与 `desktopApp.pipeline.execSubmitted/execChannelClosed` 在 master 代码中**无任何引用**，未落盘。若后续门禁接线改用这些新键名，请补提键需求。
> 状态：已落盘（C8-R2：desktopPages 38 + desktopApp 58 + common 2 新增；common.status.queued 与 C1 归并只落一份）。
