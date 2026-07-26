"""
PaddleOCR adapter for EntryPoint module runtime.

Exposes PP-OCRv4 detection + recognition + angle classification
through a FastAPI HTTP service.
"""

from __future__ import annotations

import logging
import os
import shutil
import tempfile
import time
import traceback
from pathlib import Path
from typing import Any, Optional

import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, Query, UploadFile
from fastapi.responses import JSONResponse
from pydantic import BaseModel

# ---------------------------------------------------------------------------
# Environment / configuration
# ---------------------------------------------------------------------------

EP_HOST = os.getenv("EP_HOST", "127.0.0.1")
EP_PORT = int(os.getenv("EP_PORT", "8000"))
EP_BACKEND = os.getenv("EP_BACKEND", "cpu").lower()
EP_MODEL_DIR = os.getenv("EP_MODEL_DIR", "")
EP_DEVICE_INDEX = os.getenv("EP_DEVICE_INDEX", "0")

MODULE_ID = "paddleocr"
MODULE_VERSION = "2.8.0"

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger(MODULE_ID)

# ---------------------------------------------------------------------------
# Lazy OCR engine
# ---------------------------------------------------------------------------

_ocr_engine: Any = None
_ocr_engine_lang: str | None = None
_ocr_engine_angle_cls: bool | None = None


def _get_ocr(lang: str = "ch", use_angle_cls: bool = True, det_db_thresh: float = 0.3):
    """Return a (cached) PaddleOCR instance, recreating it when params change."""
    global _ocr_engine, _ocr_engine_lang, _ocr_engine_angle_cls

    use_gpu = EP_BACKEND == "cuda"

    if (
        _ocr_engine is not None
        and _ocr_engine_lang == lang
        and _ocr_engine_angle_cls == use_angle_cls
    ):
        return _ocr_engine

    logger.info(
        "Initialising PaddleOCR  lang=%s  angle_cls=%s  gpu=%s  det_db_thresh=%.2f",
        lang,
        use_angle_cls,
        use_gpu,
        det_db_thresh,
    )

    from paddleocr import PaddleOCR  # heavy import – keep lazy

    kwargs: dict[str, Any] = dict(
        use_angle_cls=use_angle_cls,
        lang=lang,
        use_gpu=use_gpu,
        det_db_thresh=det_db_thresh,
        show_log=False,
    )

    # Point to pre-downloaded model directory when provided
    if EP_MODEL_DIR and Path(EP_MODEL_DIR).is_dir():
        det_dir = Path(EP_MODEL_DIR) / "det"
        rec_dir = Path(EP_MODEL_DIR) / "rec"
        cls_dir = Path(EP_MODEL_DIR) / "cls"
        if det_dir.is_dir():
            kwargs["det_model_dir"] = str(det_dir)
        if rec_dir.is_dir():
            kwargs["rec_model_dir"] = str(rec_dir)
        if cls_dir.is_dir():
            kwargs["cls_model_dir"] = str(cls_dir)

    _ocr_engine = PaddleOCR(**kwargs)
    _ocr_engine_lang = lang
    _ocr_engine_angle_cls = use_angle_cls
    logger.info("PaddleOCR engine ready")
    return _ocr_engine


# ---------------------------------------------------------------------------
# FastAPI app
# ---------------------------------------------------------------------------

app = FastAPI(
    title="PaddleOCR – EntryPoint Module",
    version=MODULE_VERSION,
    description="多语言 OCR：检测 + 识别 + 方向分类",
)


class RecognizeRequest(BaseModel):
    input_path: str
    lang: str = "ch"
    use_angle_cls: bool = True
    det_db_thresh: float = 0.3


# ---- health / info --------------------------------------------------------


@app.get("/health")
async def health():
    return {"status": "ok", "module": MODULE_ID, "version": MODULE_VERSION}


@app.get("/info")
async def info():
    return {
        "module": MODULE_ID,
        "version": MODULE_VERSION,
        "backend": EP_BACKEND,
        "capabilities": [
            {
                "name": "recognize",
                "description": "图片文字识别，返回文本+坐标+置信度",
                "input_type": "image",
                "output_type": "json",
                "params": {
                    "lang": {"type": "string", "default": "ch"},
                    "use_angle_cls": {"type": "boolean", "default": True},
                    "det_db_thresh": {"type": "float", "default": 0.3},
                },
            }
        ],
    }


# ---- predict --------------------------------------------------------------


def _run_ocr(image_path: str, lang: str, use_angle_cls: bool, det_db_thresh: float) -> dict:
    """Run OCR on a single image and return the structured result dict."""
    ocr = _get_ocr(lang=lang, use_angle_cls=use_angle_cls, det_db_thresh=det_db_thresh)
    raw = ocr.ocr(image_path, cls=use_angle_cls)

    lines: list[dict] = []
    full_texts: list[str] = []

    # PaddleOCR returns list-of-pages; each page is a list of detections
    pages = raw if isinstance(raw, list) else [raw]
    for page in pages:
        if not page:
            continue
        for det in page:
            try:
                bbox = [[float(pt[0]), float(pt[1])] for pt in det[0]]
                text = str(det[1][0])
                confidence = round(float(det[1][1]), 6)
                lines.append(
                    {"text": text, "confidence": confidence, "bbox": bbox}
                )
                full_texts.append(text)
            except (IndexError, TypeError, ValueError) as exc:
                logger.warning("Skipping malformed detection: %s", exc)

    return {
        "status": "completed",
        "output_type": "json",
        "result": {
            "text": "\n".join(full_texts),
            "lines": lines,
            "language": lang,
        },
    }


@app.post("/predict/recognize")
async def predict_recognize(
    file: Optional[UploadFile] = File(None),
    input_path: Optional[str] = Form(None),
    lang: str = Form("ch"),
    use_angle_cls: bool = Form(True),
    det_db_thresh: float = Form(0.3),
):
    """Recognise text in an image.

    Accepts either a multipart file upload or a JSON body with ``input_path``.
    """
    tmp_path: str | None = None

    try:
        # --- resolve image source ------------------------------------------
        if file is not None and file.filename:
            suffix = Path(file.filename).suffix or ".png"
            fd, tmp_path = tempfile.mkstemp(suffix=suffix, prefix="ep_ocr_")
            os.close(fd)
            with open(tmp_path, "wb") as f:
                shutil.copyfileobj(file.file, f)
            image_path = tmp_path
        elif input_path:
            image_path = input_path
        else:
            # Maybe the caller sent a JSON body instead of form-data.
            # FastAPI won't populate Form fields from JSON, so we return a
            # helpful error.
            raise HTTPException(
                status_code=422,
                detail="Provide either a multipart 'file' upload or an 'input_path' field.",
            )

        # --- validate -------------------------------------------------------
        if not Path(image_path).is_file():
            raise HTTPException(
                status_code=404,
                detail=f"Image not found: {image_path}",
            )

        file_size_mb = Path(image_path).stat().st_size / (1024 * 1024)
        if file_size_mb > 20:
            raise HTTPException(
                status_code=413,
                detail=f"File too large ({file_size_mb:.1f} MB). Maximum is 20 MB.",
            )

        # --- run OCR --------------------------------------------------------
        result = _run_ocr(image_path, lang, use_angle_cls, det_db_thresh)
        return JSONResponse(content=result)

    except HTTPException:
        raise
    except Exception as exc:
        logger.error("OCR failed: %s\n%s", exc, traceback.format_exc())
        return JSONResponse(
            status_code=500,
            content={
                "status": "error",
                "error": str(exc),
            },
        )
    finally:
        if tmp_path and Path(tmp_path).exists():
            try:
                os.unlink(tmp_path)
            except OSError:
                pass


# JSON-body variant (input_path as JSON) ------------------------------------
# FastAPI cannot mix Form and JSON body in the same route, so we add a
# second route that accepts a pure JSON payload.


@app.post("/predict/recognize/json")
async def predict_recognize_json(req: RecognizeRequest):
    """JSON-body variant of /predict/recognize."""
    try:
        if not Path(req.input_path).is_file():
            raise HTTPException(status_code=404, detail=f"Image not found: {req.input_path}")

        file_size_mb = Path(req.input_path).stat().st_size / (1024 * 1024)
        if file_size_mb > 20:
            raise HTTPException(
                status_code=413,
                detail=f"File too large ({file_size_mb:.1f} MB). Maximum is 20 MB.",
            )

        result = _run_ocr(req.input_path, req.lang, req.use_angle_cls, req.det_db_thresh)
        return JSONResponse(content=result)

    except HTTPException:
        raise
    except Exception as exc:
        logger.error("OCR failed: %s\n%s", exc, traceback.format_exc())
        return JSONResponse(
            status_code=500,
            content={"status": "error", "error": str(exc)},
        )


# ---------------------------------------------------------------------------
# Startup: pre-warm the engine
# ---------------------------------------------------------------------------


@app.on_event("startup")
async def _startup():
    logger.info(
        "Starting %s v%s  backend=%s  model_dir=%s",
        MODULE_ID,
        MODULE_VERSION,
        EP_BACKEND,
        EP_MODEL_DIR or "(auto-download)",
    )
    try:
        _get_ocr()
    except Exception:
        logger.warning(
            "Engine pre-warm failed (will retry on first request):\n%s",
            traceback.format_exc(),
        )


# ---------------------------------------------------------------------------
# Entrypoint
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    uvicorn.run(
        app,
        host=EP_HOST,
        port=EP_PORT,
        log_level="info",
    )
