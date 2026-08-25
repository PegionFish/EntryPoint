"""
FireRed-OCR adapter for EntryPoint module runtime.

Exposes FireRedTeam/FireRed-OCR (Qwen3-VL-2B fine-tune, Apache-2.0) through a
FastAPI HTTP service, following the official inference path (FireRed-OCR
main.py / GitHub Quick Start):

    Qwen3VLForConditionalGeneration + AutoProcessor,
    chat template(image + prompt) -> greedy generate -> Markdown text.

模型输出为纯 Markdown 文本（无坐标框/置信度），result 仅含 `text` 字段，
不虚构 regions 等上游不存在的结构。

新能力 `recognize_pdf`（MODULE_PARITY_PLAN §4.3 A7）：PDF（或单图）输入，
按 dpi（150-600）逐页渲染图片（pdf2image 优先、pypdfium2 回退，同官方
main.py PDF Tab），复用同一模型与提示词逐页 OCR，结果以 `## Page {n}` +
`---` 合并（源 app.py ocr_pdf_stream 格式）；max_pages 截断；
output_format=md + 执行器注入 output_path 走 MODULE_SPEC §5 文件产物。
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import tempfile
import threading
import time
import traceback
from pathlib import Path
from typing import Any, Optional

import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, Request, UploadFile
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import JSONResponse

# ---------------------------------------------------------------------------
# Environment / configuration
# ---------------------------------------------------------------------------

EP_HOST = os.getenv("EP_HOST", "127.0.0.1")
EP_PORT = int(os.getenv("EP_PORT", "8000"))
EP_BACKEND = os.getenv("EP_BACKEND", "cuda").lower()
EP_DEVICE_INDEX = os.getenv("EP_DEVICE_INDEX", "0")
EP_MODEL_DIR = os.getenv("EP_MODEL_DIR", "")
EP_MODEL_ID = os.getenv("EP_MODEL_ID", "")
EP_MODULE_ID = os.getenv("EP_MODULE_ID", "firered-ocr")

MODULE_VERSION = "0.2.0"
MAX_FILE_SIZE_MB = 50
MAX_PDF_SIZE_MB = 200
MIN_DPI = 150
MAX_DPI = 600
DEFAULT_DPI = 300
MAX_PAGES = 200

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger(EP_MODULE_ID)

# 官方 conv_for_infer.py 的默认提示词（PDF 图片 -> 结构化 Markdown）
DEFAULT_PROMPT = """You are an AI assistant specialized in converting PDF images to Markdown format. Please follow these instructions for the conversion:

            1. Text Processing:
            - Accurately recognize all text content in the PDF image without guessing or inferring.
            - Convert the recognized text into Markdown format.
            - Maintain the original document structure, including headings, paragraphs, lists, etc.

            2. Mathematical Formula Processing:
            - Convert all mathematical formulas to LaTeX format.
            - Enclose inline formulas with,(,). For example: This is an inline formula,( E = mc^2,)
            - Enclose block formulas with,[,]. For example:,[,frac{-b,pm,sqrt{b^2 - 4ac}}{2a},]

            3. Table Processing:
            - Convert tables to HTML format.
            - Wrap the entire table with <table> and </table>.

            4. Figure Handling:
            - Ignore figures content in the PDF image. Do not attempt to describe or convert images.

            5. Output Format:
            - Ensure the output Markdown document has a clear structure with appropriate line breaks between elements.
            - For complex layouts, try to maintain the original document's structure and format as closely as possible.

            Please strictly follow these guidelines to ensure accuracy and consistency in the conversion. Your task is to accurately convert the content of the PDF image into Markdown format without adding any extra explanations or comments."""


class ModelNotReady(RuntimeError):
    pass


def _resolve_device() -> tuple[str, Any]:
    """EP_BACKEND -> torch 设备与精度。cuda 走 BF16，cpu 回退 FP32。"""
    import torch

    if EP_BACKEND == "cpu":
        return "cpu", torch.float32
    # cuda / rocm(HIP 以 cuda 设备暴露) 均落到 CUDA 设备；无 GPU 时报错而非静默回退
    try:
        index = int(EP_DEVICE_INDEX)
    except (TypeError, ValueError):
        index = 0
    return f"cuda:{index}", torch.bfloat16


# ---------------------------------------------------------------------------
# Lazy model (thread-safe, loaded once from EP_MODEL_DIR; never downloads)
# ---------------------------------------------------------------------------

_model_lock = threading.Lock()
_model: Any = None
_processor: Any = None


def _get_model():
    global _model, _processor

    if _model is not None and _processor is not None:
        return _model, _processor

    with _model_lock:
        if _model is not None and _processor is not None:
            return _model, _processor

        if not EP_MODEL_DIR or not Path(EP_MODEL_DIR).is_dir():
            raise ModelNotReady(
                f"MODEL_NOT_LOADED: model directory not found: '{EP_MODEL_DIR}'. "
                "Download the model via the platform model manager "
                "(huggingface:modelscope = FireRedTeam/FireRed-OCR) and restart the module."
            )

        import torch
        from transformers import AutoProcessor, Qwen3VLForConditionalGeneration

        device, dtype = _resolve_device()
        logger.info(
            "Loading FireRed-OCR from %s onto %s (%s)", EP_MODEL_DIR, device, dtype
        )
        start = time.time()

        # transformers >=4.56 用 `dtype`，旧版用 `torch_dtype`（官方 main.py 写法）
        load_kwargs: dict[str, Any] = dict(low_cpu_mem_usage=True)
        try:
            model = Qwen3VLForConditionalGeneration.from_pretrained(
                EP_MODEL_DIR, dtype=dtype, **load_kwargs
            )
        except TypeError:
            model = Qwen3VLForConditionalGeneration.from_pretrained(
                EP_MODEL_DIR, torch_dtype=dtype, **load_kwargs
            )
        model = model.to(device)
        model.eval()

        processor = AutoProcessor.from_pretrained(EP_MODEL_DIR)

        _model = model
        _processor = processor
        logger.info("FireRed-OCR ready in %.1fs", time.time() - start)
        return _model, _processor


_state = "loading"
_state_detail: Optional[str] = None


def _set_state(state: str, detail: Optional[str] = None):
    global _state, _state_detail
    _state = state
    _state_detail = detail


def _prewarm():
    try:
        _get_model()
        _set_state("ready")
    except Exception as exc:
        logger.error("Model prewarm failed:\n%s", traceback.format_exc())
        _set_state("error", str(exc))


# ---------------------------------------------------------------------------
# Inference (official main.py path)
# ---------------------------------------------------------------------------


def _normalize_languages(raw: Any) -> list[str]:
    """languages 参数：JSON 列表或逗号分隔字符串均可。"""
    if raw is None:
        return []
    if isinstance(raw, (list, tuple)):
        items = raw
    else:
        items = str(raw).replace("，", ",").split(",")
    return [s.strip() for s in items if s and s.strip()]


def _run_ocr(image_path: str, languages: list[str], max_new_tokens: int) -> str:
    import torch
    from PIL import Image

    model, processor = _get_model()

    image = Image.open(image_path)
    image.load()
    if image.mode != "RGB":
        image = image.convert("RGB")

    prompt = DEFAULT_PROMPT
    if languages:
        prompt += (
            "\n\nThe text in the image mainly uses the following language(s): "
            + ", ".join(languages)
            + "."
        )

    messages = [
        {
            "role": "user",
            "content": [
                {"type": "image", "image": image},
                {"type": "text", "text": prompt},
            ],
        }
    ]

    inputs = processor.apply_chat_template(
        messages,
        tokenize=True,
        add_generation_prompt=True,
        return_dict=True,
        return_tensors="pt",
    ).to(model.device)

    with torch.inference_mode():
        generated_ids = model.generate(
            **inputs,
            max_new_tokens=max_new_tokens,
            do_sample=False,
        )

    generated_ids_trimmed = [
        out_ids[len(in_ids):]
        for in_ids, out_ids in zip(inputs.input_ids, generated_ids)
    ]
    return processor.batch_decode(
        generated_ids_trimmed,
        skip_special_tokens=True,
        clean_up_tokenization_spaces=False,
    )[0]


# ---------------------------------------------------------------------------
# recognize_pdf（MODULE_PARITY_PLAN §4.3 A7）
# ---------------------------------------------------------------------------


def _clamp_int(raw: Any, lo: int, hi: int, default: int) -> int:
    try:
        val = int(raw)
    except (TypeError, ValueError):
        val = default
    return max(lo, min(hi, val))


def _render_pdf(pdf_path: str, dpi: int, limit: int = 0) -> list[Any]:
    """PDF → PIL.Image 列表（pdf2image 优先，pypdfium2 回退）。

    limit > 0 时仅渲染前 limit 页（max_pages 截断，免渲染超限页）；
    pypdfium2 以 scale=dpi/72（pdfium 默认 72 dpi）对齐 pdf2image 的 dpi 语义。
    """
    images: list[Any] | None = None
    try:
        from pdf2image import convert_from_path
    except ImportError:
        convert_from_path = None
    if convert_from_path is not None:
        try:
            kwargs: dict[str, Any] = dict(dpi=dpi, fmt="png")
            if limit > 0:
                kwargs.update(first_page=1, last_page=limit)
            images = list(convert_from_path(pdf_path, **kwargs))
        except Exception as exc:
            logger.warning("pdf2image 渲染失败（%s），回退 pypdfium2", exc)
            images = None
    if images is not None:
        return images

    try:
        import pypdfium2 as pdfium
    except ImportError as exc:
        raise RuntimeError(
            "PDF→图片依赖缺失：请安装 pdf2image（需系统 Poppler）或 pypdfium2；"
            "当前环境两者均未安装（见 README「越权需求」）"
        ) from exc

    pdf = pdfium.PdfDocument(pdf_path)
    try:
        count = len(pdf)
        if limit > 0:
            count = min(count, limit)
        return [pdf[idx].render(scale=dpi / 72.0).to_pil() for idx in range(count)]
    finally:
        pdf.close()


def _ocr_page_parts(
    images: list[Any], languages: list[str], max_new_tokens: int
) -> list[str]:
    """逐页 OCR → `## Page {n}` 片段（源 app.py ocr_pdf_stream 合并格式）。"""
    parts: list[str] = []
    workdir = tempfile.mkdtemp(prefix="ep_firered_ocr_pdf_")
    try:
        for idx, page_image in enumerate(images, 1):
            if page_image.mode != "RGB":
                page_image = page_image.convert("RGB")
            page_path = os.path.join(workdir, f"page_{idx:04d}.png")
            page_image.save(page_path, format="PNG")
            page_md = _run_ocr(page_path, languages, max_new_tokens)
            parts.append(f"## Page {idx}\n\n{page_md}\n\n---\n")
            logger.info("Page %d/%d OCR done (%d chars)", idx, len(images), len(page_md))
    finally:
        shutil.rmtree(workdir, ignore_errors=True)
    return parts


def _run_ocr_pdf(
    pdf_path: str,
    languages: list[str],
    max_new_tokens: int,
    dpi: int,
    max_pages: int,
) -> tuple[str, int]:
    """PDF 逐页 OCR：渲染 → 逐页复用 _run_ocr → `## Page {n}`+`---` 合并。"""
    images = _render_pdf(pdf_path, dpi, limit=max_pages)
    if not images:
        raise RuntimeError("PDF 渲染为空：未获得任何页面图片")
    parts = _ocr_page_parts(images, languages, max_new_tokens)
    return "\n".join(parts), len(parts)


def _run_ocr_image_page(
    image_path: str, languages: list[str], max_new_tokens: int
) -> tuple[str, int]:
    """recognize_pdf 的图片输入分支：单图按单页合并（§4.3「输入 pdf/image」）。"""
    from PIL import Image

    img = Image.open(image_path)
    img.load()
    if img.mode != "RGB":
        img = img.convert("RGB")
    parts = _ocr_page_parts([img], languages, max_new_tokens)
    return "\n".join(parts), 1


# ---------------------------------------------------------------------------
# FastAPI app
# ---------------------------------------------------------------------------

app = FastAPI(
    title="FireRed-OCR – EntryPoint Module",
    version=MODULE_VERSION,
    description="Qwen3-VL-2B 端到端文档解析 OCR，图片转结构化 Markdown",
)


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


def _validate_image_path(image_path: str) -> None:
    if not Path(image_path).is_file():
        raise HTTPException(
            status_code=404,
            detail={
                "status": "error",
                "error_code": "FILE_NOT_FOUND",
                "message": f"Image not found: {image_path}",
                "detail": None,
            },
        )
    size_mb = Path(image_path).stat().st_size / (1024 * 1024)
    if size_mb > MAX_FILE_SIZE_MB:
        raise HTTPException(
            status_code=413,
            detail={
                "status": "error",
                "error_code": "INVALID_INPUT",
                "message": f"File too large ({size_mb:.1f} MB). Maximum is {MAX_FILE_SIZE_MB} MB.",
                "detail": None,
            },
        )


_IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg", ".bmp", ".webp", ".tif", ".tiff", ".jpe"}


def _validate_pdf_input(input_path: str) -> None:
    """recognize_pdf 输入校验：文件存在 + 大小上限 + PDF/图片后缀。"""
    p = Path(input_path)
    if not p.is_file():
        raise HTTPException(
            status_code=404,
            detail={
                "status": "error",
                "error_code": "FILE_NOT_FOUND",
                "message": f"Input not found: {input_path}",
                "detail": None,
            },
        )
    size_mb = p.stat().st_size / (1024 * 1024)
    if size_mb > MAX_PDF_SIZE_MB:
        raise HTTPException(
            status_code=413,
            detail={
                "status": "error",
                "error_code": "INVALID_INPUT",
                "message": f"File too large ({size_mb:.1f} MB). Maximum is {MAX_PDF_SIZE_MB} MB.",
                "detail": None,
            },
        )
    suffix = p.suffix.lower()
    if suffix != ".pdf" and suffix not in _IMAGE_SUFFIXES:
        raise HTTPException(
            status_code=400,
            detail={
                "status": "error",
                "error_code": "INVALID_INPUT",
                "message": f"recognize_pdf 仅支持 PDF 或图片输入（[{', '.join(sorted(_IMAGE_SUFFIXES))}]），收到后缀: {suffix or '(无)'}",
                "detail": None,
            },
        )


async def _recognize(image_path: str, params: dict) -> dict:
    started = time.time()

    languages = _normalize_languages(params.get("languages"))
    try:
        max_new_tokens = max(64, min(32768, int(params.get("max_new_tokens", 8192))))
    except (TypeError, ValueError):
        max_new_tokens = 8192

    try:
        text = await run_in_threadpool(
            _run_ocr, image_path, languages, max_new_tokens
        )
    except HTTPException:
        raise
    except Exception as exc:
        logger.error("OCR failed: %s\n%s", exc, traceback.format_exc())
        raise HTTPException(
            status_code=500,
            detail={
                "status": "error",
                "error_code": "INFERENCE_ERROR",
                "message": str(exc),
                "detail": None,
            },
        )

    result_payload = {"text": text}

    elapsed = round(time.time() - started, 3)

    # ── 文件产物模式（MODULE_SPEC §5.2：执行器注入 output_path）──
    output_format = str(params.get("output_format") or "json").strip().lower()
    output_path = params.get("output_path")
    if output_format != "json" and output_path:
        try:
            p = Path(str(output_path))
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text, encoding="utf-8")
            logger.info("OCR result written to %s (format=%s)", p, output_format)
            return {
                "status": "completed",
                "output_type": "file",
                "result": str(p),
                "output_path": str(p),
                "elapsed_seconds": elapsed,
            }
        except Exception as exc:
            logger.warning("Failed to write result file (%s); falling back to JSON", exc)

    return {
        "status": "completed",
        "output_type": "json",
        "result": result_payload,
        "output_path": None,
        "elapsed_seconds": elapsed,
    }


async def _recognize_pdf(input_path: str, params: dict) -> dict:
    """recognize_pdf 主流程（冻结契约 A7）：PDF/图片 → 多页合并 Markdown。

    dpi 钳制 150-600（默认 300）；max_pages 钳制 0-200（0=全部）；
    output_format=md + 执行器注入 output_path 走 MODULE_SPEC §5 文件产物。
    """
    started = time.time()

    languages = _normalize_languages(params.get("languages"))
    try:
        max_new_tokens = max(64, min(32768, int(params.get("max_new_tokens", 8192))))
    except (TypeError, ValueError):
        max_new_tokens = 8192

    dpi = _clamp_int(params.get("dpi", DEFAULT_DPI), MIN_DPI, MAX_DPI, DEFAULT_DPI)
    max_pages = _clamp_int(
        params.get("max_pages", 0), 0, MAX_PAGES, 0
    )

    is_pdf = Path(input_path).suffix.lower() == ".pdf"

    try:
        if is_pdf:
            text, pages = await run_in_threadpool(
                _run_ocr_pdf, input_path, languages, max_new_tokens, dpi, max_pages
            )
        else:
            text, pages = await run_in_threadpool(
                _run_ocr_image_page, input_path, languages, max_new_tokens
            )
    except HTTPException:
        raise
    except Exception as exc:
        logger.error("recognize_pdf failed: %s\n%s", exc, traceback.format_exc())
        raise HTTPException(
            status_code=500,
            detail={
                "status": "error",
                "error_code": "INFERENCE_ERROR",
                "message": str(exc),
                "detail": None,
            },
        )

    elapsed = round(time.time() - started, 3)

    # ── 文件产物模式（MODULE_SPEC §5.2：执行器注入 output_path，同 recognize）──
    output_format = str(params.get("output_format") or "json").strip().lower()
    output_path = params.get("output_path")
    if output_format != "json" and output_path:
        try:
            p = Path(str(output_path))
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text, encoding="utf-8")
            logger.info("recognize_pdf result written to %s (format=%s)", p, output_format)
            return {
                "status": "completed",
                "output_type": "file",
                "result": str(p),
                "output_path": str(p),
                "pages_processed": pages,
                "dpi": dpi if is_pdf else None,
                "elapsed_seconds": elapsed,
            }
        except Exception as exc:
            logger.warning("Failed to write result file (%s); falling back to JSON", exc)

    return {
        "status": "completed",
        "output_type": "json",
        "result": {"text": text},
        "pages_processed": pages,
        "dpi": dpi if is_pdf else None,
        "output_path": None,
        "elapsed_seconds": elapsed,
    }


@app.post("/predict/recognize")
async def predict_recognize(
    request: Request,
    file: Optional[UploadFile] = File(None),
    input_path_form: Optional[str] = Form(None, alias="input_path"),
    params_form: Optional[str] = Form(None, alias="params"),
):
    """Recognise a document image.

    接受 multipart（file 上传或 input_path 表单字段；参数走 `params` JSON
    字符串 —— ep-core executor 契约），或 JSON body `{input_path, params}`。
    """
    tmp_path: str | None = None

    try:
        params = _parse_params(params_form)

        content_type = request.headers.get("content-type", "")
        if "application/json" in content_type:
            body = await request.json()
            params.update(_parse_params(body.get("params")))
            image_path = body.get("input_path")
            if not image_path:
                raise HTTPException(
                    status_code=400,
                    detail={
                        "status": "error",
                        "error_code": "INVALID_INPUT",
                        "message": "Provide either a multipart 'file' upload or an 'input_path' field.",
                        "detail": None,
                    },
                )
        elif file is not None and file.filename:
            suffix = Path(file.filename).suffix or ".png"
            fd, tmp_path = tempfile.mkstemp(suffix=suffix, prefix="ep_firered_ocr_")
            os.close(fd)
            with open(tmp_path, "wb") as f:
                shutil.copyfileobj(file.file, f)
            image_path = tmp_path
        elif input_path_form:
            image_path = input_path_form
        else:
            raise HTTPException(
                status_code=400,
                detail={
                    "status": "error",
                    "error_code": "INVALID_INPUT",
                    "message": "Provide either a multipart 'file' upload or an 'input_path' field.",
                    "detail": None,
                },
            )

        _validate_image_path(image_path)

        if _state == "error":
            raise HTTPException(
                status_code=503,
                detail={
                    "status": "error",
                    "error_code": "MODEL_NOT_LOADED",
                    "message": "Model failed to load; see module logs.",
                    "detail": _state_detail,
                },
            )

        return JSONResponse(content=await _recognize(image_path, params))

    except HTTPException:
        raise
    finally:
        if tmp_path and Path(tmp_path).exists():
            try:
                os.unlink(tmp_path)
            except OSError:
                pass


@app.post("/predict/recognize_pdf")
async def predict_recognize_pdf(
    request: Request,
    file: Optional[UploadFile] = File(None),
    input_path_form: Optional[str] = Form(None, alias="input_path"),
    params_form: Optional[str] = Form(None, alias="params"),
):
    """Recognise a PDF（或单图，§4.3「输入 pdf/image」）为多页合并 Markdown。

    与 recognize 同契约双形态（multipart file/input_path + params JSON 字符串，
    或 JSON body {input_path, params}）；PDF 按 dpi 逐页渲染→逐页复用同模型
    同提示词 OCR（`## Page {n}` + `---` 合并，源 app.py 格式），max_pages 截断；
    output_format=md + output_path 走 MODULE_SPEC §5 文件产物。
    """
    tmp_path: str | None = None

    try:
        params = _parse_params(params_form)

        content_type = request.headers.get("content-type", "")
        if "application/json" in content_type:
            body = await request.json()
            params.update(_parse_params(body.get("params")))
            input_path = body.get("input_path")
            if not input_path:
                raise HTTPException(
                    status_code=400,
                    detail={
                        "status": "error",
                        "error_code": "INVALID_INPUT",
                        "message": "Provide either a multipart 'file' upload or an 'input_path' field.",
                        "detail": None,
                    },
                )
        elif file is not None and file.filename:
            suffix = Path(file.filename).suffix or ".pdf"
            fd, tmp_path = tempfile.mkstemp(
                suffix=suffix, prefix="ep_firered_ocr_pdf_"
            )
            os.close(fd)
            with open(tmp_path, "wb") as f:
                shutil.copyfileobj(file.file, f)
            input_path = tmp_path
        elif input_path_form:
            input_path = input_path_form
        else:
            raise HTTPException(
                status_code=400,
                detail={
                    "status": "error",
                    "error_code": "INVALID_INPUT",
                    "message": "Provide either a multipart 'file' upload or an 'input_path' field.",
                    "detail": None,
                },
            )

        _validate_pdf_input(input_path)

        if _state == "error":
            raise HTTPException(
                status_code=503,
                detail={
                    "status": "error",
                    "error_code": "MODEL_NOT_LOADED",
                    "message": "Model failed to load; see module logs.",
                    "detail": _state_detail,
                },
            )

        return JSONResponse(content=await _recognize_pdf(input_path, params))

    except HTTPException:
        raise
    finally:
        if tmp_path and Path(tmp_path).exists():
            try:
                os.unlink(tmp_path)
            except OSError:
                pass


@app.get("/health")
async def health():
    if _state == "ready":
        return {"status": "ok", "module": EP_MODULE_ID, "version": MODULE_VERSION}
    payload = {"status": _state}
    if _state_detail:
        payload["detail"] = _state_detail
    return JSONResponse(status_code=503, content=payload)


@app.get("/info")
async def info():
    return {
        "module_id": EP_MODULE_ID,
        "name": "FireRed-OCR 文档识别",
        "version": MODULE_VERSION,
        "model_id": EP_MODEL_ID,
        "device": _resolve_device()[0],
        "backend": EP_BACKEND,
        "capabilities": [
            {
                "name": "recognize",
                "description": "文档图片转 Markdown（纯文本输出，无坐标框/置信度）",
                "input_type": "image",
                "output_type": "json",
                "params": {
                    "languages": {
                        "type": "string",
                        "default": "",
                        "description": "可选语言提示，逗号分隔（如 zh,en）；留空不注入",
                    },
                    "max_new_tokens": {
                        "type": "integer",
                        "default": 8192,
                        "min": 64,
                        "max": 32768,
                    },
                    "output_format": {
                        "type": "select",
                        "options": ["json", "text", "md"],
                        "default": "json",
                    },
                },
            },
            {
                "name": "recognize_pdf",
                "description": (
                    "PDF/图片 → 多页合并 Markdown（PDF 按 DPI 逐页渲染，复用"
                    "同模型同提示词；`## Page {n}` + `---` 合并）"
                ),
                "input_type": "pdf",
                "output_type": "json",
                "params": {
                    "dpi": {
                        "type": "integer",
                        "default": DEFAULT_DPI,
                        "min": MIN_DPI,
                        "max": MAX_DPI,
                        "description": "PDF→图片分辨率（越高越清晰越慢）",
                    },
                    "max_pages": {
                        "type": "integer",
                        "default": 0,
                        "min": 0,
                        "max": MAX_PAGES,
                        "description": "最大页数（0=全部）",
                    },
                    "output_format": {
                        "type": "select",
                        "options": ["json", "md"],
                        "default": "json",
                        "description": "md 配合执行器注入的 output_path 输出 md 文件产物（MODULE_SPEC §5）",
                    },
                },
            },
        ],
    }


# ---------------------------------------------------------------------------
# Startup: pre-warm the model in background so /health flips to 200 when ready
# ---------------------------------------------------------------------------


@app.on_event("startup")
async def _startup():
    logger.info(
        "Starting %s v%s  backend=%s  device=%s  model_dir=%s",
        EP_MODULE_ID,
        MODULE_VERSION,
        EP_BACKEND,
        _resolve_device()[0],
        EP_MODEL_DIR or "(missing)",
    )
    threading.Thread(target=_prewarm, daemon=True, name="firered-ocr-prewarm").start()


if __name__ == "__main__":
    uvicorn.run(
        app,
        host=EP_HOST,
        port=EP_PORT,
        log_level="info",
    )
