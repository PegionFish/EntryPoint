# 管线规范 (Pipeline Specification)

> 版本：1.0 | 适用于 EntryPoint v0.x

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

### 3.4 kind = "external_api"

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

### 5.1 `file_input` — 文件输入源

管线的起始节点，接收用户选择的文件。

```toml
[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = { accept = "video" }   # 接受的文件类型：audio | video | image | file
```

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `accept` | string | `"file"` | 接受的文件类型 |
| `multiple` | bool | `false` | 是否允许多文件（批量处理） |

输出端口：`output`（文件路径）

### 5.2 `file_output` — 文件输出

管线的终止节点，将结果保存到用户指定位置。

```toml
[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"
params = { suffix = ".srt" }
```

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `suffix` | string | — | 输出文件后缀 |
| `output_dir` | string | — | 输出目录（默认 workspace） |

输入端口：`input`

### 5.3 `ffmpeg` — FFmpeg 命令

执行任意 FFmpeg 命令。

```toml
[[nodes]]
id = "extract"
kind = "builtin"
builtin = "ffmpeg"
params = { args = "-i {input} -vn -acodec pcm_s16le -ar 16000 -ac 1 {output}" }
```

| 参数 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `args` | string | ✅ | FFmpeg 参数模板 |

**模板变量：**
- `{input}` — 输入文件路径（由上游边提供）
- `{output}` — 输出文件路径（系统自动生成）

输入端口：`input`
输出端口：`output`

### 5.4 `srt_export` — SRT 字幕导出

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

### 5.5 `text_concat` — 文本拼接

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

### 5.6 `json_transform` — JSON 变换

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

### 5.7 `delay` — 延迟/节流

在节点间插入等待（用于 API 限流）。

```toml
[[nodes]]
id = "throttle"
kind = "builtin"
builtin = "delay"
params = { seconds = 2 }
```

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
