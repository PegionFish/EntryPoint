# 模块接入规范 (Module Specification)

> 版本：1.3-draft | 适用于 EntryPoint v0.x
>
> v1.3-draft 变更（W1 WS-G 契约增补；**未实现冻结**——字段与词表先行定义，
> 消费逻辑随 W1 各实现流落地）：§2.2 新增可选节 `[distribution]`
> （`license_note` / `guide_url`，Tier B/C 权重展示用）；§2.3 `backends` 词表
> 新增 `vulkan` 备选后端；§2.6 `requirements_by_backend` 状态由"本期不实现"
> 更新为"W1 落地消费（HETERO_DIST_PLAN M2/M3）"，schema 冻结描述不变。
>
> v1.2 变更（PACK_UNIFY_PLAN §4.3/§7）：`[[models]]` 新增 `qualified_id` 与
> 变体级 `vram_estimate_mb`（§2.4）；`[runtime] requirements_by_backend` schema
> 冻结声明（本期不实现，§2.6）；venv 命名演进方向 `<module>--<backend>`（§3.1）。
>
> v1.3 变更：`[[models]]` 新增 `sha256` / `sha256s` 完整性校验声明（§2.4）——
> 下载/本地导入完成时由 EP 主程序先校验再落 ready，失败清理残缺产物。
>
> v1.1 变更：新增 `[[models.mirrors]]` 备用下载源（§2.4）；新增"模块产物协议"章节（§5）；"模型管理"章节更新为三获取路径并修正与实现不符的旧描述（§6）。

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

**关于 start_command：**
- 当前实现无隐式默认命令，模块应显式声明
- Python 模块推荐写法：`"{venv_python} {MODULE_DIR}/{entrypoint}"`（仓库自带模块均采用此约定）
- Native 模块：`"{binary} <args>"`

**start_command 可用变量：**

变量占位符**区分大小写**：路径类变量由 daemon 以大写形式提供，端口/设备类与便捷变量由进程管理器以小写注入。

| 变量 | 说明 | 示例值 |
|---|---|---|
| `{ROOT}` | 应用根目录绝对路径 | `/opt/EntryPoint` |
| `{MODULE_DIR}` | 模块目录绝对路径 | `.../modules/faster-whisper` |
| `{MODEL_DIR}` | 当前选中模型的目录 | `.../models/faster-whisper-large-v3` |
| `{models_root}` | 模型缓存根目录（含所有变体子目录，与激活变体无关） | `.../models` |
| `{WORKSPACE}` | 任务工作区目录 | `.../workspace` |
| `{port}` | 分配的端口号 | `18001` |
| `{device}` | 计算设备标识 | `cuda:0` / `cpu` / `npu:0` |
| `{device_index}` | 设备索引（纯数字） | `0` |
| `{backend}` | 计算后端名称 | `cuda` / `rocm` / `openvino` / `cpu` |
| `{venv_python}` | 本模块 venv 内的 Python 解释器（平台自适应：Windows 为 `Scripts/python.exe`，其他平台为 `bin/python`） | `.../runtime/venvs/faster-whisper/bin/python` |
| `{entrypoint}` | `runtime.entrypoint` 的值 | `adapter.py` |
| `{binary}` | 原生二进制路径（type=native，取 `runtime.binaries` 第一项） | `bin/linux-x86_64/deep-filter` |
| `{input}` | CLI 输入文件路径（type=native, interface=cli） | `.../workspace/task-1/input.wav` |
| `{output}` | CLI 输出文件路径（type=native, interface=cli） | `.../workspace/task-1/output.wav` |

#### `[distribution]` — 分发与许可元数据（可选，v1.3-draft 新增，未实现冻结）

Tier B/C 权重的展示用元数据（分级定义见 HETERO_DIST_PLAN §2.4 三级策略）：
平台仅在模块卡片 / 模型详情页渲染这两个字段，**不做任何逻辑校验**；
Tier A 权重无需声明本节。

```toml
[distribution]
license_note = "ISNet 权重由 DIS 项目经官方渠道分发，代码 Apache-2.0；权重许可以仓库 Term of Use 为准"
guide_url = "https://github.com/xuebinqin/DIS#7-term-of-use"
```

| 字段 | 类型 | 必须 | 默认值 | 说明 |
|---|---|---|---|---|
| `license_note` | string | ❌ | — | 一句话许可提示（展示用）。Tier B：随下载按钮显示的许可说明；Tier C：手动安装前提说明 |
| `guide_url` | string | ❌ | — | 手动获取指引链接。指向上游模型发布页/条款页；模块 README 亦应包含等价指引章节 |

> 合规纪律：模块压缩包内永不携带 Tier B/C 权重（HETERO_DIST_PLAN §2.4）；逐模型族
> 的核实结论与证据见 reports/license-matrix.md。

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
| `vulkan` | 备选通用 GPU 后端（v1.3-draft 新增）：厂商栈（cuda/rocm/openvino）均不可用时的兜底路径，典型载体为 ncnn-vulkan 类引擎；由调度器经 `vulkaninfo` 探测检出，优先级置于 openvino 之后、cpu 之前（HETERO_DIST_PLAN M4） | 无标准注入（Vulkan 设备由模块自行枚举） |
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
| `qualified_id` | string | ❌ | — | 全限定模型 ID `<publisher>.<vendor>.<model>`（PACK_UNIFY_PLAN §4.3）。缺省时旧式简单 id 自动归一为 `ep.<vendor>.<model>`（向后兼容层）；整合包与管线节点 pin 统一消费 |
| `vram_estimate_mb` | u64 | ❌ | — | **变体级**显存/内存估算（MB）。VRAM 预算按变体取数：本字段优先，缺省回退模块级 `[compute].vram_estimate_mb`（§2.3） |
| `mirrors` | array | ❌ | `[]` | 备用下载源列表（见下方 `[[models.mirrors]]`） |
| `sha256` | string | ❌ | — | **单文件模型**主文件的期望 sha256（小写 hex）。下载/导入完成后 EP 主程序校验：目标目录内载荷文件（排除 `.ep_meta.json`）须恰有一个且摘要一致，否则判失败并清理残缺文件 |
| `sha256s` | table | ❌ | `{}` | **多文件模型**逐文件期望摘要：相对路径（正斜杠）→ sha256。声明的每个文件都必须存在且摘要一致（未声明的额外文件不失败，兼容 HF 仓库附带文件）。TOML 写法：`[models.sha256s]` 子表 |

> **完整性校验语义**（v1.3）：`sha256` / `sha256s` 任一声明即启用"校验通过才算
> 下载/导入成功"门禁——校验失败按残缺产物清理目标目录（状态回 `Incomplete`），
> 不会出现"半个模型文件被误判 ready"。两者均缺省时跳过校验（向后兼容）。
> 手动放置权重（不走下载/导入）不做校验，行为不变。

#### `[[models.mirrors]]` — 备用下载源（镜像，可重复）

为同一模型声明额外的下载仓库。下载时 UI/API 可按模型选源（主源或任一镜像），
用于应对单一源不可达（如 HuggingFace 访问受限）的场景。

| 字段 | 类型 | 必须 | 默认值 | 说明 |
|---|---|---|---|---|
| `source` | enum | ✅ | — | 镜像来源：`huggingface` \| `modelscope` |
| `repo_id` | string | ✅ | — | 镜像仓库 ID（如 `"pengzhendong/faster-whisper-large-v3"`） |
| `revision` | string | ❌ | 来源默认值 | 镜像侧的版本/分支（HuggingFace 默认 `main`，ModelScope 默认 `master`） |

**校验规则**（清单加载时强制检查，违反则拒绝加载）：

1. `source` 必须与主 source **不同**（不允许重复声明主源）
2. `source` 只允许仓库类来源（`huggingface` / `modelscope`）；**`url` 不能作为 mirror**
3. `repo_id` 不能为空

**源解析规则**（下载请求携带 `source` 参数时）：

- 未指定来源 → 使用主 source
- 指定的来源等于主 source → 使用主 source 的 `repo_id` / `url` / `revision`
- 指定的来源在 `mirrors` 中 → 使用对应 mirror 的字段（取第一个匹配项）
- 指定的来源不可用 → 报错，错误信息列出该模型的全部可用来源

下载请求未指定来源时，平台以 `config/app.toml` 的 `[models] default_source`
（默认 `"huggingface"`）作为回退——仅当该源在模型的可用来源列表（主源 + mirrors）内时生效，
否则仍使用主 source。

**示例（双源模型）：**

```toml
[[models]]
id = "large-v3"
name = "Whisper Large V3 (最高精度)"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"
size_estimate_mb = 3100
default = true

# 备用下载源：HuggingFace 不可达时可切换 ModelScope 源下载
[[models.mirrors]]
source = "modelscope"
repo_id = "pengzhendong/faster-whisper-large-v3"

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

### 2.6 `[runtime] requirements_by_backend` — 后端相关依赖（schema 冻结；W1 落地消费）

> **状态**：PACK_UNIFY_PLAN §7/§4.6 决策的 **schema 冻结描述保持不变**；
> `requirements_by_backend` 由"本期不实现"调整为 **W1 落地消费**——按当前后端
> 选择依赖文件、与 backend 维度的依赖哈希联动，见 HETERO_DIST_PLAN M2/M3。
> 依赖层现状补充：整合包以 `[compute].notes` 给出后端依赖提示。

schema 形状（冻结）：

```toml
[runtime]
requirements_by_backend = { cuda = "requirements-cuda.txt", rocm = "requirements-rocm.txt", cpu = "requirements.txt" }
```

| 约束 | 说明 |
|---|---|
| key | 计算后端名（`cuda` / `rocm` / `openvino` / `directml` / `vulkan` / `cpu`），与 `[compute].backends` 同一词表（§2.3，含 v1.3-draft 新增的 vulkan） |
| value | 依赖文件路径（相对于模块目录），语义同 `runtime.requirements` |
| 回退 | 当前后端无对应条目 → 使用 `runtime.requirements`（默认 `requirements.txt`） |

**post-install 钩子**（v1.3-draft，HETERO_DIST_PLAN 契约缺口补全）：
模块目录内可选提供固定名脚本 `scripts/post-install.sh`，供"pip 安装完成后还需
后处理"的依赖场景使用——典型如 CTranslate2-ROCm 的两步安装法：先装 PyPI 占位
pin，再以官方 Release 的 HIP 轮子同版本覆盖（真实用例：
`modules/faster-whisper/scripts/post-install.sh`，改造自
`scripts/hetero/whisper-rocm/setup-rocm.sh` 的覆盖段）。

| 约束 | 说明 |
|---|---|
| 触发时机 | `uv pip install` 实际执行后、`.ep_deps_hash` 落盘前；依赖未变的重入不触发 |
| 环境变量 | 注入 `VIRTUAL_ENV=<venv 目录>`、`EP_BACKEND=<backend 小写名>`（旧单 venv 口径为空串）；继承宿主其余环境 |
| 缺失行为 | 脚本不存在 → 静默跳过 |
| 失败行为 | 非零退出/超时 → 整体 `ensure_venv` 报错且哈希不落盘：半成品依赖栈不得被哈希锁定（fail-fast，下次进入自动重装并重跑钩子） |
| 执行方式 | Unix 经 `bash` 解释执行（不要求可执行位）；Windows 以同名 `.cmd`/`.bat` 存在性探测执行（落地待定稿） |

钩子成功才算环境就绪：哈希落盘即代表"依赖安装 + 后处理"完整完成，
故钩子自身须幂等可重入。

**venv 命名演进方向**（与 requirements_by_backend 配套，W1 随 M3 一并落地）：
多后端依赖分歧后，venv 目录将从现状 `runtime/venvs/<module-id>/` 演进为
`runtime/venvs/<module>--<backend>/`（每模块每后端一个 venv）。现有单 venv
布局与 `.ep_deps_hash` / `ep.lock` 语义保持不变，旧单 venv 兼容读取。

---

## 3. 运行时类型详解

### 3.1 Python 模块 (`type = "python"`)

**要求：**
- 提供 `adapter.py`（统一 REST 接口，见 ADAPTER_API.md）
- 提供 `requirements.txt`（含 fastapi、uvicorn 等 adapter 依赖）
- 系统已安装 Python（满足 `python_version` 约束）和 uv

**生命周期：**
1. 系统创建独立 venv：`runtime/venvs/<module-id>/`
   （演进方向：多后端依赖分歧后改为 `<module>--<backend>` 每后端一个 venv，
   见 §2.6；当前为单 venv）
2. 安装依赖：`uv pip install -r requirements.txt`
   （依赖栈统一：注入 `UV_CACHE_DIR` 硬链接去重 + 全局 constraints 锁版本，
   见 CONFIG_REFERENCE.md §1.5）
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
| `EP_MODEL_DIR` | 当前（激活变体）模型目录 | `D:\AI_Models\faster-whisper-large-v3` |
| `EP_MODELS_ROOT` | 模型缓存根目录（含所有变体子目录，供 `params.model` 变体覆盖解析，见 ADAPTER_API.md §1.3） | `D:\AI_Models` |
| `EP_MODEL_ID` | 当前模型 ID | `large-v3` |
| `EP_HOST` | adapter 绑定地址（固定回环 `127.0.0.1`，避免非回环监听触发 Windows 防火墙弹窗，见 ADAPTER_API.md §1.2） | `127.0.0.1` |
| `EP_PORT` | 分配端口 | `18001` |
| `EP_DEVICE` | 设备标识 | `cuda:0` / `cpu` / `npu:0` |
| `EP_DEVICE_INDEX` | 设备索引 | `0` |
| `EP_BACKEND` | 计算后端 | `cuda` / `rocm` / `openvino` / `cpu` |
| `EP_WORKSPACE` | 当前任务工作目录（管线运行时） | `...\workspace\task-abc123` |
| `EP_LOG_LEVEL` | 日志级别 | `info` / `debug` |

**adapter.py 必须读取 `EP_PORT` 并监听该端口；绑定地址读取 `EP_HOST`
（缺省 `127.0.0.1`，只绑回环，见 ADAPTER_API.md §1.2）。**

---

## 5. 模块产物协议

管线中的模块节点默认以 JSON / 文本在节点间传递结果。当节点需要产出**文件产物**
（如 ASR 输出 SRT 字幕文件）时，遵循本协议。

### 5.1 触发条件

管线节点的 `params` 中含 `output_format` 字段（非空字符串且**不等于 `"json"`）时，
管线执行器将该节点视为文件输出模式。

### 5.2 执行器注入规则

执行器在请求发出前，向节点 `params` 注入 `output_path` 字段：

```
output_path = <任务工作目录>/<node_id>_output.<fmt>
```

- `<fmt>` 取 `output_format` 的值，仅保留 ASCII 字母与数字（安全过滤）
- 示例：节点 ID `asr_1`、`output_format = "srt"` →
  `output_path = /path/to/workspace/task-abc/asr_1_output.srt`
- multipart（文件上传）与 JSON body 两种请求方式均会注入
- `output_path` 无需模块声明，模块从请求 `params` 中直接读取即可

### 5.3 模块侧约定

模块收到含 `output_path` 的请求时：

1. 将结果文件写入 `output_path`（父目录可能不存在，建议递归创建）
2. 响应中返回 `output_type: "file"`，`result` 为该文件路径（建议同时返回 `output_path` 字段）
3. `output_format` 未指定或等于 `"json"` 时，走原有路径返回 JSON 结果

文件产物响应示例：

```json
{
  "status": "completed",
  "output_type": "file",
  "result": "/path/to/workspace/task-abc/asr_1_output.srt",
  "output_path": "/path/to/workspace/task-abc/asr_1_output.srt",
  "elapsed_seconds": 12.3
}
```

下游节点将收到该文件路径作为文件类型产物；任务中心可下载该产物。

### 5.4 参考实现：faster-whisper SRT 输出

`modules/faster-whisper/adapter.py` 的 `transcribe` 能力：`output_format = "srt"` 时
将识别结果写为 SRT 字幕文件（`output_path` 由管线执行器注入）：

```python
output_format = str(params_dict.get("output_format") or "json").lower()
output_path = params_dict.get("output_path")
if output_format == "srt" and output_path:
    srt_text = _segments_to_srt(result["result"]["segments"])
    Path(output_path).parent.mkdir(parents=True, exist_ok=True)
    Path(output_path).write_text(srt_text, encoding="utf-8")
    return {
        "status": "completed",
        "output_type": "file",
        "result": str(output_path),
        "output_path": str(output_path),
    }
```

> 提示：可在能力的 `params` schema 中可选地声明 `output_format` 参数
> （如 `select` 类型，选项 `json` / `srt`），便于管线编辑器在 UI 中展示该参数。

---

## 6. 模型管理

模型统一存放于 `[models] cache_dir`（默认应用根目录下的 `models/`），
每个模型一个目录，目录名为 `[[models]].target_dir`。

### 6.1 三种获取路径

| 路径 | 说明 | 元数据 |
|---|---|---|
| 在线下载 | HuggingFace / ModelScope / URL 三源 + `[[models.mirrors]]` 镜像源，可按模型选源 | 写入 `.ep_meta.json` |
| 浏览器上传 | WebUI 上传：文件夹多文件、zip、tar.gz(.tgz) | 写入 `.ep_meta.json` |
| 服务器本地导入 | 从服务器上已有的目录导入 | 写入 `.ep_meta.json` |

### 6.2 在线下载

- 下载命令在模块的 venv 环境中执行：HuggingFace 使用 `huggingface_hub.snapshot_download`，ModelScope 使用 `modelscope.snapshot_download`；URL 源直接下载文件（`.tar.gz` / `.tgz` 自动解压）
- **选源**：UI/API 下载时可指定来源（主源或任一 mirror）；未指定时回退 `config/app.toml` 的 `[models] default_source`（见 §2.4 源解析规则）
- **镜像站**：`[models] hf_endpoint` 在 HuggingFace 下载时注入为 `HF_ENDPOINT` 环境变量
- **代理**：`[network] http_proxy` / `https_proxy` / `no_proxy` 以环境变量形式（大写 + 小写键同时）注入下载进程
- 模块 venv 尚不存在时，下载前会自动准备 Python 环境（见 §6.6 已知限制）
- 断点续传行为依赖底层 Python 库（`huggingface_hub` / `modelscope`）自身的实现，不是 EntryPoint 平台实现的功能

### 6.3 浏览器上传

通过 `POST /api/models/:module_id/upload`（multipart/form-data）上传：

- **文件夹模式**：多个 `files` 文件块 + 同序的 `paths` 相对路径（浏览器 `webkitRelativePath`），服务端逐 chunk 流式落盘到暂存区，不整块进内存，随后按相对路径还原目录结构
- **归档模式**：仅一个文件且文件名以 `.zip` / `.tar.gz` / `.tgz` 结尾时，服务端解包；逐条目进行路径安全校验（防路径穿越）；压缩包内只有一个顶层目录时剥掉一层作为模型根
- 上传完成后写入 `.ep_meta.json`，记录来源为上传

### 6.4 本地导入与手动放置

- 通过 UI/API 导入服务器上的已有目录：写入 `.ep_meta.json`，支持检查更新
- 手动将模型文件复制到 `<cache_dir>/<target_dir>/`：有 `.ep_meta.json` → 系统识别来源，支持检查更新；无 → 视为手动放置，直接使用，不校验

### 6.5 多模型切换

- 同一模块声明多个 `[[models]]` 时，用户在 UI 中选择使用哪个
- 切换模型需重启模块进程（模型加载到内存/显存）
- `EP_MODEL_DIR` 和 `EP_MODEL_ID` 随选择变化；`EP_MODELS_ROOT` 恒指模型缓存根目录，
  供 adapter 在 `params.model` 临时覆盖时解析非激活变体的本地权重（无需重启，
  参照实现：rembg adapter；契约见 ADAPTER_API.md §1.3）

### 6.6 已知限制

- 首次下载需自动准备模块 venv，含 torch 等大型依赖时约需 15–20 分钟，客户端超时需放宽或重试
- 下载并发上限由 `[models] max_concurrent_downloads` 控制（默认 2）

---

## 7. 完整示例

### 7.1 Python HTTP 模块：faster-whisper

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
tags = ["speech", "recognition", "multilingual", "timestamps"]

[runtime]
type = "python"
python_version = ">=3.10,<3.13"
requirements = "requirements.txt"
entrypoint = "adapter.py"
start_command = "{venv_python} {MODULE_DIR}/{entrypoint}"

[compute]
backends = ["cuda", "rocm", "cpu"]
default_backend = "cuda"
vram_estimate_mb = 4096
min_vram_mb = 2048

[compute.env]
cuda = { CUDA_VISIBLE_DEVICES = "{device_index}" }
rocm = { HIP_VISIBLE_DEVICES = "{device_index}" }

# 三个模型均为 HuggingFace 主源 + ModelScope 镜像（双源可选下载）
[[models]]
id = "large-v3"
name = "Whisper Large V3 (最高精度)"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"
size_estimate_mb = 3100
default = true

[[models.mirrors]]
source = "modelscope"
repo_id = "pengzhendong/faster-whisper-large-v3"

[[models]]
id = "medium"
name = "Whisper Medium (平衡)"
source = "huggingface"
repo_id = "Systran/faster-whisper-medium"
target_dir = "faster-whisper-medium"
size_estimate_mb = 1500

[[models.mirrors]]
source = "modelscope"
repo_id = "pengzhendong/faster-whisper-medium"

[[models]]
id = "small"
name = "Whisper Small (轻量)"
source = "huggingface"
repo_id = "Systran/faster-whisper-small"
target_dir = "faster-whisper-small"
size_estimate_mb = 500

[[models.mirrors]]
source = "modelscope"
repo_id = "pengzhendong/faster-whisper-small"

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

### 7.2 Python HTTP 模块（轻量 ONNX 模型）：deep-filter

> 仓库现役实况（`modules/deep-filter/module.toml`）：Python 运行时 + HTTP 接口，
> ONNX 小模型走 URL 直链下载（HuggingFace 仓库资产）。

```toml
# modules/deep-filter/module.toml

[module]
id = "deep-filter"
name = "DeepFilter 音频降噪"
version = "0.5.6"
description = "基于 DeepFilterNet 的深度学习语音增强/降噪"
category = "denoise"
genre = "deep-filter"
authors = ["EntryPoint Community"]
license = "MIT"
homepage = "https://github.com/Rikorose/DeepFilterNet"
tags = ["denoise", "audio", "enhancement", "realtime"]

[runtime]
type = "python"
python_version = ">=3.10,<3.13"
requirements = "requirements.txt"
entrypoint = "adapter.py"
start_command = "{venv_python} {MODULE_DIR}/{entrypoint}"

[compute]
backends = ["cuda", "cpu"]
default_backend = "cpu"
vram_estimate_mb = 512
min_vram_mb = 256

[compute.env]
cuda = { CUDA_VISIBLE_DEVICES = "{device_index}" }

[[models]]
id = "df3"
name = "DeepFilterNet3 (默认)"
source = "url"
url = "https://huggingface.co/Serkan007/DeepFilterNet3-ONNX/resolve/main/DeepFilterNet3_onnx.tar.gz"
target_dir = "deep-filter-df3"
size_estimate_mb = 8
default = true

[interface]
type = "http"
health_endpoint = "/health"
ready_timeout_secs = 30

[[interface.capabilities]]
name = "denoise"
description = "AI 语音降噪，输出增强后的音频文件"
input_type = "audio"
output_type = "audio"
max_file_size_mb = 500

[interface.capabilities.params]
attenuation = { type = "integer", default = 100, min = 0, max = 100, description = "降噪强度 (dB)" }
min_db = { type = "float", default = -60.0, min = -100.0, max = 0.0, step = 1.0, description = "最小增益 (dB)" }
```

调用走统一 HTTP 契约（ADAPTER_API.md）：`POST /predict/denoise`；能力声明的
`output_type = "audio"` 用于端口类型校验，adapter 实际返回文件产物时须写
`output_type = "file"` + `result` 为路径字符串（ADAPTER_API.md §2.3 result 规则）。

### 7.2.1 原生 CLI 模块形态（示意，仓库暂无现役实例）

> 以下为 `type = "native"` + `interface = "cli"` 的**示意**写法，说明字段形状；
> 仓库现役 5 个模块均为 Python HTTP 形态。

```toml
# 示意：modules/<native-tool>/module.toml

[module]
id = "native-tool"
name = "原生 CLI 工具示例"
version = "1.0.0"
description = "原生二进制 + CLI 接口示例"
category = "custom"
genre = "native-tool"

[runtime]
type = "native"
start_command = "{binary} {input} -o {output}"

[runtime.binaries]
windows-x86_64 = "bin/windows-x86_64/native-tool.exe"
linux-x86_64 = "bin/linux-x86_64/native-tool"

[compute]
backends = ["cpu"]
default_backend = "cpu"

[interface]
type = "cli"

[[interface.capabilities]]
name = "process"
description = "处理输入文件"
input_type = "audio"
output_type = "audio"
```

CLI 调用时系统按 `start_command` 构建命令（`{input}` / `{output}` 替换为
任务工作目录路径，§2.2），进程退出码 0 = 成功。

> 注：该执行路径当前未实现（现役无 native CLI 实例），仅为字段形状示意；
> 管线执行器对模块节点只走 HTTP（executor.rs `execute_module_node`）。

### 7.3 Python HTTP 模块（NPU 支持）：qwen3-asr

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

## 8. 模块开发检查清单

- [ ] `module.toml` 所有必填字段已填写
- [ ] `id` 与目录名一致
- [ ] `category` 和 `genre` 正确分类
- [ ] `start_command` 已显式声明（推荐 `{venv_python} {MODULE_DIR}/{entrypoint}`）
- [ ] `backends` 列表反映实际支持的计算后端
- [ ] `[[models]]` 的 `repo_id` 可公开访问
- [ ] 声明 `[[models.mirrors]]` 时：`source` 与主源不同、为 `huggingface` / `modelscope` 之一、`repo_id` 非空
- [ ] 支持文件产物输出时：遵循模块产物协议（§5）——读取 `params` 中的 `output_path` 写文件，返回 `output_type = "file"`
- [ ] Python 模块：`adapter.py` 实现了 `/health`、`/info`、`/predict/<capability>`
- [ ] Python 模块：`requirements.txt` 包含 fastapi + uvicorn
- [ ] Python 模块：adapter 读取 `EP_PORT` 环境变量并监听
- [ ] Python 模块：adapter 读取 `EP_MODEL_DIR` 加载模型
- [ ] Python 模块：adapter 读取 `EP_DEVICE` / `EP_BACKEND` 选择计算设备
- [ ] 原生模块：`bin/` 下至少有一个平台的二进制
- [ ] CLI 模块：命令模板中 `{input}` 和 `{output}` 位置正确
- [ ] 在本地测试通过（手动启动 → curl /health → curl /predict）

---

## 9. 调试方法

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
