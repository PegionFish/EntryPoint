# 配置参考 (Configuration Reference)

> 版本：1.3 | 适用于 EntryPoint v0.x
>
> v1.3 变更（PLAN_TRIGGER_UNIFIED_LOG §5.7）：新增 `[general].log_retention_days`
> （`runtime/logs/` 下事件日志与模块日志的保留天数，0 = 永久）；目录结构总览
> 补 `runtime/logs/`（模块日志 + 统一事件日志）、`runtime/tasks/`（任务注册表）、
> `runtime/watchers.json`（触发规则注册表），并修订「不产生 logs/ 目录」的过时注释。
>
> v1.2 变更（PACK_UNIFY_PLAN §3/§8.3）：新增 `[python].uv_cache_dir` /
> `[python].constraints`、`[compute].cuda_libs_dir`、`[packs].staging_dir`、
> `[active_models]`；`PUT /api/config` 深度合并语义与 `requires_restart` 标记；
> `compute.env` 后端环境变量注入已实现（§3.2）；下载并发闸已生效（§1.4）。

本文档是 EntryPoint 所有配置项、环境变量和内部文件格式的完整参考。

---

## 1. 全局配置 (`config/app.toml`)

### 1.1 `[general]` — 通用设置

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `language` | string | `"zh-CN"` | 界面语言（`zh-CN` / `en-US`） |
| `theme` | string | `"dark"` | 主题（`dark` / `light`） |
| `log_level` | string | `"info"` | 日志级别（`trace` / `debug` / `info` / `warn` / `error`） |
| `check_updates` | bool | `true` | 启动时检查模块更新 |
| `log_retention_days` | u64 | `90` | 统一日志保留天数：`runtime/logs/` 下**统一事件日志**（`events-YYYY-MM.jsonl`）与**模块日志**（`*.log`）按文件 mtime 清理巡检（daemon 每小时一轮）的保留窗口。**`0` = 永久保留**（跳过清理）。daemon 自身运行日志不落盘（控制台 / journal），不受此项影响 |

```toml
[general]
language = "zh-CN"
theme = "dark"
log_level = "info"
check_updates = true
log_retention_days = 90   # 0 = 永久保留
```

### 1.2 `[compute]` — 计算设备

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `strategy` | string | `"least_memory"` | 设备分配策略 |
| `disabled_backends` | string[] | `[]` | 禁用的计算后端列表 |
| `refresh_interval_secs` | u32 | `2` | 设备状态刷新间隔 |
| `allow_overcommit` | bool | `true` | 允许显存超额分配（仅警告） |
| `cuda_libs_dir` | string | `"runtime/cuda-libs"` | 共享 CUDA 库目录（§3.1 依赖栈统一）。启动模块时前置注入（Linux `LD_LIBRARY_PATH` / Windows `PATH`），多模块共用同一份 cuBLAS 等库；空字符串 = 不注入；相对路径基于应用根目录 |

**strategy 可选值：**

| 值 | 说明 |
|---|---|
| `manual` | 用户在 UI 中为每个模块手动指定设备 |
| `least_memory` | 自动选择剩余显存最大的设备 |
| `round_robin` | 轮询分配到各设备 |
| `single` | 所有模块使用同一设备（需配合 `single_device`） |

```toml
[compute]
strategy = "least_memory"
disabled_backends = []
refresh_interval_secs = 2
allow_overcommit = true
cuda_libs_dir = "runtime/cuda-libs"
# single_device = "cuda:0"    # strategy = "single" 时指定
```

### 1.3 `[ports]` — 端口管理

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `range_start` | u16 | `18000` | 端口范围起始 |
| `range_end` | u16 | `19000` | 端口范围结束 |

```toml
[ports]
range_start = 18000
range_end = 19000
```

### 1.4 `[models]` — 模型管理

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `cache_dir` | string | `"models"` | 模型缓存目录（相对或绝对路径） |
| `cache_paths` | string[] | `[]` | 本地模型缓存搜索路径（按优先级排序），用于发现用户已有的模型文件 |
| `hf_endpoint` | string | `""` | HuggingFace 镜像站 URL（空=官方）。**仅对 HuggingFace 源生效**（下载时注入 `HF_ENDPOINT`） |
| `default_source` | string | `"huggingface"` | 默认下载源（见下方生效规则） |
| `max_concurrent_downloads` | u32 | `2` | 最大并行下载数。已生效：下载并发闸（Semaphore），超额请求以 `queued` 状态排队，空位释放后按提交顺序自动启动；运行时改小不影响在途下载 |

**`default_source` 生效规则：**

- 可选值：`huggingface` / `modelscope` / `url`
- 下载请求**未显式指定下载源**时，若该值落在模型的可用来源（主源 + `[[models.mirrors]]` 镜像）之内，则使用它；否则回退模型声明的主源
- 下载请求显式指定了下载源（如 WebUI 中手动选择）时，以请求为准

**备选下载源（`[[models.mirrors]]`）：**

每个模型可在 module.toml 中声明若干备选源，主源不可用时（如网络受限）可切换。格式与 `[[models]]` 完整声明见 [MODULE_SPEC.md](MODULE_SPEC.md)：

```toml
[[models]]
id = "large-v3"
source = "huggingface"
repo_id = "Systran/faster-whisper-large-v3"
target_dir = "faster-whisper-large-v3"

[[models.mirrors]]
source = "modelscope"
repo_id = "pengzhendong/faster-whisper-large-v3"
# revision = "master"   # 可选，缺省用该来源的默认分支
```

内置模块现状：faster-whisper 的 large-v3 / medium / small 均已配置 ModelScope 镜像（`pengzhendong/*`）；deep-filter 的 df3 模型 URL 源已指向 HuggingFace 仓库资产（`Serkan007/DeepFilterNet3-ONNX`，原 GitHub release 资产失效）。

> **已知限制（首次下载与 venv 准备）**：模型下载前会自动准备模块的 Python 虚拟环境。全新安装时该步骤耗时取决于依赖规模（含 torch 的模块约 15–20 分钟），可能超过常见 HTTP 客户端超时。建议将客户端超时设为 ≥20 分钟，或超时后直接重试——venv 已存在时下载会立即开始。

```toml
[models]
cache_dir = "models"
# cache_dir = "D:/AI_Models"
cache_paths = []
hf_endpoint = ""
# hf_endpoint = "https://hf-mirror.com"
default_source = "huggingface"
max_concurrent_downloads = 2
```

### 1.5 `[python]` — Python 环境

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `path` | string | `""` | Python 解释器路径（空=自动检测） |
| `uv_path` | string | `""` | uv 可执行文件路径（空=自动检测） |
| `uv_cache_dir` | string | `"runtime/.uv-cache"` | uv 缓存目录（依赖栈统一，PACK_UNIFY_PLAN §3.1）。对模块 venv 安装注入 `UV_CACHE_DIR`：缓存与 venv 同盘时硬链接生效，跨模块同版本依赖只占一份物理空间；缓存随应用目录可移植。**属重启敏感项**（影响后续 venv 构建） |
| `constraints` | string | `"config/constraints.txt"` | 全局 pip constraints 文件（锁 torch 全家桶等版本，保证多模块解析到同一版本 → 硬链接去重）。`uv pip install` 追加 `-c <file>`；文件不存在则跳过；**显式空字符串 = 停用**。constraints 内容变化会触发依赖哈希变更 → 自动重装 |

```toml
[python]
path = ""
uv_path = ""
uv_cache_dir = "runtime/.uv-cache"
constraints = "config/constraints.txt"
# path = "C:/Python312/python.exe"
# uv_path = "C:/Users/me/.local/bin/uv.exe"
# constraints = ""   # 显式停用 constraints
```

**自动检测顺序：**
1. 用户指定路径（非空时）
2. 系统 PATH 中的 `python3` / `python`
3. uv 管理的 Python（`uv python find`）

### 1.6 `[pipeline]` — 管线引擎

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `max_parallel` | u32 | `4` | 最大并行节点数 |
| `default_timeout_secs` | u32 | `600` | 任务级**空闲看门狗**超时（秒）：任务持续此时长无任何节点进度/心跳才判死（`0` = 停用看门狗）。**不再是任务总时长硬上限**——只要执行器持续产生心跳（节点开始/完成/失败，及长调用期间的周期心跳），任务可运行任意时长 |
| `default_node_timeout_secs` | u32 | `0` | 节点级**硬超时**全局缺省（秒）：节点未声明 `timeout_secs` 且管线未声明 `[pipeline] node_timeout_secs` 时，作为单节点 wall-clock 硬超时。`0`（缺省）= 跟随 `default_timeout_secs`（旧配置行为不变） |
| `keep_workspace` | bool | `true` | 任务完成后保留工作目录 |
| `workspace_dir` | string | `"workspace"` | 工作目录路径 |

```toml
[pipeline]
max_parallel = 4
default_timeout_secs = 600
default_node_timeout_secs = 0
keep_workspace = true
workspace_dir = "workspace"
```

### 1.7 `[server]` — HTTP 服务（daemon）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `host` | string | 代码缺省 `"0.0.0.0"`；仓库自带配置模板为 `"127.0.0.1"`（仅本机） | 监听地址 |
| `port` | u16 | `9800` | 监听端口（WebUI 与 REST API） |
| `allow_public` | bool | `false` | 是否允许公网访问。`false` 时启用 IP 过滤，仅放行 RFC 1918 私有地址 |

```toml
[server]
host = "0.0.0.0"
port = 9800
allow_public = false
```

### 1.8 `[network]` — 网络代理

统一控制模型下载、依赖安装与模块子进程的出口代理。取代此前"子进程隐式继承 daemon 环境变量"的不可控方式：所有需要联网的子进程显式注入这里配置的环境变量。

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `http_proxy` | string | `""` | HTTP 代理地址。空 = 不覆盖（继承 daemon 进程的系统环境变量） |
| `https_proxy` | string | `""` | HTTPS 代理地址。空 = 不覆盖（同上） |
| `no_proxy` | string | `"localhost,127.0.0.1"` | 不走代理的地址列表 |

**注入规则：**

- 非空字段同时注入大写 + 小写两套键（`HTTP_PROXY`/`http_proxy`、`HTTPS_PROXY`/`https_proxy`、`NO_PROXY`/`no_proxy`），兼容不同工具的探测习惯
- 字段为空则不注入对应键，不覆盖子进程从 daemon 继承的环境变量
- 注入目标：模型下载 Python 子进程、uv/pip 依赖安装进程、模块运行子进程

**生效时机：** 新启动的子进程。已运行的模块不受影响（需重启模块生效）。

```toml
[network]
http_proxy = ""
https_proxy = ""
no_proxy = "localhost,127.0.0.1"

# 有代理时示例：
# http_proxy = "http://127.0.0.1:7890"
# https_proxy = "http://127.0.0.1:7890"
```

### 1.9 `[packs]` — 整合包（PACK_UNIFY_PLAN §8.3）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `staging_dir` | string | `".pack-staging"` | 整合包导入/构建暂存根目录（相对路径基于应用根目录）。解包 → CHECKSUMS 校验 → 落位均在此目录内完成，结束后清理 |

```toml
[packs]
staging_dir = ".pack-staging"
```

### 1.10 `[active_models]` — 每模块激活变体（版本单槽位）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `<module_id>` | string | — | 该模块当前激活的模型变体 id（`[[models]].id`）。键值对形式，每模块一条 |

**单槽位语义**（PACK_UNIFY_PLAN §5.2）：每模块同一时间一个激活变体。
daemon 启动模块时按三级回退选择模型：`[active_models]` 配置 → manifest
`default = true` → 首个变体。变体切换端点
（`PUT /api/models/{m}/{mid}/variant`）写入本表并落盘；激活变体与管线节点
`model` pin 不一致时执行前报错并引导切换（不静默热切换）。

```toml
[active_models]
faster-whisper = "large-v3"
qwen3-tts = "default"
```

### 1.11 `[api]` — 统一推理 API（`/api/v1/*`）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `enabled` | bool | `true` | v1 门面总开关。`false` 时不强制鉴权（直通） |
| `token` | string | 无（不启用鉴权） | 可选 Bearer token。配置后 `/api/v1/*` 端点要求 `Authorization: Bearer <token>` 或 `X-API-Key: <token>`，不匹配返回 `401` |

仅保护 `/api/v1/*` 外部契约层；`/api` 其余 WebUI 内部端点不受影响。
端点用法、错误码与演进路线详见 `docs/INFERENCE_API.md`。

```toml
[api]
enabled = true
# token = ""   # 对外/公网暴露前建议配置随机长字符串
```

### 1.12 `[modules]` — 模块生命周期（空闲自动下线）

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `idle_timeout_secs` | u64 | `1800` | 空闲自动释放阈值（秒）：运行中模块持续无任务触达超过该时长即自动停止，释放模型内存/显存与功耗；下次任务经既有拉起路径按需重载。`0` = 停用（7×24 常驻） |

**巡检机制：**

- daemon 每 **30 秒**巡检一轮运行中模块；空闲基准 = `max(进程启动时刻, 最近触达时刻)`
- **活跃触点**（以下五类事件刷新对应模块的"最近触达时刻"，重置空闲时钟）：
  1. 手动启动模块（模块页「启动」）
  2. 任务提交（管线/直跑入队即视为触达其引用的全部模块）
  3. 节点开始执行（on_node_start 回调）
  4. 节点执行完成（on_node_complete 回调）
  5. 任务进入终态（completed/failed/cancelled 的 finalize 阶段）
- **排队/运行任务引用豁免**：被任何排队或运行中任务引用的模块一律不回收，
  即使空闲时钟已超限
- **回收动作**：停止模块进程（杀进程树）并释放所分配端口；**venv 与模型文件保留**——
  下次拉起无需重新准备 Python 环境，也无需重新下载权重
- **生效时机**：实时读取——运行期经 WebUI 设置页或 `PUT /api/config` 修改即时生效
  （含改为 `0` 停用），无需重启 daemon

```toml
[modules]
idle_timeout_secs = 1800   # 空闲 30 分钟自动下线；0 = 常驻
```

---

## 2. 完整 app.toml 示例

```toml
# EntryPoint 全局配置
# 路径说明：相对路径基于应用根目录解析

[server]
host = "0.0.0.0"
port = 9800
allow_public = false

[general]
language = "zh-CN"
theme = "dark"
log_level = "info"
check_updates = true
log_retention_days = 90

[compute]
strategy = "least_memory"
disabled_backends = []
refresh_interval_secs = 2
allow_overcommit = true
cuda_libs_dir = "runtime/cuda-libs"

[ports]
range_start = 18000
range_end = 19000

[models]
cache_dir = "models"
cache_paths = []
hf_endpoint = "https://hf-mirror.com"
default_source = "huggingface"
max_concurrent_downloads = 2

[python]
path = ""
uv_path = ""
uv_cache_dir = "runtime/.uv-cache"
constraints = "config/constraints.txt"

[pipeline]
max_parallel = 4
default_timeout_secs = 600
default_node_timeout_secs = 0
keep_workspace = true

[network]
http_proxy = ""
https_proxy = ""
no_proxy = "localhost,127.0.0.1"

[packs]
staging_dir = ".pack-staging"

[modules]
idle_timeout_secs = 1800

[api]
enabled = true
# token = ""   # 可选：配置后 /api/v1/* 要求 Bearer / X-API-Key

[active_models]
# faster-whisper = "large-v3"   # 每模块激活变体（变体切换端点自动写入）
```

> 通过 WebUI「设置」页或 `PUT /api/config` 修改配置会**直接落盘**到 `config/app.toml`，重启后不丢失。
>
> **`PUT /api/config` 为深度合并语义**（PACK_UNIFY_PLAN §8.2）：请求体中缺省的
> 字段**保留原值**，只有显式给出的字段被更新——补丁式修改单个配置项不再需要
> 回传整份配置。重启敏感项（监听地址/端口、Python/uv 解释器路径等影响已建
> 运行时状态的字段）变更时，响应携带 `requires_restart: true` 标记，提示客户端
> 需重启 daemon 才能完全生效。

---

## 3. 环境变量

### 3.1 系统注入到模块进程的环境变量

| 变量 | 说明 | 示例 |
|---|---|---|
| `EP_ROOT` | 应用根目录 | `G:\AI_Applications\EntryPoint` |
| `EP_MODULE_DIR` | 模块目录 | `...\modules\faster-whisper` |
| `EP_MODULE_ID` | 模块 ID（已注入） | `faster-whisper` |
| `EP_MODEL_DIR` | 当前（激活变体）模型目录 | `D:\AI_Models\faster-whisper-large-v3` |
| `EP_MODELS_ROOT` | 模型缓存根目录（含所有变体子目录，供 `params.model` 变体覆盖解析，见 ADAPTER_API.md §1.3） | `D:\AI_Models` |
| `EP_MODEL_ID` | 当前模型 ID（已注入；模块无激活模型时缺省） | `large-v3` |
| `EP_HOST` | adapter 绑定地址（固定回环 `127.0.0.1`，根治 Windows 防火墙弹窗；adapter 以 `os.getenv("EP_HOST", "127.0.0.1")` 读取，见 ADAPTER_API.md §1.2） | `127.0.0.1` |
| `EP_PORT` | 分配端口 | `18001` |
| `EP_DEVICE` | 设备标识 | `cuda:0` / `cpu` / `npu:0` |
| `EP_DEVICE_INDEX` | 设备索引 | `0` |
| `EP_BACKEND` | 计算后端 | `cuda` / `rocm` / `openvino` / `cpu` |
| `EP_WORKSPACE` | 任务工作区根目录（任务级子目录经 predict 请求参数传递） | `...\workspace` |
| `EP_LOG_LEVEL` | 日志级别（已注入，固定 `info`） | `info` |
| `EP_ENTRYPOINT` | 启动命令模板辅助变量：module.toml `[runtime].entrypoint`（占位符 `{entrypoint}`） | `adapter.py` |
| `EP_BINARY` | 启动命令模板辅助变量：module.toml `[runtime].binaries` 首个二进制路径（占位符 `{binary}`） | `bin/native-tool` |
| `EP_VENV_PYTHON` | 启动命令模板辅助变量：平台自适应的模块 venv Python 解释器路径（占位符 `{venv_python}`；Windows `Scripts\python.exe` / Linux `bin/python`） | `...\runtime\venvs\faster-whisper\Scripts\python.exe` |

### 3.2 计算后端相关环境变量（`compute.env` 接线，已实现）

启动模块进程时，按当前设备的后端读取 module.toml 的 `[compute.env].<backend>`
表，替换 `{device_index}` 占位符后注入（多卡隔离立即生效）。格式与每后端
键表见 MODULE_SPEC.md §2.3。

| 后端 | 典型变量 | 值 |
|---|---|---|
| CUDA | `CUDA_VISIBLE_DEVICES` | 设备索引（`{device_index}` 替换后） |
| ROCm | `HIP_VISIBLE_DEVICES` | 设备索引 |
| OpenVINO | `OPENVINO_DEVICE` | 设备名（`GPU.0` / `NPU.0`） |
| CPU | —（模块自定义，如 `TORCH_DEVICE`） | — |

> 共享 CUDA 库目录（`[compute].cuda_libs_dir`）另以搜索路径前置方式注入
> （Linux `LD_LIBRARY_PATH` / Windows `PATH`），不属于本表。

### 3.3 用户可设置的环境变量（影响 EntryPoint 自身）

| 变量 | 说明 | 默认 |
|---|---|---|
| `EP_ROOT` | 应用根目录覆盖（配置、模块、日志等均基于它解析） | 可执行文件位置推断，兜底当前工作目录 |

**由系统注入（用户不直接设置）：**

| 变量 | 说明 |
|---|---|
| `HF_ENDPOINT` | 由 `[models].hf_endpoint`（非空时）注入模型下载进程，仅 HuggingFace 源生效 |
| `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` 等 | 由 `[network]` 节注入，见 §1.9 |

**计划中（当前代码未实现，设置无效）：** `EP_CONFIG_DIR`（配置目录固定为 `<root>/config`）、`EP_LOG_DIR`（daemon 日志走 systemd journal / 控制台）、`HF_TOKEN`、`MODELSCOPE_CACHE`。后两者虽无显式处理，但仍可经 daemon 进程环境隐式继承到子进程（如通过 systemd `Environment=` 设置）。

---

## 4. 内部文件格式

### 4.1 `.ep_meta.json` — 模型元数据

位于模型缓存目录内每个模型文件夹下。

```json
{
  "module_id": "faster-whisper",
  "model_id": "large-v3",
  "source": "huggingface",
  "repo_id": "Systran/faster-whisper-large-v3",
  "revision": "main",
  "downloaded_at": "2026-07-20T10:30:00Z",
  "total_size_bytes": 3094850000,
  "qualified_id": "ep.systran.faster-whisper",
  "tags": ["字幕"],
  "pack_id": "pigeonfish.subtitle-kit"
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `module_id` | string | 所属模块 ID |
| `model_id` | string | 模型 ID（对应 module.toml 中 [[models]].id） |
| `source` | string | 获取来源（`huggingface` / `modelscope` / `url` / `pack`——整合包导入落位为 `pack`） |
| `repo_id` | string | 仓库 ID |
| `revision` | string | 版本/分支 |
| `downloaded_at` | string | 下载完成时间（ISO 8601） |
| `total_size_bytes` | u64 | 总大小 |
| `qualified_id` | string? | 全限定模型 ID（§4.3 PACK_UNIFY_PLAN；旧数据可缺省） |
| `tags` | string[] | 用户/整合包标签（统一页 chips 筛选，随整合包流转） |
| `pack_id` | string? | 来源整合包 id（可空；非 pack 来源无此字段） |

**用户可安全删除此文件**。删除后系统视为手动放置的模型。

### 4.2 `.ep_deps_hash` — 依赖哈希标记

位于 `runtime/venvs/<module-id>/` 下。

```
sha256:a1b2c3d4e5f6...
```

单行文本。哈希输入 = `requirements.txt` 字节 + constraints 文件字节（若存在）
+ link-mode 版本号（依赖栈统一，PACK_UNIFY_PLAN §3.1）：任一输入变化（改依赖、
改 constraints、换链接模式）都会触发 venv 重装。

### 4.3 `ep.lock` — 依赖锁定文件

位于 `runtime/venvs/<module-id>/` 下。

```
# ep.lock — 由 uv pip freeze 生成
fastapi==0.115.0
uvicorn==0.30.0
faster-whisper==1.1.0
...
```

精确版本锁定，用于跨机器还原相同环境。

### 4.4 任务工作目录结构

```
workspace/<task-id>/
├── task.json                  ← 任务元信息
├── input/
│   └── source.mp4             ← 输入文件（或符号链接）
├── extract/
│   └── output.wav             ← FFmpeg 输出
├── denoise/
│   └── output.wav             ← 降噪输出
├── asr/
│   └── output.json            ← ASR 结果
├── translate/
│   └── output.json            ← 翻译结果
└── srt/
    └── output.srt             ← 最终字幕
```

**task.json：**
```json
{
  "task_id": "abc123",
  "pipeline_id": "video-to-srt",
  "pipeline_name": "视频转字幕",
  "started_at": "2026-07-20T10:30:00Z",
  "completed_at": "2026-07-20T10:35:22Z",
  "status": "completed",
  "input_file": "C:/Videos/test.mp4",
  "nodes": {
    "extract": {"status": "completed", "elapsed_secs": 2.1},
    "denoise": {"status": "completed", "elapsed_secs": 15.3},
    "asr": {"status": "completed", "elapsed_secs": 180.5},
    "translate": {"status": "completed", "elapsed_secs": 45.2},
    "srt": {"status": "completed", "elapsed_secs": 0.1}
  }
}
```

---

## 5. 目录结构总览

```
EntryPoint/                        ← 应用根目录
├── bin/                           ← 二进制（Windows server 包布局；源码树为 target/release/）
│   ├── ep-daemon[.exe]            ← 主程序（daemon：托管 WebUI + REST API/WebSocket）
│   └── ep-pack[.exe]              ← 整合包 CLI
├── config/
│   ├── app.toml                   ← 全局配置（本文档 §1）
│   ├── constraints.txt            ← 全局 pip constraints（§1.5）
│   └── pipelines/                 ← 管线定义
│       ├── video_to_srt.toml
│       └── audio_extract.toml
├── modules/                       ← 模块目录
│   └── <module-id>/
│       ├── module.toml
│       ├── adapter.py
│       └── requirements.txt
├── runtime/                       ← 运行时（自动生成）
│   ├── venvs/
│   │   └── <module-id>/
│   │       ├── .ep_deps_hash
│   │       ├── ep.lock
│   │       └── ... (venv 内容)
│   ├── .uv-cache/                 ← uv 缓存（[python].uv_cache_dir，硬链接去重源）
│   ├── cuda-libs/                 ← 共享 CUDA 库（[compute].cuda_libs_dir）
│   ├── packs/                     ← 已装整合包注册表（<pack-id>.json）
│   ├── tasks/                     ← 任务注册表（原子落盘；重启后非终态任务改判 failed）
│   ├── watchers.json              ← 触发器规则注册表（原子落盘；WebUI 触发器页 CRUD）
│   └── logs/                      ← 模块日志（<module>*.log）+ 统一事件日志
│       ├── <module>*.log          ←   daemon 捕获的模块子进程输出（WebUI 日志抽屉展示）
│       └── events-YYYY-MM.jsonl   ←   统一事件日志（watcher_trigger / task_terminal，
│                                        单行 JSON 追加、按月滚动；log_retention_days 清理）
├── .pack-staging/                 ← 整合包导入/构建暂存（[packs].staging_dir，随用随清）
├── workspace/                     ← 管线任务工作目录
│   └── <task-id>/
└── docs/                          ← 文档
    ├── MODULE_SPEC.md
    ├── ADAPTER_API.md
    ├── PIPELINE_SPEC.md
    ├── CONFIG_REFERENCE.md
    └── WEBUI_GUIDE.md

<model_cache_dir>/                 ← 模型缓存（用户可指定位置）
├── faster-whisper-large-v3/
│   ├── .ep_meta.json
│   ├── model.bin
│   └── config.json
└── ...
```

---

## 6. 配置优先级

配置来源从高到低：

1. 运行时修改（WebUI 设置页 / `PUT /api/config`）——立即生效并落盘到 `config/app.toml`
2. `config/app.toml`（启动时加载）
3. 内置默认值（文件中缺失的字段用默认值补齐）

应用根目录的解析顺序：环境变量 `EP_ROOT` → 可执行文件位置推断（需含 `config/` + `modules/`）→ 当前工作目录。

---

## 7. 首次启动行为

```
1. 检测 config/app.toml 是否存在
   - 不存在 → 从内置模板生成默认配置
2. 检测/自动安装系统依赖（ffmpeg 等，仅 Linux 包管理器路径）；
   Python/uv 不做启动期检测——缺失会在首次模块 venv 准备/模型下载时
   报错（模块页与 daemon 日志可见）
3. 扫描 modules/ 目录
   - 解析所有 module.toml
   - 标记各模块状态（就绪/缺依赖/缺模型）
4. 检测计算设备
   - 枚举所有可用后端和设备
5. daemon 开始托管 WebUI 与 REST API（监听地址以 [server].host 为准，
   代码缺省 0.0.0.0；仓库自带配置缺省 127.0.0.1），本机浏览器访问 http://127.0.0.1:9800 即可使用；
   Windows server 包的 start-daemon.bat 会自动打开默认浏览器
```
