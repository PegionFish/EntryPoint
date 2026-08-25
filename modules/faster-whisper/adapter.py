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
model_device = "cpu"  # 模型实际加载所用设备（回退后为 cpu）
model_compute_type: Optional[str] = None  # 模型实际加载所用精度（compute_type）
model_load_error: Optional[str] = None


def _load_model():
    """加载 faster-whisper 模型"""
    global model, model_device, model_compute_type, model_load_error

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
        loaded_compute_type = compute_type

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
            fallback = "int8"
            logger.warning(
                "compute_type=%s not supported, falling back to %s",
                compute_type, fallback,
            )
            model = WhisperModel(
                str(model_dir),
                device=device,
                compute_type=fallback,
            )
            loaded_compute_type = fallback
        model_device = device
        model_compute_type = loaded_compute_type
        logger.info("Model loaded successfully")
    except Exception as exc:
        model_load_error = f"Failed to load model: {exc}"
        logger.exception(model_load_error)


def _reload_model_on_cpu() -> bool:
    """GPU 推理失败时的设备级回退：以 CPU 重新加载模型。成功返回 True。"""
    global model, model_device, model_compute_type, model_load_error
    try:
        from faster_whisper import WhisperModel

        logger.warning("Reloading model on CPU (device-level fallback)")
        model = WhisperModel(str(Path(EP_MODEL_DIR)), device="cpu", compute_type="int8")
        model_device = "cpu"
        model_compute_type = "int8"
        model_load_error = None
        return True
    except Exception as exc:
        model_load_error = f"CPU fallback reload failed: {exc}"
        logger.exception(model_load_error)
        return False


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


PRECISION_OPTIONS = ("float16", "int8")


def _resolve_precision(requested) -> str:
    """请求精度 → 实际 compute_type：CPU 强制 int8（即使参数 float16）。"""
    if EP_BACKEND == "cpu" or str(EP_DEVICE).startswith("cpu"):
        return "int8"
    if isinstance(requested, str) and requested in PRECISION_OPTIONS:
        return requested
    return "float16"


def _ensure_precision(precision: str) -> bool:
    """确保模型以指定精度加载；已一致时不动作。重新加载失败时返回 False
    （model_load_error 已内含原因，旧模型保留）。"""
    global model, model_device, model_compute_type, model_load_error
    if model is None:
        return True
    if model_device == "cpu":
        # 设备级回退后（GPU 故障 → CPU int8）：CPU 锁定 int8，不发起 GPU 换载
        precision = "int8"
    if model_compute_type == precision:
        return True
    try:
        from faster_whisper import WhisperModel

        device = DEVICE_MAP.get(EP_BACKEND, "cpu")
        logger.info(
            "Reloading model with compute_type=%s (precision switch)", precision,
        )
        model = WhisperModel(
            str(Path(EP_MODEL_DIR)),
            device=device,
            compute_type=precision,
        )
        model_device = device
        model_compute_type = precision
        model_load_error = None
        return True
    except Exception as exc:
        model_load_error = f"Precision switch to {precision} failed: {exc}"
        logger.exception(model_load_error)
        return False


def _srt_timestamp(seconds: float) -> str:
    """秒 → SRT 时间戳 HH:MM:SS,mmm"""
    ms = int(round(seconds * 1000))
    h, ms = divmod(ms, 3600_000)
    m, ms = divmod(ms, 60_000)
    s, ms = divmod(ms, 1000)
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


def _segments_to_srt(segments) -> str:
    """识别片段 → SRT 字幕文本"""
    lines = []
    for idx, seg in enumerate(segments, start=1):
        lines.append(str(idx))
        lines.append(
            f"{_srt_timestamp(seg['start'])} --> {_srt_timestamp(seg['end'])}"
        )
        lines.append(seg.get("text", "").strip())
        lines.append("")
    return "\n".join(lines)


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
                    "precision": {"type": "select", "options": ["float16", "int8"], "default": "float16"},
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
        precision = _resolve_precision(params_dict.get("precision", "float16"))

        if not _ensure_precision(precision):
            return error_response(
                503, "MODEL_LOAD_ERROR",
                f"Failed to switch model precision to {precision}",
                model_load_error,
            )

        # 6) 执行推理（GPU 失败时设备级回退 CPU 重试一次）
        def _do_transcribe():
            t0 = time.perf_counter()
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

        try:
            result = _do_transcribe()
        except Exception as exc:
            if model_device != "cpu" and _reload_model_on_cpu():
                logger.warning(
                    "GPU inference failed (%s); retrying on CPU", exc,
                )
                try:
                    result = _do_transcribe()
                except Exception as exc_cpu:
                    logger.exception("Inference failed on CPU")
                    return error_response(
                        500, "INFERENCE_ERROR",
                        f"Transcription failed: {exc_cpu}",
                    )
            else:
                logger.exception("Inference failed")
                return error_response(
                    500, "INFERENCE_ERROR",
                    f"Transcription failed: {exc}",
                )

        # 7) 文件导出：output_format=srt 时把识别结果写成字幕文件
        #    （output_path 由管线执行器注入，见 MODULE_SPEC 模块产物协议）
        output_format = str(params_dict.get("output_format") or "json").lower()
        output_path = params_dict.get("output_path")
        if output_format == "srt" and output_path:
            try:
                srt_text = _segments_to_srt(result["result"]["segments"])
                Path(output_path).parent.mkdir(parents=True, exist_ok=True)
                Path(output_path).write_text(srt_text, encoding="utf-8")
                logger.info("SRT written to %s", output_path)
                return {
                    "status": "completed",
                    "output_type": "file",
                    "result": str(output_path),
                    "output_path": str(output_path),
                    "elapsed_seconds": result["elapsed_seconds"],
                }
            except Exception as exc:
                logger.exception("SRT export failed")
                return error_response(
                    500, "EXPORT_ERROR",
                    f"SRT export failed: {exc}",
                )
        return result
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
    # 绑定地址读 EP_HOST（daemon 注入，缺省回环）——硬编码 0.0.0.0 会触发
    # Windows 防火墙弹窗，见 ep-core process.rs build_module_env（EP_HOST=127.0.0.1）
    uvicorn.run(app, host=os.getenv("EP_HOST", "127.0.0.1"), port=EP_PORT, log_level="info")
