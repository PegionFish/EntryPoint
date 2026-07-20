# EntryPoint — AI 工具管控平台 设计文档

> 状态：草案 v0.1 | 日期：2026-07-20

## 1. 项目概述

EntryPoint 是一个 **Windows/Linux 原生桌面应用**，用于统一管控本地部署的多个 AI 工具/模型服务。

核心能力：
- **多 GPU 调度**：将不同模型分配到不同 GPU 并行运行，或在单 GPU 上串行复用
- **服务生命周期管理**：启动 / 停止 / 重启 / 健康监控 / 日志查看
- **模型管理**：按工具独立配置、下载、更新
- **DAG 管线引擎**：将多个工具串联为有向无环图工作流（如：视频 → 提取音频 → 降噪 → ASR → 翻译 → SRT）

## 2. 技术栈

| 层级 | 选型 | 说明 |
|---|---|---|
| GUI | **egui + eframe** | 纯 Rust 即时模式 GUI，wgpu/OpenGL 自绘渲染，跨平台 |
| 后端 | **Rust** | 进程管理、GPU 调度、管线引擎、HTTP 客户端 |
| 节点编辑器 | **egui_node_graph** (或自研) | DAG 可视化编辑 |
| 序列化 | **serde + serde_json / toml** | 配置文件、管线定义 |
| 异步运行时 | **tokio** | 异步进程管理、HTTP 请求、并发任务 |
| HTTP 客户端 | **reqwest** | 调用各工具的 HTTP API、外部 LLM API |
| GPU 检测 | **nvidia-smi** (子进程调用) | 检测 GPU 列表、显存占用、利用率 |
| 日志 | **tracing + tracing-subscriber** | 结构化日志 |

## 3. 系统架构

```
┌─────────────────────────────────────────────────────────┐
│                    egui 前端 (UI 层)                      │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────────┐  │
│  │ 仪表盘   │ │ 服务管理 │ │ 管线编辑 │ │ 模型管理   │  │
│  │Dashboard │ │ Services │ │ Pipeline │ │ Models     │  │
│  └────┬─────┘ └────┬─────┘ └────┬─────┘ └─────┬──────┘  │
│       └─────────────┴────────────┴─────────────┘         │
│                         │                                │
│              ┌──────────▼──────────┐                     │
│              │   AppState (共享)    │                     │
│              └──────────┬──────────┘                     │
└─────────────────────────┼───────────────────────────────┘
                          │
┌─────────────────────────▼───────────────────────────────┐
│                  Rust 后端 (Core 层)                      │
│                                                          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐  │
│  │ Process     │  │ Gpu         │  │ Port            │  │
│  │ Manager     │  │ Scheduler   │  │ Manager         │  │
│  │ 进程生命周期 │  │ GPU 分配/监控│  │ 端口分配/冲突检测│  │
│  └──────┬──────┘  └──────┬──────┘  └────────┬────────┘  │
│         └────────────────┴──────────────────┘            │
│                          │                               │
│  ┌─────────────┐  ┌─────▼───────┐  ┌─────────────────┐  │
│  │ Model       │  │ Pipeline    │  │ Config          │  │
│  │ Registry    │  │ Engine      │  │ Store           │  │
│  │ 工具目录/版本│  │ DAG 执行引擎│  │ 持久化配置      │  │
│  └─────────────┘  └─────────────┘  └─────────────────┘  │
│                                                          │
└──────────────────────────┬───────────────────────────────┘
                           │
┌──────────────────────────▼───────────────────────────────┐
│                  外部工具 / 子进程                         │
│                                                          │
│  DeepFilter(CLI)  faster-whisper(HTTP)  Qwen3-ASR(HTTP)  │
│  Qwen3-TTS(HTTP)  ComfyUI(HTTP)  RemBg(HTTP)  FFmpeg    │
│  外部 LLM API (OpenAI / SiliconFlow / Ollama ...)        │
│  ...                                                     │
└──────────────────────────────────────────────────────────┘
```

## 4. 核心模块设计

### 4.1 ProcessManager — 进程管理器

负责所有 AI 工具子进程的生命周期。

```rust
struct ProcessManager {
    instances: HashMap<String, ServiceInstance>,  // tool_id → 实例
}

struct ServiceInstance {
    tool_id: String,
    status: ServiceStatus,       // Stopped / Starting / Running / Error
    child: Option<tokio::process::Child>,
    gpu_id: Option<u32>,         // 分配的 GPU 编号
    port: u16,                   // 实际监听端口
    config: ToolConfig,          // 运行时配置
    log_buffer: RingBuffer<String>, // 最近 N 行日志
}

enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Error(String),
}
```

**关键行为：**
- 启动时注入环境变量：`CUDA_VISIBLE_DEVICES=<gpu_id>`、`PORT=<port>`、`HF_HUB_OFFLINE=1`
- 对 HTTP 类工具：启动后轮询 `GET /` 或指定 health endpoint 直到就绪
- 对 CLI 类工具（如 DeepFilter）：按需启动，任务完成即退出
- 对 Docker 类工具：调用 `docker-compose up -d` / `down`
- 捕获 stdout/stderr 写入环形日志缓冲区
- 进程意外退出时更新状态为 Error 并通知 UI

### 4.2 GpuScheduler — GPU 调度器

```rust
struct GpuScheduler {
    gpus: Vec<GpuInfo>,
    assignments: HashMap<String, u32>,  // tool_id → gpu_id
}

struct GpuInfo {
    id: u32,
    name: String,           // e.g. "NVIDIA RTX 4090"
    total_vram_mb: u32,
    used_vram_mb: u32,      // 定期刷新
    utilization: u8,         // 0-100%
}
```

**调度策略：**
- 通过 `nvidia-smi --query-gpu=... --format=csv` 定期采集（2s 间隔）
- 分配模式：
  - **手动**：用户在 UI 中为每个工具指定 GPU
  - **自动（最小显存占用）**：选择当前 used_vram 最低的 GPU
  - **自动（轮询）**：依次分配
- 单 GPU 模式：所有工具共享 GPU 0，串行启动（避免 OOM）
- 显存预估：每个 ToolDefinition 声明 `vram_estimate_mb`，分配前检查剩余显存

### 4.3 PortManager — 端口管理器

解决现有工具端口冲突问题（6 个工具默认 7860）。

```rust
struct PortManager {
    base_port: u16,          // 起始端口，默认 18000
    allocations: HashMap<String, u16>,  // tool_id → port
}
```

- 启动时扫描已分配端口，避免与系统服务冲突
- 每个工具分配唯一端口，通过环境变量或命令行参数传入
- 对于不支持自定义端口的工具，使用反向代理或跳过

### 4.4 ModelRegistry — 模型/工具注册表

```rust
struct ToolDefinition {
    id: String,                    // "faster-whisper-offline"
    name: String,                  // "Faster-Whisper 离线字幕"
    category: ToolCategory,        // ASR / TTS / ImageGen / VideoGen / OCR / ...
    runtime: RuntimeType,          // PythonEmbedded / NativeExe / Docker / SystemPython
    gpu_required: bool,
    vram_estimate_mb: Option<u32>,
    default_port: Option<u16>,
    start_command: StartCommand,   // 启动命令模板
    health_check: HealthCheck,     // HTTP GET / TCP / ProcessAlive
    config_schema: ConfigSchema,   // 可配置项定义
    install_path: PathBuf,         // 工具安装路径
    version: Option<String>,
    update_source: Option<UpdateSource>,  // 更新来源（URL / Git）
}

enum ToolCategory {
    AudioDenoise,
    ASR,
    TTS,
    ImageGeneration,
    VideoGeneration,
    ImageProcessing,   // RemBg, SAM, IOPaint
    OCR,
    FaceProcessing,    // FaceFusion, LatentSync
    Translation,
    Custom,
}

enum RuntimeType {
    PythonEmbedded { python_dir: PathBuf, script: PathBuf },
    NativeExe { exe_path: PathBuf },
    Docker { compose_file: PathBuf },
    SystemPython { script: PathBuf },
}

enum StartCommand {
    Bat(PathBuf),                    // Windows .bat
    Shell(PathBuf),                  // Linux .sh
    Direct { program: String, args: Vec<String> },
    DockerCompose { file: PathBuf },
}

enum HealthCheck {
    HttpGet { url: String, timeout_secs: u32 },
    TcpConnect { port: u16 },
    ProcessAlive,
    None,  // CLI 工具，按需运行
}
```

注册表以 **TOML 文件** 存储在 `config/tools.toml`，用户可手动编辑添加自定义工具。

### 4.5 PipelineEngine — DAG 管线引擎

#### 数据模型

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
    node_type: NodeType,
    label: String,
    config: serde_json::Value,     // 节点参数
    position: [f32; 2],            // 编辑器中的位置
}

struct Edge {
    from_node: String,
    from_port: String,     // 输出端口名，如 "audio_out"
    to_node: String,
    to_port: String,       // 输入端口名，如 "audio_in"
}

enum NodeType {
    // 内置节点
    FFmpegExtractAudio,    // 视频 → 音频
    FFmpegMergeAV,         // 音频 + 视频 → 合并
    AudioDenoise,          // DeepFilter 降噪
    ASRTranscribe,         // 语音 → 文字/时间戳
    LLMTranslate,          // 调用外部 LLM API 翻译
    SRTExport,             // 生成 SRT 字幕文件
    TTSGenerate,           // 文字 → 语音
    ComfyUIWorkflow,       // 调用 ComfyUI API
    RemBgRemove,           // 去除背景
    // 通用节点
    GenericHTTP,           // 任意 HTTP 调用
    GenericCLI,            // 任意命令行
    FileInput,             // 文件输入源
    FileOutput,            // 文件输出终点
}
```

#### 执行引擎

```rust
struct PipelineEngine {
    running_tasks: HashMap<String, PipelineTask>,
}

struct PipelineTask {
    pipeline_id: String,
    status: TaskStatus,
    node_states: HashMap<String, NodeState>,
    artifacts: HashMap<String, PathBuf>,  // node_id → 输出文件路径
    started_at: DateTime<Utc>,
}

enum NodeState {
    Pending,
    Running,
    Completed { output: NodeOutput },
    Failed { error: String },
    Skipped,
}

enum NodeOutput {
    File(PathBuf),
    Text(String),
    Json(serde_json::Value),
    Multiple(Vec<(String, PathBuf)>),  // 多输出端口
}
```

**执行流程：**
1. 验证 DAG（无环检测、端口类型匹配）
2. 拓扑排序，得到执行层级
3. 同层节点并行执行（tokio::spawn）
4. 每个节点执行前检查依赖节点的输出
5. 文件类输出写入临时工作目录 `workspace/<task_id>/`
6. 节点失败时：标记失败，下游节点标记 Skipped，可选重试
7. 全部完成后清理临时文件（或保留供用户检查）

#### 示例管线：视频 → SRT 字幕

```
[FileInput] ──video──▶ [FFmpegExtractAudio] ──audio──▶ [AudioDenoise]
                                                            │
                                                          audio
                                                            │
                                                            ▼
[SRTExport] ◀──text── [LLMTranslate] ◀──text── [ASRTranscribe]
```

节点配置示例：
- `FFmpegExtractAudio`: `{ "format": "wav", "sample_rate": 16000 }`
- `AudioDenoise`: `{ "tool": "deep-filter", "model": "default" }`
- `ASRTranscribe`: `{ "service": "qwen3-asr-1.7b", "language": "auto", "timestamps": true }`
- `LLMTranslate`: `{ "api_base": "https://api.siliconflow.cn/v1", "model": "deepseek-v3", "target_lang": "zh" }`
- `SRTExport`: `{ "max_chars_per_line": 42 }`

### 4.6 ConfigStore — 配置持久化

```
config/
├── app.toml              # 全局设置（语言、主题、默认 GPU 策略等）
├── tools.toml            # 工具注册表
├── gpu_assignments.toml  # GPU 分配方案
└── pipelines/            # 管线定义
    ├── video_to_srt.toml
    └── ...
```

## 5. UI 页面规划

### 5.1 仪表盘 (Dashboard)
- GPU 卡片：每块 GPU 的名称、显存用量（进度条）、利用率、温度
- 服务状态列表：运行中 / 已停止 / 错误，一键启停
- 最近任务日志

### 5.2 服务管理 (Services)
- 工具列表（按类别分组）
- 每个工具：启动/停止按钮、GPU 分配下拉框、端口显示、配置面板
- 日志查看器（实时滚动）
- 批量操作：全部启动 / 全部停止

### 5.3 管线编辑器 (Pipeline Editor)
- 节点画布：拖拽添加节点、连线、删除
- 节点参数面板：选中节点后编辑配置
- 工具栏：保存 / 加载 / 运行 / 停止
- 运行时：节点颜色表示状态（灰=等待、蓝=运行中、绿=完成、红=失败）

### 5.4 模型管理 (Models)
- 已安装工具列表 + 版本
- 检查更新 / 下载更新
- 安装新工具（从本地 ZIP 或 URL）
- 配置编辑器

### 5.5 任务队列 (Tasks)
- 管线任务列表：状态、进度、耗时
- 任务详情：每个节点的执行状态和日志
- 输出文件浏览

## 6. 与现有工具的集成方式

| 工具 | 集成方式 | 说明 |
|---|---|---|
| DeepFilter | CLI 调用 | `deep-filter.exe input.wav -o output.wav` |
| faster-whisper / whisperx | HTTP API (Gradio) | 调用 Gradio `/api/predict` 或 `/run/predict` |
| Qwen3-ASR 0.6B / 1.7B | HTTP API (Gradio) | 同上 |
| Qwen3-ASR-Stream | HTTP API (Flask) | `/api/start` → `/api/chunk` → `/api/finish` |
| Qwen3-TTS | HTTPS API (Gradio) | 需忽略自签名证书 |
| ComfyUI | HTTP API | `/prompt` 提交工作流，`/history` 查询结果 |
| RemBg | HTTP API (Gradio) | Gradio API 调用 |
| SD-WebUI A1111 | HTTP API | `/sdapi/v1/txt2img` 等 |
| LLM-Translator | 不直接集成 | 管线中用 LLMTranslate 节点替代 |
| FFmpeg | CLI 调用 | 系统安装或工具自带 |
| 外部 LLM API | HTTP 客户端 | OpenAI 兼容 `/v1/chat/completions` |

## 7. 开发阶段

### Phase 1 — 骨架 (Foundation)
- [ ] Cargo 项目初始化（workspace 结构）
- [ ] egui 应用骨架：窗口、侧边栏导航、页面切换
- [ ] GPU 检测模块（nvidia-smi 解析）
- [ ] 配置系统（app.toml 读写）

### Phase 2 — 服务管理 (Service Management)
- [ ] ProcessManager：启动/停止/重启子进程
- [ ] 环境变量注入（CUDA_VISIBLE_DEVICES, PORT）
- [ ] 健康检查（HTTP 轮询 / 进程存活）
- [ ] 日志捕获与 UI 显示
- [ ] 工具注册表（tools.toml）
- [ ] 端口管理器

### Phase 3 — 管线引擎 (Pipeline Engine)
- [ ] DAG 数据结构 + 验证（环检测、类型检查）
- [ ] 拓扑排序 + 并行执行
- [ ] 内置节点实现：FFmpeg、Denoise、ASR、Translate、SRT
- [ ] 任务状态追踪 + 进度回调
- [ ] 临时工作目录管理

### Phase 4 — 管线编辑器 (Pipeline Editor UI)
- [ ] egui 节点画布（拖拽、连线、删除）
- [ ] 节点参数面板
- [ ] 管线保存/加载（TOML）
- [ ] 运行时状态可视化

### Phase 5 — 模型管理 + 打磨 (Polish)
- [ ] 模型/工具版本检查
- [ ] 下载管理器（进度条、断点续传）
- [ ] Linux 适配（.sh 脚本、路径分隔符、nvidia-smi）
- [ ] 错误处理完善
- [ ] 国际化（中/英）

## 8. 项目结构（预期）

```
EntryPoint/
├── Cargo.toml                  # workspace root
├── DESIGN.md                   # 本文档
├── README.md
├── config/                     # 默认配置模板
│   ├── app.toml
│   └── tools.toml
├── crates/
│   ├── ep-core/                # 核心逻辑（无 UI 依赖）
│   │   ├── src/
│   │   │   ├── process.rs      # ProcessManager
│   │   │   ├── gpu.rs          # GpuScheduler
│   │   │   ├── port.rs         # PortManager
│   │   │   ├── registry.rs     # ModelRegistry / ToolDefinition
│   │   │   ├── pipeline/       # DAG 引擎
│   │   │   │   ├── mod.rs
│   │   │   │   ├── dag.rs
│   │   │   │   ├── executor.rs
│   │   │   │   └── nodes/      # 各节点实现
│   │   │   ├── config.rs       # ConfigStore
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── ep-ui/                  # egui 前端
│   │   ├── src/
│   │   │   ├── app.rs          # 主应用
│   │   │   ├── pages/          # 各页面
│   │   │   │   ├── dashboard.rs
│   │   │   │   ├── services.rs
│   │   │   │   ├── pipeline_editor.rs
│   │   │   │   ├── models.rs
│   │   │   │   └── tasks.rs
│   │   │   ├── widgets/        # 自定义组件
│   │   │   └── main.rs
│   │   └── Cargo.toml
│   └── ep-nodes/               # 管线节点实现（可选独立 crate）
│       └── ...
└── assets/                     # 图标、字体等
```

## 9. 开放问题

| # | 问题 | 备注 |
|---|---|---|
| 1 | Gradio API 调用方式：各工具的 Gradio 版本不同，API 格式可能有差异 | 需要逐个测试 `/api/predict` vs `/run/predict` vs `/queue/join` |
| 2 | 部分工具不支持自定义端口（硬编码） | 可能需要修改启动脚本或使用端口转发 |
| 3 | Linux 下各工具的兼容性 | 现有 .bat 脚本需要对应 .sh；嵌入式 Python 是 Windows 专用 |
| 4 | Docker 类工具的管理方式 | 是否要求用户预装 Docker？还是作为可选功能？ |
| 5 | 未解压的 7 个 ZIP 工具 | 需要先解压分析后才能编写 ToolDefinition |
| 6 | 显存预估的准确性 | 不同模型/精度/批大小显存差异大，可能需要用户自行校准 |
| 7 | 多用户/远程访问 | 当前设计为单用户本地桌面，是否需要远程 Web 面板？ |
