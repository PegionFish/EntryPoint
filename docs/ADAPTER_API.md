# Adapter REST API 规范

> 版本：1.0 | 适用于 EntryPoint v0.x

本文档定义 Python 模块 `adapter.py` 必须实现的 HTTP 接口契约。
EntryPoint 核心仅通过此标准接口与模块通信，不感知底层框架（Gradio/Flask/FastAPI 等）差异。

---

## 1. 概述

### 1.1 设计原则

- **统一接口**：所有 Python HTTP 模块暴露相同的端点结构
- **框架无关**：adapter 内部可用任何框架，对外只暴露标准 REST
- **文件优先**：大文件（音频/视频/图片）通过文件路径传递，避免内存中转
- **无状态**：每次 `/predict` 调用独立，不依赖前次调用状态

### 1.2 基础约定

| 项目 | 约定 |
|---|---|
| 协议 | HTTP/1.1（暂不要求 HTTPS） |
| 监听地址 | `0.0.0.0:{EP_PORT}` |
| 内容类型 | `application/json` 或 `multipart/form-data` |
| 字符编码 | UTF-8 |
| 超时 | 由调用方控制（管线引擎设置 per-node 超时） |

### 1.3 模型环境变量与变体覆盖

平台（ep-core `build_module_env`）向模块子进程注入以下模型相关环境变量
（均带 `EP_` 前缀）：

| 环境变量 | 含义 |
|---|---|
| `EP_MODEL_DIR` | **激活变体**的模型目录（`models/<target_dir>`，受 `config.models.cache_dir` 影响）；`EP_MODEL_ID` 为其模型 ID |
| `EP_MODELS_ROOT` | **模型缓存根目录**（`models/` 本身的绝对路径），含所有变体子目录，与激活变体无关 |

**变体覆盖行为（`params.model`）**：`EP_MODEL_DIR` 恒指激活变体，端到端
切换激活变体仍走 `PUT /api/models/{module}/{model}/variant` + 重启模块。
但当请求参数以 `params.model` 临时覆盖为其它变体时，支持此行为的 adapter
应从 `EP_MODELS_ROOT` 下按 `module.toml [[models]]` 的 `model_id → target_dir`
约定解析对应变体子目录，命中本地权重则直接使用（参照实现：rembg adapter）。

**本地缺失时的契约**：请求的模型在本地（变体目录与激活目录）均无权重时，
adapter **不得静默联网下载**，应返回 `MODEL_NOT_LOADED`（503），错误信息
指出缺失的预期文件路径与获取方式（平台模型管理器下载，或经 variant API
切换激活变体后重启模块）。

---

## 2. 端点定义

### 2.1 `GET /health` — 健康检查

系统启动模块后轮询此端点，直到返回 200 才标记为 Running。

**请求：** 无参数

**响应：**
```json
{
  "status": "ok"
}
```

**状态码：**
- `200` — 服务就绪
- `503` — 服务启动中（模型加载中）
- 连接拒绝 — 进程未启动

**约束：**
- 响应时间 < 1 秒
- 不执行重计算
- 模型未加载完成时返回 503（而非 200）

---

### 2.2 `GET /info` — 模块信息

返回模块元信息和能力列表。系统启动后调用一次用于验证。

**请求：** 无参数

**响应：**
```json
{
  "module_id": "faster-whisper",
  "name": "Faster-Whisper ASR",
  "version": "1.1.0",
  "model_id": "large-v3",
  "device": "cuda:0",
  "backend": "cuda",
  "capabilities": [
    {
      "name": "transcribe",
      "input_type": "audio",
      "output_type": "json",
      "params": {
        "language": {"type": "string", "default": "auto"},
        "timestamps": {"type": "boolean", "default": true}
      }
    }
  ]
}
```

**字段说明：**

| 字段 | 类型 | 说明 |
|---|---|---|
| `module_id` | string | 与 module.toml 中 id 一致 |
| `name` | string | 显示名称 |
| `version` | string | 模块版本 |
| `model_id` | string | 当前加载的模型 ID |
| `device` | string | 当前使用的设备 |
| `backend` | string | 当前计算后端 |
| `capabilities` | array | 能力列表（与 module.toml 声明一致） |

---

### 2.3 `POST /predict/{capability}` — 调用能力

核心端点。执行指定的 AI 推理任务。

#### 请求格式 A：文件输入（multipart/form-data）

适用于 input_type 为 `audio` / `video` / `image` / `file` 的能力。

```
POST /predict/transcribe HTTP/1.1
Content-Type: multipart/form-data; boundary=----boundary

------boundary
Content-Disposition: form-data; name="file"; filename="input.wav"
Content-Type: audio/wav

<binary data>
------boundary
Content-Disposition: form-data; name="params"

{"language": "zh", "timestamps": true}
------boundary--
```

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `file` | binary | ✅ | 输入文件 |
| `params` | string (JSON) | ❌ | 参数字典（符合 capability 的 params schema） |

#### 请求格式 B：路径输入（application/json）

适用于文件已存在于本地磁盘的场景（管线内部调用优先使用此方式，避免重复传输）。

```json
POST /predict/transcribe
Content-Type: application/json

{
  "input_path": "G:/AI_Applications/EntryPoint/workspace/task-1/denoise/output.wav",
  "params": {
    "language": "zh",
    "timestamps": true
  }
}
```

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `input_path` | string | ✅ | 输入文件绝对路径 |
| `params` | object | ❌ | 参数字典 |

#### 请求格式 C：文本/JSON 输入（application/json）

适用于 input_type 为 `text` / `json` 的能力。

```json
POST /predict/translate
Content-Type: application/json

{
  "input_text": "Hello world, this is a test.",
  "params": {
    "target_lang": "zh"
  }
}
```

| 字段 | 类型 | 必须 | 说明 |
|---|---|---|---|
| `input` | any | 条件 | 文本/JSON 输入（ep-core executor 文本类产物实际使用的键名） |
| `input_text` | string | 条件 | 文本输入（input_type=text，兼容别名） |
| `input_json` | object | 条件 | JSON 输入（input_type=json，兼容别名） |
| `input_path` | string | 条件 | 文件路径（文本文件） |
| `params` | object | ❌ | 参数字典 |

> 注：管线执行器（ep-core executor）对文本/JSON 类上游产物发送
> `{"input": <文本或JSON>, "params": {...}}`；adapter 应同时兼容
> `input` 与 `input_text`。文件类上游产物则走 multipart `file` 字段
> （即便是文本文件），adapter 可按需读取文件内容。

---

#### 响应格式

**成功（200）：**

```json
{
  "status": "completed",
  "output_type": "json",
  "result": {
    "text": "你好世界，这是一个测试。",
    "segments": [
      {"start": 0.0, "end": 1.2, "text": "你好世界"},
      {"start": 1.2, "end": 2.5, "text": "这是一个测试"}
    ],
    "language": "zh",
    "duration_seconds": 2.5
  },
  "output_path": null,
  "elapsed_seconds": 3.2
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `status` | string | `"completed"` |
| `output_type` | string | 与 capability 声明的 output_type 一致 |
| `result` | any | 输出内容（文本/JSON/见下方规则） |
| `output_path` | string \| null | 输出文件路径（output_type 为文件类时） |
| `elapsed_seconds` | float | 推理耗时 |

**result 字段规则（按 output_type，以 ep-core executor 实际解析为准）：**

| output_type | result 内容 | 说明 |
|---|---|---|
| `text` | 字符串 | → 文本产物 |
| `json`（或缺省） | JSON 值 | → JSON 产物 |
| `file` | **输出文件绝对路径（字符串）** | → 文件产物；可同时携带 `output_path` 冗余字段 |

> **注意**：执行器只识别 `file` / `text` / `json` 三种 output_type，
> 其余取值（如 `audio` / `image`）会被警告并按 JSON 处理；执行器**不读取**
> 顶层 `output_path` 字段。因此文件类输出（音频/视频/图片）必须返回
> `output_type: "file"` 且 `result` 为路径字符串，参照实现：
> faster-whisper / paddleocr / rembg / deep-filter / qwen3-tts adapter。

**文件输出约定：**
- 输出文件写入 `EP_WORKSPACE/<node_id>/` 目录（管线运行时）
- 或写入系统临时目录（独立调用时）
- adapter 负责创建输出目录
- 文件命名：`output.<ext>`（如 `output.wav`、`output.png`）

---

**失败（4xx/5xx）：**

```json
{
  "status": "error",
  "error_code": "MODEL_NOT_LOADED",
  "message": "Model file not found at EP_MODEL_DIR",
  "detail": "Expected model.bin in G:/AI_Models/faster-whisper-large-v3/"
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `status` | string | `"error"` |
| `error_code` | string | 机器可读错误码 |
| `message` | string | 人类可读错误描述 |
| `detail` | string \| null | 额外调试信息 |

---

## 3. 错误码

| 错误码 | HTTP 状态 | 说明 |
|---|---|---|
| `INVALID_CAPABILITY` | 404 | 请求的 capability 不存在 |
| `INVALID_PARAMS` | 422 | 参数校验失败（缺少必填/类型错误/超出范围） |
| `INVALID_INPUT` | 400 | 输入文件格式不支持/损坏/超出大小限制 |
| `FILE_NOT_FOUND` | 400 | input_path 指向的文件不存在 |
| `MODEL_NOT_LOADED` | 503 | 模型未加载（启动中或加载失败） |
| `DEVICE_ERROR` | 500 | 计算设备错误（OOM、设备不可用） |
| `INFERENCE_ERROR` | 500 | 推理过程中出错 |
| `TIMEOUT` | 504 | 推理超时 |
| `INTERNAL_ERROR` | 500 | 未分类内部错误 |

---

## 4. 长时间任务与进度（可选扩展）

对于耗时较长的任务（如大文件 ASR），adapter 可选择支持异步模式：

### 4.1 同步模式（默认）

`POST /predict/{capability}` 阻塞直到完成，直接返回结果。
适用于大多数场景（< 5 分钟的任务）。

### 4.2 异步模式（可选）

```json
// 请求时添加 "async": true
POST /predict/transcribe
{"input_path": "...", "params": {...}, "async": true}

// 立即返回任务 ID
{
  "status": "accepted",
  "task_id": "abc123"
}

// 轮询进度
GET /task/abc123
{
  "status": "running",
  "progress": 0.65,
  "message": "Processing segment 13/20"
}

// 完成
GET /task/abc123
{
  "status": "completed",
  "result": {...},
  "output_path": "..."
}
```

> 异步模式为可选扩展。管线引擎默认使用同步模式；
> 仅当模块在 `/info` 中声明 `"supports_async": true` 时才使用异步。

---

## 5. Adapter 实现模板

### 5.1 最小模板（FastAPI）

```python
"""adapter.py — EntryPoint 模块适配器"""
import os
import json
import logging
from pathlib import Path
from contextlib import asynccontextmanager

from fastapi import FastAPI, UploadFile, File, Form, HTTPException
from fastapi.responses import JSONResponse
import uvicorn

# ── 环境变量 ──────────────────────────────────────────────
EP_PORT = int(os.environ.get("EP_PORT", "18000"))
EP_MODEL_DIR = os.environ.get("EP_MODEL_DIR", "")
EP_MODELS_ROOT = os.environ.get("EP_MODELS_ROOT", "")  # 模型缓存根目录（§1.3）
EP_MODEL_ID = os.environ.get("EP_MODEL_ID", "")
EP_DEVICE = os.environ.get("EP_DEVICE", "cpu")
EP_BACKEND = os.environ.get("EP_BACKEND", "cpu")
EP_DEVICE_INDEX = os.environ.get("EP_DEVICE_INDEX", "0")
EP_WORKSPACE = os.environ.get("EP_WORKSPACE", "")
EP_MODULE_ID = os.environ.get("EP_MODULE_ID", "unknown")

logger = logging.getLogger(EP_MODULE_ID)

# ── 模型加载 ──────────────────────────────────────────────
model = None

@asynccontextmanager
async def lifespan(app: FastAPI):
    """启动时加载模型，关闭时释放资源"""
    global model
    logger.info(f"Loading model from {EP_MODEL_DIR} on {EP_DEVICE}")
    model = load_model(EP_MODEL_DIR, EP_DEVICE)  # ← 模块自行实现
    logger.info("Model loaded")
    yield
    del model

app = FastAPI(lifespan=lifespan)

# ── 标准端点 ──────────────────────────────────────────────

@app.get("/health")
def health():
    if model is None:
        return JSONResponse({"status": "loading"}, status_code=503)
    return {"status": "ok"}

@app.get("/info")
def info():
    return {
        "module_id": EP_MODULE_ID,
        "name": "My Module",
        "version": "1.0.0",
        "model_id": EP_MODEL_ID,
        "device": EP_DEVICE,
        "backend": EP_BACKEND,
        "capabilities": [
            {
                "name": "process",
                "input_type": "audio",
                "output_type": "json",
                "params": {}
            }
        ]
    }

@app.post("/predict/{capability}")
async def predict(
    capability: str,
    file: UploadFile | None = File(None),
    input_path: str | None = Form(None),
    params: str = Form("{}"),
):
    if capability != "process":
        raise HTTPException(404, detail={
            "status": "error",
            "error_code": "INVALID_CAPABILITY",
            "message": f"Unknown capability: {capability}"
        })

    params_dict = json.loads(params)

    # 获取输入文件路径
    if input_path:
        work_path = Path(input_path)
    elif file:
        work_path = Path(EP_WORKSPACE or "/tmp") / EP_MODULE_ID / file.filename
        work_path.parent.mkdir(parents=True, exist_ok=True)
        work_path.write_bytes(await file.read())
    else:
        raise HTTPException(400, detail={
            "status": "error",
            "error_code": "INVALID_INPUT",
            "message": "No input provided (need 'file' or 'input_path')"
        })

    # 执行推理 ← 模块自行实现
    result = model.process(str(work_path), **params_dict)

    return {
        "status": "completed",
        "output_type": "json",
        "result": result,
        "output_path": None,
        "elapsed_seconds": 0.0
    }

# ── 启动 ──────────────────────────────────────────────────
if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    uvicorn.run(app, host="0.0.0.0", port=EP_PORT, log_level="info")
```

### 5.2 文件输出模板

```python
@app.post("/predict/denoise")
async def denoise(
    file: UploadFile | None = File(None),
    input_path: str | None = Form(None),
    params: str = Form("{}"),
):
    # ... 获取输入路径（同上）...

    # 构建输出路径
    out_dir = Path(EP_WORKSPACE or "/tmp") / EP_MODULE_ID / "denoise"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "output.wav"

    # 执行降噪
    model.denoise(str(work_path), str(out_path), **params_dict)

    return {
        "status": "completed",
        "output_type": "audio",
        "result": None,
        "output_path": str(out_path),
        "elapsed_seconds": 0.0
    }
```

### 5.3 JSON 输入模板（文本类能力）

```python
from fastapi import Request

@app.post("/predict/translate")
async def translate(request: Request):
    body = await request.json()
    input_text = body.get("input_text", "")
    params = body.get("params", {})

    if not input_text:
        raise HTTPException(400, detail={
            "status": "error",
            "error_code": "INVALID_INPUT",
            "message": "input_text is required"
        })

    result = model.translate(input_text, **params)

    return {
        "status": "completed",
        "output_type": "text",
        "result": result,
        "output_path": None,
        "elapsed_seconds": 0.0
    }
```

---

## 6. 包装现有 Gradio 应用的策略

对于已有 Gradio WebUI 的工具，adapter 有两种实现路径：

### 策略 A：绕过 Gradio，直接调用 Python API（推荐）

```python
# 不启动 Gradio，直接 import 底层模块
from my_tool.core import transcribe_audio

@app.post("/predict/transcribe")
async def predict(...):
    result = transcribe_audio(file_path, **params)
    return {"status": "completed", ...}
```

优点：无额外进程、无 HTTP 转发开销、完全控制接口格式。

### 策略 B：启动 Gradio 作为子进程，adapter 做代理

```python
import subprocess, httpx

# 启动 Gradio（内部端口）
gradio_proc = subprocess.Popen(["python", "original_app.py", "--port", "17999"])

@app.post("/predict/transcribe")
async def predict(...):
    # 转发到 Gradio API（处理版本差异）
    async with httpx.AsyncClient() as client:
        resp = await client.post("http://127.0.0.1:17999/api/predict", json={...})
    # 转换为标准格式
    return normalize_gradio_response(resp.json())
```

优点：不需要理解底层代码。缺点：多一层进程和 HTTP 转发。

**推荐策略 A**。仅在底层代码耦合过深、无法直接 import 时使用策略 B。

---

## 7. 测试验证

模块开发者在提交前应验证：

```bash
# 1. 启动 adapter（手动设置环境变量）
EP_PORT=18001 EP_MODEL_DIR=/path/to/model EP_DEVICE=cpu python adapter.py

# 2. 健康检查
curl -s http://localhost:18001/health | jq .
# 期望: {"status": "ok"}

# 3. 信息
curl -s http://localhost:18001/info | jq .

# 4. 文件上传调用
curl -X POST http://localhost:18001/predict/transcribe \
  -F "file=@test.wav" \
  -F 'params={"language":"zh"}'

# 5. 路径调用
curl -X POST http://localhost:18001/predict/transcribe \
  -H "Content-Type: application/json" \
  -d '{"input_path": "/path/to/test.wav", "params": {"language": "zh"}}'

# 6. 错误处理
curl -X POST http://localhost:18001/predict/nonexistent
# 期望: 404 + INVALID_CAPABILITY
```
