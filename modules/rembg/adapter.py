"""
RemBG 智能去背景 — EntryPoint adapter
基于 rembg 库的 HTTP 服务，提供图像背景移除能力。
"""

from __future__ import annotations

import io
import logging
import os
import time
import uuid
from pathlib import Path
from typing import Optional

import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import JSONResponse

# ---------------------------------------------------------------------------
# 环境变量
# ---------------------------------------------------------------------------
EP_HOST: str = os.getenv("EP_HOST", "127.0.0.1")
EP_PORT: int = int(os.getenv("EP_PORT", "8900"))
EP_WORKSPACE: str = os.getenv("EP_WORKSPACE", os.path.join(os.getcwd(), "workspace"))
EP_MODEL_NAME: str = os.getenv("EP_MODEL_NAME", "u2net")
EP_DEVICE_INDEX: str = os.getenv("EP_DEVICE_INDEX", "0")
EP_LOG_LEVEL: str = os.getenv("EP_LOG_LEVEL", "INFO")

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
        "backends": ["cuda", "cpu"],
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


@app.post("/predict/remove_bg")
async def predict_remove_bg(
    file: Optional[UploadFile] = File(None),
    input_path: Optional[str] = Form(None),
    model: Optional[str] = Form(None),
    alpha_matting: bool = Form(False),
    post_process: bool = Form(True),
):
    """
    移除图片背景，输出透明 PNG。

    支持两种输入方式（二选一）：
    - multipart file 上传
    - input_path 指定服务器端文件路径
    """
    # ---- 解析输入 ----
    input_bytes: bytes
    source_name: str

    if file is not None and file.filename:
        input_bytes = await file.read()
        source_name = file.filename
    elif input_path:
        p = Path(input_path)
        if not p.is_file():
            raise HTTPException(status_code=400, detail=f"input_path not found: {input_path}")
        input_bytes = p.read_bytes()
        source_name = p.name
    else:
        raise HTTPException(
            status_code=400,
            detail="Provide either a multipart 'file' or an 'input_path' form field.",
        )

    if not input_bytes:
        raise HTTPException(status_code=400, detail="Empty input image.")

    # 大小限制 50 MB
    if len(input_bytes) > 50 * 1024 * 1024:
        raise HTTPException(status_code=413, detail="Input exceeds 50 MB limit.")

    model_name = model or EP_MODEL_NAME

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
        raise HTTPException(status_code=500, detail=f"Inference error: {exc}")

    # ---- 写出到 workspace ----
    try:
        ws = Path(EP_WORKSPACE)
        ws.mkdir(parents=True, exist_ok=True)

        stem = Path(source_name).stem or "output"
        out_name = f"{stem}_{uuid.uuid4().hex[:8]}.png"
        out_path = ws / out_name
        out_path.write_bytes(output_bytes)
    except Exception as exc:
        logger.exception("Failed to write output")
        raise HTTPException(status_code=500, detail=f"Output write error: {exc}")

    logger.info("Done: %s -> %s (%d bytes)", source_name, out_path, len(output_bytes))

    return {
        "status": "ok",
        "output_path": str(out_path),
        "model": model_name,
        "alpha_matting": alpha_matting,
        "post_process": post_process,
        "output_size_bytes": len(output_bytes),
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
