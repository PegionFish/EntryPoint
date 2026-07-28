# EntryPoint — AI 工具管控平台 设计文档

> 状态：草案 v0.4 | 日期：2026-07-29
>
> v0.4 变更：新增 WebUI 技术栈（React + Vite + TypeScript）、引用 DESIGN_SYSTEM.md、
> 更新 UI 页面规划为已实现状态
>
> v0.3 变更：模型缓存目录用户可指定、uv/Python 不内嵌（系统调用+引导安装）、
> NPU 仅 Intel（可扩展）、Adapter 统一 REST 接口、genre 分类与多模型对比
>
> v0.2 变更：计算设备抽象（多后端）、模块化插件系统、虚拟环境自动管理、
> 可移植性设计、模型按需下载、移除 ComfyUI/SD 相关讨论

---

## 1. 项目概述

EntryPoint 是一个 **Windows/Linux 原生桌面应用 + WebUI 管理界面**，用于统一管控本地部署的多个 AI 模型服务。桌面端（egui）提供本地操作体验，WebUI（React）支持浏览器远程管控。

核心能力：

- **异构计算调度**：支持 NVIDIA CUDA / AMD ROCm / Intel GPU / NPU / CPU，将不同模型分配到不同计算设备并行运行
- **模块化插件系统**：每个 AI 工具以标准化 Module Manifest 接入，第三方开发者可照文档自行扩展
- **虚拟环境自动管理**：自动创建/维护 Python venv，自动安装依赖
- **模型按需下载**：发布不携带模型文件，运行时从 HuggingFace / ModelScope 标准下载
- **完全可移植**：用户复制整个文件夹即可在另一台机器（甚至另一平台）上使用
- **DAG 管线引擎**：将多个模块串联为有向无环图工作流

### 1.1 设计原则

| 原则 | 说明 |
|---|---|
| **可移植优先** | 所有路径相对于应用根目录；venv 不可移植时自动重建；无注册表/系统服务依赖 |
| **模块即文件夹** | 一个模块 = 一个目录 + 一份 `module.toml`，放入 `modules/` 即被识别 |
| **计算后端无关** | 核心不绑定任何 GPU 厂商，通过 ComputeBackend 抽象层适配 |
| **模型与代码分离** | 模型文件存放在用户可指定的缓存目录（默认 `models/`），模块只声明下载源，不打包模型 |
| **渐进式复杂度** | 用户只需双击启动 → 选模块 → 下载模型 → 运行；高级用户可编辑 manifest/管线 |

### 1.2 初期范围

**纳入**：音频降噪、ASR、TTS、翻译（外部 API）、OCR、图像处理（RemBg/SAM）、视频处理（FFmpeg）
**暂不纳入**：ComfyUI、Stable Diffusion、视频生成（LTX/Wan2.2）、换脸/唇同步

---

## 2. 技术栈

| 层级 | 选型 | 说明 |
|---|---|---|
| 桌面 GUI | **egui + eframe** | 纯 Rust 即时模式 GUI，wgpu 自绘渲染，跨平台 |
| **WebUI** | **React 19 + TypeScript** | 浏览器管理界面，远程管控（见 [DESIGN_SYSTEM.md](docs/DESIGN_SYSTEM.md)） |
| WebUI 构建 | **Vite 8** | 前端构建 + 开发服务器（HMR） |
| WebUI 样式 | **TailwindCSS 4 + shadcn/ui** | 原子化 CSS + 无障碍组件库（new-york 风格） |
| WebUI 节点编辑器 | **@xyflow/react (React Flow)** | DAG 管线可视化编排 |
| WebUI 状态管理 | **Zustand** | 轻量全局状态（主题、连接状态） |
| 后端 | **Rust** | 进程管理、计算调度、管线引擎 |
| HTTP 服务 | **axum** | REST API + WebSocket + 静态资源服务 |
| 异步运行时 | **tokio** | 异步进程管理、HTTP、并发任务 |
| HTTP 客户端 | **reqwest** | 调用模块 HTTP API、外部 LLM API、模型下载 |
| 序列化 | **serde + toml / json** | 配置、模块清单、管线定义 |
| 桌面节点编辑器 | **egui_node_graph** 或自研 | DAG 可视化（桌面端） |
| 日志 | **tracing** | 结构化日志 |
| Python 环境 | **uv**（系统安装） | 极速 venv 创建 + 依赖安装；不内嵌，调用系统 uv/python |
| 模型下载 | **huggingface-hub** / **modelscope** (Python) | 标准模型下载，支持断点续传 |
| 计算设备检测 | 多后端（见 §4.2） | nvidia-smi / rocm-smi / xpu-smi / OpenVINO |

---

## 3. 目录结构与可移植性

### 3.1 应用根目录布局

```
EntryPoint/                        ← 应用根目录（可整体复制）
├── entrypoint.exe / entrypoint    ← 主程序二进制（Win/Linux 各一份）
├── config/
│   ├── app.toml                   ← 全局设置（含模型缓存路径）
│   └── pipelines/                 ← 用户保存的管线
│       └── video_to_srt.toml
├── modules/                       ← 模块目录（放入即识别）
│   ├── faster-whisper/
│   │   ├── module.toml            ← 模块清单（核心）
│   │   ├── adapter.py             ← 启动/适配脚本（统一 REST 接口）
│   │   ├── requirements.txt       ← Python 依赖
│   │   └── README.md              ← 模块说明
│   ├── deep-filter/
│   │   ├── module.toml
│   │   └── bin/                   ← 原生二进制（按平台分子目录）
│   │       ├── windows-x86_64/deep-filter.exe
│   │       └── linux-x86_64/deep-filter
│   └── qwen3-asr/
│       ├── module.toml
│       ├── adapter.py
│       └── requirements.txt
├── runtime/                       ← 运行时环境（自动生成，不随发布）
│   └── venvs/                     ← 各模块的虚拟环境（uv 创建）
│       ├── faster-whisper/
│       └── qwen3-asr/
├── workspace/                     ← 管线任务临时文件
│   └── <task-id>/
└── logs/                          ← 运行日志

# 模型缓存目录（用户可自定义，默认在应用根目录下）
<model_cache_dir>/                 ← 由 config/app.toml [models].cache_dir 指定
├── faster-whisper-large-v3/
│   ├── model.bin
│   ├── config.json
│   └── .ep_meta.json              ← 轻量元数据（来源、版本，可删除）
├── qwen3-asr-1.7b/
└── ...
```

### 3.2 可移植性规则

| 规则 | 实现 |
|---|---|
| **零绝对路径** | 所有配置中的路径均相对于应用根目录，运行时解析为绝对路径 |
| **venv 不复制** | `runtime/venvs/` 在 `.gitignore` 中；复制到新机器后首次启动自动重建 |
| **模型目录可分离** | 模型缓存路径由用户在 `app.toml` 中指定（默认 `models/`）；用户可自行备份、迁移、共享模型目录 |
| **平台自适应** | 启动时检测 OS/arch，选择对应的二进制 |
| **无系统依赖** | 不写注册表、不装系统服务、不要求管理员权限 |
| **Python/uv 系统提供** | 不内嵌 Python 或 uv；调用系统 PATH 中的 `python3`/`uv`；缺失时引导用户安装 |

### 3.3 跨平台迁移流程

```
Windows → Linux:
1. 复制整个 EntryPoint/ 文件夹
2. 替换 entrypoint.exe 为 Linux 版 entrypoint 二进制
3. 启动 → 自动检测平台 → 重建 venvs → 检查模型完整性 → 就绪
```

---

## 4. 核心模块设计

### 4.1 ModuleSystem — 模块化插件系统

#### 4.1.1 Module Manifest (`module.toml`)

每个模块通过一份标准化的 `module.toml` 声明自身。这是整个系统的接入契约。

```toml
# modules/faster-whisper/module.toml

[module]
id = "faster-whisper"
name = "Faster-Whisper ASR"
version = "1.1.0"
description = "基于 CTranslate2 的高速语音识别，支持词级时间戳"
category = "asr"                    # asr | tts | denoise | ocr | image | translate | video | custom
genre = "whisper"                   # 同类模型分组标签，用于多模型对比（如 "whisper" / "qwen-asr"）
authors = ["EntryPoint Community"]
license = "MIT"

# ── 运行时 ──────────────────────────────────────────────
[runtime]
type = "python"                     # python | native | docker
python_version = ">=3.10,<3.13"     # 版本约束
requirements = "requirements.txt"   # 相对于模块目录
entrypoint = "adapter.py"           # 启动入口
# 启动命令模板，支持变量替换：
#   {root}       应用根目录
#   {module_dir} 模块目录
#   {model_dir}  该模块的模型目录
#   {port}       分配的端口
#   {device}     计算设备标识（如 "cuda:0", "cpu", "npu:0"）
start_command = "python {entrypoint} --port {port} --device {device} --model-dir {model_dir}"

# ── 计算后端 ────────────────────────────────────────────
[compute]
backends = ["cuda", "rocm", "openvino", "cpu"]   # 支持的后端，按优先级排序
default_backend = "cuda"
vram_estimate_mb = 4096             # 预估显存/内存占用
# 各后端的环境变量注入（可选覆盖）
[compute.env]
cuda = { CUDA_VISIBLE_DEVICES = "{device_index}" }
rocm = { HIP_VISIBLE_DEVICES = "{device_index}" }
openvino = { OPENVINO_DEVICE = "{device_name}" }

# ── 模型 ────────────────────────────────────────────────
[[models]]
id = "large-v3"
name = "Whisper Large V3"
source = "huggingface"              # huggingface | modelscope | url
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"   # 相对于 models/
# 可选：指定 revision/分支
# revision = "main"

[[models]]
id = "medium"
name = "Whisper Medium (轻量)"
source = "huggingface"
repo_id = "Systran/faster-whisper-medium"
target_dir = "faster-whisper-medium"

# ── 接口 ────────────────────────────────────────────────
[interface]
type = "http"                       # http | cli | python
# HTTP 类：
health_endpoint = "/health"         # 健康检查路径
ready_timeout_secs = 120            # 启动就绪超时
# 声明该模块对外暴露的能力（供管线引擎使用）
[[interface.capabilities]]
name = "transcribe"
description = "语音转文字（带时间戳）"
input_type = "audio"                # audio | video | image | text | json
output_type = "text"
# 参数 schema（JSON Schema 子集）
[interface.capabilities.params]
language = { type = "string", default = "auto", description = "语言代码或 auto" }
timestamps = { type = "boolean", default = true }

# ── CLI 类模块示例 ──────────────────────────────────────
# [interface]
# type = "cli"
# [[interface.capabilities]]
# name = "denoise"
# input_type = "audio"
# output_type = "audio"
# command = "{bin}/deep-filter {input} -o {output}"
```

#### 4.1.2 原生二进制模块示例

```toml
# modules/deep-filter/module.toml

[module]
id = "deep-filter"
name = "DeepFilter 音频降噪"
version = "0.5.6"
category = "denoise"

[runtime]
type = "native"
# 按平台指定二进制路径
[runtime.binaries]
windows-x86_64 = "bin/windows-x86_64/deep-filter.exe"
linux-x86_64 = "bin/linux-x86_64/deep-filter"

[compute]
backends = ["cpu"]                  # 纯 CPU 工具
default_backend = "cpu"

[interface]
type = "cli"
[[interface.capabilities]]
name = "denoise"
description = "AI 语音降噪"
input_type = "audio"
output_type = "audio"
command = "{binary} {input} -o {output}"
```

#### 4.1.3 模块生命周期

```
发现 → 校验 → 环境准备 → 模型下载 → 就绪 → 启动 → 运行 → 停止
 │       │        │           │
 │       │        │           └─ 从 HF/ModelScope 下载模型文件
 │       │        └─ 创建 venv + 安装依赖（uv）
 │       └─ 解析 module.toml，验证必填字段
 └─ 扫描 modules/ 目录
```

#### 4.1.4 模块接入文档（面向第三方开发者）

系统提供 `docs/MODULE_SPEC.md`，内容包括：
- `module.toml` 完整字段参考
- 各 `runtime.type` 的接入方式
- `interface.capabilities` 声明规范
- 适配器脚本编写指南（`adapter.py` 模板）
- 测试与调试方法
- 示例模块（faster-whisper、deep-filter）

### 4.2 ComputeManager — 异构计算设备管理

#### 4.2.1 设备抽象

```rust
/// 一个物理计算设备
struct ComputeDevice {
    id: DeviceId,                    // 全局唯一标识
    backend: ComputeBackend,
    name: String,                    // "NVIDIA RTX 4090" / "AMD RX 7900" / "Intel NPU"
    total_memory_mb: Option<u32>,    // 显存/内存（NPU 可能未知）
    used_memory_mb: Option<u32>,
    utilization: Option<u8>,         // 0-100%
    temperature: Option<u8>,
}

enum ComputeBackend {
    Cuda,        // NVIDIA
    Rocm,        // AMD
    OpenVINO,    // Intel CPU/GPU/NPU（当前 NPU 仅支持 Intel，trait 预留扩展）
    DirectML,    // Windows 通用 GPU 加速
    CPU,         // 纯 CPU（始终可用）
    // 未来扩展：QualcommNPU, AppleANE, ...
}

/// 设备标识，用于传递给子进程
enum DeviceId {
    Cuda(u32),           // GPU 索引
    Rocm(u32),
    OpenVINO(String),    // "GPU.0", "NPU.0"
    CPU,
}
```

#### 4.2.2 设备检测（多后端）

| 后端 | 检测方式 | 环境变量注入 |
|---|---|---|
| CUDA | `nvidia-smi --query-gpu=index,name,memory.total,memory.used,utilization.gpu,temperature.gpu --format=csv,noheader` | `CUDA_VISIBLE_DEVICES={index}` |
| ROCm | `rocm-smi --showid --showproductname --showmeminfo vram --showuse` | `HIP_VISIBLE_DEVICES={index}` |
| OpenVINO | Python: `openvino.Core().available_devices` 或 `benchmark_app --list_devices` | `OPENVINO_DEVICE={device}` |
| DirectML | Windows: `dxdiag` 或 Python `onnxruntime.get_available_providers()` | 由 ONNX Runtime 内部管理 |
| CPU | 始终可用，检测核心数/内存 | 无 |

检测策略：
- 启动时全量检测，结果缓存
- 后台定时刷新（2s 间隔）已检测到的设备状态
- 某后端检测工具不存在时静默跳过（如未装 NVIDIA 驱动则跳过 CUDA）
- 用户可在 `config/app.toml` 中手动禁用/启用特定后端

#### 4.2.3 设备分配

```rust
struct ComputeScheduler {
    devices: Vec<ComputeDevice>,
    assignments: HashMap<String, DeviceId>,   // module_id → device
    strategy: AssignStrategy,
}

enum AssignStrategy {
    Manual,              // 用户在 UI 中逐个指定
    LeastMemory,         // 自动选剩余显存最大的设备
    RoundRobin,          // 轮询分配
    SingleDevice(DeviceId),  // 全部跑在一个设备上
}
```

分配前检查：
- 模块声明的 `backends` 是否包含目标设备的 backend
- 预估显存是否超出剩余（警告但不阻止，用户可强制）
- 同一设备上已运行的模块数量（避免过多并发）

### 4.3 EnvManager — 虚拟环境管理

#### 4.3.1 职责

- 为每个 Python 类模块创建独立 venv
- 使用 **uv** 极速安装依赖（比 pip 快 10-100x）
- 跟踪依赖版本，检测 requirements.txt 变更
- 跨平台迁移时自动重建
- **不内嵌 Python 或 uv**，调用系统已安装的版本

#### 4.3.2 前置依赖检测

启动时检查系统环境：

```
1. 检测 uv:  which uv / where uv
   - 存在 → 记录路径，继续
   - 缺失 → 提示用户安装：
     - Windows: 弹窗 "需要安装 uv"，按钮自动打开浏览器
       → https://docs.astral.sh/uv/getting-started/installation/
       （或提示 winget install uv / pip install uv）
     - Linux: 提示 curl -LsSf https://astral.sh/uv/install.sh | sh
              或 apt install uv / pip install uv
   - uv 未安装时，所有 Python 类模块标记为 "环境未就绪"

2. 检测 python: python3 --version / python --version
   - 存在且版本满足 → 记录路径
   - 缺失 → 提示用户安装：
     - Windows: 弹窗 "需要安装 Python"，按钮自动打开
       → https://www.python.org/downloads/
       （或提示 winget install Python.Python.3.12）
     - Linux: 提示 sudo apt install python3 / sudo dnf install python3
   - 也可由 uv 代管: uv python install 3.12（安装到 uv 管理目录）
```

#### 4.3.3 venv 创建流程

```
模块启动（首次或依赖变更时）:
1. 检查 runtime/venvs/<module_id>/ 是否存在
2. 不存在 → uv venv --python <version> runtime/venvs/<module_id>/
3. 比对 requirements.txt 哈希与 .ep_deps_hash 标记文件
4. 不一致 → uv pip install -r requirements.txt --python <venv>/bin/python
5. 写入新哈希
6. 使用该 venv 的 python 启动模块
```

#### 4.3.4 Python 解释器来源（优先级）

1. 用户手动指定 — `config/app.toml` 中 `[python].path`
2. 系统 PATH — `python3` / `python`（版本需满足模块约束）
3. uv 管理 — `uv python install <version>`（安装到 uv 默认目录，非应用内）

#### 4.3.5 依赖锁定

- 首次安装后生成 `runtime/venvs/<module_id>/ep.lock`（`uv pip freeze` 输出）
- 后续启动优先使用 lock 文件精确还原
- 用户可手动触发"更新依赖"

### 4.4 ModelManager — 模型下载管理

#### 4.4.1 模型缓存目录

模型文件**不强制存放在应用目录内**，用户可在 `config/app.toml` 中指定：

```toml
[models]
cache_dir = "models"                    # 默认：应用根目录下的 models/
# cache_dir = "D:/AI_Models"           # Windows 示例：指定到独立磁盘
# cache_dir = "/data/shared-models"    # Linux 示例：多用户共享
```

- 支持绝对路径和相对路径（相对于应用根目录）
- 用户可自行备份、复制、共享该目录
- 多个 EntryPoint 实例可指向同一模型缓存目录（节省磁盘）
- 模型目录内的文件夹名即 `module.toml` 中声明的 `target_dir`

#### 4.4.2 下载源

| 源 | 下载方式 | 说明 |
|---|---|---|
| HuggingFace | `huggingface-hub` Python 库 (`snapshot_download`) | 标准方式，支持断点续传、镜像站 |
| ModelScope | `modelscope` Python 库 (`snapshot_download`) | 国内镜像，速度快 |
| 直链 URL | HTTP 下载 (reqwest) | 兜底方案，用于特殊文件 |

#### 4.4.3 下载流程

```
用户点击"下载模型" / 模块首次启动:
1. 读取 module.toml 中 [[models]] 声明
2. 检查 <cache_dir>/<target_dir>/ 是否已存在
   - 存在且有 .ep_meta.json → 校验来源信息，视为已下载
   - 存在但无 .ep_meta.json → 视为用户手动放置，直接使用（不校验）
   - 不存在 → 执行下载
3. 下载：在模块 venv 中执行
   - HuggingFace:
     python -c "from huggingface_hub import snapshot_download;
                snapshot_download(repo_id='...', local_dir='...')"
   - ModelScope: 类似
   - 支持 HF_ENDPOINT 环境变量（镜像站）
4. 下载进度实时回传 UI（解析 stdout）
5. 完成后写入 .ep_meta.json
```

#### 4.4.4 模型元数据（.ep_meta.json）

轻量元数据文件，记录模型来源信息。**用户可安全删除**——删除后系统视为手动放置的模型，直接使用，仅失去自动检查更新的能力。

```json
// <cache_dir>/faster-whisper-large-v3/.ep_meta.json
{
  "module_id": "faster-whisper",
  "model_id": "large-v3",
  "source": "huggingface",
  "repo_id": "Systran/faster-whisper-large-v3",
  "revision": "main",
  "downloaded_at": "2026-07-20T10:30:00Z",
  "total_size_bytes": 3094850000
}
```

设计取舍：
- **不记录逐文件哈希**：模型文件通常数 GB，逐文件校验成本高且意义有限（用户自行维护完整性）
- **仅记录来源**：核心目的是支持"检查更新"（对比远端 revision）和跨机器识别
- **可删除**：不阻塞正常使用，尊重用户对模型目录的完全控制权

### 4.5 ProcessManager — 进程管理器

```rust
struct ProcessManager {
    instances: HashMap<String, ServiceInstance>,
}

struct ServiceInstance {
    module_id: String,
    status: ServiceStatus,
    child: Option<tokio::process::Child>,
    device: Option<DeviceId>,
    port: Option<u16>,
    log_buffer: RingBuffer<String>,
    started_at: Option<DateTime<Utc>>,
}

enum ServiceStatus {
    Stopped,
    Preparing,       // 正在准备环境/下载模型
    Starting,        // 进程已启动，等待健康检查通过
    Running,
    Error(String),
}
```

**启动流程：**
1. 检查模块状态（依赖已安装？模型已下载？）
2. 分配计算设备 + 端口
3. 构建环境变量：
   - 计算设备相关（`CUDA_VISIBLE_DEVICES` / `HIP_VISIBLE_DEVICES` / ...）
   - `EP_PORT={port}`
   - `EP_MODEL_DIR={cache_dir}/{target_dir}`
   - `EP_ROOT={root}`
   - 模块 manifest 中声明的额外环境变量
4. 构建启动命令（变量替换）
5. 启动子进程，捕获 stdout/stderr
6. 轮询健康检查直到就绪或超时
7. 更新状态为 Running

**CLI 类模块**（如 DeepFilter）：
- 不常驻，由管线引擎按需调用
- 每次调用构建命令行，等待退出，收集输出

### 4.6 PortManager — 端口管理器

```rust
struct PortManager {
    range: (u16, u16),              // 默认 (18000, 19000)
    allocations: HashMap<String, u16>,
}
```

- 从配置范围内分配未占用端口
- 绑定前用 `TcpListener::bind` 验证可用性
- 通过 `EP_PORT` 环境变量传递给模块
- 模块 adapter 脚本需读取 `EP_PORT` 并监听该端口

### 4.7 PipelineEngine — DAG 管线引擎

#### 4.7.1 数据模型

```rust
struct Pipeline {
    id: String,
    name: String,
    description: String,
    nodes: Vec<PipelineNode>,
    edges: Vec<Edge>,
}

struct PipelineNode {
    id: String,
    module_id: String,              // 关联的模块（或 "builtin"）
    capability: String,             // 使用的能力名（如 "transcribe"）
    label: String,
    params: serde_json::Value,      // 参数（符合 capability 的 params schema）
    position: [f32; 2],
}

struct Edge {
    from_node: String,
    from_port: String,              // 输出端口（"output" 或具名）
    to_node: String,
    to_port: String,                // 输入端口（"input" 或具名）
}
```

#### 4.7.2 节点类型

```rust
enum NodeKind {
    /// 调用已注册模块的 capability
    Module { module_id: String, capability: String },
    /// 内置工具节点
    Builtin(BuiltinNode),
    /// 外部 API 调用（如 LLM 翻译）
    ExternalApi { endpoint: String, config: serde_json::Value },
}

enum BuiltinNode {
    FileInput,                      // 文件输入源
    FileOutput,                     // 文件输出
    FFmpeg { args_template: String }, // FFmpeg 命令
    SRTExport,                      // 字幕导出
    TextConcat,                     // 文本拼接
    JsonTransform,                  // JSON 变换
}
```

#### 4.7.3 执行引擎

```rust
struct PipelineTask {
    id: String,
    pipeline: Pipeline,
    status: TaskStatus,
    node_states: HashMap<String, NodeState>,
    artifacts: HashMap<String, Artifact>,   // node_id → 输出
    work_dir: PathBuf,                       // workspace/<task_id>/
}

enum NodeState {
    Pending,
    WaitingDeps,
    Running { progress: Option<f32> },
    Completed { artifact: Artifact },
    Failed { error: String, retryable: bool },
    Skipped,
}

enum Artifact {
    File(PathBuf),
    Text(String),
    Json(serde_json::Value),
}
```

**执行流程：**
1. 验证 DAG（无环、端口类型兼容、模块已安装且模型已下载）
2. 拓扑排序 → 分层
3. 同层节点 `tokio::spawn` 并行执行
4. HTTP 类模块：调用其 capability 对应的 API
5. CLI 类模块：构建命令行，等待完成
6. 外部 API：reqwest 调用
7. 输出写入 `workspace/<task_id>/<node_id>/`
8. 下游节点从上游输出目录读取输入
9. 失败处理：标记 Failed，下游 Skipped，支持单节点重试

#### 4.7.4 示例管线：视频 → SRT 字幕

```
[FileInput] ──video──▶ [FFmpeg 提取音频] ──audio──▶ [DeepFilter 降噪]
                                                         │
                                                       audio
                                                         │
                                                         ▼
[SRTExport] ◀──text── [LLM 翻译] ◀──text── [Faster-Whisper ASR]
```

管线 TOML：

```toml
# config/pipelines/video_to_srt.toml
[pipeline]
id = "video-to-srt"
name = "视频转字幕"
description = "提取音频 → 降噪 → ASR → 翻译 → SRT"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
label = "输入视频"

[[nodes]]
id = "extract"
kind = "builtin"
builtin = "ffmpeg"
label = "提取音频"
params = { args = "-i {input} -vn -acodec pcm_s16le -ar 16000 -ac 1 {output}" }

[[nodes]]
id = "denoise"
kind = "module"
module_id = "deep-filter"
capability = "denoise"
label = "AI 降噪"

[[nodes]]
id = "asr"
kind = "module"
module_id = "faster-whisper"
capability = "transcribe"
label = "语音识别"
params = { language = "auto", timestamps = true }

[[nodes]]
id = "translate"
kind = "external_api"
label = "LLM 翻译"
params = {
    api_base = "https://api.siliconflow.cn/v1",
    model = "deepseek-ai/DeepSeek-V3",
    target_lang = "zh",
    prompt_template = "将以下字幕文本翻译为{target_lang}，保持时间戳格式：\n{text}"
}

[[nodes]]
id = "srt"
kind = "builtin"
builtin = "srt_export"
label = "导出 SRT"
params = { max_chars_per_line = 42 }

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
```

### 4.8 ConfigStore — 配置持久化

```toml
# config/app.toml

[general]
language = "zh-CN"
theme = "dark"

[compute]
strategy = "least_memory"           # manual | least_memory | round_robin | single
disabled_backends = []              # 可禁用特定后端，如 ["directml"]

[ports]
range_start = 18000
range_end = 19000

[models]
cache_dir = "models"                    # 模型缓存目录（相对或绝对路径）
hf_endpoint = "https://hf-mirror.com"   # HuggingFace 镜像（可选）
default_source = "huggingface"          # huggingface | modelscope

[python]
path = ""                           # 留空则自动检测系统 PATH
# path = "C:/Python312/python.exe"  # Windows 手动指定
# path = "/usr/bin/python3"         # Linux 手动指定
```

---

## 5. UI 页面规划

> WebUI 视觉与交互规范见 [docs/DESIGN_SYSTEM.md](docs/DESIGN_SYSTEM.md)（唯一权威参考）。
> 以下页面已在 WebUI 中实现（`crates/ep-webui/frontend/src/pages/`），桌面端（egui）保持同步。

### 5.1 仪表盘 (Dashboard) — ✅ 已实现
- 计算设备卡片：每个设备的名称、后端类型、显存/内存用量、利用率、温度
- 模块状态概览：运行中 / 已停止 / 错误 / 未安装
- 最近管线任务
- 统计数字：设备数、模块数、运行中数量

### 5.2 模块管理 (Modules) — ✅ 已实现
- 模块列表（按 category 分组，同 genre 内聚合显示）
- 每个模块卡片：
  - 状态徽章（StatusBadge 组件，含过渡态脉冲动画）
  - 模型下载状态 + 进度条
  - 计算设备分配下拉框
  - 启动 / 停止按钮（乐观更新 + 确认对话框）
  - 日志查看器（LogViewer 组件，等宽字体，自动滚动）
- **模块详情页** (`/modules/:id`)：完整配置、日志、模型信息
- **多模型对比**：同 genre 的模块可勾选多个同时运行；
  管线中用户手动放置多个同 genre 节点接同一输入源，任务详情页并列展示各节点输出
- 安装新模块（拖入文件夹 / 指定路径）

### 5.3 管线编辑器 (Pipeline Editor) — ✅ 已实现
- 基于 @xyflow/react (React Flow) 的节点画布：拖拽、连线、删除
- 左侧面板：可用节点列表（已安装模块的 capabilities + 内置节点）
- 右侧面板：选中节点的参数编辑
- 运行时状态着色：灰=等待 / 蓝=运行 / 绿=完成 / 红=失败
- 保存 / 加载 / 导入 / 导出

### 5.4 任务中心 (Tasks) — ✅ 已实现
- 管线任务列表：状态、进度、耗时
- 任务详情：各节点执行状态 + 日志
- 输出文件浏览 / 打开所在目录

### 5.5 模型管理 (Models) — ✅ 已实现
- 模型列表：按模块分组，显示来源、大小、下载状态
- 模型下载 / 导入 / 删除
- 下载进度实时反馈

### 5.6 设置 (Settings) — ✅ 已实现
- 服务器配置（端口、公开访问）
- 计算设备策略
- 端口范围
- 模型缓存目录（路径选择器）
- HuggingFace 镜像
- Python / uv 路径（显示检测结果，缺失时提供安装引导）
- 语言 / 主题（深色/浅色切换）

---

## 6. 模块接入规范（摘要）

> 完整规范见 `docs/MODULE_SPEC.md`（待编写）

### 6.1 接入清单

第三方开发者接入一个新模块需要：

1. **创建模块目录** `modules/<module-id>/`
2. **编写 `module.toml`**（必填字段见 §4.1.1）
3. **提供启动入口**：
   - Python 模块：`adapter.py`（读取 `EP_PORT`、`EP_DEVICE`、`EP_MODEL_DIR` 环境变量）
   - 原生模块：提供各平台二进制
4. **声明 capabilities**：输入/输出类型、参数 schema
5. **声明模型来源**：HuggingFace / ModelScope repo_id
6. **（可选）编写测试**

### 6.2 Adapter 脚本约定

Python adapter 需遵守的环境变量契约：

| 环境变量 | 说明 | 示例 |
|---|---|---|
| `EP_ROOT` | 应用根目录 | `/home/user/EntryPoint` |
| `EP_MODULE_DIR` | 模块目录 | `.../modules/faster-whisper` |
| `EP_MODEL_DIR` | 模型目录 | `.../models/faster-whisper-large-v3` |
| `EP_PORT` | 分配端口 | `18001` |
| `EP_DEVICE` | 计算设备标识 | `cuda:0` / `cpu` / `npu:0` |
| `EP_DEVICE_INDEX` | 设备索引 | `0` |
| `EP_BACKEND` | 计算后端 | `cuda` / `rocm` / `openvino` / `cpu` |

### 6.3 Adapter 统一 REST 接口

**核心设计**：无论底层工具使用 Gradio 3.x/4.x/5.x、Flask、FastAPI 还是其他框架，
每个 Python 模块的 `adapter.py` 负责将其包装为**统一的 REST 接口**。
EntryPoint 核心只与这套标准接口通信，不感知底层框架差异。

#### 标准端点

| 端点 | 方法 | 说明 |
|---|---|---|
| `/health` | GET | 健康检查，返回 `{"status": "ok"}` |
| `/info` | GET | 模块信息（名称、版本、capabilities 列表） |
| `/predict/<capability>` | POST | 调用指定能力，请求体为 JSON 参数 |
| `/predict/<capability>` | POST (multipart) | 文件上传类调用（音频/图片/视频） |

#### 请求/响应格式

```
POST /predict/transcribe
Content-Type: multipart/form-data

file: <audio.wav>
params: {"language": "auto", "timestamps": true}

→ 200 OK
{
  "status": "completed",
  "output_type": "text",
  "result": "识别文本内容...",
  "metadata": {
    "segments": [
      {"start": 0.0, "end": 2.5, "text": "你好世界"}
    ]
  }
}
```

```
POST /predict/denoise
Content-Type: multipart/form-data

file: <noisy.wav>

→ 200 OK
{
  "status": "completed",
  "output_type": "audio",
  "result": "/workspace/task-1/denoise/output.wav"
}
```

#### Adapter 模板（Python 伪代码）

```python
# adapter.py — 模块适配器模板
import os, json
from fastapi import FastAPI, UploadFile, Form
from fastapi.responses import JSONResponse
import uvicorn

app = FastAPI()

# 从环境变量获取配置
PORT = int(os.environ.get("EP_PORT", 18000))
MODEL_DIR = os.environ.get("EP_MODEL_DIR", "")
DEVICE = os.environ.get("EP_DEVICE", "cpu")

# 加载底层模型（各模块自行实现）
model = None

@app.on_event("startup")
def load():
    global model
    model = load_model(MODEL_DIR, DEVICE)  # 模块自定义

@app.get("/health")
def health():
    return {"status": "ok"}

@app.get("/info")
def info():
    return {"name": "...", "version": "...", "capabilities": ["transcribe"]}

@app.post("/predict/{capability}")
async def predict(capability: str, file: UploadFile = None, params: str = "{}"):
    params = json.loads(params)
    if capability == "transcribe":
        result = model.transcribe(file.file, **params)
        return {"status": "completed", "output_type": "text", "result": result}
    return {"status": "error", "message": f"Unknown capability: {capability}"}

if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=PORT)
```

#### 设计要点

- **Gradio 版本差异在 adapter 内部消化**：adapter 可以选择直接调用底层 Python API（绕过 Gradio），
  或启动 Gradio 后通过内部 HTTP 调用再转发——对上层透明
- **CLI 类模块无需 adapter**：由管线引擎直接构建命令行调用
- **FastAPI + uvicorn 作为推荐 adapter 框架**：轻量、异步、自带 OpenAPI 文档；
  已包含在模块的 `requirements.txt` 中
- **文件传递**：输入文件通过 multipart 上传或传入文件路径（`params.input_path`）；
  输出文件写入工作目录并返回路径

### 6.4 Capability 类型系统

管线引擎通过 input_type / output_type 验证连线合法性：

| 类型 | 说明 | 传递方式 |
|---|---|---|
| `audio` | 音频文件 (wav/mp3/flac) | 文件路径 |
| `video` | 视频文件 | 文件路径 |
| `image` | 图片文件 | 文件路径 |
| `text` | 纯文本 / 带时间戳文本 | 字符串或文件 |
| `json` | 结构化数据 | JSON 值 |
| `file` | 任意文件 | 文件路径 |

---

## 7. 开发阶段

### Phase 1 — 骨架 + 模块系统 (Foundation)
- [ ] Cargo workspace 初始化
- [ ] egui 应用骨架（窗口、导航、页面切换）
- [ ] Module Manifest 解析器（`module.toml` 读取/验证）
- [ ] 模块发现（扫描 `modules/` 目录）
- [ ] 配置系统（`app.toml`）
- [ ] 计算设备检测（先实现 CUDA + CPU，其他后端预留 trait）

### Phase 2 — 环境 + 模型管理 (Environment)
- [ ] uv 集成（venv 创建、依赖安装）
- [ ] Python 解释器管理（内嵌/系统/自动安装）
- [ ] 模型下载器（HuggingFace Hub + ModelScope）
- [ ] 下载进度 UI
- [ ] 模型完整性校验

### Phase 3 — 服务管理 (Service Management)
- [ ] ProcessManager（启动/停止/重启）
- [ ] 环境变量注入（多后端）
- [ ] 健康检查
- [ ] 日志捕获 + UI
- [ ] 端口管理
- [ ] 计算设备分配

### Phase 4 — 管线引擎 (Pipeline Engine)
- [ ] DAG 数据结构 + 验证
- [ ] 拓扑排序 + 并行执行
- [ ] 内置节点（FFmpeg、SRT、FileIO）
- [ ] 模块 capability 调用（HTTP / CLI）
- [ ] 外部 API 节点（LLM 翻译）
- [ ] 任务状态 + 进度

### Phase 5 — 管线编辑器 UI (Pipeline Editor)
- [ ] egui 节点画布
- [ ] 拖拽连线
- [ ] 参数面板（从 schema 自动生成）
- [ ] 管线 TOML 保存/加载
- [ ] 运行时可视化

### Phase 6 — 打磨 + 扩展 (Polish)
- [ ] ROCm / OpenVINO / DirectML 后端实现
- [ ] Linux 全面适配
- [ ] 模块接入文档 `MODULE_SPEC.md`
- [ ] 示例模块（faster-whisper、deep-filter、qwen3-asr）
- [ ] 错误处理 + 用户引导
- [ ] 国际化（中/英）

---

## 8. 项目结构

```
EntryPoint/
├── Cargo.toml                     # workspace root
├── DESIGN.md
├── README.md
├── docs/
│   └── MODULE_SPEC.md             # 模块接入规范（面向第三方）
├── config/                        # 默认配置模板
│   ├── app.toml
│   └── pipelines/
├── modules/                       # 模块目录
│   ├── faster-whisper/
│   ├── deep-filter/
│   └── qwen3-asr/
├── crates/
│   ├── ep-core/                   # 核心逻辑（无 UI 依赖）
│   │   └── src/
│   │       ├── module/            # 模块系统
│   │       │   ├── manifest.rs    # module.toml 解析
│   │       │   ├── discovery.rs   # 模块发现
│   │       │   └── lifecycle.rs   # 生命周期管理
│   │       ├── compute/           # 计算设备
│   │       │   ├── mod.rs         # ComputeManager trait
│   │       │   ├── cuda.rs
│   │       │   ├── rocm.rs
│   │       │   ├── openvino.rs
│   │       │   └── cpu.rs
│   │       ├── env/               # 环境管理
│   │       │   ├── venv.rs        # venv 创建/管理
│   │       │   └── python.rs      # Python 解释器管理
│   │       ├── model/             # 模型管理
│   │       │   ├── download.rs    # 下载器
│   │       │   ├── huggingface.rs
│   │       │   └── modelscope.rs
│   │       ├── process.rs         # ProcessManager
│   │       ├── port.rs            # PortManager
│   │       ├── pipeline/          # DAG 引擎
│   │       │   ├── dag.rs
│   │       │   ├── executor.rs
│   │       │   └── nodes/
│   │       ├── config.rs
│   │       └── lib.rs
│   ├── ep-desktop/                # egui 桌面前端
│   │   └── src/
│   │       ├── app.rs
│   │       ├── pages/
│   │       │   ├── dashboard.rs
│   │       │   ├── modules.rs
│   │       │   ├── pipeline_editor.rs
│   │       │   ├── tasks.rs
│   │       │   └── settings.rs
│   │       ├── widgets/
│   │       └── main.rs
│   └── ep-webui/                  # WebUI（axum 静态服务 + React 前端）
│       ├── src/
│       │   └── lib.rs             # 静态资源嵌入
│       ├── static/                # 构建产物（vite build 输出）
│       └── frontend/              # React 前端源码
│           ├── src/
│           │   ├── api/           # REST 客户端 + WebSocket 管理器
│           │   ├── components/    # 布局 + shadcn/ui + 共享业务组件
│           │   ├── hooks/         # 自定义 hooks（轮询、WS 状态）
│           │   ├── lib/           # 工具函数 + 常量
│           │   ├── pages/         # 路由页面（7 个业务页 + 404）
│           │   ├── store/         # Zustand 状态（主题）
│           │   ├── App.tsx        # 布局 + 路由
│           │   └── main.tsx       # 入口
│           ├── package.json
│           └── vite.config.ts
├── scripts/
│   ├── build.sh                   # 统一构建（cargo + npm）
│   ├── install-service.sh         # systemd 服务安装
│   ├── entrypoint.service         # systemd unit 文件
│   └── start.sh                   # 开发启动脚本
└── assets/
```

---

## 9. 开放问题

| # | 问题 | 决定 |
|---|---|---|
| 1 | ~~uv 是否内嵌~~ | **不内嵌**，调用系统 uv，缺失时引导安装 |
| 2 | ~~Python 内嵌方案~~ | **不内嵌**，系统 PATH 优先，回退 uv python install |
| 3 | ~~NPU 支持范围~~ | 当前仅 Intel (OpenVINO)，ComputeBackend trait 预留扩展 |
| 4 | ~~Gradio API 适配~~ | Adapter 层统一 REST 接口，核心不感知 Gradio 版本 |
| 5 | ~~模型离线导入~~ | 支持：用户手动放入模型目录，无 .ep_meta.json 时直接使用 |
| 6 | ~~多实例~~ | 支持 module_id + instance_id |
| 7 | ~~Docker 支持~~ | 暂不需要，优先原生进程管理 |
| 8 | ~~genre 对比模式~~ | **手动多节点**：DAG 天然支持一对多扇出，用户手动放置同 genre 的多个节点并接同一输入源，结果在任务详情页并列展示。不引入额外节点类型 |
| 9 | ~~缓存目录迁移~~ | **不迁移**：更换 cache_dir 后系统只在新路径查找，找不到则提示下载。旧文件由用户自行处理 |
| 10 | ~~adapter 依赖归属~~ | **模块自行声明**：每个模块的 requirements.txt 包含 fastapi/uvicorn 等 adapter 依赖，venv 完全独立 |

> 所有开放问题已关闭。后续实现中如遇新设计决策，追加至此表。
