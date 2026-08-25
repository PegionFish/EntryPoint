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
import threading
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
EP_MODELS_ROOT = os.getenv("EP_MODELS_ROOT", "")
EP_DEVICE_INDEX = os.getenv("EP_DEVICE_INDEX", "0")

MODULE_ID = "paddleocr"
MODULE_VERSION = "2.9.0"

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
# PP-StructureV3 document understanding (doc_understand)
# ---------------------------------------------------------------------------
# 与源应用 app.py（PaddleOcrPPStructureV3 PP-StructureV3-gpu-offline）对齐：
#   pipeline = PPStructureV3(**PIPELINE_KWARGS, device=device)
#   output = pipeline.predict(input=path, use_doc_orientation_classify=...,
#                             use_doc_unwarping=..., use_table_orientation_classify=False)
# 模型按 <models_cache>/paddleocr-pp-structure-v3/（module.toml 变体
# pp-structure-v3 的 target_dir）下的 14 个子模型目录装载；chart 子目录在源
# zip 中是 HF 风格（models/chart/PP-Chart2Table/），故一律 os.listdir 探测，
# 禁止硬编码内部目录名。

PPSTRUCTURE_TARGET_DIR = "paddleocr-pp-structure-v3"
PPSTRUCTURE_MAX_FILE_SIZE_MB = 100
PPSTRUCTURE_MAX_PDF_PAGES = 100
PPSTRUCTURE_MAX_STRUCT_BYTES = 512 * 1024

# PP-StructureV3 构造参数 → models/<target_dir>/子目录名（与源 app.py PIPELINE_KWARGS 同口径）
_PPS3_SUBMODELS: dict[str, str] = {
    "layout_detection_model_dir": "layout",
    "region_detection_model_dir": "region",
    "doc_orientation_classify_model_dir": "doc_ori",
    "doc_unwarping_model_dir": "doc_unwarping",
    "text_detection_model_dir": "ocr_det",
    "text_recognition_model_dir": "ocr_rec",
    "formula_recognition_model_dir": "formula",
    "chart_recognition_model_dir": "chart",
    "textline_orientation_model_dir": "textline_orientation",
    "table_classification_model_dir": "table_cls",
    "wired_table_structure_recognition_model_dir": "table_wired_structure",
    "wireless_table_structure_recognition_model_dir": "table_wireless_structure",
    "wired_table_cells_detection_model_dir": "table_wired_cells",
    "wireless_table_cells_detection_model_dir": "table_wireless_cells",
}

# 每个显式开关需要的子模型构造参数（基础链路 5 个恒需：layout/region/
# ocr_det/ocr_rec/textline_orientation）
_PPS3_FLAG_REQUIRED: dict[str, tuple[str, ...]] = {
    "doc_orientation": ("doc_orientation_classify_model_dir",),
    "doc_unwarping": ("doc_unwarping_model_dir",),
    "table": (
        "table_classification_model_dir",
        "wired_table_structure_recognition_model_dir",
        "wireless_table_structure_recognition_model_dir",
        "wired_table_cells_detection_model_dir",
        "wireless_table_cells_detection_model_dir",
    ),
    "formula": ("formula_recognition_model_dir",),
    "chart": ("chart_recognition_model_dir",),
}
_PPS3_BASELINE_REQUIRED = (
    "layout_detection_model_dir",
    "region_detection_model_dir",
    "text_detection_model_dir",
    "text_recognition_model_dir",
    "textline_orientation_model_dir",
)


class Pps3NotReadyError(RuntimeError):
    """PP-StructureV3 模型目录/子模型未就绪：返回 503 而非静默下载。"""


_MODEL_ARTIFACTS = (
    "config.json",
    "config.yml",
    "inference.json",
    "inference.yml",
    "inference.pdiparams",
    "inference.pdparams",
    "model.pdiparams",
    "model_state.pdparams",
)


def _looks_like_model_dir(path: Path) -> bool:
    """探测目录是否直接含 paddlex/HF 模型仓产物（不硬编码子目录名）。"""
    try:
        with os.scandir(path) as it:
            for entry in it:
                if entry.is_file() and entry.name in _MODEL_ARTIFACTS:
                    return entry.stat().st_size > 0
    except OSError:
        return False
    return False


def _probe_submodel_dir(root: Path, subdir: str) -> Path | None:
    """<root>/<subdir> 探测：顶层即模型产物则用之；否则取第一个（非隐藏、
    非 .cache 的）内部模型目录 —— 覆盖 chart/PP-Chart2Table 这类嵌套结构。"""
    top = root / subdir
    if not top.is_dir():
        return None
    if _looks_like_model_dir(top):
        return top
    try:
        entries = sorted(
            e for e in os.listdir(top)
            if not e.startswith(".") and e != ".cache"
        )
    except OSError:
        return None
    for name in entries:
        child = top / name
        if child.is_dir() and _looks_like_model_dir(child):
            return child
    return None


def _resolve_pps3_model_root() -> Path | None:
    """定位 PP-StructureV3 模型根目录。

    优先级：<EP_MODELS_ROOT>/<target_dir>（契约位）→ 激活变体目录
    EP_MODEL_DIR（当激活的恰是 pp-structure-v3 变体）。判定标记：目录
    内含 layout/ 子目录（14 子模型根结构）。
    """
    if EP_MODELS_ROOT:
        cand = Path(EP_MODELS_ROOT) / PPSTRUCTURE_TARGET_DIR
        if (cand / "layout").is_dir():
            return cand
    if EP_MODEL_DIR:
        cand = Path(EP_MODEL_DIR)
        if (cand / "layout").is_dir():
            return cand
    return None


_pps3_engine: Any = None
_pps3_model_root: str | None = None
_pps3_wired: dict[str, str] = {}
_pps3_lock = threading.Lock()


def _get_pps3() -> Any:
    """Return a cached PPStructureV3 pipeline built from the local submodels."""
    global _pps3_engine, _pps3_model_root, _pps3_wired

    if _pps3_engine is not None:
        return _pps3_engine

    with _pps3_lock:
        if _pps3_engine is not None:
            return _pps3_engine

        root = _resolve_pps3_model_root()
        if root is None:
            raise Pps3NotReadyError(
                f"PP-StructureV3 模型目录未就绪: 预期 <models_cache>/{PPSTRUCTURE_TARGET_DIR}/"
                "(14 子模型: layout/region/doc_ori/doc_unwarping/ocr_det/ocr_rec/formula/"
                "chart/table_cls/table_wired_*/table_wireless_*/textline_orientation)。"
                "请经平台模型管理「本地导入」或切换激活变体为 pp-structure-v3 后重启模块。"
            )

        device = "cpu"
        if EP_BACKEND == "cuda":
            try:
                device = f"gpu:{int(EP_DEVICE_INDEX)}"
            except (TypeError, ValueError):
                device = "gpu"

        try:
            from paddleocr import PPStructureV3  # noqa: F401
        except Exception as exc:
            raise RuntimeError(
                "paddleocr 3.x（PPStructureV3/paddlex）依赖未就绪，见 README「越权需求」: "
                f"{exc}"
            ) from exc

        wired: dict[str, str] = {}
        selected_map = dict(_PPS3_SUBMODELS)
        for sub in sorted(set(selected_map.values())):
            found = _probe_submodel_dir(root, sub)
            if found is not None:
                for kw, name in selected_map.items():
                    if name == sub:
                        wired[kw] = str(found)
        logger.info(
            "PP-StructureV3 子模型命中 %d 个子目录: %s",
            len(wired),
            ", ".join(sorted(wired.values())),
        )

        kwargs: dict[str, Any] = dict(**wired)
        # 与源 app.py PIPELINE_KWARGS 同口径：构造时默认关闭文档预处理，
        # 每次 predict 再按参数开关覆盖。
        kwargs["use_doc_orientation_classify"] = False
        kwargs["use_doc_unwarping"] = False

        logger.info(
            "Initialising PP-StructureV3 pipeline  device=%s  base_models=%d",
            device,
            sum(1 for k in _PPS3_BASELINE_REQUIRED if k in wired),
        )
        _pps3_engine = PPStructureV3(**kwargs, device=device)
        _pps3_model_root = str(root)
        _pps3_wired = wired
        logger.info("PP-StructureV3 pipeline ready (%s)", root)
        return _pps3_engine


def _check_pps3_switches(engine: Any, switches: dict[str, bool]) -> None:
    """按开关校验所需子模型已接线；缺失即 503（不静默联网下载）。"""
    required = list(_PPS3_BASELINE_REQUIRED)
    for flag in ("doc_orientation", "doc_unwarping", "table", "formula", "chart"):
        if switches.get(flag):
            required.extend(_PPS3_FLAG_REQUIRED[flag])
    wired = getattr(engine, "_pps3_wired", None) or _pps3_wired
    missing = [kw for kw in required if kw not in wired]
    if missing:
        subdirs = ", ".join(_PPS3_SUBMODELS[k] for k in missing)
        raise Pps3NotReadyError(
            f"以下子模型未就绪（开关已开启但本地模型缺失）: {subdirs}。"
            f"预期位于 {PPSTRUCTURE_TARGET_DIR}/ 下，请由平台模型管理导入后重试。"
        )


def _pdf_to_pngs(pdf_path: str, dpi: int, out_dir: Path) -> list[Path]:
    """PDF 逐页渲染 PNG（pdf2image 优先，pypdfium2 回退——后者由 paddlex
    自带，源 zip 环境即为此状况）。"""
    images = None
    try:
        from pdf2image import convert_from_path
    except ImportError:
        convert_from_path = None
    if convert_from_path is not None:
        try:
            images = convert_from_path(pdf_path, dpi=dpi, fmt="png")
        except Exception as exc:
            logger.warning("pdf2image 渲染失败（%s），回退 pypdfium2", exc)
            images = None
    if images is not None:
        pages: list[Path] = []
        for idx, img in enumerate(images):
            page = out_dir / f"page_{idx + 1:04d}.png"
            try:
                img.save(str(page), format="PNG")
            except AttributeError:
                from PIL import Image

                Image.fromarray(img).save(str(page), format="PNG")
            pages.append(page)
        return pages

    try:
        import pypdfium2 as pdfium
    except ImportError as exc:
        raise RuntimeError(
            "当前环境缺少 PDF→图片依赖（pdf2image>=1.17 或 pypdfium2），"
            "见 README「越权需求」"
        ) from exc
    try:
        pdf = pdfium.PdfDocument(pdf_path)
    except Exception as exc:
        raise RuntimeError(f"无法打开 PDF: {pdf_path} ({exc})") from exc
    try:
        count = len(pdf)
        pages = []
        for idx in range(count):
            bitmap = pdf[idx].render(scale=dpi / 72.0)
            page = out_dir / f"page_{idx + 1:04d}.png"
            bitmap.to_pil().save(str(page), format="PNG")
            pages.append(page)
        return pages
    finally:
        pdf.close()


def _extract_markdown(res: Any) -> str:
    """按源 app.py 的官方提取方式取 markdown（res.markdown 为 dict）。"""
    md: Any = None
    try:
        if isinstance(res, dict):
            md = res.get("markdown")
    except Exception:
        md = None
    if md is None:
        try:
            md = getattr(res, "markdown", None)
        except Exception:
            md = None
    if isinstance(md, dict):
        text = md.get("text")
        if not text:
            text = md.get("markdown_texts", "")
        if isinstance(text, list):
            return "\n\n".join(str(t) for t in text if str(t).strip())
        return str(text or "")
    if md is None:
        return ""
    return str(md)


def _extract_result_json(res: Any, max_bytes: int = PPSTRUCTURE_MAX_STRUCT_BYTES) -> Any:
    """单页原始结构转 JSON（to_json 优先，退化到 dict 序列化；超大字段截断）。"""
    obj: Any = None
    try:
        if hasattr(res, "to_json"):
            txt = res.to_json()
            obj = json.loads(txt) if isinstance(txt, str) else txt
    except Exception:
        obj = None
    if obj is None:
        try:
            if isinstance(res, dict):
                obj = {k: v for k, v in res.items()}
        except Exception:
            obj = None
    if obj is None:
        return None
    try:
        if isinstance(obj, dict):
            out = {}
            for key, val in obj.items():
                try:
                    raw = json.dumps(val, ensure_ascii=False, default=str)
                except Exception:
                    raw = str(val)
                if len(raw) > max_bytes:
                    out[key] = f"[truncated {len(raw)} bytes]"
                else:
                    out[key] = json.loads(raw)
            return out
    except Exception:
        pass
    return obj


def _clamp_dpi(dpi: Any) -> int:
    try:
        val = int(dpi)
    except (TypeError, ValueError):
        val = 300
    return max(150, min(600, val))


def _run_doc_understand(input_path: str, params: dict | None) -> dict:
    """文档理解主分派：按扩展名分派（pdf→逐页渲染，image→单页）。

    params 开关（冻结契约）：doc_orientation/doc_unwarping/table/formula/
    chart/dpi/output_format。产物（output_format=md 且执行器注入
    output_path）同样支持 MODULE_SPEC §5 文件产出模式。
    """
    src = Path(input_path)
    if not src.is_file():
        raise FileNotFoundError(f"Input file not found: {input_path}")

    params = params or {}
    suffix = src.suffix.lower()
    is_pdf = suffix == ".pdf"
    if not is_pdf and suffix not in (
        ".png", ".jpg", ".jpeg", ".bmp", ".webp", ".tif", ".tiff",
    ):
        raise ValueError(
            f"doc_understand 仅支持图片（PNG/JPG/BMP/WEBP/TIF）或 PDF，收到: {suffix}"
        )

    dpi = _clamp_dpi(params.get("dpi", 300))
    switches = {
        "doc_orientation": bool(params.get("doc_orientation", False)),
        "doc_unwarping": bool(params.get("doc_unwarping", False)),
        "table": bool(params.get("table", True)),
        "formula": bool(params.get("formula", True)),
        # chart 默认 false：paddlex 3.4.3 的 ChartRecognition 子模型构建
        # 存在官方 bug（create_pipeline config 与 ink kwargs 均无法初始化
        # chart_recognition_model）；开启会抛 AttributeError，仅在上游修复
        # 后恢复默认 true（MODULE_PARITY_PLAN A2 已记录）。
        "chart": bool(params.get("chart", False)),
    }

    engine = _get_pps3()
    _check_pps3_switches(engine, switches)

    workdir = Path(tempfile.mkdtemp(prefix="ep_docu_"))
    try:
        if is_pdf:
            page_images = _pdf_to_pngs(str(src), dpi, workdir)
            if not page_images:
                raise ValueError("PDF 未渲染出任何页面（文件是否损坏/为空？）")
            if len(page_images) > PPSTRUCTURE_MAX_PDF_PAGES:
                raise ValueError(
                    f"PDF 页数超过上限 {PPSTRUCTURE_MAX_PDF_PAGES}（实际 "
                    f"{len(page_images)}），请降低页数后重试。"
                )
        else:
            page_images = [src]

        md_parts: list[str] = []
        pages: list[dict] = []
        for idx, img in enumerate(page_images, start=1):
            output = engine.predict(
                input=str(img),
                use_doc_orientation_classify=switches["doc_orientation"],
                use_doc_unwarping=switches["doc_unwarping"],
                use_table_recognition=switches["table"],
                use_formula_recognition=switches["formula"],
                use_chart_recognition=switches["chart"],
                use_table_orientation_classify=False,  # 源 app.py 固定关闭（本地无该子模型）
            )
            page_md: list[str] = []
            structures: list[Any] = []
            for res in _as_list(output):
                md = _extract_markdown(res)
                if md:
                    page_md.append(md)
                structure = _extract_result_json(res)
                if structure is not None:
                    structures.append(structure)
            page_text = "\n\n".join(page_md)
            md_parts.append(page_text)
            pages.append(
                {
                    "index": idx,
                    "requested": str(img) if str(img) != str(src) else "input",
                    "markdown": page_text,
                    "structure": structures if structures else None,
                }
            )
            logger.info(
                "doc_understand page %d/%d done (md %d chars)",
                idx, len(page_images), len(page_text),
            )

        md_full = "\n\n".join(part for part in md_parts if part).rstrip()
        payload = {
            "markdown": md_full,
            "pages": pages,
            "input": {
                "path": str(src),
                "kind": "pdf" if is_pdf else "image",
                "pages": len(page_images),
                "dpi": dpi if is_pdf else None,
            },
            "options": dict(switches),
        }
        # ── 文件产物模式（MODULE_SPEC §5：执行器注入 output_path）──
        output_format = str(params.get("output_format") or "json").strip().lower()
        output_path = params.get("output_path")
        if output_format == "md" and output_path:
            p = Path(str(output_path))
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(md_full, encoding="utf-8")
            logger.info("doc_understand markdown written to %s", p)
            return {
                "status": "completed",
                "output_type": "file",
                "result": str(p),
                "output_path": str(p),
            }
        if output_format == "md":
            return {
                "status": "completed",
                "output_type": "text",
                "result": md_full,
            }
        return {
            "status": "completed",
            "output_type": "json",
            "result": payload,
        }
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


def _as_list(value: Any) -> list:
    if value is None:
        return []
    if isinstance(value, (list, tuple)):
        return list(value)
    return [value]


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
            },
            {
                "name": "doc_understand",
                "description": "文档理解：PP-StructureV3 版面/表格/公式/图表解析 → Markdown（图片或 PDF）",
                "input_type": "image",
                "output_type": "json",
                "params": {
                    "doc_orientation": {"type": "boolean", "default": False},
                    "doc_unwarping": {"type": "boolean", "default": False},
                    "table": {"type": "boolean", "default": True},
                    "formula": {"type": "boolean", "default": True},
                    "chart": {"type": "boolean", "default": True},
                    "dpi": {"type": "integer", "default": 300, "min": 150, "max": 600},
                    "output_format": {
                        "type": "select",
                        "options": ["json", "md"],
                        "default": "json",
                    },
                },
            },
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
# doc_understand endpoint
# ---------------------------------------------------------------------------


@app.post("/predict/doc_understand")
async def predict_doc_understand(
    request: Request,
    file: Optional[UploadFile] = File(None),
    input_path_form: Optional[str] = Form(None, alias="input_path"),
    params_form: Optional[str] = Form(None, alias="params"),
):
    """PP-StructureV3 文档理解：图片/PDF → Markdown（含结构化 JSON）。

    与 recognize 同契约：multipart（file/input_path + params JSON 字符串）
    或 JSON body（{"input_path": ..., "params": {...}}）。
    """
    tmp_path: str | None = None
    params = _parse_params(params_form)

    try:
        content_type = request.headers.get("content-type", "")
        input_path: str | None = None
        if "application/json" in content_type:
            body = await request.json()
            params.update(_parse_params(body.get("params")))
            input_path = body.get("input_path")
            if not input_path:
                raise HTTPException(
                    status_code=422,
                    detail="Provide either a multipart 'file' upload or an 'input_path' field.",
                )
        elif file is not None and file.filename:
            suffix = Path(file.filename).suffix or ".png"
            fd, tmp_path = tempfile.mkstemp(suffix=suffix, prefix="ep_docu_")
            os.close(fd)
            with open(tmp_path, "wb") as f:
                shutil.copyfileobj(file.file, f)
            input_path = tmp_path
        elif input_path_form:
            input_path = input_path_form
        else:
            raise HTTPException(
                status_code=422,
                detail="Provide either a multipart 'file' upload or an 'input_path' field.",
            )

        if not Path(input_path).is_file():
            raise HTTPException(
                status_code=404,
                detail=f"Input file not found: {input_path}",
            )

        file_size_mb = Path(input_path).stat().st_size / (1024 * 1024)
        if file_size_mb > PPSTRUCTURE_MAX_FILE_SIZE_MB:
            raise HTTPException(
                status_code=413,
                detail=(
                    f"File too large ({file_size_mb:.1f} MB). "
                    f"Maximum is {PPSTRUCTURE_MAX_FILE_SIZE_MB} MB."
                ),
            )

        result = _run_doc_understand(input_path, params)
        return JSONResponse(content=result)

    except Pps3NotReadyError as exc:
        return JSONResponse(
            status_code=503,
            content={
                "status": "error",
                "error_code": "MODEL_NOT_LOADED",
                "message": "PP-StructureV3 model not ready",
                "detail": str(exc),
            },
        )
    except HTTPException:
        raise
    except Exception as exc:
        logger.error("doc_understand failed: %s\n%s", exc, traceback.format_exc())
        return JSONResponse(
            status_code=500,
            content={
                "status": "error",
                "error_code": "INFERENCE_ERROR",
                "message": "doc_understand inference failed",
                "detail": str(exc),
            },
        )
    finally:
        if tmp_path and Path(tmp_path).exists():
            try:
                os.unlink(tmp_path)
            except OSError:
                pass


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
    # doc_understand 引擎较重，不在启动期加载；仅探测模型目录就绪状态
    try:
        pps3_root = _resolve_pps3_model_root()
        if pps3_root is not None:
            logger.info(
                "PP-StructureV3 model root ready: %s (loads on first doc_understand call)",
                pps3_root,
            )
    except OSError:
        pass


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
