"""
RemBG 智能去背景 — EntryPoint adapter
基于 rembg 库的 HTTP 服务，提供图像背景移除能力。
"""

from __future__ import annotations

import io
import json
import logging
import os
import time
import uuid
from pathlib import Path
from typing import Optional

import uvicorn
from fastapi import FastAPI, File, Form, Request, UploadFile
from fastapi.responses import JSONResponse

# ---------------------------------------------------------------------------
# 环境变量
# ---------------------------------------------------------------------------
EP_HOST: str = os.getenv("EP_HOST", "127.0.0.1")
EP_PORT: int = int(os.getenv("EP_PORT", "8900"))
EP_WORKSPACE: str = os.getenv("EP_WORKSPACE", os.path.join(os.getcwd(), "workspace"))
# daemon 注入的模型键名是 EP_MODEL_ID（ep-core process.rs build_module_env，
# 取激活变体 id，如 "u2net"）；旧实现读 EP_MODEL_NAME（daemon 从不注入该键），
# 导致变体切换静默失效、恒用默认值。
EP_MODEL_NAME: str = os.getenv("EP_MODEL_ID", os.getenv("EP_MODEL_NAME", "u2net"))
EP_DEVICE_INDEX: str = os.getenv("EP_DEVICE_INDEX", "0")
EP_LOG_LEVEL: str = os.getenv("EP_LOG_LEVEL", "INFO")

# daemon 将模型下载到 EP_MODEL_DIR（models/<target_dir>/<model>.onnx）。
# rembg 按 <model>.onnx 文件名在其模型主目录查找/下载；将该目录指向
# EP_MODEL_DIR，使 daemon 预下载的模型被真正消费、变体切换端到端生效。
EP_MODEL_DIR: str = os.getenv("EP_MODEL_DIR", "")
if EP_MODEL_DIR:
    os.environ.setdefault("U2NET_HOME", EP_MODEL_DIR)

# ---------------------------------------------------------------------------
# 日志
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=getattr(logging, EP_LOG_LEVEL.upper(), logging.INFO),
    format="%(asctime)s [rembg-adapter] %(levelname)s %(message)s",
)
logger = logging.getLogger("rembg-adapter")

# ---------------------------------------------------------------------------
# 全局状态
# ---------------------------------------------------------------------------
_session = None
_session_model: Optional[str] = None
_ready = False

app = FastAPI(
    title="RemBG Adapter",
    version="2.0.50",
    description="EntryPoint rembg 图像去背景模块",
)


# ---------------------------------------------------------------------------
# 模型加载
# ---------------------------------------------------------------------------
def _load_session(model_name: str):
    """懒加载 rembg session，按需切换模型。"""
    global _session, _session_model, _ready
    if _session is not None and _session_model == model_name:
        return _session

    logger.info("Loading rembg session: model=%s ...", model_name)
    t0 = time.time()
    try:
        from rembg import new_session

        _session = new_session(model_name)
        _session_model = model_name
        _ready = True
        logger.info("Session loaded in %.1fs", time.time() - t0)
    except Exception as exc:
        _ready = False
        logger.exception("Failed to load session: %s", exc)
        raise
    return _session


# ---------------------------------------------------------------------------
# 启动时预加载
# ---------------------------------------------------------------------------
@app.on_event("startup")
async def _startup():
    try:
        _load_session(EP_MODEL_NAME)
    except Exception:
        logger.warning("Preload failed; will retry on first request.")


# ---------------------------------------------------------------------------
# 健康检查 & 信息
# ---------------------------------------------------------------------------
@app.get("/health")
async def health():
    return JSONResponse(
        content={
            "status": "ok" if _ready else "loading",
            "model": _session_model,
        },
        status_code=200 if _ready else 503,
    )


@app.get("/info")
async def info():
    return {
        "module": "rembg",
        "version": "2.0.50",
        "model": _session_model,
        "ready": _ready,
        "capabilities": ["remove_bg"],
        # 与 module.toml [compute].backends 保持一致：rembg[cpu] 栈仅 CPU
        "backends": ["cpu"],
    }


# ---------------------------------------------------------------------------
# 核心推理
# ---------------------------------------------------------------------------
def _run_remove_bg(
    input_bytes: bytes,
    model_name: str,
    alpha_matting: bool = False,
    post_process: bool = True,
) -> bytes:
    """调用 rembg 执行背景移除，返回 PNG bytes。"""
    from rembg import remove

    session = _load_session(model_name)
    output_bytes = remove(
        input_bytes,
        session=session,
        alpha_matting=alpha_matting,
        post_process_mask=post_process,
    )
    return output_bytes


def _error(status_code: int, error_code: str, message: str, detail: Optional[str] = None):
    """构造 ADAPTER_API.md §2.3 定义的错误响应。"""
    return JSONResponse(
        status_code=status_code,
        content={
            "status": "error",
            "error_code": error_code,
            "message": message,
            "detail": detail,
        },
    )


def _parse_bool(value, default: bool) -> bool:
    """宽容解析布尔参数（JSON bool / 字符串 "true"/"false"）。"""
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        v = value.strip().lower()
        if v in ("true", "1", "yes"):
            return True
        if v in ("false", "0", "no"):
            return False
    return default


@app.post("/predict/remove_bg")
async def predict_remove_bg(
    request: Request,
    file: Optional[UploadFile] = File(None),
    input_path_form: Optional[str] = Form(None, alias="input_path"),
    params_form: Optional[str] = Form(None, alias="params"),
    model_form: Optional[str] = Form(None, alias="model"),
    alpha_matting_form: Optional[str] = Form(None, alias="alpha_matting"),
    post_process_form: Optional[str] = Form(None, alias="post_process"),
):
    """
    移除图片背景，输出透明 PNG。

    支持三种输入方式：
    - multipart file 上传（ep-core executor 文件类产物路径）
    - multipart/JSON 的 input_path 指定服务器端文件路径
    - JSON body {"input_path": ...}（ADAPTER_API.md 格式 B）

    参数来源：`params` 字段（JSON 对象/字符串，executor 契约）优先，
    兼容旧版平铺表单字段（model/alpha_matting/post_process）。

    响应契约（ep-core executor 权威）：
    {status:"completed", output_type:"file", result:<输出路径>,
     output_path:<输出路径>, metadata:{...}, elapsed_seconds:<float>}
    """
    t0 = time.time()

    # ---- 解析输入与参数 ----
    input_bytes: bytes = b""
    source_name: str = ""
    params: dict = {}
    model_override: Optional[str] = None
    alpha_matting = False
    post_process = True

    content_type = request.headers.get("content-type", "")
    try:
        if "application/json" in content_type:
            # JSON body：路径输入（格式 B）
            body = await request.json()
            params = body.get("params") or {}
            if not isinstance(params, dict):
                params = {}
            input_path = body.get("input_path")
            if not input_path:
                return _error(
                    400, "INVALID_INPUT",
                    "No input provided (need 'input_path' in JSON body or 'file' in multipart)",
                )
            p = Path(input_path)
            if not p.is_file():
                return _error(400, "FILE_NOT_FOUND", f"input_path not found: {input_path}")
            input_bytes = p.read_bytes()
            source_name = p.name
        else:
            # multipart/form-data：params 为 JSON 字符串（executor 契约）
            if params_form:
                try:
                    parsed = json.loads(params_form)
                    if isinstance(parsed, dict):
                        params = parsed
                except json.JSONDecodeError:
                    pass
            input_path = input_path_form
            model_override = model_form
            alpha_matting = _parse_bool(alpha_matting_form, False)
            post_process = _parse_bool(post_process_form, True)

            if file is not None and file.filename:
                input_bytes = await file.read()
                source_name = file.filename
            elif input_path:
                p = Path(input_path)
                if not p.is_file():
                    return _error(400, "FILE_NOT_FOUND", f"input_path not found: {input_path}")
                input_bytes = p.read_bytes()
                source_name = p.name
            else:
                return _error(
                    400, "INVALID_INPUT",
                    "Provide either a multipart 'file' or an 'input_path' field.",
                )
    except Exception as exc:
        logger.exception("Failed to parse request")
        return _error(400, "INVALID_INPUT", f"Failed to parse request: {exc}")

    # params（executor 注入/用户参数）覆盖平铺表单默认值
    if "model" in params:
        model_override = str(params.get("model") or model_override)
    if "alpha_matting" in params:
        alpha_matting = _parse_bool(params.get("alpha_matting"), False)
    if "post_process" in params:
        post_process = _parse_bool(params.get("post_process"), True)

    if not input_bytes:
        return _error(400, "INVALID_INPUT", "Empty input image.")

    # 大小限制 50 MB
    if len(input_bytes) > 50 * 1024 * 1024:
        return _error(413, "INVALID_INPUT", "Input exceeds 50 MB limit.")

    model_name = model_override or EP_MODEL_NAME

    # ---- 推理 ----
    try:
        output_bytes = _run_remove_bg(
            input_bytes,
            model_name=model_name,
            alpha_matting=alpha_matting,
            post_process=post_process,
        )
    except Exception as exc:
        logger.exception("Inference failed")
        return _error(500, "INFERENCE_ERROR", f"Inference error: {exc}")

    # ---- 写出：params.output_path（模块产物协议注入）优先，否则 workspace ----
    try:
        injected = params.get("output_path")
        if injected:
            out_path = Path(str(injected))
            out_path.parent.mkdir(parents=True, exist_ok=True)
        else:
            ws = Path(EP_WORKSPACE)
            ws.mkdir(parents=True, exist_ok=True)
            stem = Path(source_name).stem or "output"
            out_path = ws / f"{stem}_{uuid.uuid4().hex[:8]}.png"
        out_path.write_bytes(output_bytes)
    except Exception as exc:
        logger.exception("Failed to write output")
        return _error(500, "INTERNAL_ERROR", f"Output write error: {exc}")

    elapsed = round(time.time() - t0, 3)
    logger.info("Done: %s -> %s (%d bytes, %.2fs)", source_name, out_path, len(output_bytes), elapsed)

    return {
        "status": "completed",
        "output_type": "file",
        "result": str(out_path),
        "output_path": str(out_path),
        "metadata": {
            "model": model_name,
            "alpha_matting": alpha_matting,
            "post_process": post_process,
            "output_size_bytes": len(output_bytes),
        },
        "elapsed_seconds": elapsed,
    }


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    logger.info(
        "Starting rembg adapter on %s:%d (model=%s, workspace=%s)",
        EP_HOST,
        EP_PORT,
        EP_MODEL_NAME,
        EP_WORKSPACE,
    )
    uvicorn.run(
        app,
        host=EP_HOST,
        port=EP_PORT,
        log_level=EP_LOG_LEVEL.lower(),
    )
