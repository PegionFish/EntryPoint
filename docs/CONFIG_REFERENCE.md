# 配置参考 (Configuration Reference)

> 版本：1.0 | 适用于 EntryPoint v0.x

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

```toml
[general]
language = "zh-CN"
theme = "dark"
log_level = "info"
check_updates = true
```

### 1.2 `[compute]` — 计算设备

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `strategy` | string | `"least_memory"` | 设备分配策略 |
| `disabled_backends` | string[] | `[]` | 禁用的计算后端列表 |
| `refresh_interval_secs` | u32 | `2` | 设备状态刷新间隔 |
| `allow_overcommit` | bool | `true` | 允许显存超额分配（仅警告） |

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
| `hf_endpoint` | string | `""` | HuggingFace 镜像站 URL（空=官方） |
| `default_source` | string | `"huggingface"` | 默认下载源 |
| `max_concurrent_downloads` | u32 | `2` | 最大并行下载数 |

```toml
[models]
cache_dir = "models"
# cache_dir = "D:/AI_Models"
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

```toml
[python]
path = ""
uv_path = ""
# path = "C:/Python312/python.exe"
# uv_path = "C:/Users/me/.local/bin/uv.exe"
```

**自动检测顺序：**
1. 用户指定路径（非空时）
2. 系统 PATH 中的 `python3` / `python`
3. uv 管理的 Python（`uv python find`）

### 1.6 `[pipeline]` — 管线引擎

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `max_parallel` | u32 | `4` | 最大并行节点数 |
| `default_timeout_secs` | u32 | `600` | 节点默认超时 |
| `keep_workspace` | bool | `true` | 任务完成后保留工作目录 |
| `workspace_dir` | string | `"workspace"` | 工作目录路径 |

```toml
[pipeline]
max_parallel = 4
default_timeout_secs = 600
keep_workspace = true
workspace_dir = "workspace"
```

### 1.7 `[ui]` — 界面设置

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `scale_factor` | f32 | `1.0` | UI 缩放（高 DPI 适配） |
| `font_size` | f32 | `14.0` | 基础字号 |
| `dashboard_refresh_secs` | u32 | `2` | 仪表盘刷新间隔 |

```toml
[ui]
scale_factor = 1.0
font_size = 14.0
dashboard_refresh_secs = 2
```

---

## 2. 完整 app.toml 示例

```toml
# EntryPoint 全局配置
# 路径说明：相对路径基于应用根目录解析

[general]
language = "zh-CN"
theme = "dark"
log_level = "info"
check_updates = true

[compute]
strategy = "least_memory"
disabled_backends = []
refresh_interval_secs = 2
allow_overcommit = true

[ports]
range_start = 18000
range_end = 19000

[models]
cache_dir = "models"
hf_endpoint = "https://hf-mirror.com"
default_source = "huggingface"
max_concurrent_downloads = 2

[python]
path = ""
uv_path = ""

[pipeline]
max_parallel = 4
default_timeout_secs = 600
keep_workspace = true

[ui]
scale_factor = 1.0
font_size = 14.0
```

---

## 3. 环境变量

### 3.1 系统注入到模块进程的环境变量

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
| `EP_WORKSPACE` | 任务工作目录 | `...\workspace\task-abc123` |
| `EP_LOG_LEVEL` | 日志级别 | `info` |

### 3.2 计算后端相关环境变量

由 `[compute.env]` 或默认规则注入：

| 后端 | 变量 | 值 |
|---|---|---|
| CUDA | `CUDA_VISIBLE_DEVICES` | 设备索引 |
| ROCm | `HIP_VISIBLE_DEVICES` | 设备索引 |
| OpenVINO | `OPENVINO_DEVICE` | 设备名（`GPU.0` / `NPU.0`） |
| CPU | — | — |

### 3.3 用户可设置的环境变量（影响 EntryPoint 自身）

| 变量 | 说明 | 默认 |
|---|---|---|
| `EP_CONFIG_DIR` | 配置文件目录覆盖 | `<root>/config` |
| `EP_LOG_DIR` | 日志目录覆盖 | `<root>/logs` |
| `HF_ENDPOINT` | HuggingFace 镜像（传递给下载进程） | — |
| `HF_TOKEN` | HuggingFace 访问令牌（私有模型） | — |
| `MODELSCOPE_CACHE` | ModelScope 缓存目录 | — |

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
  "total_size_bytes": 3094850000
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `module_id` | string | 所属模块 ID |
| `model_id` | string | 模型 ID（对应 module.toml 中 [[models]].id） |
| `source` | string | 下载源（`huggingface` / `modelscope` / `url`） |
| `repo_id` | string | 仓库 ID |
| `revision` | string | 版本/分支 |
| `downloaded_at` | string | 下载完成时间（ISO 8601） |
| `total_size_bytes` | u64 | 总大小 |

**用户可安全删除此文件**。删除后系统视为手动放置的模型。

### 4.2 `.ep_deps_hash` — 依赖哈希标记

位于 `runtime/venvs/<module-id>/` 下。

```
sha256:a1b2c3d4e5f6...
```

单行文本，记录 `requirements.txt` 的 SHA-256 哈希。
用于检测依赖是否变更，避免每次启动都重新安装。

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
├── entrypoint[.exe]               ← 主程序
├── config/
│   ├── app.toml                   ← 全局配置（本文档 §1）
│   └── pipelines/                 ← 管线定义
│       ├── video-to-srt.toml
│       └── asr-compare.toml
├── modules/                       ← 模块目录
│   └── <module-id>/
│       ├── module.toml
│       ├── adapter.py
│       └── requirements.txt
├── runtime/                       ← 运行时（自动生成）
│   └── venvs/
│       └── <module-id>/
│           ├── .ep_deps_hash
│           ├── ep.lock
│           └── ... (venv 内容)
├── workspace/                     ← 管线任务工作目录
│   └── <task-id>/
├── logs/                          ← 日志
│   ├── entrypoint.log
│   └── modules/
│       ├── faster-whisper.log
│       └── ...
└── docs/                          ← 文档
    ├── MODULE_SPEC.md
    ├── ADAPTER_API.md
    ├── PIPELINE_SPEC.md
    └── CONFIG_REFERENCE.md

<model_cache_dir>/                 ← 模型缓存（用户可指定位置）
├── faster-whisper-large-v3/
│   ├── .ep_meta.json
│   ├── model.bin
│   └── config.json
└── ...
```

---

## 6. 配置优先级

当同一配置项有多个来源时，优先级从高到低：

1. 命令行参数（`--config-dir`、`--model-dir` 等）
2. 环境变量（`EP_CONFIG_DIR`、`HF_ENDPOINT` 等）
3. `config/app.toml`
4. 内置默认值

---

## 7. 首次启动行为

```
1. 检测 config/app.toml 是否存在
   - 不存在 → 从内置模板生成默认配置
2. 检测 Python 和 uv
   - 缺失 → 弹窗引导安装（Windows 自动打开下载 URL）
3. 扫描 modules/ 目录
   - 解析所有 module.toml
   - 标记各模块状态（就绪/缺依赖/缺模型）
4. 检测计算设备
   - 枚举所有可用后端和设备
5. 进入主界面
```
