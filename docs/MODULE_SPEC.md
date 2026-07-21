# 模块接入规范 (Module Specification)

> 版本：1.0 | 适用于 EntryPoint v0.x

本文档是第三方开发者将 AI 工具接入 EntryPoint 平台的完整参考。
一个模块 = 一个目录 + 一份 `module.toml`，放入 `modules/` 目录即被系统识别。

---

## 1. 模块目录结构

```
modules/<module-id>/
├── module.toml            ← 必须。模块清单，声明一切元信息
├── adapter.py             ← Python 模块必须。统一 REST 接口适配器
├── requirements.txt       ← Python 模块必须。pip 依赖列表
├── README.md              ← 推荐。模块说明
├── bin/                   ← 原生模块。按平台存放二进制
│   ├── windows-x86_64/
│   └── linux-x86_64/
└── assets/                ← 可选。模块附带的静态资源
```

**命名规则：**
- `module-id` 使用小写字母、数字、连字符（如 `faster-whisper`、`qwen3-asr-1.7b`）
- 同一 `category` + `genre` 下可有多个模块（如多个 ASR 实现）

---

## 2. module.toml 完整字段参考

### 2.1 `[module]` — 基本信息

| 字段 | 类型 | 必须 | 默认值 | 说明 |
|---|---|---|---|---|
| `id` | string | ✅ | — | 全局唯一标识，与目录名一致 |
| `name` | string | ✅ | — | 显示名称 |
| `version` | string | ✅ | — | 模块版本（语义化版本号） |
| `description` | string | ✅ | — | 一句话描述 |
| `category` | enum | ✅ | — | 功能类别（见下表） |
| `genre` | string | ✅ | — | 同类模型分组标签，用于对比（如 `"whisper"`、`"qwen-asr"`） |
| `authors` | string[] | ❌ | `[]` | 作者列表 |
| `license` | string | ❌ | — | 许可证标识（SPDX） |
| `homepage` | string | ❌ | — | 项目主页 URL |
| `tags` | string[] | ❌ | `[]` | 搜索标签 |

**category 枚举值：**

| 值 | 说明 | 典型模块 |
|---|---|---|
| `asr` | 语音识别 | faster-whisper, qwen3-asr, whisperx |
| `tts` | 语音合成 | qwen3-tts |
| `denoise` | 音频降噪 | deep-filter |
| `ocr` | 文字识别 | paddlerocr, firered-ocr |
| `image` | 图像处理（分割/去背景/修复） | rembg, sam3, iopaint |
| `translate` | 翻译 | llm-translator |
| `video` | 视频处理 | ffmpeg 工具链 |
| `face` | 人脸处理 | facefusion, latentsync |
| `custom` | 自定义/其他 | — |

### 2.2 `[runtime]` — 运行时配置

| 字段 | 类型 | 必须 | 默认值 | 说明 |
|---|---|---|---|---|
| `type` | enum | ✅ | — | `python` \| `native` |
| `python_version` | string | 条件 | — | Python 版本约束（type=python 时必须），如 `">=3.10,<3.13"` |
| `requirements` | string | 条件 | `"requirements.txt"` | 依赖文件路径（相对于模块目录） |
| `entrypoint` | string | 条件 | `"adapter.py"` | 启动入口脚本（相对于模块目录） |
| `start_command` | string | 条件 | 见下方 | 启动命令模板（支持变量替换） |

**start_command 默认值：**
- Python: `"python {entrypoint} --port {port} --device {device} --model-dir {model_dir}"`
- Native: `"{binary} {args}"`

**start_command 可用变量：**

| 变量 | 说明 | 示例值 |
|---|---|---|
| `{root}` | 应用根目录绝对路径 | `G:\AI_Applications\EntryPoint` |
| `{module_dir}` | 模块目录绝对路径 | `...\modules\faster-whisper` |
| `{model_dir}` | 当前选中模型的目录 | `D:\AI_Models\faster-whisper-large-v3` |
| `{port}` | 分配的端口号 | `18001` |
| `{device}` | 计算设备标识 | `cuda:0` / `cpu` / `npu:0` |
| `{device_index}` | 设备索引（纯数字） | `0` |
| `{backend}` | 计算后端名称 | `cuda` / `rocm` / `openvino` / `cpu` |
| `{binary}` | 原生二进制路径（type=native） | `...\bin\windows-x86_64\deep-filter.exe` |
| `{input}` | CLI 输入文件路径（type=native, interface=cli） | `...\workspace\task-1\input.wav` |
| `{output}` | CLI 输出文件路径（type=native, interface=cli） | `...\workspace\task-1\output.wav` |

#### `[runtime.binaries]` — 原生二进制路径（type=native 时必须）

按 `<os>-<arch>` 为 key：

```toml
[runtime.binaries]
windows-x86_64 = "bin/windows-x86_64/deep-filter.exe"
linux-x86_64 = "bin/linux-x86_64/deep-filter"
linux-aarch64 = "bin/linux-aarch64/deep-filter"
```

支持的平台标识：`windows-x86_64`、`linux-x86_64`、`linux-aarch64`

### 2.3 `[compute]` — 计算后端

| 字段 | 类型 | 必须 | 默认值 | 说明 |
|---|---|---|---|---|
| `backends` | string[] | ✅ | — | 支持的后端列表，按优先级排序 |
| `default_backend` | string | ❌ | `backends[0]` | 默认后端 |
| `vram_estimate_mb` | u32 | ❌ | — | 预估显存/内存占用（MB），用于调度参考 |
| `min_vram_mb` | u32 | ❌ | — | 最低显存要求（低于此值警告） |

**backends 可选值：**

| 值 | 说明 | 环境变量注入 |
|---|---|---|
| `cuda` | NVIDIA GPU | `CUDA_VISIBLE_DEVICES={device_index}` |
| `rocm` | AMD GPU | `HIP_VISIBLE_DEVICES={device_index}` |
| `openvino` | Intel CPU/GPU/NPU | `OPENVINO_DEVICE={device_name}` |
| `directml` | Windows 通用 GPU | 由 ONNX Runtime 管理 |
| `cpu` | 纯 CPU（始终可用） | 无 |

#### `[compute.env]` — 自定义环境变量覆盖（可选）

```toml
[compute.env]
cuda = { CUDA_VISIBLE_DEVICES = "{device_index}", TORCH_DEVICE = "cuda" }
rocm = { HIP_VISIBLE_DEVICES = "{device_index}", TORCH_DEVICE = "cuda" }
openvino = { OPENVINO_DEVICE = "{device_name}" }
cpu = { TORCH_DEVICE = "cpu" }
```

### 2.4 `[[models]]` — 模型声明（可重复）

每个模块可声明多个可选模型（如 whisper-large / whisper-medium）。

| 字段 | 类型 | 必须 | 默认值 | 说明 |
|---|---|---|---|---|
| `id` | string | ✅ | — | 模型标识（模块内唯一） |
| `name` | string | ✅ | — | 显示名称 |
| `source` | enum | ✅ | — | `huggingface` \| `modelscope` \| `url` |
| `repo_id` | string | 条件 | — | HF/ModelScope 仓库 ID（source=huggingface/modelscope 时必须） |
| `url` | string | 条件 | — | 下载直链（source=url 时必须） |
| `target_dir` | string | ✅ | — | 下载目标目录名（相对于模型缓存目录） |
| `revision` | string | ❌ | `"main"` | Git 分支/标签/commit |
| `size_estimate_mb` | u32 | ❌ | — | 预估大小（用于 UI 显示） |
| `default` | bool | ❌ | `false` | 是否为默认选中模型 |

**示例：**

```toml
[[models]]
id = "large-v3"
name = "Whisper Large V3 (最高精度)"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"
size_estimate_mb = 3100
default = true

[[models]]
id = "medium"
name = "Whisper Medium (平衡)"
source = "huggingface"
repo_id = "Systran/faster-whisper-medium"
target_dir = "faster-whisper-medium"
size_estimate_mb = 1500

[[models]]
id = "small-ms"
name = "Whisper Small (ModelScope 源)"
source = "modelscope"
repo_id = "pengzhendong/faster-whisper-small"
target_dir = "faster-whisper-small"
```

### 2.5 `[interface]` — 接口声明

| 字段 | 类型 | 必须 | 默认值 | 说明 |
|---|---|---|---|---|
| `type` | enum | ✅ | — | `http` \| `cli` |
| `health_endpoint` | string | 条件 | `"/health"` | 健康检查路径（type=http） |
| `ready_timeout_secs` | u32 | ❌ | `120` | 启动就绪超时（秒） |
| `working_dir` | string | ❌ | 模块目录 | 进程工作目录 |

#### `[[interface.capabilities]]` — 能力声明（可重复）

| 字段 | 类型 | 必须 | 默认值 | 说明 |
|---|---|---|---|---|
| `name` | string | ✅ | — | 能力标识（如 `"transcribe"`、`"denoise"`） |
| `description` | string | ✅ | — | 能力描述 |
| `input_type` | enum | ✅ | — | 输入数据类型 |
| `output_type` | enum | ✅ | — | 输出数据类型 |
| `max_file_size_mb` | u32 | ❌ | — | 最大输入文件大小限制 |
| `supports_batch` | bool | ❌ | `false` | 是否支持批量处理 |

**input_type / output_type 枚举值：**

| 值 | 说明 | 传递方式 |
|---|---|---|
| `audio` | 音频文件 (wav/mp3/flac/ogg/m4a) | 文件路径 |
| `video` | 视频文件 (mp4/mkv/avi/webm) | 文件路径 |
| `image` | 图片文件 (png/jpg/webp/bmp) | 文件路径 |
| `text` | 纯文本 / 带时间戳文本 | 字符串或文件 |
| `json` | 结构化数据 | JSON 值 |
| `file` | 任意文件 | 文件路径 |

#### `[interface.capabilities.params]` — 参数 Schema

使用 JSON Schema 子集声明参数，供 UI 自动生成配置面板：

```toml
[interface.capabilities.params]
language = { type = "string", default = "auto", description = "语言代码（如 zh/en/ja）或 auto 自动检测" }
timestamps = { type = "boolean", default = true, description = "是否输出词级时间戳" }
beam_size = { type = "integer", default = 5, min = 1, max = 20, description = "束搜索宽度" }
vad_filter = { type = "boolean", default = true, description = "启用 VAD 过滤静音段" }
```

**支持的参数类型：**

| type | 额外字段 | 说明 |
|---|---|---|
| `string` | `enum` (可选值列表) | 字符串 |
| `integer` | `min`, `max` | 整数 |
| `float` | `min`, `max`, `step` | 浮点数 |
| `boolean` | — | 布尔 |
| `select` | `options` (string[]) | 下拉选择 |

---

## 3. 运行时类型详解

### 3.1 Python 模块 (`type = "python"`)

**要求：**
- 提供 `adapter.py`（统一 REST 接口，见 ADAPTER_API.md）
- 提供 `requirements.txt`（含 fastapi、uvicorn 等 adapter 依赖）
- 系统已安装 Python（满足 `python_version` 约束）和 uv

**生命周期：**
1. 系统创建独立 venv：`runtime/venvs/<module-id>/`
2. 安装依赖：`uv pip install -r requirements.txt`
3. 启动：使用 venv 内的 python 执行 `start_command`
4. 健康检查：轮询 `GET /health` 直到 200
5. 就绪：标记为 Running

**adapter.py 职责：**
- 读取环境变量（`EP_PORT`、`EP_DEVICE`、`EP_MODEL_DIR` 等）
- 加载模型到指定设备
- 暴露标准 REST 端点（`/health`、`/info`、`/predict/<capability>`）
- 将底层工具（Gradio/Flask/直接 Python API）包装为统一接口

### 3.2 原生模块 (`type = "native"`)

**要求：**
- 在 `bin/` 下按平台提供可执行文件
- 无需 venv，无需 adapter

**接口类型：**
- `cli`：管线引擎按需调用命令行，传入 `{input}` / `{output}` 路径
- `http`：原生程序自带 HTTP 服务（需声明 `health_endpoint`）

**CLI 调用方式：**
```
<binary> <args with {input} and {output} substituted>
```
进程退出码 0 = 成功，非 0 = 失败。stdout/stderr 捕获为日志。

---

## 4. 环境变量契约

系统启动模块进程时注入以下环境变量：

| 变量 | 说明 | 示例 |
|---|---|---|
| `EP_ROOT` | 应用根目录 | `G:\AI_Applications\EntryPoint` |
| `EP_MODULE_DIR` | 模块目录 | `...\modules\faster-whisper` |
| `EP_MODULE_ID` | 模块 ID | `faster-whisper` |
| `EP_MODEL_DIR` | 当前模型目录 | `D:\AI_Models\faster-whisper-large-v3` |
| `EP_MODEL_ID` | 当前模型 ID | `large-v3` |
| `EP_PORT` | 分配端口 | `18001` |
| `EP_DEVICE` | 设备标识 | `cuda:0` / `cpu` / `npu:0` |
| `EP_DEVICE_INDEX` | 设备索引 | `0` |
| `EP_BACKEND` | 计算后端 | `cuda` / `rocm` / `openvino` / `cpu` |
| `EP_WORKSPACE` | 当前任务工作目录（管线运行时） | `...\workspace\task-abc123` |
| `EP_LOG_LEVEL` | 日志级别 | `info` / `debug` |

**adapter.py 必须读取 `EP_PORT` 并监听该端口。**

---

## 5. 模型管理

### 5.1 下载

- 系统根据 `[[models]]` 声明，使用 `huggingface-hub` 或 `modelscope` 标准下载到 `<cache_dir>/<target_dir>/`
- 下载在模块的 venv 环境中执行（确保 huggingface-hub 已安装）
- 支持 `HF_ENDPOINT` 镜像站配置

### 5.2 离线导入

用户可手动将模型文件放入 `<cache_dir>/<target_dir>/`：
- 有 `.ep_meta.json` → 系统识别来源，支持检查更新
- 无 `.ep_meta.json` → 系统视为手动放置，直接使用，不校验

### 5.3 多模型切换

- 同一模块声明多个 `[[models]]` 时，用户在 UI 中选择使用哪个
- 切换模型需重启模块进程（模型加载到内存/显存）
- `EP_MODEL_DIR` 和 `EP_MODEL_ID` 随选择变化

---

## 6. 完整示例

### 6.1 Python HTTP 模块：faster-whisper

```toml
# modules/faster-whisper/module.toml

[module]
id = "faster-whisper"
name = "Faster-Whisper ASR"
version = "1.1.0"
description = "基于 CTranslate2 的高速语音识别，支持词级时间戳和多语言"
category = "asr"
genre = "whisper"
authors = ["EntryPoint Community"]
license = "MIT"
homepage = "https://github.com/SYSTRAN/faster-whisper"
tags = ["speech", "recognition", "multilingual"]

[runtime]
type = "python"
python_version = ">=3.10,<3.13"
requirements = "requirements.txt"
entrypoint = "adapter.py"

[compute]
backends = ["cuda", "rocm", "cpu"]
default_backend = "cuda"
vram_estimate_mb = 4096
min_vram_mb = 2048

[compute.env]
cuda = { CUDA_VISIBLE_DEVICES = "{device_index}" }
rocm = { HIP_VISIBLE_DEVICES = "{device_index}" }

[[models]]
id = "large-v3"
name = "Whisper Large V3"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"
size_estimate_mb = 3100
default = true

[[models]]
id = "medium"
name = "Whisper Medium"
source = "huggingface"
repo_id = "Systran/faster-whisper-medium"
target_dir = "faster-whisper-medium"
size_estimate_mb = 1500

[[models]]
id = "small"
name = "Whisper Small (轻量)"
source = "huggingface"
repo_id = "Systran/faster-whisper-small"
target_dir = "faster-whisper-small"
size_estimate_mb = 500

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 90

[[interface.capabilities]]
name = "transcribe"
description = "语音转文字，支持词级时间戳"
input_type = "audio"
output_type = "json"
max_file_size_mb = 2048

[interface.capabilities.params]
language = { type = "string", default = "auto", description = "语言代码或 auto" }
timestamps = { type = "boolean", default = true, description = "输出词级时间戳" }
beam_size = { type = "integer", default = 5, min = 1, max = 20 }
vad_filter = { type = "boolean", default = true, description = "VAD 静音过滤" }
condition_on_previous = { type = "boolean", default = true, description = "上下文条件推理" }
```

```
# modules/faster-whisper/requirements.txt
fastapi>=0.100.0
uvicorn[standard]>=0.23.0
python-multipart>=0.0.6
faster-whisper>=1.0.0
```

### 6.2 原生 CLI 模块：deep-filter

```toml
# modules/deep-filter/module.toml

[module]
id = "deep-filter"
name = "DeepFilter 音频降噪"
version = "0.5.6"
description = "基于深度学习的实时语音增强/降噪"
category = "denoise"
genre = "deep-filter"
license = "MIT"
homepage = "https://github.com/Rikorose/DeepFilterNet"

[runtime]
type = "native"

[runtime.binaries]
windows-x86_64 = "bin/windows-x86_64/deep-filter.exe"
linux-x86_64 = "bin/linux-x86_64/deep-filter"

[compute]
backends = ["cpu"]
default_backend = "cpu"

[interface]
type = "cli"

[[interface.capabilities]]
name = "denoise"
description = "AI 语音降噪，输出增强后的音频"
input_type = "audio"
output_type = "audio"

[interface.capabilities.params]
attenuation = { type = "integer", default = 100, min = 0, max = 100, description = "降噪强度 (dB)" }
```

CLI 调用时系统构建命令：
```
deep-filter.exe input.wav -o output.wav --attenuation 100
```

### 6.3 Python HTTP 模块（NPU 支持）：qwen3-asr

```toml
# modules/qwen3-asr/module.toml

[module]
id = "qwen3-asr"
name = "Qwen3-ASR 语音识别"
version = "1.0.0"
description = "基于 Qwen3 的语音识别，支持 NPU 加速"
category = "asr"
genre = "qwen-asr"
license = "Apache-2.0"

[runtime]
type = "python"
python_version = ">=3.10,<3.13"
requirements = "requirements.txt"
entrypoint = "adapter.py"

[compute]
backends = ["cuda", "openvino", "cpu"]
default_backend = "cuda"
vram_estimate_mb = 4000

[compute.env]
cuda = { CUDA_VISIBLE_DEVICES = "{device_index}" }
openvino = { OPENVINO_DEVICE = "{device_name}" }

[[models]]
id = "1.7b"
name = "Qwen3-ASR 1.7B"
source = "modelscope"
repo_id = "Qwen/Qwen3-ASR-1.7B"
target_dir = "qwen3-asr-1.7b"
size_estimate_mb = 3500
default = true

[[models]]
id = "0.6b"
name = "Qwen3-ASR 0.6B (轻量)"
source = "modelscope"
repo_id = "Qwen/Qwen3-ASR-0.6B"
target_dir = "qwen3-asr-0.6b"
size_estimate_mb = 1300

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 120

[[interface.capabilities]]
name = "transcribe"
description = "语音转文字（带时间戳）"
input_type = "audio"
output_type = "json"

[interface.capabilities.params]
language = { type = "string", default = "auto" }
timestamps = { type = "boolean", default = true }
```

---

## 7. 模块开发检查清单

- [ ] `module.toml` 所有必填字段已填写
- [ ] `id` 与目录名一致
- [ ] `category` 和 `genre` 正确分类
- [ ] `backends` 列表反映实际支持的计算后端
- [ ] `[[models]]` 的 `repo_id` 可公开访问
- [ ] Python 模块：`adapter.py` 实现了 `/health`、`/info`、`/predict/<capability>`
- [ ] Python 模块：`requirements.txt` 包含 fastapi + uvicorn
- [ ] Python 模块：adapter 读取 `EP_PORT` 环境变量并监听
- [ ] Python 模块：adapter 读取 `EP_MODEL_DIR` 加载模型
- [ ] Python 模块：adapter 读取 `EP_DEVICE` / `EP_BACKEND` 选择计算设备
- [ ] 原生模块：`bin/` 下至少有一个平台的二进制
- [ ] CLI 模块：命令模板中 `{input}` 和 `{output}` 位置正确
- [ ] 在本地测试通过（手动启动 → curl /health → curl /predict）

---

## 8. 调试方法

### 手动启动模块（绕过 EntryPoint）

```bash
# 设置环境变量（模拟 EntryPoint 注入）
export EP_ROOT=/path/to/EntryPoint
export EP_MODULE_DIR=/path/to/EntryPoint/modules/faster-whisper
export EP_MODEL_DIR=/path/to/models/faster-whisper-large-v3
export EP_PORT=18001
export EP_DEVICE=cuda:0
export EP_BACKEND=cuda
export EP_DEVICE_INDEX=0

# 激活 venv
source /path/to/EntryPoint/runtime/venvs/faster-whisper/bin/activate

# 启动
python adapter.py
```

### 测试端点

```bash
# 健康检查
curl http://localhost:18001/health

# 模块信息
curl http://localhost:18001/info

# 调用能力
curl -X POST http://localhost:18001/predict/transcribe \
  -F "file=@test.wav" \
  -F 'params={"language": "zh", "timestamps": true}'
```
