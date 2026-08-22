"""
ONNX Matting 图像抠图 — EntryPoint adapter
基于 BiRefNet ONNX 权重（ZhengPeng7，MIT）的 HTTP 服务，提供通用/人像 alpha 抠图。
ONNX Runtime 多 EP：EP_BACKEND ∈ {cuda, rocm, openvino, cpu}，providers 恒以 CPU
兜底收尾；openvino 时读 OPENVINO_DEVICE 注入 provider option 键 device_type。

预处理对齐 BiRefNet 官方推理脚本：resize 到 1024x1024、/255、ImageNet
mean/std 归一化、NCHW float32；输出经 sigmoid 得 alpha mask，上采样回原尺寸，
与原图合成透明 PNG。
"""

from __future__ import annotations

import io
import json
import logging
import os
import time
import uuid
from pathlib import Path
from typing import Dict, List, Optional

import numpy as np
import uvicorn
from fastapi import FastAPI, File, Form, Request, UploadFile
from fastapi.responses import JSONResponse
from PIL import Image

from ort_ep import ProviderSpec, provider_names, resolve_providers_from_env

# ---------------------------------------------------------------------------
# 环境变量
# ---------------------------------------------------------------------------
EP_HOST: str = os.getenv("EP_HOST", "127.0.0.1")
EP_PORT: int = int(os.getenv("EP_PORT", "8901"))
EP_WORKSPACE: str = os.getenv("EP_WORKSPACE", os.path.join(os.getcwd(), "workspace"))
# daemon 注入激活变体 id（ep-core process.rs build_module_env）
EP_MODEL_NAME: str = os.getenv("EP_MODEL_ID", os.getenv("EP_MODEL_NAME", "birefnet-general"))
EP_DEVICE_INDEX: str = os.getenv("EP_DEVICE_INDEX", "0")
EP_LOG_LEVEL: str = os.getenv("EP_LOG_LEVEL", "INFO")
EP_BACKEND: str = os.getenv("EP_BACKEND", "cpu")
EP_MODEL_DIR: str = os.getenv("EP_MODEL_DIR", "")
EP_MODELS_ROOT: str = os.getenv("EP_MODELS_ROOT", "")

# 与 module.toml [[models]] 的 target_dir 约定对齐
MODEL_TARGET_DIRS: Dict[str, str] = {
    "birefnet-general": "onnx-matting-birefnet-general",
    "birefnet-portrait": "onnx-matting-birefnet-portrait",
}

# BiRefNet 输入规格（官方 repo config：input size 1024x1024）
INPUT_SIZE = (1024, 1024)
IMAGENET_MEAN = np.array([0.485, 0.456, 0.406], dtype=np.float32)
IMAGENET_STD = np.array([0.229, 0.224, 0.225], dtype=np.float32)

MAX_INPUT_BYTES = 100 * 1024 * 1024


class ModelLocalMissingError(RuntimeError):
    """请求的模型本地 ONNX 权重缺失：明确报错并给出获取指引。"""


def resolve_local_model_file(model_name: str) -> Optional[Path]:
    """解析 model_name 的本地 ONNX 文件路径。

    命名约定：<target_dir>/<model_name>.onnx（如 onnx-matting-birefnet-general/
    birefnet-general.onnx）；目录内无约定名但恰有一个 *.onnx 时按手动放置宽容接受。
    """
    candidates: List[Path] = []
    target = MODEL_TARGET_DIRS.get(model_name)
    if target and EP_MODELS_ROOT:
        candidates.append(Path(EP_MODELS_ROOT) / target)
    if EP_MODEL_DIR:
        candidates.append(Path(EP_MODEL_DIR))
    for d in candidates:
        exact = d / f"{model_name}.onnx"
        if exact.is_file():
            return exact
        onnx_files = sorted(d.glob("*.onnx"))
        if len(onnx_files) == 1:
            return onnx_files[0]
    return None


logging.basicConfig(
    level=getattr(logging, EP_LOG_LEVEL.upper(), logging.INFO),
    format="%(asctime)s [onnx-matting-adapter] %(levelname)s %(message)s",
)
logger = logging.getLogger("onnx-matting-adapter")

PROVIDERS: List[ProviderSpec] = resolve_providers_from_env()


def _sess_opts():
    """openvino 后端按 OV EP 官方建议关闭 ORT 图优化；其余走默认。"""
    if EP_BACKEND.strip().lower() != "openvino":
        return None
    try:
        import onnxruntime as ort

        opts = ort.SessionOptions()
        opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_DISABLE_ALL
        return opts
    except Exception as exc:  # pragma: no cover - 防御路径
        logger.warning("SessionOptions unavailable, using defaults: %s", exc)
        return None


_sessions: Dict[str, object] = {}
_effective_providers: List[str] = []

app = FastAPI(
    title="ONNX Matting Adapter",
    version="0.1.0",
    description="EntryPoint BiRefNet 图像抠图模块",
)


def _get_session(model_name: str):
    """懒加载 InferenceSession 并缓存（变体间切换无需重启进程）。"""
    global _effective_providers
    existing = _sessions.get(model_name)
    if existing is not None:
        return existing

    model_path = resolve_local_model_file(model_name)
    if model_path is None or not model_path.is_file():
        target = MODEL_TARGET_DIRS.get(
            model_name, f"onnx-matting-{model_name}"
        )
        expected_dir = (
            Path(EP_MODELS_ROOT) / target
            if EP_MODELS_ROOT
            else Path("<EP_MODELS_ROOT>") / target
        )
        raise ModelLocalMissingError(
            f"Local ONNX weights for model '{model_name}' not found: expected "
            f"{expected_dir / f'{model_name}.onnx'}. See README '权重获取' "
            f"(upstream ZhengPeng7/{model_name.replace('-', '_')}, MIT); place the "
            f".onnx file there and retry."
        )

    logger.info(
        "Loading ONNX session: model=%s weights=%s providers=%s ...",
        model_name,
        model_path,
        provider_names(PROVIDERS),
    )
    t0 = time.time()
    try:
        import onnxruntime as ort

        session = ort.InferenceSession(
            str(model_path),
            sess_options=_sess_opts(),
            providers=PROVIDERS,
        )
        _sessions[model_name] = session
        _effective_providers = list(session.get_providers())
        logger.info(
            "Session loaded in %.1fs (active providers=%s)",
            time.time() - t0,
            _effective_providers,
        )
        return session
    except Exception as exc:
        logger.exception("Failed to load session: %s", exc)
        raise


# ---------------------------------------------------------------------------
# 预处理 / 推理
# ---------------------------------------------------------------------------
def _preprocess(img: Image.Image) -> np.ndarray:
    """RGB resize→1024x1024，/255，ImageNet mean/std，CHW，batch=1。"""
    im = img.convert("RGB").resize(INPUT_SIZE, Image.Resampling.LANCZOS)
    arr = np.asarray(im, dtype=np.float32) / 255.0
    arr = (arr - IMAGENET_MEAN) / IMAGENET_STD
    return arr.transpose(2, 0, 1)[None, ...].astype(np.float32)


def _sigmoid(x: np.ndarray) -> np.ndarray:
    # clip 抑制 exp 溢出（logits 幅值大时）
    return 1.0 / (1.0 + np.exp(-np.clip(x, -30.0, 30.0)))


def _run_matte(input_bytes: bytes, model_name: str) -> bytes:
    """BiRefNet 推理：logits → sigmoid → alpha mask 回原尺寸 → RGBA PNG。"""
    session = _get_session(model_name)
    img = Image.open(io.BytesIO(input_bytes))
    x = _preprocess(img)

    input_name = session.get_inputs()[0].name
    logits = session.run(None, {input_name: x})[0]

    mask = np.squeeze(np.asarray(logits, dtype=np.float32))
    if mask.ndim == 3:  # [1, H, W] → [H, W]
        mask = mask[0]
    if mask.ndim != 2:
        raise ValueError(f"Unexpected output shape after squeeze: {mask.shape}")
    alpha = (_sigmoid(mask) * 255.0).round().astype(np.uint8)

    alpha_img = Image.fromarray(alpha, mode="L").resize(img.size, Image.Resampling.LANCZOS)
    rgba = img.convert("RGB")
    rgba.putalpha(alpha_img)

    buf = io.BytesIO()
    rgba.save(buf, format="PNG")
    return buf.getvalue()


# ---------------------------------------------------------------------------
# HTTP 层（契约同 rembg adapter：ADAPTER_API.md + 模块产物协议 §5）
# ---------------------------------------------------------------------------
def _error(status_code: int, error_code: str, message: str, detail: Optional[str] = None):
    return JSONResponse(
        status_code=status_code,
        content={
            "status": "error",
            "error_code": error_code,
            "message": message,
            "detail": detail,
        },
    )


@app.get("/health")
async def health():
    ready = bool(_sessions)
    return JSONResponse(
        content={
            "status": "ok" if ready else "loading",
            "model": next(iter(_sessions), None),
        },
        status_code=200 if ready else 503,
    )


@app.get("/info")
async def info():
    return {
        "module": "onnx-matting",
        "version": "0.1.0",
        "model": next(iter(_sessions), None),
        "ready": bool(_sessions),
        "capabilities": ["matte"],
        # 与 module.toml [compute].backends 保持一致
        "backends": ["cuda", "rocm", "openvino", "cpu"],
        "ep_backend": EP_BACKEND,
        "requested_providers": provider_names(PROVIDERS),
        "providers": _effective_providers,
    }


def _parse_bool(value, default: bool) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        v = value.strip().lower()
        if v in ("true", "1", "yes"):
            return True
        if v in ("false", "0", "no"):
            return False
    return default


@app.post("/predict/matte")
async def predict_matte(
    request: Request,
    file: Optional[UploadFile] = File(None),
    input_path_form: Optional[str] = Form(None, alias="input_path"),
    params_form: Optional[str] = Form(None, alias="params"),
    model_form: Optional[str] = Form(None, alias="model"),
):
    """
    图像 alpha 抠图，输出透明 PNG（原分辨率）。

    支持三种输入方式：
    - multipart file 上传（ep-core executor 文件类产物路径）
    - multipart/JSON 的 input_path 指定服务器端文件路径
    - JSON body {"input_path": ...}（ADAPTER_API.md 格式 B）

    params.model 可覆盖变体（解析 EP_MODELS_ROOT 下对应 target_dir）。
    params.output_path 注入时写文件产物协议响应。
    """
    t0 = time.time()

    input_bytes: bytes = b""
    source_name: str = ""
    params: dict = {}
    model_override: Optional[str] = None

    content_type = request.headers.get("content-type", "")
    try:
        if "application/json" in content_type:
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
            if params_form:
                try:
                    parsed = json.loads(params_form)
                    if isinstance(parsed, dict):
                        params = parsed
                except json.JSONDecodeError:
                    pass
            input_path = input_path_form
            model_override = model_form

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

    if "model" in params:
        requested = str(params.get("model") or "").strip()
        if requested:
            model_override = requested

    if not input_bytes:
        return _error(400, "INVALID_INPUT", "Empty input image.")

    if len(input_bytes) > MAX_INPUT_BYTES:
        return _error(413, "INVALID_INPUT", f"Input exceeds {MAX_INPUT_BYTES // (1024*1024)} MB limit.")

    model_name = model_override or EP_MODEL_NAME

    # ---- 推理 ----
    try:
        output_bytes = _run_matte(input_bytes, model_name)
    except ModelLocalMissingError as exc:
        logger.error("Model weights missing: %s", exc)
        return _error(503, "MODEL_NOT_LOADED", str(exc))
    except ValueError as exc:
        logger.error("Inference failed: %s", exc)
        return _error(500, "INFERENCE_ERROR", f"Inference error: {exc}")
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
            "output_size_bytes": len(output_bytes),
        },
        "elapsed_seconds": elapsed,
    }


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    logger.info(
        "Starting onnx-matting adapter on %s:%d (model=%s, backend=%s, providers=%s, workspace=%s)",
        EP_HOST,
        EP_PORT,
        EP_MODEL_NAME,
        EP_BACKEND,
        provider_names(PROVIDERS),
        EP_WORKSPACE,
    )
    uvicorn.run(
        app,
        host=EP_HOST,
        port=EP_PORT,
        log_level=EP_LOG_LEVEL.lower(),
    )
