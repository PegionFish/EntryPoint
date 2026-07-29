"""adapter.py — EntryPoint faster-whisper ASR 模块适配器"""

import json
import logging
import os
import sys
import tempfile
import time
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Optional, Union

# Windows: 将 venv Scripts 目录加入 DLL 搜索路径（CUDA 库等）
if sys.platform == "win32":
    _scripts = Path(sys.executable).parent
    if hasattr(os, "add_dll_directory"):
        os.add_dll_directory(str(_scripts))
    os.environ["PATH"] = str(_scripts) + os.pathsep + os.environ.get("PATH", "")

import uvicorn
from fastapi import FastAPI, Request, UploadFile, File, Form
from fastapi.responses import JSONResponse

# ── 环境变量 ──────────────────────────────────────────────
EP_PORT = int(os.environ.get("EP_PORT", "18000"))
EP_MODEL_DIR = os.environ.get("EP_MODEL_DIR", "")
EP_MODEL_ID = os.environ.get("EP_MODEL_ID", "large-v3")
EP_DEVICE = os.environ.get("EP_DEVICE", "cpu")
EP_BACKEND = os.environ.get("EP_BACKEND", "cpu")
EP_DEVICE_INDEX = os.environ.get("EP_DEVICE_INDEX", "0")
EP_WORKSPACE = os.environ.get("EP_WORKSPACE", "")
EP_MODULE_ID = os.environ.get("EP_MODULE_ID", "faster-whisper")

MODULE_NAME = "Faster-Whisper ASR"
MODULE_VERSION = "1.1.0"

logger = logging.getLogger(EP_MODULE_ID)

# ── 计算类型映射 ──────────────────────────────────────────
COMPUTE_TYPE_MAP = {
    "cuda": "float16",
    "rocm": "float16",
    "cpu": "int8",
}

DEVICE_MAP = {
    "cuda": "cuda",
    "rocm": "cuda",
    "cpu": "cpu",
}

# ── 模型状态 ──────────────────────────────────────────────
model = None
model_load_error: Optional[str] = None


def _load_model():
    """加载 faster-whisper 模型"""
    global model, model_load_error

    if not EP_MODEL_DIR:
        model_load_error = "EP_MODEL_DIR environment variable is not set"
        logger.error(model_load_error)
        return

    model_dir = Path(EP_MODEL_DIR)
    if not model_dir.exists():
        model_load_error = f"Model directory not found: {EP_MODEL_DIR}"
        logger.error(model_load_error)
        return

    try:
        from faster_whisper import WhisperModel

        device = DEVICE_MAP.get(EP_BACKEND, "cpu")
        compute_type = COMPUTE_TYPE_MAP.get(EP_BACKEND, "int8")

        logger.info(
            "Loading model from %s | device=%s compute_type=%s",
            EP_MODEL_DIR, device, compute_type,
        )
        try:
            model = WhisperModel(
                str(model_dir),
                device=device,
                compute_type=compute_type,
            )
        except ValueError:
            # 部分 GPU（如 Tesla P4）不支持 float16，回退到 int8
            fallback = "int8" if device == "cuda" else "int8"
            logger.warning(
                "compute_type=%s not supported, falling back to %s",
                compute_type, fallback,
            )
            model = WhisperModel(
                str(model_dir),
                device=device,
                compute_type=fallback,
            )
        logger.info("Model loaded successfully")
    except Exception as exc:
        model_load_error = f"Failed to load model: {exc}"
        logger.exception(model_load_error)


@asynccontextmanager
async def lifespan(app: FastAPI):
    """启动时加载模型，关闭时释放资源"""
    _load_model()
    yield
    global model
    if model is not None:
        del model
        model = None


app = FastAPI(title=MODULE_NAME, version=MODULE_VERSION, lifespan=lifespan)


# ── 辅助函数 ──────────────────────────────────────────────

def error_response(status_code: int, error_code: str, message: str, detail: Optional[str] = None):
    return JSONResponse(
        status_code=status_code,
        content={
            "status": "error",
            "error_code": error_code,
            "message": message,
            "detail": detail,
        },
    )


def _parse_params(raw: Union[dict, str, None]) -> dict:
    """解析参数，支持 dict 或 JSON 字符串"""
    if raw is None:
        return {}
    if isinstance(raw, str):
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return {}
    return raw


def _clamp_beam_size(value) -> int:
    try:
        v = int(value)
        return max(1, min(20, v))
    except (TypeError, ValueError):
        return 5


# ── 标准端点 ──────────────────────────────────────────────

@app.get("/health")
def health():
    if model is None:
        return JSONResponse(
            status_code=503,
            content={"status": "loading", "detail": model_load_error},
        )
    return {"status": "ok"}


@app.get("/info")
def info():
    return {
        "module_id": EP_MODULE_ID,
        "name": MODULE_NAME,
        "version": MODULE_VERSION,
        "model_id": EP_MODEL_ID,
        "device": EP_DEVICE,
        "backend": EP_BACKEND,
        "capabilities": [
            {
                "name": "transcribe",
                "input_type": "audio",
                "output_type": "json",
                "params": {
                    "language": {"type": "string", "default": "auto"},
                    "timestamps": {"type": "boolean", "default": True},
                    "beam_size": {"type": "integer", "default": 5, "min": 1, "max": 20},
                    "vad_filter": {"type": "boolean", "default": True},
                    "condition_on_previous": {"type": "boolean", "default": True},
                },
            }
        ],
    }


# ── 推理端点 ──────────────────────────────────────────────

@app.post("/predict/{capability}")
async def predict(
    capability: str,
    request: Request,
    file: Optional[UploadFile] = File(None),
    params_form: Optional[str] = Form(None, alias="params"),
):
    # 1) 校验 capability
    if capability != "transcribe":
        return error_response(
            404, "INVALID_CAPABILITY",
            f"Unknown capability: {capability}",
            "Supported capabilities: transcribe",
        )

    # 2) 校验模型
    if model is None:
        return error_response(
            503, "MODEL_NOT_LOADED",
            "Model is not loaded yet",
            model_load_error,
        )

    # 3) 解析输入 —— 根据 Content-Type 区分 multipart 与 JSON
    content_type = request.headers.get("content-type", "")
    audio_path: Optional[str] = None
    params_dict: dict = {}
    tmp_file: Optional[Path] = None

    try:
        if "multipart/form-data" in content_type:
            # 格式 A: multipart 文件上传
            params_dict = _parse_params(params_form)
            if file is not None and file.filename:
                work_dir = Path(EP_WORKSPACE or tempfile.gettempdir()) / EP_MODULE_ID
                work_dir.mkdir(parents=True, exist_ok=True)
                tmp_file = work_dir / file.filename
                tmp_file.write_bytes(await file.read())
                audio_path = str(tmp_file)
            else:
                return error_response(
                    400, "INVALID_INPUT",
                    "No file provided in multipart request",
                )
        else:
            # 格式 B/C: JSON body
            try:
                body = await request.json()
            except Exception:
                return error_response(
                    400, "INVALID_INPUT",
                    "Request body is not valid JSON",
                )
            params_dict = _parse_params(body.get("params"))
            audio_path = body.get("input_path")
            if not audio_path:
                return error_response(
                    400, "INVALID_INPUT",
                    "No input provided: need 'input_path' in JSON body or 'file' in multipart",
                )

        # 4) 校验文件存在
        if not Path(audio_path).is_file():
            return error_response(
                400, "FILE_NOT_FOUND",
                f"Input file not found: {audio_path}",
            )

        # 5) 提取参数
        language = params_dict.get("language", "auto")
        if language == "auto":
            language = None
        word_timestamps = bool(params_dict.get("timestamps", True))
        beam_size = _clamp_beam_size(params_dict.get("beam_size", 5))
        vad_filter = bool(params_dict.get("vad_filter", True))
        condition_on_previous = bool(params_dict.get("condition_on_previous", True))

        # 6) 执行推理
        t0 = time.perf_counter()
        try:
            segments_gen, transcribe_info = model.transcribe(
                audio_path,
                language=language,
                beam_size=beam_size,
                vad_filter=vad_filter,
                word_timestamps=word_timestamps,
                condition_on_previous_text=condition_on_previous,
            )

            # 消费生成器
            segments_out = []
            full_text_parts = []
            for seg in segments_gen:
                seg_data = {
                    "start": round(seg.start, 3),
                    "end": round(seg.end, 3),
                    "text": seg.text.strip(),
                }
                if word_timestamps and seg.words:
                    seg_data["words"] = [
                        {
                            "start": round(w.start, 3),
                            "end": round(w.end, 3),
                            "word": w.word,
                            "probability": round(w.probability, 4),
                        }
                        for w in seg.words
                    ]
                segments_out.append(seg_data)
                full_text_parts.append(seg.text.strip())

            elapsed = round(time.perf_counter() - t0, 3)
            full_text = " ".join(full_text_parts)

            return {
                "status": "completed",
                "output_type": "json",
                "result": {
                    "text": full_text,
                    "segments": segments_out,
                    "language": transcribe_info.language,
                    "duration_seconds": round(transcribe_info.duration, 3),
                },
                "output_path": None,
                "elapsed_seconds": elapsed,
            }
        except Exception as exc:
            logger.exception("Inference failed")
            return error_response(
                500, "INFERENCE_ERROR",
                f"Transcription failed: {exc}",
            )
    finally:
        # 清理临时文件
        if tmp_file is not None and tmp_file.exists():
            try:
                tmp_file.unlink()
            except OSError:
                pass


# ── 启动 ──────────────────────────────────────────────────
if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    uvicorn.run(app, host="0.0.0.0", port=EP_PORT, log_level="info")
