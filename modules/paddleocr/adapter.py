"""
PaddleOCR adapter for EntryPoint module runtime.

Exposes PP-OCR detection + recognition + textline orientation classification
through a FastAPI HTTP service.

Targeting paddleocr 3.x (paddlex pipeline): constructor uses `device` /
`use_textline_orientation` (2.x `use_gpu` / `use_angle_cls` / `show_log` are
rejected with "Unknown argument"); results are OCRResult dicts with
`rec_texts` / `rec_scores` / `rec_polys`.
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import tempfile
import time
import traceback
from pathlib import Path
from typing import Any, Optional

# paddlepaddle 3.3.x Windows CPU 下 PIR + oneDNN 推理存在已知崩溃
# （ConvertPirAttribute2RuntimeAttribute not support ... onednn_instruction）；
# paddlex 默认在 CPU 上启用 MKLDNN，关闭后回退原生 CPU 内核，规避该缺陷。
# GPU 路径不受影响（runner 对 GPU 本就 disable_mkldnn）。
os.environ.setdefault("PADDLE_PDX_ENABLE_MKLDNN_BYDEFAULT", "0")

import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, Query, Request, UploadFile
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
    """Return a (cached) PaddleOCR 3.x instance, recreating it when params change.

    3.x API 适配：`use_gpu` → `device`；`use_angle_cls` →
    `use_textline_orientation`；`show_log` 移除（日志走标准 logging）；
    `det_db_thresh` 语义最接近的 3.x 参数为 `text_det_box_thresh`。
    """
    global _ocr_engine, _ocr_engine_lang, _ocr_engine_angle_cls

    device = "cpu"
    if EP_BACKEND == "cuda":
        # 3.x 在 GPU 不可用时自动回退 CPU（仅警告）；paddlepaddle 为 CPU 轮子时同理
        try:
            device = f"gpu:{int(EP_DEVICE_INDEX)}"
        except (TypeError, ValueError):
            device = "gpu"

    if (
        _ocr_engine is not None
        and _ocr_engine_lang == lang
        and _ocr_engine_angle_cls == use_angle_cls
    ):
        return _ocr_engine

    logger.info(
        "Initialising PaddleOCR 3.x  lang=%s  textline_orientation=%s  device=%s  box_thresh=%.2f",
        lang,
        use_angle_cls,
        device,
        det_db_thresh,
    )

    from paddleocr import PaddleOCR  # heavy import – keep lazy

    kwargs: dict[str, Any] = dict(
        lang=lang,
        use_textline_orientation=use_angle_cls,
        # 文档预处理子模型默认开启，纯 OCR 场景关闭以降低开销
        use_doc_orientation_classify=False,
        use_doc_unwarping=False,
        device=device,
    )
    try:
        if 0.0 <= float(det_db_thresh) <= 1.0:
            kwargs["text_det_box_thresh"] = float(det_db_thresh)
    except (TypeError, ValueError):
        pass

    # Point to pre-downloaded model directory when provided.
    # 3.x 参数名：text_detection_model_dir / text_recognition_model_dir /
    # textline_orientation_model_dir（旧布局 cls/ 映射到行方向分类模型）。
    if EP_MODEL_DIR and Path(EP_MODEL_DIR).is_dir():
        det_dir = Path(EP_MODEL_DIR) / "det"
        rec_dir = Path(EP_MODEL_DIR) / "rec"
        cls_dir = Path(EP_MODEL_DIR) / "cls"
        if det_dir.is_dir():
            kwargs["text_detection_model_dir"] = str(det_dir)
        if rec_dir.is_dir():
            kwargs["text_recognition_model_dir"] = str(rec_dir)
        if cls_dir.is_dir():
            kwargs["textline_orientation_model_dir"] = str(cls_dir)

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
    params: dict = {}


def _parse_params(raw: Any) -> dict:
    """解析参数，支持 dict 或 JSON 字符串（multipart params 字段为字符串）。"""
    if raw is None:
        return {}
    if isinstance(raw, str):
        try:
            parsed = json.loads(raw)
            return parsed if isinstance(parsed, dict) else {}
        except json.JSONDecodeError:
            return {}
    return raw if isinstance(raw, dict) else {}


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


def _run_ocr(image_path: str, lang: str, use_angle_cls: bool, det_db_thresh: float, params: dict | None = None) -> dict:
    """Run OCR on a single image and return the structured result dict.

    3.x result shape: list of OCRResult (dict-like) with keys
    `rec_texts` / `rec_scores` / `rec_polys` (fallback `dt_polys`).

    直跑/管线文件产物模式（MODULE_SPEC §5）：params 含 `output_format`
    （非 json）与执行器注入的 `output_path` 时，将识别结果写入该文件
    （`output_format="text"` 仅写文本，其余格式写完整 JSON），返回
    output_type="file" + result=路径 —— 使 JSON 型能力在直跑退化 DAG
    （file_input → module → file_output）中也能产出可归集文件产物。
    """
    ocr = _get_ocr(lang=lang, use_angle_cls=use_angle_cls, det_db_thresh=det_db_thresh)
    raw = ocr.predict(image_path)

    lines: list[dict] = []
    full_texts: list[str] = []

    results = raw if isinstance(raw, (list, tuple)) else [raw]
    for res in results:
        if res is None:
            continue
        try:
            texts = res["rec_texts"] or []
            scores = res.get("rec_scores") or []
            polys = res.get("rec_polys")
            if polys is None:
                polys = res.get("dt_polys")
        except (TypeError, KeyError, IndexError) as exc:
            logger.warning("Skipping malformed result: %s", exc)
            continue

        for idx, text in enumerate(texts):
            try:
                confidence = round(float(scores[idx]), 6) if idx < len(scores) else 0.0
                bbox: list = []
                if polys is not None and idx < len(polys):
                    poly = polys[idx]
                    pts = getattr(poly, "tolist", lambda: poly)()
                    bbox = [[float(pt[0]), float(pt[1])] for pt in pts]
                lines.append(
                    {"text": str(text), "confidence": confidence, "bbox": bbox}
                )
                full_texts.append(str(text))
            except (IndexError, TypeError, ValueError) as exc:
                logger.warning("Skipping malformed detection: %s", exc)

    result_payload = {
        "text": "\n".join(full_texts),
        "lines": lines,
        "language": lang,
    }

    # ── 文件产物模式（MODULE_SPEC §5.2：执行器注入 output_path）──
    params = params or {}
    output_format = str(params.get("output_format") or "json").strip().lower()
    output_path = params.get("output_path")
    if output_format != "json" and output_path:
        try:
            p = Path(str(output_path))
            p.parent.mkdir(parents=True, exist_ok=True)
            if output_format == "text":
                p.write_text(result_payload["text"], encoding="utf-8")
            else:
                p.write_text(
                    json.dumps(result_payload, ensure_ascii=False, indent=2),
                    encoding="utf-8",
                )
            logger.info("OCR result written to %s (format=%s)", p, output_format)
            return {
                "status": "completed",
                "output_type": "file",
                "result": str(p),
                "output_path": str(p),
            }
        except Exception as exc:
            logger.warning("Failed to write result file (%s); falling back to JSON", exc)

    return {
        "status": "completed",
        "output_type": "json",
        "result": result_payload,
    }


@app.post("/predict/recognize")
async def predict_recognize(
    request: Request,
    file: Optional[UploadFile] = File(None),
    input_path_form: Optional[str] = Form(None, alias="input_path"),
    params_form: Optional[str] = Form(None, alias="params"),
    lang_form: str = Form("ch", alias="lang"),
    use_angle_cls_form: bool = Form(True, alias="use_angle_cls"),
    det_db_thresh_form: float = Form(0.3, alias="det_db_thresh"),
):
    """Recognise text in an image.

    Accepts a multipart request (file 上传或 input_path 表单字段；参数走
    `params` JSON 字符串 — ep-core executor 契约，兼容旧版平铺表单字段），
    或 JSON body（见 /predict/recognize/json）。
    """
    tmp_path: str | None = None

    # 参数：params JSON 优先，平铺表单字段兼容
    params = _parse_params(params_form)
    lang = str(params.get("lang", lang_form))
    use_angle_cls = bool(params.get("use_angle_cls", use_angle_cls_form))
    try:
        det_db_thresh = float(params.get("det_db_thresh", det_db_thresh_form))
    except (TypeError, ValueError):
        det_db_thresh = det_db_thresh_form

    try:
        # --- resolve image source ------------------------------------------
        content_type = request.headers.get("content-type", "")
        image_path: str | None = None
        if "application/json" in content_type:
            body = await request.json()
            params.update(_parse_params(body.get("params")))
            lang = str(params.get("lang", lang))
            use_angle_cls = bool(params.get("use_angle_cls", use_angle_cls))
            try:
                det_db_thresh = float(params.get("det_db_thresh", det_db_thresh))
            except (TypeError, ValueError):
                pass
            image_path = body.get("input_path")
            if not image_path:
                raise HTTPException(
                    status_code=422,
                    detail="Provide either a multipart 'file' upload or an 'input_path' field.",
                )
        elif file is not None and file.filename:
            suffix = Path(file.filename).suffix or ".png"
            fd, tmp_path = tempfile.mkstemp(suffix=suffix, prefix="ep_ocr_")
            os.close(fd)
            with open(tmp_path, "wb") as f:
                shutil.copyfileobj(file.file, f)
            image_path = tmp_path
        elif input_path_form:
            image_path = input_path_form
        else:
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
        result = _run_ocr(image_path, lang, use_angle_cls, det_db_thresh, params)
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
