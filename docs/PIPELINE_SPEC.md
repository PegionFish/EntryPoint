# 管线规范 (Pipeline Specification)

> ⚠️ **Sunset 横幅（2026-08-13）**：本文档所述 **ep-desktop 桌面端已于 2026-08-13 退役**，WebUI 为唯一 UI（server 形态交付）。本页保留为历史记录，不再维护；详见 [DESKTOP_SUNSET_PLAN.md](DESKTOP_SUNSET_PLAN.md)。

> 版本：1.1 | 适用于 EntryPoint v0.x
>
> v1.1 变更：新增「节点开发指南」章节（§11，决策 5：module/builtin 两条路径全面文档化）；
> `external_api` 节点更名 `llm`（builtin，旧名保留为别名，§5.8/§11.4，决策 4）；
> `[pipeline] max_instances` 管线级并发上限（§2.2）；§5 内置节点参考对齐实现
> （file_input/file_output/ffmpeg 参数修正，未实现节点显式标注）。

本文档定义管线（Pipeline）的 TOML 文件格式、执行语义、内置节点参考和类型系统。

---

## 1. 概述

管线是一个 **有向无环图 (DAG)**，将多个处理节点串联/并联为自动化工作流。

典型场景：
```
视频文件 → 提取音频 → AI 降噪 → 语音识别 → LLM 翻译 → 导出 SRT 字幕
```

管线定义以 TOML 文件存储在 `config/pipelines/` 目录下。

---

## 2. 文件格式

### 2.1 顶层结构

```toml
[pipeline]
id = "video-to-srt"              # 唯一标识（kebab-case）
name = "视频转字幕"               # 显示名称
description = "提取音频 → 降噪 → ASR → 翻译 → SRT"
version = "1.0"                   # 管线版本（可选）

[[nodes]]
# ... 节点定义（见 §3）

[[edges]]
# ... 边定义（见 §4）
```

### 2.2 `[pipeline]` 字段

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `id` | string | ✅ | 唯一标识，用于文件名和任务关联 |
| `name` | string | ✅ | 显示名称 |
| `description` | string | ❌ | 描述 |
| `version` | string | ❌ | 管线格式版本 |
| `default_params` | table | ❌ | 全局默认参数（可被节点覆盖） |
| `max_instances` | u32 | ❌ | 管线级并发上限（缺省跟随全局 `[pipeline] max_parallel`；GPU 重管线可锁 `1` 防显存打架，见 §11.6） |

---

## 3. 节点定义 (`[[nodes]]`)

### 3.1 通用字段

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `id` | string | ✅ | 节点唯一标识（管线内唯一，kebab-case） |
| `kind` | enum | ✅ | `"module"` \| `"builtin"` \| `"external_api"` |
| `label` | string | ❌ | 显示标签（默认用 id） |
| `params` | table | ❌ | 节点参数 |
| `position` | [x, y] | ❌ | 编辑器画布位置（仅 UI 使用） |
| `timeout_secs` | u32 | ❌ | 节点超时（默认 600） |
| `retry_count` | u32 | ❌ | 失败重试次数（默认 0） |
| `condition` | string | ❌ | 条件表达式（满足时才执行，见 §6） |

### 3.2 kind = "module"

调用已安装模块的 capability。

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `module_id` | string | ✅ | 目标模块 ID |
| `capability` | string | ✅ | 调用的能力名 |
| `model_id` | string | ❌ | 指定使用的模型（默认用模块当前选中模型） |
| `device` | string | ❌ | 指定计算设备（默认用模块分配的设备） |

```toml
[[nodes]]
id = "asr"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
label = "语音识别"
params = { language = "auto", timestamps = true, beam_size = 5 }
timeout_secs = 300
```

### 3.3 kind = "builtin"

内置工具节点（无需模块，由 EntryPoint 直接执行）。

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `builtin` | string | ✅ | 内置节点类型（见 §5） |

```toml
[[nodes]]
id = "extract-audio"
kind = "builtin"
builtin = "ffmpeg"
label = "提取音频"
params = { args = "-i {input} -vn -acodec pcm_s16le -ar 16000 -ac 1 {output}" }
```

### 3.4 kind = "external_api"（遗留形状，规范名 `llm`）

> **状态（决策 4）**：该节点种类已改造为 builtin `llm` 节点（§11.4），功能限定为
> 接入 **OpenAI 兼容 LLM 端点**（chat/completions 单一形状）。`kind = "external_api"`
> 与 `kind = "llm"` 仍被解析器接受为**别名**（kind 级 `endpoint`/`api_key_env`
> 并入 params 同名字段），新管线一律写 `kind = "builtin"` + `builtin = "llm"`。
> 参数表见 §11.4，此处保留旧示例仅供存量文件对照。

调用外部 HTTP API（如 LLM 翻译服务）。

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `endpoint` | string | ✅ | API 基础 URL |
| `api_type` | string | ❌ | `"openai"` \| `"custom"`（默认 `"openai"`） |
| `api_key_env` | string | ❌ | API Key 环境变量名（如 `"SILICONFLOW_API_KEY"`） |

```toml
[[nodes]]
id = "translate"
kind = "external_api"
label = "LLM 翻译"
endpoint = "https://api.siliconflow.cn/v1"
api_type = "openai"
api_key_env = "SILICONFLOW_API_KEY"
params = {
    model = "deepseek-ai/DeepSeek-V3",
    target_lang = "zh",
    system_prompt = "你是专业字幕翻译，保持时间戳格式不变。",
    max_tokens = 4096,
    temperature = 0.3
}
```

---

## 4. 边定义 (`[[edges]]`)

边连接两个节点的端口，定义数据流向。

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `from` | [node_id, port] | ✅ | 源节点和输出端口 |
| `to` | [node_id, port] | ✅ | 目标节点和输入端口 |

```toml
[[edges]]
from = ["extract-audio", "output"]
to = ["denoise", "input"]
```

### 4.1 端口命名

| 端口 | 说明 |
|---|---|
| `"input"` | 默认输入端口 |
| `"output"` | 默认输出端口 |
| 自定义名称 | 多输入/多输出节点使用（如 ffmpeg 的 `"video_out"`、`"audio_out"`） |

### 4.2 连线规则

- 一个输出端口可连接多个输入端口（扇出/一对多）
- 一个输入端口只能连接一个输出端口
- 不允许形成环（DAG 约束）
- 连线两端的数据类型必须兼容（见 §7 类型系统）

---

## 5. 内置节点参考

> **实现状态**：当前执行器实现的 builtin 节点为 `file_input` / `file_output` /
> `ffmpeg` / `llm`（§5.8）四个；其余（`srt_export` / `text_concat` /
> `json_transform` / `delay`）为规范预留形状，**尚未实现**——加载含这些节点的
> 管线会在执行时报 `unknown builtin node type`。字幕导出等需求请用模块节点的
> 产物协议替代（MODULE_SPEC.md §5，`params.output_format = "srt"`）。

### 5.1 `file_input` — 文件输入源

管线的起始节点，接收用户选择的文件。

```toml
[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { path = "C:/Videos/input.mp4" }
```

| 参数 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `path` | string | ✅ | 服务器本地输入文件路径；`POST /api/pipelines/execute` 的 `inputs` 可按节点覆盖（见 AUTOMATION.md） |

输出端口：`output`（文件路径）

### 5.2 `file_output` — 文件输出

管线的终止节点，将结果保存到指定位置。

```toml
[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"
params = { extension = "srt" }
```

| 参数 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `path` | string | ❌ | 输出文件完整路径（显式指定时原样使用） |
| `extension` | string | ❌ | 缺省 `path` 时按 `<work_dir>/<node_id>_output.<extension>` 派生（默认 `out`）；产物归集自动收录 |

输入端口：`input`

### 5.3 `ffmpeg` — FFmpeg 命令

执行任意 FFmpeg 命令。

```toml
[[nodes]]
id = "extract"
kind = "builtin"
builtin = "ffmpeg"
params = { args = ["-i", "{input}", "-vn", "-acodec", "pcm_s16le", "-ar", "16000", "-ac", "1", "{output}"], output_extension = "wav" }
```

| 参数 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `args` | string[] | ✅ | FFmpeg 参数模板（**契约形状为数组**；历史字符串形状按 shell 词法拆分兼容，见 §8.4 警告语义） |
| `output` | string | ❌ | 显式输出文件完整路径（优先于派生路径） |
| `output_extension` | string | ❌ | 派生输出路径的扩展名（ffmpeg 依扩展名推断容器格式，shipped 管线均依赖此参数） |

**模板变量：**
- `{input}` — 输入文件路径（由上游边提供）；出现后 args 视为已自行声明输入
- `{output}` — 输出文件路径（`output` 参数或派生路径）；出现后不再在末尾追加输出参数
- 两个占位符均缺省 → 向后兼容旧行为（上游文件前置为输入，输出追加到末尾）

输入端口：`input`
输出端口：`output`

### 5.4 `srt_export` — SRT 字幕导出（未实现，预留）

将带时间戳的文本/JSON 转换为 SRT 字幕文件。

```toml
[[nodes]]
id = "srt"
kind = "builtin"
builtin = "srt_export"
params = { max_chars_per_line = 42, max_lines = 2 }
```

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `max_chars_per_line` | u32 | 42 | 每行最大字符数 |
| `max_lines` | u32 | 2 | 每条字幕最大行数 |
| `offset_ms` | i32 | 0 | 时间偏移（毫秒） |
| `encoding` | string | `"utf-8"` | 输出编码 |

输入端口：`input`（JSON segments 或带时间戳文本）
输出端口：`output`（.srt 文件路径）

**输入格式（JSON segments）：**
```json
{
  "segments": [
    {"start": 0.0, "end": 2.5, "text": "你好世界"},
    {"start": 2.5, "end": 5.0, "text": "这是测试"}
  ]
}
```

### 5.5 `text_concat` — 文本拼接（未实现，预留）

合并多个文本输入。

```toml
[[nodes]]
id = "merge"
kind = "builtin"
builtin = "text_concat"
params = { separator = "\n\n" }
```

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `separator` | string | `"\n"` | 分隔符 |

输入端口：`input`（可接收多条边）
输出端口：`output`

### 5.6 `json_transform` — JSON 变换（未实现，预留）

使用 JSONPath 或简单模板提取/变换 JSON 数据。

```toml
[[nodes]]
id = "extract-text"
kind = "builtin"
builtin = "json_transform"
params = { extract = "$.segments[*].text", join_with = "\n" }
```

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `extract` | string | — | JSONPath 表达式 |
| `join_with` | string | — | 数组元素连接符 |
| `template` | string | — | 输出模板（`{value}` 占位） |

### 5.7 `delay` — 延迟/节流（未实现，预留）

在节点间插入等待（用于 API 限流）。

```toml
[[nodes]]
id = "throttle"
kind = "builtin"
builtin = "delay"
params = { seconds = 2 }
```

### 5.8 `llm` — OpenAI 兼容 LLM 调用

接入 OpenAI 兼容 chat/completions 端点（翻译、摘要、字幕润色等文本生成）。
`external_api` 为可执行别名，两者完全等价。**完整参数表与错误语义见 §11.4。**

```toml
[[nodes]]
id = "translate"
kind = "builtin"
builtin = "llm"
label = "LLM 翻译"
params = { base_url = "https://api.openai.com/v1", model = "gpt-4o-mini", api_key_env = "OPENAI_API_KEY", system_prompt = "将以下字幕翻译为中文：{input}", output_format = "text" }
timeout_secs = 120
```

输入端口：`input`（text）
输出端口：`output`（text）

---

## 6. 条件执行（可选）

节点可设置 `condition` 字段，仅当条件满足时执行：

```toml
[[nodes]]
id = "translate"
kind = "external_api"
condition = "input.language != 'zh'"   # 仅非中文时翻译
```

条件表达式支持：
- 比较：`==`、`!=`、`>`、`<`
- 逻辑：`&&`、`||`、`!`
- 变量：`input.<field>`（上游输出的字段）

> 条件执行为 v1.1 预留特性，初版可不实现。

---

## 7. 类型系统

### 7.1 数据类型

| 类型 | 说明 | 在节点间传递的内容 |
|---|---|---|
| `audio` | 音频文件 | 文件绝对路径 (string) |
| `video` | 视频文件 | 文件绝对路径 (string) |
| `image` | 图片文件 | 文件绝对路径 (string) |
| `text` | 纯文本 | 字符串 (string) |
| `json` | 结构化数据 | JSON 值 (object/array) |
| `file` | 任意文件 | 文件绝对路径 (string) |

### 7.2 类型兼容矩阵

连线时，源端口 output_type 必须与目标端口 input_type 兼容：

| 源 ↓ 目标 → | audio | video | image | text | json | file |
|---|---|---|---|---|---|---|
| **audio** | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ |
| **video** | ❌ | ✅ | ❌ | ❌ | ❌ | ✅ |
| **image** | ❌ | ❌ | ✅ | ❌ | ❌ | ✅ |
| **text** | ❌ | ❌ | ❌ | ✅ | ❌ | ✅ |
| **json** | ❌ | ❌ | ❌ | ✅* | ✅ | ✅ |
| **file** | ✅ | ✅ | ✅ | ❌ | ❌ | ✅ |

\* json → text：序列化为 JSON 字符串

### 7.3 隐式转换

- `json` → `text`：自动 JSON.stringify
- 文件类 → `file`：任何文件类型可流入 `file` 端口
- `file` → 具体文件类型：运行时检查扩展名

---

## 8. 执行语义

### 8.1 执行流程

```
1. 加载管线 TOML
2. 验证：
   - 所有节点 id 唯一
   - 所有边引用的节点/端口存在
   - 无环（DAG 检测）
   - 端口类型兼容
   - module 类节点：模块已安装、模型已下载
   - external_api 类节点：API Key 已配置
3. 拓扑排序 → 分层（同层节点无依赖关系）
4. 创建任务工作目录：workspace/<task-id>/
5. 按层执行：
   - 同层节点并行（tokio::spawn）
   - 每个节点等待所有上游完成
   - 执行成功 → 输出写入 workspace/<task-id>/<node-id>/
   - 执行失败 → 标记 Failed，下游标记 Skipped
6. 全部完成 → 任务状态 Completed
7. 清理（可选保留 workspace 供用户检查）
```

### 8.2 节点执行方式（按 kind）

| kind | 执行方式 |
|---|---|
| `module` (interface=http) | `POST http://localhost:{port}/predict/{capability}` |
| `module` (interface=cli) | 构建命令行，spawn 进程，等待退出 |
| `builtin` | 内部 Rust 函数直接执行 |
| `external_api` | `POST {endpoint}/chat/completions`（OpenAI 格式）或自定义 |

### 8.3 数据传递

- **文件类**：上游节点输出文件路径，下游通过 `input_path` 参数接收
- **文本/JSON 类**：上游节点输出值，下游通过 `input_text` / `input_json` 接收
- 所有中间文件存储在 `workspace/<task-id>/<node-id>/output.<ext>`

### 8.4 错误处理

| 情况 | 行为 |
|---|---|
| 节点超时 | 终止进程/取消请求，标记 Failed |
| 节点返回错误 | 标记 Failed，记录 error message |
| 节点进程崩溃 | 标记 Failed，记录 exit code + stderr |
| 上游失败 | 下游标记 Skipped（不执行） |
| retry_count > 0 | 失败后重试指定次数 |

### 8.5 并行与资源

- 同层节点并行执行，但受限于：
  - 同一模块的多个节点串行（避免并发访问同一服务）
  - 可配置全局最大并行数（`app.toml [pipeline].max_parallel`）
- 不同模块的节点可真正并行（各自独立进程/端口）

---

## 9. 完整示例

### 9.1 视频转字幕（带降噪 + 翻译）

```toml
# config/pipelines/video-to-srt.toml

[pipeline]
id = "video-to-srt"
name = "视频转字幕"
description = "视频 → 提取音频 → 降噪 → ASR → 翻译 → SRT"

# ── 节点 ──────────────────────────────────────────────────

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
label = "输入视频"
params = { accept = "video" }
position = [50, 200]

[[nodes]]
id = "extract"
kind = "builtin"
builtin = "ffmpeg"
label = "提取音频"
params = { args = "-i {input} -vn -acodec pcm_s16le -ar 16000 -ac 1 {output}" }
position = [250, 200]

[[nodes]]
id = "denoise"
kind = "module"
module_id = "deep-filter"
capability = "denoise"
label = "AI 降噪"
position = [450, 200]

[[nodes]]
id = "asr"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
label = "语音识别"
params = { language = "auto", timestamps = true, beam_size = 5 }
timeout_secs = 600
position = [650, 200]

[[nodes]]
id = "translate"
kind = "external_api"
label = "LLM 翻译"
endpoint = "https://api.siliconflow.cn/v1"
api_type = "openai"
api_key_env = "SILICONFLOW_API_KEY"
params = {
    model = "deepseek-ai/DeepSeek-V3",
    target_lang = "zh",
    system_prompt = "将以下英文字幕翻译为中文，保持 segments 格式不变，仅翻译 text 字段。",
    temperature = 0.3
}
timeout_secs = 120
position = [850, 200]

[[nodes]]
id = "srt"
kind = "builtin"
builtin = "srt_export"
label = "导出 SRT"
params = { max_chars_per_line = 42, max_lines = 2 }
position = [1050, 200]

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"
label = "保存文件"
params = { suffix = ".srt" }
position = [1250, 200]

# ── 边 ────────────────────────────────────────────────────

[[edges]]
from = ["input", "output"]
to = ["extract", "input"]

[[edges]]
from = ["extract", "output"]
to = ["denoise", "input"]

[[edges]]
from = ["denoise", "output"]
to = ["asr", "input"]

[[edges]]
from = ["asr", "output"]
to = ["translate", "input"]

[[edges]]
from = ["translate", "output"]
to = ["srt", "input"]

[[edges]]
from = ["srt", "output"]
to = ["save", "input"]
```

### 9.2 多模型 ASR 对比

```toml
[pipeline]
id = "asr-compare"
name = "ASR 模型对比"
description = "同一音频分别用 faster-whisper 和 qwen3-asr 识别，对比结果"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { accept = "audio" }

[[nodes]]
id = "asr-whisper"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
label = "Faster-Whisper"
params = { language = "auto", timestamps = true }

[[nodes]]
id = "asr-qwen"
kind = "module"
module_id = "qwen3-asr"
capability = "transcribe"
label = "Qwen3-ASR"
params = { language = "auto", timestamps = true }

[[nodes]]
id = "save-whisper"
kind = "builtin"
builtin = "file_output"
label = "Whisper 结果"
params = { suffix = ".json" }

[[nodes]]
id = "save-qwen"
kind = "builtin"
builtin = "file_output"
label = "Qwen 结果"
params = { suffix = ".json" }

# 扇出：同一输入 → 两个 ASR
[[edges]]
from = ["input", "output"]
to = ["asr-whisper", "input"]

[[edges]]
from = ["input", "output"]
to = ["asr-qwen", "input"]

[[edges]]
from = ["asr-whisper", "output"]
to = ["save-whisper", "input"]

[[edges]]
from = ["asr-qwen", "output"]
to = ["save-qwen", "input"]
```

### 9.3 音频转配音（ASR → 翻译 → TTS）

```toml
[pipeline]
id = "audio-dub"
name = "音频翻译配音"
description = "音频 → ASR → 翻译 → TTS 生成配音"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { accept = "audio" }

[[nodes]]
id = "asr"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
params = { language = "en", timestamps = true }

[[nodes]]
id = "translate"
kind = "external_api"
endpoint = "https://api.siliconflow.cn/v1"
api_key_env = "SILICONFLOW_API_KEY"
params = { model = "deepseek-ai/DeepSeek-V3", target_lang = "zh" }

[[nodes]]
id = "tts"
kind = "module"
module_id = "qwen3-tts"
capability = "synthesize"
label = "语音合成"
params = { voice = "default", speed = 1.0 }

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"
params = { suffix = ".wav" }

[[edges]]
from = ["input", "output"]
to = ["asr", "input"]

[[edges]]
from = ["asr", "output"]
to = ["translate", "input"]

[[edges]]
from = ["translate", "output"]
to = ["tts", "input"]

[[edges]]
from = ["tts", "output"]
to = ["save", "input"]
```

---

## 10. 管线验证规则

系统在加载/保存管线时执行以下验证：

| # | 规则 | 错误级别 |
|---|---|---|
| 1 | 所有节点 `id` 唯一 | Error |
| 2 | 边引用的节点 id 存在 | Error |
| 3 | 边引用的端口名有效 | Error |
| 4 | 无环（DAG） | Error |
| 5 | 至少一个 `file_input` 节点 | Error |
| 6 | 端口类型兼容（§7.2 矩阵） | Error |
| 7 | module 节点的 module_id 已安装 | Warning |
| 8 | module 节点的模型已下载 | Warning |
| 9 | external_api 节点的 API Key 已配置 | Warning |
| 10 | 无孤立节点（无边连接） | Warning |

Error = 阻止保存/运行；Warning = 允许保存但运行时可能失败。

---

## 11. 节点开发指南

> 本章是 PACK_UNIFY_PLAN §6.6 **决策 5**（builtin 注册表重构不做、全面文档化）
> 的落地文档：EntryPoint 中新增"管线节点"有两条路径——**module 节点**（扩展正路，
> 推荐）与 **builtin 节点**（引擎内建）。选择标准很简单：
>
> - 新能力是"调用某个模型/工具处理数据" → 写**模块**，零引擎改动；
> - 新能力需要引擎级数据流原语（新的产物类型、新的调度语义） → builtin，四处同改。

### 11.1 两条路径总览

| 维度 | module 节点（推荐） | builtin 节点 |
|---|---|---|
| 载体 | `modules/<module-id>/`（module.toml + adapter.py） | ep-core / ep-daemon / 前端源码 |
| 引擎改动 | **零**（能力声明驱动） | 四处清单（§11.3） |
| 进入编辑器方式 | 自动（`/api/modules` 返回 capabilities，节点库数据驱动渲染） | 手工注册（前端 BUILTIN_DEFS） |
| 分发 | 模块目录随整合包/仓库分发 | 随 EntryPoint 版本发布 |
| 典型例子 | faster-whisper `transcribe`、deep-filter `denoise` | `file_input` / `file_output` / `ffmpeg` / `llm` |

### 11.2 module 节点路径（扩展正路）

**新节点 = 新模块**：只要在 `modules/` 下建一个合规模块，它声明的每个 capability
都会自动成为管线编辑器里可拖拽、可连线、可执行的节点。全程不碰引擎代码。

步骤：

1. **建模块目录** `modules/<module-id>/`，命名规则与目录结构见 MODULE_SPEC.md §1；
2. **module.toml 声明 capability**（`[[interface.capabilities]]` +
   `[interface.capabilities.params]` 参数 schema，MODULE_SPEC.md §2.5）：

   ```toml
   [[interface.capabilities]]
   name = "transcribe"
   description = "语音转文字，支持词级时间戳"
   input_type = "audio"        # 决定编辑器端口类型校验（§7 类型系统）
   output_type = "json"
   max_file_size_mb = 2048

   [interface.capabilities.params]
   language = { type = "string", default = "auto", description = "语言代码或 auto" }
   beam_size = { type = "integer", default = 5, min = 1, max = 20 }
   ```

   - `input_type` / `output_type` 直接映射为节点的输入/输出端口数据类型，
     编辑器连线类型校验（§7.2 兼容矩阵）据此工作；
   - `params` schema（type/default/min/max/enum）由 UI **自动生成参数表单**
     （WebUI 节点参数面板、直跑抽屉、桌面端同款），无需写任何前端代码。
3. **adapter.py 实现 `/predict/<capability>`**（Python HTTP 模块；请求/响应契约见
   ADAPTER_API.md）：

   ```python
   @app.post("/predict/transcribe")
   async def predict_transcribe(file: UploadFile | None, params: str = Form("{}")):
       ...  # 读取 EP_MODEL_DIR / EP_DEVICE，调用底层模型
       return {"status": "completed", "output_type": "json", "result": {...}}
   ```

   产出文件产物（如 SRT）时遵循 MODULE_SPEC.md §5 产物协议（读取注入的
   `output_path`，返回 `output_type = "file"`）。
4. **重启 daemon（或刷新模块列表）**：`GET /api/modules` 的 ModuleResponse 会携带
   `capabilities`（manifest 原样序列化），编辑器节点库即出现新节点——可拖入画布、
   按类型连线、参数面板自动渲染、执行走统一执行器。

验证清单（本地自测，不依赖 UI）：

```bash
# 手动起模块（MODULE_SPEC.md §9 环境变量契约）
curl http://localhost:<EP_PORT>/health
curl -X POST http://localhost:<EP_PORT>/predict/<capability> \
  -F "file=@sample.wav" -F 'params={"language": "zh"}'
```

### 11.3 builtin 节点路径（四处清单）

builtin 节点由引擎直接执行（无模块进程）。当前实现为分派硬编码，**新增一个
builtin 节点必须同步修改以下四处**，缺一即断：

| # | 位置 | 文件 | 改动内容 |
|---|---|---|---|
| 1 | **执行层** | `crates/ep-core/src/pipeline/executor.rs` | `execute_builtin_node` 的 `match builtin` 增加分支 → 新增 `execute_builtin_xxx` 异步函数（消费 `upstream: &[Artifact]`，产物落 `work_dir`，返回 `Artifact`） |
| 2 | **校验层** | `crates/ep-core/src/pipeline/dag.rs` | 若新节点引入新端口/类型/结构规则，更新 `Pipeline::validate` 与端口类型兼容逻辑（§7/§10）；纯参数型节点可不动 |
| 3 | **前端定义** | `crates/ep-webui/frontend/src/components/shared/pipeline-node.tsx` | `BuiltinKind` 联合类型 + `BUILTIN_DEFS` 定义表（label/description/端口/参数 ParamSpec，文案走 i18n `components:pipeline.builtin.*`）+ `BUILTIN_LIST` 注册 |
| 4 | **桥接层** | `crates/ep-daemon/src/pipeline_bridge.rs` | 画布 spec ↔ `Pipeline` 双向转换（`spec_to_pipeline` / `pipeline_to_spec`）确认新 builtin 的 params/端口原样透传；桌面端（ep-desktop）直连 ep-core，不经此桥 |

注意事项：

- builtin 的错误文案若用户可见，走 i18n（`pipeline:error.*` / `pipeline:warn.*`
  命名空间，规则 8：键需求提交 C8/编排者落盘）；日志（tracing）永远英文字面量。
- builtin 注册表重构（数据驱动化）**本期不做**（决策 5）；本章即该决策的
  文档化交付。未来若重构，本节四处清单即重构验收基线。
- ffmpeg `args` 契约形状为数组；字符串输入走 shell 词法拆分兼容并发
  `pipeline:warn.ffmpegStringArgs` 警告（§5.3）。

### 11.4 `llm` builtin 节点参数表（决策 4）

规范形状：`kind = "builtin"` + `builtin = "llm"`；`external_api` 保留为可执行别名
（两种写法执行完全等价；遗留 kind 级 `endpoint`/`api_key_env` 字段并入 params
同名字段，kind 级非空值优先）。

| 参数 | 类型 | 必须 | 默认 | 说明 |
|---|---|---|---|---|
| `base_url` | string | ✅ | — | OpenAI 兼容端点（如 `https://api.openai.com/v1` 或本地服务；请求发往 `<base_url>/chat/completions`，尾部 `/` 自动剥除） |
| `model` | string | ✅ | — | 模型名（透传给端点） |
| `api_key_env` | string | ❌ | — | **环境变量名**（如 `OPENAI_API_KEY`），执行时读取；API Key 只从环境变量读取，**不落盘** |
| `system_prompt` | string | ❌ | — | 系统提示词；支持 `{input}` 占位符（替换为上游文本）。留空 → 仅发 user 消息 |
| `temperature` | float | ❌ | 端点默认 | 采样温度（越界/非法 → `pipeline:error.llmParamRange`） |
| `max_tokens` | integer | ❌ | 端点默认 | 最大生成 token 数 |
| `output_format` | enum | ❌ | `"text"` | `"text"` \| `"json"`；`json` 时响应必须是合法 JSON，否则报 `pipeline:error.llmInvalidJsonOutput` |

I/O 与执行语义：

- 输入端口类型 `text`（上游可接 ASR 的 json→text 转换或任意文本产物），
  输出端口类型 `text`；
- 上游为文件时必须是文本类扩展名（否则 `pipeline:error.llmInputNotText`）；
- 失败语义与模块节点一致：`retry_count` 重试 + `timeout_secs` 超时管辖（§3.1）；
- 错误文案全部走 i18n `pipeline:error.llm*` 键（缺 base_url/model、环境变量
  未设置/为空、非 2xx、响应缺 `choices[0].message.content` 等）。

### 11.5 节点 schema 扩展字段（§6.2 冻结契约）

module 节点在 §3.2 基础字段之上支持以下扩展（TOML/编辑器双侧一致）：

```toml
[[nodes]]
id = "asr"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
model = "ep.systran.faster-whisper@medium"   # 变体 pin（可选）
device = "cuda:0"                             # 设备软约束（可选）
params = { beam_size = 5 }
timeout_secs = 300                            # 节点超时（默认 600，同 §3.1）
retry_count = 1                               # 失败重试次数（默认 0，同 §3.1）
```

| 字段 | 语义 |
|---|---|
| `model` | **变体 pin**：两种形态均合法——裸变体 id（如 `"medium"`）或全限定 pin `<qualified_id>@<variant>`（如 `"ep.systran.faster-whisper@medium"`，§4.3 PACK_UNIFY_PLAN）。缺省 = 跟随该模块当前激活变体（`config/app.toml [active_models]` → manifest `default=true`）。执行前校验：pin 变体与激活变体不一致 → **报错 + 一键切换引导**，不做静默热切换（避免执行中重启模块的复杂交互） |
| `device` | **设备软约束**：`"auto"` \| `"cuda:0"` \| `"rocm:1"` \| `"openvino:GPU.0"` 等。导入/加载时本机无此设备 → 警告（`pipeline:warn.deviceFallback`）+ 回退 `auto`，**不硬失败** |
| `timeout_secs` | 节点级超时（秒），覆盖 `[pipeline].default_timeout_secs` |
| `retry_count` | 失败重试次数（0 = 不重试） |

#### `model` pin 双形态（后端兼容口径）

```toml
# 形态一：裸变体 id（旧管线常见，等价于"该模块的这个变体"）
model = "medium"

# 形态二：全限定 pin（新契约，含发布者/厂商/模型命名，跨机器无歧义）
model = "ep.systran.faster-whisper@medium"
```

- 后端解析按 `rsplit('@')` 取变体段：含 `@` → 取最后一段为变体；不含 `@` →
  整体即变体 id。VRAM 预算（`POST /api/pipelines/vram-budget`）、变体一致性
  校验、激活变体回退三条消费路径均按此口径兼容两种形态；
- 序列化回写不改动原形态（TOML 往返保留作者写法）；
- 新管线推荐全限定 pin：导入他人管线/整合包时，缺失模型提示
  （`pipeline:io.missingVariant`）能给出完整 qualified_id 以便直接去统一页下载；
- 语法非法的 pin（如含非法字符的 qualified 段）在导入/加载时报
  `pipeline:io.invalidPin`（Warning 级，不阻断注册）。

### 11.6 并发模型速览（与节点开发的关系）

- 提交执行后任务可能进入 `queued` 状态：等待全局 `[pipeline] max_parallel`
  闸门或管线级 `max_instances`（§2.2）空位；
- 同一模块的多个节点串行执行（避免并发访问同一服务）——module 节点作者
  **无需**在 adapter 内做并发防护；
- 并发管线 pin 同模块不同变体时，后到任务在模块节点前显式报错（§11.5
  `model` pin 语义），不静默重启模块。
