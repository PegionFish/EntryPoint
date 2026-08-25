"""adapter.py — EntryPoint qwen3-asr 模块适配器（Qwen3-ASR 转写 + ForcedAligner 词级对齐）

能力：
  POST /predict/transcribe : audio -> {text, segments:[{start,end,text}], words:[...]}
  POST /predict/align      : audio + 参考文本 -> 词级 [{word,start,end}]

底层为官方 qwen_asr 包（PyPI，Apache-2.0）：
  - Qwen3ASRModel.from_pretrained(<asr_dir>, forced_aligner=<aligner_dir>, ...)
  - asr.transcribe(audio=<path>, context=..., language=..., return_time_stamps=True)
  - asr.forced_aligner.align(audio=<path>, text=<参考文本>, language=<语言名>)

加载策略：懒加载（首次请求才载模型）；Aligner 权重存在时随 ASR 一并挂载
（与官方 gradio demo 相同形态），align 能力复用同一实例，不重复占显存。
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Optional

# Windows: 将 venv Scripts 目录加入 DLL 搜索路径（CUDA 库等）
if sys.platform == "win32":
    _scripts = Path(sys.executable).parent
    if hasattr(os, "add_dll_directory"):
        os.add_dll_directory(str(_scripts))
    os.environ["PATH"] = str(_scripts) + os.pathsep + os.environ.get("PATH", "")

import uvicorn
from fastapi import FastAPI, File, Form, Request, UploadFile
from fastapi.responses import JSONResponse

# ── 环境变量 ──────────────────────────────────────────────
MODULE_DIR = Path(os.environ.get("EP_MODULE_DIR", Path(__file__).resolve().parent))
MODEL_DIR = Path(os.environ.get("EP_MODEL_DIR", ""))
MODELS_ROOT = Path(os.environ["EP_MODELS_ROOT"]) if os.environ.get("EP_MODELS_ROOT") else None
MODEL_ID = os.environ.get("EP_MODEL_ID", "qwen3-asr-0.6b")
WORKSPACE = Path(os.environ.get("EP_WORKSPACE") or tempfile.gettempdir()) / "qwen3-asr"
PORT = int(os.environ.get("EP_PORT", "18002"))
DEVICE = os.environ.get("EP_DEVICE", "cuda").strip().lower()
# 设备判定以 EP_BACKEND（裸后端名）为准——daemon 注入的 EP_DEVICE 形如
# "cuda:0"，直接 == "cuda" 比较恒为 False；EP_BACKEND 缺省时回退取冒号前缀。
BACKEND = os.environ.get("EP_BACKEND", "").strip().lower() or (
    DEVICE.split(":")[0] if DEVICE else "cuda"
)
# Aligner 目录可选覆盖；缺省按 EP_MODELS_ROOT/<target_dir> 与激活模型兄弟目录探测
ALIGNER_DIR_ENV = os.environ.get("EP_ALIGNER_DIR", "")

MODULE_NAME = "Qwen3-ASR 语音识别"
MODULE_VERSION = "1.0.0"

logger = logging.getLogger("qwen3-asr")

# ── 变体与语言表 ──────────────────────────────────────────
# params.model 变体覆盖（ADAPTER_API.md §1.3）：id 简写/全名 → 模型缓存根下 target_dir
VARIANT_DIR_MAP: dict[str, str] = {
    "0.6b": "qwen3-asr-0.6b",
    "qwen3-asr-0.6b": "qwen3-asr-0.6b",
    "1.7b": "qwen3-asr-1.7b",
    "qwen3-asr-1.7b": "qwen3-asr-1.7b",
}
ALIGNER_TARGET_DIR = "qwen3-forced-aligner-0.6b"
ALIGNER_REPO_HINT = (
    "Qwen/Qwen3-ForcedAligner-0.6B（HF 主源，ModelScope 同名镜像）；"
    f"经平台模型管理器下载到 <models_root>/{ALIGNER_TARGET_DIR} 后重启模块"
)

# 30 语言枚举（来源 qwen_asr.SUPPORTED_LANGUAGES，锁定顺序与官方一致）；
# 供 language 校验、错误提示与 /info 说明使用，不改推理行为
SUPPORTED_LANGUAGES: tuple[str, ...] = (
    "Chinese", "English", "Cantonese", "Arabic", "German", "French", "Spanish",
    "Portuguese", "Indonesian", "Italian", "Korean", "Russian", "Thai",
    "Vietnamese", "Japanese", "Turkish", "Hindi", "Malay", "Dutch", "Swedish",
    "Danish", "Finnish", "Polish", "Czech", "Filipino", "Persian", "Greek",
    "Romanian", "Hungarian", "Macedonian",
)
LANGS_HINT = "、".join(SUPPORTED_LANGUAGES)

# 常用 ISO 639-1/639-3 代码 → qwen_asr 规范语言名（SUPPORTED_LANGUAGES 共 30 种，
# 此处收录有通行代码者；规范名可直接传入，其余交由底层校验报错）
ISO_LANG_MAP: dict[str, str] = {
    "zh": "Chinese", "cmn": "Chinese", "en": "English", "yue": "Cantonese",
    "ar": "Arabic", "de": "German", "fr": "French", "es": "Spanish",
    "pt": "Portuguese", "id": "Indonesian", "it": "Italian", "ko": "Korean",
    "ru": "Russian", "th": "Thai", "vi": "Vietnamese", "ja": "Japanese",
    "tr": "Turkish", "hi": "Hindi", "ms": "Malay", "nl": "Dutch",
    "sv": "Swedish", "da": "Danish", "fi": "Finnish", "pl": "Polish",
    "cs": "Czech", "fil": "Filipino", "tl": "Filipino", "fa": "Persian",
    "el": "Greek", "ro": "Romanian", "hu": "Hungarian", "mk": "Macedonian",
}

# ── 全局状态（懒加载） ────────────────────────────────────
_asr: Any = None                 # Qwen3ASRModel 实例（可能挂载 forced_aligner）
_asr_model_dir: Optional[str] = None
_asr_load_error: Optional[str] = None
_aligner_attached = False


class ModelSelectionError(Exception):
    """params.model 不是已声明的变体。"""


class WeightsMissingError(Exception):
    """本地权重缺失（ADAPTER_API.md §1.3：不得静默联网下载）。"""


class AlignerNotLoadedError(Exception):
    """Aligner 权重缺失或未挂载。"""


class _AsyncLoadLock:
    """跨异步请求的加载互斥（线程锁非阻塞抢占 + 事件循环休眠等待）。"""

    def __init__(self) -> None:
        self._lock = threading.Lock()

    async def __aenter__(self) -> "_AsyncLoadLock":
        while not self._lock.acquire(blocking=False):
            await asyncio.sleep(0.05)
        return self

    async def __aexit__(self, *exc) -> None:
        self._lock.release()


_load_lock = _AsyncLoadLock()


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


def _as_bool(v: Any, default: bool = False) -> bool:
    """健壮布尔解析（防字符串 "false" 被 bool() 误判为 True）。"""
    if isinstance(v, bool):
        return v
    if v is None:
        return default
    if isinstance(v, str):
        return v.strip().lower() in ("1", "true", "yes", "on")
    return bool(v)


def _normalize_language(raw: Any) -> Optional[str]:
    """"auto"/空 → None（自动检测）；ISO 代码 → 规范语言名；其余透传由底层校验。"""
    s = str(raw or "").strip()
    if not s or s.lower() in ("auto", "und"):
        return None
    return ISO_LANG_MAP.get(s.lower(), s)


def _check_language(language: Optional[str]) -> Optional[str]:
    """30 语言校验：None（auto）放行；不在 SUPPORTED_LANGUAGES 的返回错误文案。"""
    if language is None:
        return None
    if language in SUPPORTED_LANGUAGES:
        return None
    return (
        f"不支持的语言: {language!r}；支持 auto/规范语言名之一（{LANGS_HINT}）"
        "或常用 ISO 代码（zh/en/yue/ja/...）"
    )


def _has_config(path: Path) -> bool:
    return path.is_dir() and (path / "config.json").is_file()


def _resolve_variant_dir(model_param: str) -> Optional[Path]:
    """params.model 变体覆盖解析：在 EP_MODELS_ROOT 下找 target_dir 子目录。"""
    target = VARIANT_DIR_MAP.get(str(model_param).strip())
    if not target or MODELS_ROOT is None:
        return None
    candidate = MODELS_ROOT / target
    return candidate if _has_config(candidate) else None


def _resolve_aligner_dir() -> Optional[Path]:
    """定位 ForcedAligner 权重目录（不联网下载，缺失返回 None）。

    查找顺序：EP_ALIGNER_DIR > <EP_MODELS_ROOT>/qwen3-forced-aligner-0.6b
    > 激活模型目录自身（若其父目录即 aligner target_dir）/ 同级兄弟目录。
    """
    candidates: list[Path] = []
    if ALIGNER_DIR_ENV:
        candidates.append(Path(ALIGNER_DIR_ENV))
    if MODELS_ROOT is not None:
        candidates.append(MODELS_ROOT / ALIGNER_TARGET_DIR)
    if MODEL_DIR.is_dir():
        if MODEL_DIR.name == ALIGNER_TARGET_DIR:
            candidates.append(MODEL_DIR)
        candidates.append(MODEL_DIR.parent / ALIGNER_TARGET_DIR)
    for c in candidates:
        if _has_config(c):
            return c
    return None


def _device_and_dtype():
    import torch

    device_index = os.environ.get("EP_DEVICE_INDEX", "")
    if BACKEND == "cuda" and torch.cuda.is_available():
        return f"cuda:{int(device_index or 0)}", torch.bfloat16
    if BACKEND == "cuda":
        logger.warning("CUDA 不可用，回退到 CPU（fp32）")
    return "cpu", torch.float32


def _load_asr_sync(model_path: Path):
    """构造 Qwen3ASRModel；Aligner 权重可用时一并挂载（官方 demo 形态）。

    返回 (model, aligner_dir_or_None)。异常向上抛出。
    """
    import torch  # noqa: F401  （确保 CUDA 上下文随 qwen_asr 导入就绪）
    from qwen_asr import Qwen3ASRModel

    device_map, dtype = _device_and_dtype()
    aligner_dir = _resolve_aligner_dir()

    kwargs: dict[str, Any] = dict(
        dtype=dtype,
        device_map=device_map,
        max_inference_batch_size=4,
        max_new_tokens=512,
    )
    if aligner_dir is not None:
        kwargs["forced_aligner"] = str(aligner_dir)
        kwargs["forced_aligner_kwargs"] = dict(dtype=dtype, device_map=device_map)

    logger.info(
        "正在加载 Qwen3-ASR: %s (backend=%s, device=%s, dtype=%s, aligner=%s)",
        model_path, BACKEND, device_map, dtype, aligner_dir or "缺失（词级时间戳将降级）",
    )
    model = Qwen3ASRModel.from_pretrained(str(model_path), **kwargs)
    logger.info("Qwen3-ASR 加载完成 (device=%s)", device_map)
    return model, aligner_dir


async def _get_asr(model_param: Any = None):
    """懒加载 ASR（并发防护 + params.model 变体覆盖切换）。

    返回 (model, aligner_attached)；权重目录变化时释放旧实例并重载。
    """
    global _asr, _asr_model_dir, _asr_load_error, _aligner_attached

    override = str(model_param).strip() if model_param else ""
    if override and override not in VARIANT_DIR_MAP:
        raise ModelSelectionError(
            f"未知模型变体: {override}",
            f"可用变体: {sorted(set(VARIANT_DIR_MAP.values()))}",
        )

    target_dir_name = VARIANT_DIR_MAP.get(override, "") if override else ""
    loop = asyncio.get_running_loop()

    async with _load_lock:
        already_loaded = (
            _asr is not None
            and _asr_model_dir is not None
            and (
                not target_dir_name
                or Path(_asr_model_dir).name == target_dir_name
            )
        )
        if already_loaded:
            return _asr, _aligner_attached

        # 解析目标权重目录：显式变体覆盖优先（缺失即报错，不静默换变体），
        # 其次激活模型目录；仅在无覆盖时才按声明顺序在 EP_MODELS_ROOT 兜底探测
        model_path: Optional[Path] = None
        expected_hint = ""
        if override:
            model_path = _resolve_variant_dir(override)
            if model_path is None:
                expected_hint = str((MODELS_ROOT or Path("<models_root>")) / target_dir_name)
                raise WeightsMissingError(
                    f"请求的变体 {override} 本地权重缺失",
                    f"预期路径: {expected_hint}。请经平台模型管理器下载后重试；"
                    "adapter 不做静默联网下载，也不会替换为其它已就绪变体。",
                )
        if model_path is None and _has_config(MODEL_DIR):
            model_path = MODEL_DIR
        if model_path is None and MODEL_DIR.is_dir():
            for child in sorted(MODEL_DIR.iterdir()):
                if child.is_dir() and (child / "config.json").exists():
                    model_path = child
                    break
        if model_path is None and not override and MODELS_ROOT is not None:
            for target in ("qwen3-asr-0.6b", "qwen3-asr-1.7b"):
                cand = MODELS_ROOT / target
                if _has_config(cand):
                    model_path = cand
                    break

        if model_path is None:
            hint = expected_hint or str(MODEL_DIR or "<EP_MODEL_DIR 未设置>")
            raise WeightsMissingError(
                "Qwen3-ASR 本地权重缺失",
                f"预期路径: {hint}。请经平台模型管理器下载"
                "（module.toml 已声明 HF 主源 + ModelScope 镜像），"
                "或经 variant API 切换已就绪的激活变体；adapter 不做静默联网下载。",
            )

        def _construct():
            global _asr, _asr_model_dir, _asr_load_error, _aligner_attached
            try:
                model, aligner_dir = _load_asr_sync(model_path)
                _asr = model
                _asr_model_dir = str(model_path.resolve())
                _aligner_attached = aligner_dir is not None
                _asr_load_error = None
                logger.info("激活权重: %s | aligner_attached=%s", _asr_model_dir, _aligner_attached)
            except Exception as exc:
                logger.exception("Qwen3-ASR 加载失败")
                _asr = None
                _asr_model_dir = None
                _aligner_attached = False
                _asr_load_error = f"{type(exc).__name__}: {exc}"
                raise

        await loop.run_in_executor(None, _construct)
        return _asr, _aligner_attached


def _require_aligner(model):
    """取 ASR 加载时挂载的 ForcedAligner 实例（同一实例，零额外显存）。"""
    aligner = getattr(model, "forced_aligner", None)
    if aligner is None:
        raise AlignerNotLoadedError(
            "ForcedAligner 未挂载（加载 ASR 时未找到 Qwen3-ForcedAligner-0.6B 本地权重）",
            f"获取方式: {ALIGNER_REPO_HINT}；查找顺序: "
            f"EP_ALIGNER_DIR > <EP_MODELS_ROOT>/{ALIGNER_TARGET_DIR} > 激活模型同级目录。",
        )
    return aligner


# ---------------------------------------------------------------------------
# 词级时间戳 → segments 切分（移植官方 demo 的标点断句逻辑，纯字符数对齐口径）
# ---------------------------------------------------------------------------
_SENT_SPLIT_RE = re.compile(r"[^，。！？；：,.!?:; \n]+[，。！？；：,.!?:; \n]*")
_PURE_CHAR_RE = re.compile(r"[^a-zA-Z0-9\u4e00-\u9fff]")
_LINE_END_RE = re.compile(r"[，。！？；：,.!?:;\n]\s*$")


def _pure_len(text: str) -> int:
    return len(_PURE_CHAR_RE.sub("", text))


def _segments_from_words(full_text: str, items: list, max_chars: int = 40) -> list:
    """按标点把整段文本切成句级 segments，时间取所覆盖词项的首尾。

    items 为 ForcedAlignItem 序列（.text/.start_time/.end_time，秒）；
    对齐口径与官方 demo 一致：以“纯字符数”（字母数字 + CJK）累计匹配。
    """
    segments: list[dict] = []
    if not full_text or not items:
        return segments
    ts_idx = 0
    line_items: list = []
    line_text = ""
    for m in _SENT_SPLIT_RE.finditer(full_text):
        seg_text = m.group(0)
        need = _pure_len(seg_text)
        if need == 0:
            line_text += seg_text
            continue
        matched, start_i = 0, ts_idx
        while ts_idx < len(items) and matched < need:
            matched += _pure_len(getattr(items[ts_idx], "text", ""))
            ts_idx += 1
        if ts_idx > start_i:
            line_items.extend(items[start_i:ts_idx])
        line_text += seg_text
        if _LINE_END_RE.search(seg_text) or _pure_len(line_text) >= max_chars:
            if line_items:
                segments.append({
                    "start": round(float(line_items[0].start_time), 3),
                    "end": round(float(line_items[-1].end_time), 3),
                    "text": line_text.strip(),
                })
            line_items, line_text = [], ""
    if line_items:
        segments.append({
            "start": round(float(line_items[0].start_time), 3),
            "end": round(float(line_items[-1].end_time), 3),
            "text": line_text.strip(),
        })
    return segments


def _items_to_words(items: list) -> list:
    """ForcedAlignItem 列表 → [{word,start,end}]（直接映射上游 .text/.start_time/.end_time）。"""
    return [
        {
            "word": getattr(it, "text", ""),
            "start": round(float(getattr(it, "start_time", 0.0)), 3),
            "end": round(float(getattr(it, "end_time", 0.0)), 3),
        }
        for it in items
    ]


def _audio_duration(path: str) -> float:
    try:
        import soundfile as sf
        return round(sf.info(path).duration, 3)
    except Exception:
        return 0.0


# ---------------------------------------------------------------------------
# SRT 字幕产物（srt_output 显式参数 / output_format=srt 注入双路径）
# ---------------------------------------------------------------------------
def _srt_timestamp(seconds: float) -> str:
    """秒 → SRT 时间戳 HH:MM:SS,mmm（faster-whisper 同口径）。"""
    ms = int(round(seconds * 1000))
    h, ms = divmod(ms, 3600_000)
    m, ms = divmod(ms, 60_000)
    s, ms = divmod(ms, 1000)
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


def _segments_to_srt(segments: list, fallback_text: str = "", duration: float = 0.0) -> str:
    """segments [{start,end,text}] → SRT 文本；无 segments 时降级为单条全文。"""
    if not segments:
        if not fallback_text:
            return ""
        segments = [{"start": 0.0, "end": duration if duration > 0.0 else 1.0, "text": fallback_text}]
    lines: list[str] = []
    for idx, seg in enumerate(segments, start=1):
        lines.append(str(idx))
        lines.append(
            f"{_srt_timestamp(float(seg['start']))} --> {_srt_timestamp(float(seg['end']))}"
        )
        lines.append(str(seg.get("text", "")).strip())
        lines.append("")
    return "\n".join(lines)


def _export_srt(srt_text: str, output_path: Optional[str] = None) -> str:
    """写 .srt 文件。优先写入执行器注入的 output_path，否则写 WORKSPACE。"""
    if output_path:
        out = Path(output_path)
        out.parent.mkdir(parents=True, exist_ok=True)
    else:
        WORKSPACE.mkdir(parents=True, exist_ok=True)
        out = WORKSPACE / f"transcribe_{int(time.time() * 1000)}.srt"
    out.write_text(srt_text, encoding="utf-8")
    logger.info("SRT 产物: %s (%d bytes)", out, out.stat().st_size)
    return str(out)


# ---------------------------------------------------------------------------
# 输入解析（multipart 文件上传 / JSON input_path 双形态）
# ---------------------------------------------------------------------------
async def _extract_audio_request(request: Request, file: Optional[UploadFile], params_form: Optional[str]):
    """统一解析音频输入与参数。返回 (audio_path, tmp_file, params)。"""
    content_type = request.headers.get("content-type", "")
    params: dict = {}
    audio_path: Optional[str] = None
    tmp_file: Optional[Path] = None

    if "multipart/form-data" in content_type:
        params = _parse_params(params_form)
        if file is not None and file.filename:
            WORKSPACE.mkdir(parents=True, exist_ok=True)
            safe_name = Path(file.filename).name or "upload.audio"
            tmp_file = WORKSPACE / f"{int(time.time())}_{safe_name}"
            tmp_file.write_bytes(await file.read())
            audio_path = str(tmp_file)
    else:
        try:
            body = await request.json()
        except Exception:
            body = {}
        params = _parse_params(body.get("params"))
        audio_path = body.get("input_path")

    if not audio_path:
        return None, tmp_file, params
    if not Path(audio_path).is_file():
        raise FileNotFoundError(audio_path)
    return audio_path, tmp_file, params


async def _run_infer(fn, *args):
    return await asyncio.get_running_loop().run_in_executor(None, fn, *args)


# ---------------------------------------------------------------------------
# FastAPI 应用
# ---------------------------------------------------------------------------
app = FastAPI(title=MODULE_NAME, version=MODULE_VERSION)


@app.get("/health")
async def health():
    return {
        "status": "ok",
        "engine": "qwen-asr",
        "model_loaded": _asr is not None,
        "model_id": MODEL_ID,
        "device": DEVICE,
        "backend": BACKEND,
        "load_error": _asr_load_error,
    }


@app.get("/info")
async def info():
    return {
        "module_id": "qwen3-asr",
        "name": MODULE_NAME,
        "version": MODULE_VERSION,
        "engine": "qwen-asr(transformers)",
        "active_model": {
            "model_id": MODEL_ID,
            "model_dir": str(MODEL_DIR),
            "loaded_dir": _asr_model_dir,
            "loaded": _asr is not None,
            "aligner_attached": _aligner_attached,
        },
        "device": DEVICE,
        "backend": BACKEND,
        "workspace": str(WORKSPACE),
        "capabilities": [
            {
                "name": "transcribe",
                "input_type": "audio",
                "output_type": "json",
                "params": {
                    "language": {"type": "string", "default": "auto"},
                    "context": {"type": "string", "default": ""},
                    "timestamps": {"type": "boolean", "default": True},
                    "srt_output": {"type": "boolean", "default": False, "description": "同时输出 .srt 字幕文件产物（需 timestamps）"},
                    "model": {"type": "string", "default": ""},
                },
                "supported_languages": list(SUPPORTED_LANGUAGES),
                "language_hint": f"auto 或规范语言名/ISO 代码之一（共 30 种：{LANGS_HINT}）",
            },
            {
                "name": "align",
                "input_type": "audio",
                "output_type": "json",
                "params": {
                    "text": {"type": "string", "default": ""},
                    "language": {"type": "string", "default": ""},
                },
            },
        ],
        "load_error": _asr_load_error,
    }


@app.post("/predict/transcribe")
async def predict_transcribe(
    request: Request,
    file: Optional[UploadFile] = File(None),
    params_form: Optional[str] = Form(None, alias="params"),
):
    """语音转文字。params: language(auto)/context 提示文本/timestamps/srt_output/model 变体覆盖。"""
    t0 = time.perf_counter()
    tmp_file: Optional[Path] = None
    try:
        try:
            audio_path, tmp_file, params = await _extract_audio_request(request, file, params_form)
        except FileNotFoundError as exc:
            return _error(400, "FILE_NOT_FOUND", f"输入文件不存在: {exc}")
        if not audio_path:
            return _error(400, "INVALID_INPUT", "缺少音频输入（multipart 'file' 或 JSON 'input_path'）")

        language_raw = str(params.get("language", "auto"))
        context = str(params.get("context", "") or "")
        timestamps = bool(params.get("timestamps", True))
        language = _normalize_language(language_raw)
        lang_err = _check_language(language)
        if lang_err:
            return _error(422, "INVALID_PARAMS", lang_err)

        try:
            model, aligner_ok = await _get_asr(params.get("model"))
        except ModelSelectionError as exc:
            return _error(422, "INVALID_PARAMS", str(exc))
        except WeightsMissingError as exc:
            return _error(503, "MODEL_NOT_LOADED", exc.args[0], exc.args[1] if len(exc.args) > 1 else None)
        except Exception as exc:
            # 懒加载期任何失败（依赖缺失/CUDA 错误/OOM）都收敛为契约化 503
            logger.exception("ASR 加载失败")
            return _error(503, "MODEL_NOT_LOADED", f"模型加载失败: {exc}", _asr_load_error)

        def _do():
            results = model.transcribe(
                audio=audio_path,
                context=context,
                language=language,
                return_time_stamps=bool(timestamps and aligner_ok),
            )
            return results[0]

        try:
            r = await _run_infer(_do)
        except ValueError as exc:
            # 语言不受支持等参数类错误由底层以 ValueError 抛出
            return _error(422, "INVALID_PARAMS", f"参数不被接受: {exc}")
        except Exception as exc:
            logger.exception("转写失败")
            return _error(500, "INFERENCE_ERROR", f"Transcription failed: {exc}")

        full_text = getattr(r, "text", "") or ""
        detected = getattr(r, "language", "") or ""
        words: list = []
        segments: list = []
        if timestamps and aligner_ok:
            ts_payload = getattr(r, "time_stamps", None)
            items = list(getattr(ts_payload, "items", []) or []) if ts_payload else []
            words = _items_to_words(items)
            segments = _segments_from_words(full_text, items)

        # SRT 产物：srt_output=true（显式参数）或 output_format=srt（执行器注入，带 output_path）
        srt_output = _as_bool(params.get("srt_output", False))
        output_format = str(params.get("output_format") or "").strip().lower()
        injected_output_path = str(params.get("output_path") or "").strip() or None
        srt_path: Optional[str] = None
        if srt_output or output_format == "srt":
            try:
                srt_text = _segments_to_srt(
                    segments,
                    fallback_text=full_text,
                    duration=_audio_duration(audio_path),
                )
                srt_path = _export_srt(srt_text, injected_output_path)
            except OSError as exc:
                logger.exception("SRT 写入失败")
                return _error(500, "FILE_WRITE_ERROR", f"SRT 文件写入失败: {exc}")

        elapsed = round(time.perf_counter() - t0, 3)
        # output_format=srt 注入契约：返回文件产物形态（faster-whisper 同口径）
        if output_format == "srt":
            return {
                "status": "completed",
                "output_type": "file",
                "result": srt_path,
                "output_path": srt_path,
                "elapsed_seconds": elapsed,
            }
        return {
            "status": "completed",
            "output_type": "json",
            "result": {
                "text": full_text,
                "segments": segments,
                "words": words,
                "language": detected,
                "duration_seconds": _audio_duration(audio_path),
                "timestamps": bool(words),
                "timestamps_degraded": bool(timestamps and not words),
            },
            "output_path": srt_path,
            "elapsed_seconds": elapsed,
        }
    finally:
        if tmp_file is not None and tmp_file.exists():
            try:
                tmp_file.unlink()
            except OSError:
                pass


@app.post("/predict/align")
async def predict_align(
    request: Request,
    file: Optional[UploadFile] = File(None),
    params_form: Optional[str] = Form(None, alias="params"),
):
    """强制对齐：音频 + 参考文本 -> 词级 [{word,start,end}]。

    文本来源优先级：params.text > JSON body.input_text/text/input。
    language 必填（对齐不做语种自动检测），接受规范语言名或 ISO 代码。
    """
    t0 = time.perf_counter()
    tmp_file: Optional[Path] = None
    try:
        try:
            audio_path, tmp_file, params = await _extract_audio_request(request, file, params_form)
        except FileNotFoundError as exc:
            return _error(400, "FILE_NOT_FOUND", f"输入文件不存在: {exc}")

        ref_text = str(params.get("text", "") or "").strip()
        if not ref_text and "multipart/form-data" not in request.headers.get("content-type", ""):
            # 复用上面已解析的 body 不方便，这里仅在 JSON 形态下补一次读取
            try:
                body = await request.json()
            except Exception:
                body = {}
            for key in ("input_text", "text", "input"):
                val = body.get(key)
                if isinstance(val, str) and val.strip():
                    ref_text = val.strip()
                    break
        if not ref_text:
            return _error(
                422, "INVALID_PARAMS",
                "缺少参考文本（params.text 或 JSON body.input_text）",
                "align 需要待对齐的文本；管线中可将上游 ASR/翻译节点的文本产物映射到该字段。",
            )

        raw_lang = str(params.get("language", "") or "").strip()
        language = _normalize_language(raw_lang)
        if language is None:
            return _error(
                422, "INVALID_PARAMS",
                "align 必须显式指定 language（不支持 auto 自动检测）",
                f"支持规范语言名或 ISO 代码（共 30 种: {LANGS_HINT}）；收到: {raw_lang!r}",
            )
        lang_err = _check_language(language)
        if lang_err:
            return _error(422, "INVALID_PARAMS", lang_err)

        if not audio_path:
            return _error(400, "INVALID_INPUT", "缺少音频输入（multipart 'file' 或 JSON 'input_path'）")

        try:
            model, _attached = await _get_asr(params.get("model"))
            aligner = _require_aligner(model)
        except ModelSelectionError as exc:
            return _error(422, "INVALID_PARAMS", str(exc))
        except WeightsMissingError as exc:
            return _error(503, "MODEL_NOT_LOADED", exc.args[0], exc.args[1] if len(exc.args) > 1 else None)
        except AlignerNotLoadedError as exc:
            return _error(503, "MODEL_NOT_LOADED", exc.args[0], exc.args[1] if len(exc.args) > 1 else None)
        except Exception as exc:
            logger.exception("ASR/Aligner 加载失败")
            return _error(503, "MODEL_NOT_LOADED", f"模型加载失败: {exc}", _asr_load_error)

        def _do():
            results = aligner.align(audio=audio_path, text=ref_text, language=language)
            return results[0]

        try:
            align_result = await _run_infer(_do)
        except Exception as exc:
            logger.exception("对齐失败")
            return _error(500, "INFERENCE_ERROR", f"Alignment failed: {exc}")

        items = list(getattr(align_result, "items", []) or [])
        elapsed = round(time.perf_counter() - t0, 3)
        return {
            "status": "completed",
            "output_type": "json",
            "result": {
                "words": _items_to_words(items),
                "language": language,
                "text": ref_text,
                "duration_seconds": _audio_duration(audio_path),
            },
            "output_path": None,
            "elapsed_seconds": elapsed,
        }
    finally:
        if tmp_file is not None and tmp_file.exists():
            try:
                tmp_file.unlink()
            except OSError:
                pass


@app.post("/predict/{capability}")
async def predict_unknown(capability: str):
    return _error(
        404, "INVALID_CAPABILITY",
        f"Unknown capability: {capability}",
        "Supported capabilities: transcribe, align",
    )


if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )
    logger.info(
        "qwen3-asr adapter 启动 | MODEL_DIR=%s | MODELS_ROOT=%s | BACKEND=%s | DEVICE=%s | MODEL_ID=%s",
        MODEL_DIR, MODELS_ROOT, BACKEND, DEVICE, MODEL_ID,
    )
    uvicorn.run(
        app,
        # 绑定地址读 EP_HOST（daemon 注入，恒回环）——硬编码 0.0.0.0 会触发
        # Windows 防火墙弹窗，见 ep-core process.rs build_module_env
        host=os.getenv("EP_HOST", "127.0.0.1"),
        port=PORT,
        log_level="info",
    )
