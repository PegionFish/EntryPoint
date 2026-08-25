"""
Qwen3-TTS 语音合成模块 — EntryPoint adapter
=============================================

三能力（对齐源应用 AI_Applications/Qwen3TTS，官方 qwen-tts SDK 三模型类）：

  POST /predict/synthesize   : text (+language/instruct/voice/speed/sample_rate)
                                → WAV
                                - VoiceDesign 模型：instruct 自然语言声音描述
                                - CustomVoice 模型：voice 音色 + instruct 风格（兼容旧调用，0.6B 无风格）
  POST /predict/clone_voice  : text (+ref_audio/ref_text/language/…) → WAV
                                 Base 模型克隆；ref_text 留空 → x-vector 零样本克隆；
                                 调用失败/模型不匹配 → 回退 synthesize（metadata.note 标注）
  POST /predict/custom_voice : text (+spk_id 九音色/instruct 情绪) → WAV
                                CustomVoice 模型

底层为官方 qwen-tts 库（requirements.txt 指定 pip install -U qwen-tts；
源码权威参考 /home/bob/AI_Applications/Qwen3TTS/Qwen3-TTS/qwen_tts/
inference/qwen3_tts_model.py）：
  - Qwen3TTSModel.from_pretrained(path, device_map=…, dtype=…)
  - generate_voice_design(text, instruct, language, **kwargs) → (wavs, sr)
  - generate_voice_clone(text, ref_audio, ref_text, x_vector_only_mode,
                         language, **kwargs) → (wavs, sr)
  - generate_custom_voice(text, speaker, language, instruct, **kwargs) → (wavs, sr)

变体流转（module.toml [[models]].target_dir，EP_MODEL_DIR/EP_MODEL_ID 约定不变）：
  1) EP_MODEL_ID → MODEL_DIR_MAP → <EP_MODEL_DIR>/<target_dir>；
  2) daemon 布局 EP_MODEL_DIR 直指激活变体目录（config.json 平铺）时直接取用；
  3) 能力与激活模型类型不匹配时，按能力挑 EP_MODELS_ROOT/<target_dir> 变体重载；
  4) 权重缺失拒绝联网下载，返回可执行报错。
"""

from __future__ import annotations

import asyncio
import inspect
import json
import logging
import os
import sys
import threading
import time
import uuid
import wave
from pathlib import Path
from typing import Any, Optional

import uvicorn
from fastapi import FastAPI, File, Form, Request, UploadFile
from fastapi.responses import JSONResponse

# ---------------------------------------------------------------------------
# 环境变量
# ---------------------------------------------------------------------------
MODULE_DIR = Path(os.environ.get("EP_MODULE_DIR", Path(__file__).resolve().parent))
MODEL_DIR = Path(os.environ.get("EP_MODEL_DIR", MODULE_DIR / "models"))
# 模型缓存根目录（ep-core process.rs build_module_env 注入）：EP_MODEL_DIR 恒指激活
# 变体目录，适配器需按 module.toml [[models]].target_dir 在此解析非激活变体权重。
MODELS_ROOT = Path(os.environ["EP_MODELS_ROOT"]) if os.environ.get("EP_MODELS_ROOT") else None
WORKSPACE = Path(os.environ.get("EP_WORKSPACE", "."))
PORT = int(os.environ.get("EP_PORT", "8000"))
DEVICE = os.environ.get("EP_DEVICE", "cuda").strip().lower()
# 设备判定以 EP_BACKEND（裸后端名，如 "cuda"）为准 —— daemon 注入的
# EP_DEVICE 形如 "cuda:0"（ep-core process.rs build_module_env），
# 直接 `== "cuda"` 比较恒为 False。EP_BACKEND 缺省时回退取 EP_DEVICE 冒号前缀。
BACKEND = os.environ.get("EP_BACKEND", "").strip().lower() or (
    DEVICE.split(":")[0] if DEVICE else "cuda"
)
MODEL_ID = os.environ.get("EP_MODEL_ID", "1.7b")

# 模型 ID → 子目录名映射（module.toml [[models]].target_dir）
MODEL_DIR_MAP: dict[str, str] = {
    "1.7b": "qwen3-tts-1.7b",
    "0.6b": "qwen3-tts-0.6b",
    # 1.7B 三模变体（MODULE_PARITY_PLAN §4.3 A1 / §3 A1）
    "tts-voice-design": "qwen3-tts-12hz-1.7b-voice-design",
    "tts-base-clone": "qwen3-tts-12hz-1.7b-base-clone",
    "tts-custom-voice": "qwen3-tts-12hz-1.7b-custom-voice",
}

# 能力 → 可用模型类型（config.json tts_model_type）
_CAP_REQUIRED_TYPES: dict[str, tuple[str, ...]] = {
    "synthesize": ("voice_design", "custom_voice"),
    "clone_voice": ("base",),
    "custom_voice": ("custom_voice",),
}

# 能力 → 变体 target_dir 兜底顺序（激活变体类型不匹配时，在缓存根下挑权重重载）
_CAP_VARIANT_DIRS: dict[str, list[str]] = {
    "synthesize": [
        "qwen3-tts-12hz-1.7b-voice-design",
        "qwen3-tts-12hz-1.7b-custom-voice",
        "qwen3-tts-1.7b",
        "qwen3-tts-0.6b",
    ],
    "clone_voice": [
        "qwen3-tts-12hz-1.7b-base-clone",
    ],
    "custom_voice": [
        "qwen3-tts-12hz-1.7b-custom-voice",
        "qwen3-tts-1.7b",
        "qwen3-tts-0.6b",
    ],
}

# 语言枚举：auto + 10 种正名（模型卡 Supported Languages；
# 底层 codec_language_id 还含 beijing_dialect/sichuan_dialect，随 speaker 自动方言化，
# 不在用户可选枚举中）。契约值均为小写，传输给模型时转规范正名。
_SUPPORTED_LANGUAGES: tuple[str, ...] = (
    "chinese", "english", "german", "italian",
    "portuguese", "spanish", "japanese", "korean",
    "french", "russian",
)
_CANONICAL_LANG: dict[str, str] = {
    lang.lower(): lang.title() for lang in (
        "Chinese", "English", "German", "Italian", "Portuguese",
        "Spanish", "Japanese", "Korean", "French", "Russian",
    )
}

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("qwen3-tts")


class _EpProgress:
    """[EP-PROGRESS] 进度上报：msg 以 0–100 整数开头，前端取首个整数。

    百分比单调递增；同类事件节流（步进 ≥ min_step_pct 或距上次 ≥
    min_interval_s 才打印）；100 恒定上报。仅 print(flush=True)。
    """

    __slots__ = ("_min_step", "_min_interval", "_last_pct", "_last_ts")

    def __init__(self, min_step_pct: int = 1, min_interval_s: float = 0.5) -> None:
        self._min_step = max(1, int(min_step_pct))
        self._min_interval = float(min_interval_s)
        self._last_pct = -1
        self._last_ts = 0.0

    def report(self, pct: int, msg: str = "") -> None:
        pct = max(0, min(100, int(pct)))
        if pct < self._last_pct:
            return
        now = time.monotonic()
        if (
            pct < 100
            and self._last_pct >= 0
            and (pct - self._last_pct) < self._min_step
            and (now - self._last_ts) < self._min_interval
        ):
            return
        self._last_pct = pct
        self._last_ts = now
        text = f"{pct} {msg}".strip()
        print(f"[EP-PROGRESS] {text}", flush=True)

# ---------------------------------------------------------------------------
# 全局状态
# ---------------------------------------------------------------------------
_model: Any = None
_model_dir: Optional[str] = None
_engine: str = "none"  # "qwen3-tts" | "edge-tts" | "none"
_load_error: Optional[str] = None

# 1.7B/0.6B CustomVoice 变体支持的音色（模型卡 Speakers 表；
# 底层 spk_id 键全小写，串行端保留模型卡首字母大写名）
_SPEAKERS: tuple[str, ...] = (
    "Vivian", "Serena", "Uncle_Fu", "Dylan", "Eric",
    "Ryan", "Aiden", "Ono_Anna", "Sohee",
)
_SPEAKER_LIT = ", ".join(_SPEAKERS)


class CapabilityMismatchError(Exception):
    """已加载模型不含该能力所需模型类型（提示切换变体）。"""


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


def _map_speaker(voice: str) -> str:
    """将 API voice 参数映射为 CustomVoice 音色名（兼容旧调用，未知回退 Vivian）。"""
    v = (voice or "").strip()
    if not v or v.lower() in ("default", "xiaoxiao"):
        return "Vivian"
    for s in _SPEAKERS:
        if s.lower() == v.lower():
            return s
    logger.warning("未知音色 %r，回退默认音色 Vivian", voice)
    return "Vivian"


def _validate_speaker(spk_id: str) -> str:
    """custom_voice 出口严格校验：非九音色直接抛错（select 枚举前端层已拘束）。"""
    v = (spk_id or "").strip()
    if not v:
        return "Vivian"
    for s in _SPEAKERS:
        if s.lower() == v.lower():
            return s
    raise ValueError(f"未知内置音色 {spk_id!r}；支持：{_SPEAKER_LIT}")


def _normalize_language(raw: Any) -> str:
    """语言枚举归一：auto/空 → "Auto"；10 正名（大小写均可）→ 模型卡规范名。

    不在枚举内抛 ValueError（上层 400 INVALID_PARAM）。
    """
    s = str(raw or "").strip()
    if not s or s.lower() == "auto":
        return "Auto"
    canon = _CANONICAL_LANG.get(s.lower())
    if canon is None:
        raise ValueError(
            f"不支持的语言 {s!r}；支持：auto + " + ", ".join(_SUPPORTED_LANGUAGES)
        )
    return canon


# ---------------------------------------------------------------------------
# 模型目录解析
# ---------------------------------------------------------------------------
def _has_config(path: Path) -> bool:
    return path.is_dir() and (path / "config.json").is_file()


def _probe_model_type(path: Path) -> str:
    """免加载探测变体 config.json 的 tts_model_type（同步、纯标准库）。"""
    if not path:
        return ""
    try:
        with (path / "config.json").open("r", encoding="utf-8") as f:
            return str(json.load(f).get("tts_model_type", ""))
    except (OSError, ValueError, json.JSONDecodeError):
        return ""


def _resolve_active_model_path() -> Optional[Path]:
    """根据 EP_MODEL_ID / EP_MODEL_DIR 定位激活模型目录。"""
    subdir = MODEL_DIR_MAP.get(MODEL_ID)
    if subdir:
        candidate = MODEL_DIR / subdir
        if candidate.is_dir():
            return candidate
    # daemon 布局：EP_MODEL_DIR 直指变体目录（ep-core model.rs local_dir=target_dir，
    # snapshot_download 将 config.json 平铺其下），此时 MODEL_DIR 本身即模型目录
    if (MODEL_DIR / "config.json").is_file():
        return MODEL_DIR
    # 回退：扫描 MODEL_DIR 下任何包含 config.json 的子目录
    if MODEL_DIR.is_dir():
        for child in sorted(MODEL_DIR.iterdir()):
            if _has_config(child):
                return child
    return None


def _candidate_roots() -> list[Path]:
    """按作用域收集可放置变体 target_dir 的根目录（去重）。"""
    roots: list[Path] = []
    if MODELS_ROOT is not None:
        roots.append(MODELS_ROOT)
    if MODEL_DIR.is_dir():
        roots.append(MODEL_DIR.parent)  # 激活变体的兄弟目录
        roots.append(MODEL_DIR)         # MODEL_DIR 本身即缓存根（直跑旧布局）
    seen: set[str] = set()
    out: list[Path] = []
    for r in roots:
        key = str(r)
        if key not in seen:
            seen.add(key)
            out.append(r)
    return out


def _resolve_capability_dir(cap: str) -> Optional[Path]:
    """按能力在变体兜底表中解析权重目录（探测类型一致才返回）。"""
    required = _CAP_REQUIRED_TYPES.get(cap, ())
    for target_dir in _CAP_VARIANT_DIRS.get(cap, ()):
        for root in _candidate_roots():
            candidate = root / target_dir
            if _has_config(candidate) and _probe_model_type(candidate) in required:
                return candidate
    return None


# ---------------------------------------------------------------------------
# 模型加载
# ---------------------------------------------------------------------------
def _qwen_model_type(model: Any) -> str:
    """读取 qwen-tts 加载实例的 tts_model_type（0.6B/1.7B Base/CustomVoice/VoiceDesign）。"""
    try:
        return str(getattr(getattr(model, "model", None), "tts_model_type", ""))
    except Exception:
        return ""


def _load_qwen3_tts(model_path: Path) -> bool:
    """加载 Qwen3-TTS 模型（官方 qwen-tts 库），成功返回 True。"""
    global _model, _engine, _load_error, _model_dir

    try:
        import torch
        from qwen_tts import Qwen3TTSModel

        # 设备选择：EP_BACKEND=="cuda" 且 CUDA 可用 → cuda:<EP_DEVICE_INDEX>，
        # 否则回退 CPU（保持既有回退语义）。
        device_index = os.environ.get("EP_DEVICE_INDEX", "")
        if BACKEND == "cuda" and torch.cuda.is_available():
            device_map = f"cuda:{int(device_index or 0)}"
            dtype = torch.bfloat16  # 模型卡推荐精度（sm_120 原生支持 bf16）
        else:
            if BACKEND == "cuda":
                logger.warning("CUDA 不可用，回退到 CPU")
            device_map = "cpu"
            dtype = torch.float32

        _EpProgress().report(0, f"model load start {model_path.name}")
        logger.info(
            "正在加载 Qwen3-TTS 模型: %s (backend=%s, device_map=%s, dtype=%s)",
            model_path, BACKEND, device_map, dtype,
        )
        _model = Qwen3TTSModel.from_pretrained(
            str(model_path),
            device_map=device_map,
            dtype=dtype,
        )
        _model_dir = str(model_path)
        _engine = "qwen3-tts"
        _load_error = None
        logger.info(
            "Qwen3-TTS 模型加载完成 (device_map=%s, type=%s)",
            device_map, _qwen_model_type(_model),
        )
        _EpProgress().report(40, "model ready")
        return True
    except Exception as exc:
        _load_error = f"Qwen3-TTS 加载失败: {exc}"
        logger.warning("%s", _load_error)
        _model = None
        return False


async def _ensure_capability_model(cap: str) -> Optional[str]:
    """确保已加载模型能服务 cap；不能则按能力挑变体重载。

    返回 None 表示就绪，否则返回可直接透传给用户的可执行错误信息。
    """
    global _engine

    if _engine == "none":
        return f"无可用合成引擎: {_load_error or '未知错误'}"
    if _engine == "edge-tts":
        return (
            "本地 Qwen3-TTS 模型不可用（当前为 edge-tts 在线降级引擎）；"
            f"「{cap}」能力需要本地模型权重，请先经平台模型管理器导入后重启模块"
        )

    target = _resolve_active_model_path()
    if _model is not None and target is not None:
        if _qwen_model_type(_model) in _CAP_REQUIRED_TYPES.get(cap, ()):
            return None
    if target is None:
        target = _resolve_capability_dir(cap)

    if target is None:
        hint_dirs = ", ".join(_CAP_VARIANT_DIRS.get(cap, ())) or "<非法能力>"
        return (
            f"未找到可服务「{cap}」的 Qwen3-TTS 权重目录 "
            f"(MODELS_ROOT={MODELS_ROOT or MODEL_DIR})；所需变体：{hint_dirs}。"
            "请经模型管理器本地导入或下载后重启模块"
        )

    if _model_dir == str(target) and _model is not None:
        return None

    async with _load_lock:
        if _model_dir == str(target) and _model is not None:
            return None
        logger.info("能力「%s」需要 %s，切换加载 %s", cap, _probe_model_type(target), target)
        ok = await asyncio.get_event_loop().run_in_executor(
            None, _load_qwen3_tts, target
        )
        if not ok:
            return _load_error or "模型加载失败"
    return None


def _check_edge_tts() -> bool:
    """检查 edge-tts 是否可用作 fallback。"""
    try:
        import edge_tts  # noqa: F401
        return True
    except ImportError:
        return False


def _init_engine() -> None:
    """初始化合成引擎（启动时调用一次）。"""
    global _engine, _load_error

    if _resolve_active_model_path() is not None:
        if _load_qwen3_tts(_resolve_active_model_path()):  # type: ignore[arg-type]
            return
        _engine = "edge-tts" if _check_edge_tts() else "none"
        if _engine == "edge-tts":
            logger.info("模型加载失败，使用 edge-tts 作为 fallback 引擎")
        else:
            _load_error = f"{_load_error}; edge-tts 亦不可用"
            logger.error("%s", _load_error)
        return

    _load_error = f"模型目录未找到 (MODEL_DIR={MODEL_DIR}, MODEL_ID={MODEL_ID})"
    logger.warning("Qwen3-TTS 模型不可用: %s", _load_error)
    if _check_edge_tts():
        _engine = "edge-tts"
        logger.info("使用 edge-tts 作为 fallback 引擎")
        return

    _engine = "none"
    _load_error = "Qwen3-TTS 和 edge-tts 均不可用"
    logger.error("%s", _load_error)


# ---------------------------------------------------------------------------
# 音频合成（qwen-tts 三 generate 出口）
# ---------------------------------------------------------------------------
def _write_result_wav(result: Any, output_path: Path) -> tuple[float, int]:
    """把 generate_* 返回值 (wavs, sr) 写成 WAV，返回 (时长秒, sr)。

    wavs 兼容列表/tuple/独立 array，元素兼容 numpy/torch。
    """
    import numpy as np
    import soundfile as sf

    if isinstance(result, (tuple, list)) and len(result) >= 2:
        wavs, sr = result[0], result[1]
    else:
        raise TypeError(f"模型返回格式无法解析: {type(result)}")

    audio_data = wavs[0] if isinstance(wavs, (list, tuple)) else wavs

    if not isinstance(audio_data, np.ndarray):
        try:
            import torch
            if isinstance(audio_data, torch.Tensor):
                audio_data = audio_data.detach().cpu().numpy()
            else:
                audio_data = np.array(audio_data)
        except ImportError:
            audio_data = np.array(audio_data)

    if audio_data.ndim > 1:
        audio_data = audio_data.squeeze()

    # 归一化到 [-1, 1]（如果是整数格式）
    if audio_data.dtype in (np.int16, np.int32):
        max_val = np.iinfo(audio_data.dtype).max
        audio_data = audio_data.astype(np.float32) / max_val

    sf.write(str(output_path), audio_data, sr)
    duration = len(audio_data) / sr
    return duration, int(sr)


def _sync_generate(
    cap: str,
    text: str,
    params: dict,
    output_path: Path,
) -> tuple[float, int]:
    """同步推理分派：按能力 + 已加载模型类型选 generate_* 出口。

    校准依赖 _ensure_capability_model 已完成；类型不匹配抛 CapabilityMismatchError。
    """
    model_type = _qwen_model_type(_model)
    language = str(params.get("__language", "Auto"))
    speed = float(params.get("speed", 1.0))

    if model_type not in _CAP_REQUIRED_TYPES.get(cap, ()):
        raise CapabilityMismatchError(
            f"已加载模型类型 {model_type!r} 不服务「{cap}」"
        )

    if cap == "clone_voice":
        generate = getattr(_model, "generate_voice_clone", None)
        ref_text = str(params.get("ref_text", "") or "").strip()
        kwargs = {
            "text": text,
            "language": language,
            "ref_audio": str(params["__ref_audio"]),
            "ref_text": ref_text,
            "x_vector_only_mode": not bool(ref_text),
        }
        if generate is None:
            raise CapabilityMismatchError("qwen-tts 库未提供 generate_voice_clone")
        if "speed" in inspect.signature(generate).parameters:
            kwargs["speed"] = speed
        logger.info(
            "语音克隆: ref=%s, icl=%s, language=%s, %d 字符",
            params["__ref_audio"], "ref_text" if ref_text else "x-vector零样本", language, len(text),
        )
        return _write_result_wav(generate(**kwargs), output_path)

    if cap == "custom_voice":
        speaker = params["__speaker"]
        instruct = str(params.get("instruct", "") or "").strip()
        generate = getattr(_model, "generate_custom_voice", None)
        kwargs = {
            "text": text,
            "speaker": speaker,
            "language": language,
            "instruct": instruct,
        }
        if generate is None:
            raise CapabilityMismatchError("qwen-tts 库未提供 generate_custom_voice")
        if "speed" in inspect.signature(generate).parameters:
            kwargs["speed"] = speed
        logger.info(
            "合成[custom]: speaker=%s, instruct=%s, language=%s, %d 字符",
            speaker, instruct or "(无)", language, len(text),
        )
        return _write_result_wav(generate(**kwargs), output_path)

    # cap == "synthesize"：voice_design → generate_voice_design；custom_voice → 兼容旧调用
    instruct = str(params.get("instruct", "") or "").strip()
    if model_type == "voice_design":
        generate = getattr(_model, "generate_voice_design", None)
        if generate is None:
            raise CapabilityMismatchError("qwen-tts 库未提供 generate_voice_design")
        kwargs = {"text": text, "language": language, "instruct": instruct}
        if "speed" in inspect.signature(generate).parameters:
            kwargs["speed"] = speed
        logger.info(
            "合成[voice_design]: instruct=%s, language=%s, %d 字符",
            instruct or "(无)", language, len(text),
        )
        return _write_result_wav(generate(**kwargs), output_path)

    speaker = params.get("__speaker")
    generate = getattr(_model, "generate_custom_voice", None)
    kwargs = {
        "text": text,
        "speaker": speaker,
        "language": language,
        "instruct": instruct,
    }
    if generate is None:
        raise CapabilityMismatchError("qwen-tts 库未提供 generate_custom_voice")
    if "speed" in inspect.signature(generate).parameters:
        kwargs["speed"] = speed
    logger.info(
        "合成[custom]: speaker=%s, instruct=%s, language=%s, %d 字符",
        speaker, instruct or "(无)", language, len(text),
    )
    return _write_result_wav(generate(**kwargs), output_path)


# ---------------------------------------------------------------------------
# edge-tts 降级（仅供 synthesize 兼容）
# ---------------------------------------------------------------------------
async def _synthesize_edge_tts(
    text: str,
    voice: str,
    sample_rate: int,
    output_path: Path,
) -> float:
    """使用 edge-tts 在线合成语音，返回时长（秒）。"""
    import edge_tts

    # edge-tts 音色映射：将 "default" 映射为中文女声
    voice_map: dict[str, str] = {
        "default": "zh-CN-XiaoxiaoNeural",
        "xiaoxiao": "zh-CN-XiaoxiaoNeural",
        "yunxi": "zh-CN-YunxiNeural",
        "en": "en-US-JennyNeural",
    }
    tts_voice = voice_map.get(voice, voice if "-" in voice else voice_map["default"])

    communicate = edge_tts.Communicate(text, tts_voice)

    # edge-tts 输出 mp3，先保存临时文件再转 WAV
    tmp_mp3 = output_path.with_suffix(".mp3")
    try:
        await communicate.save(str(tmp_mp3))

        # 尝试用 pydub 转 WAV；如果不可用则直接重命名
        try:
            from pydub import AudioSegment
            audio_seg = AudioSegment.from_mp3(str(tmp_mp3))
            audio_seg = audio_seg.set_frame_rate(sample_rate).set_channels(1)
            audio_seg.export(str(output_path), format="wav")
        except ImportError:
            # 无 pydub 时尝试 ffmpeg
            import subprocess
            try:
                subprocess.run(
                    [
                        "ffmpeg", "-y", "-i", str(tmp_mp3),
                        "-ar", str(sample_rate), "-ac", "1",
                        str(output_path),
                    ],
                    check=True,
                    capture_output=True,
                )
            except (FileNotFoundError, subprocess.CalledProcessError):
                # 最终 fallback：直接输出 mp3（改扩展名）
                logger.warning("无法转换为 WAV，输出 MP3 格式")
                output_path_mp3 = output_path.with_suffix(".mp3")
                if tmp_mp3 != output_path_mp3:
                    tmp_mp3.rename(output_path_mp3)
                return _estimate_duration_from_size(output_path_mp3)

        return _get_wav_duration(output_path)
    finally:
        if tmp_mp3.exists() and tmp_mp3 != output_path:
            try:
                tmp_mp3.unlink()
            except OSError:
                pass


def _get_wav_duration(path: Path) -> float:
    """读取 WAV 文件时长。"""
    try:
        with wave.open(str(path), "rb") as wf:
            frames = wf.getnframes()
            rate = wf.getframerate()
            return frames / rate if rate > 0 else 0.0
    except Exception:
        return 0.0


def _estimate_duration_from_size(path: Path) -> float:
    """粗略估算音频时长（用于无法解析时）。"""
    try:
        size = path.stat().st_size
        # 假设 128kbps mp3
        return size / (128 * 1024 / 8)
    except OSError:
        return 0.0


# ---------------------------------------------------------------------------
# FastAPI 应用
# ---------------------------------------------------------------------------
app = FastAPI(
    title="Qwen3-TTS 语音合成",
    description="基于 Qwen3 的高质量多语言语音合成（VoiceDesign / 克隆 / CustomVoice）— EntryPoint 模块",
    version="1.1.0",
)


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


async def _extract_request(
    request: Request,
    file: Optional[UploadFile],
    params_form: Optional[str],
) -> tuple[Optional[dict], Optional[str]]:
    """解析 /predict/<cap> 请求体（交互协议同 ADAPTER_API.md / ep-core executor）。

    返回 (params, text)；错误已转成 dict 错误响应时返回 (None, err)。
    """
    content_type = request.headers.get("content-type", "")
    try:
        if "multipart/form-data" in content_type:
            params = _parse_params(params_form)
            text = ""
            if file is not None and file.filename:
                raw = await file.read()
                try:
                    text = raw.decode("utf-8")
                except UnicodeDecodeError:
                    text = raw.decode("gbk", errors="replace")
            return params, text
        body = await request.json()
        params = _parse_params(body.get("params"))
        raw_input = body.get("input")
        if raw_input is None:
            raw_input = body.get("input_text")
        if isinstance(raw_input, str):
            text = raw_input
        elif raw_input is not None:
            text = json.dumps(raw_input, ensure_ascii=False)
        if not (text or "").strip():
            input_path = body.get("input_path")
            if input_path:
                p = Path(input_path)
                if not p.is_file():
                    return None, json.dumps({
                        "status": "error",
                        "error_code": "FILE_NOT_FOUND",
                        "message": f"input_path 不存在: {input_path}",
                    })
                text = p.read_text(encoding="utf-8", errors="replace")
        return params, text
    except Exception as exc:
        return None, json.dumps({
            "status": "error",
            "error_code": "INVALID_INPUT",
            "message": f"请求解析失败: {exc}",
        })


async def _predict(
    request: Request,
    file: Optional[UploadFile],
    params_form: Optional[str],
    cap: str,
):
    """/predict/<cap> 通用实现：入参解析 → 能力模型就位 → 合成 → 契约响应。"""
    if _engine == "none":
        return _error(503, "MODEL_NOT_LOADED", f"无可用合成引擎: {_load_error or '未知错误'}")

    params, text = await _extract_request(request, file, params_form)
    if params is None:
        err = json.loads(text or "{}")
        return JSONResponse(status_code=400, content=err)
    text = (text or "").strip()
    if not text:
        return _error(
            400, "INVALID_INPUT",
            "缺少输入文本（需 multipart 'file' 文本文件或 JSON 'input'/'input_text'/'input_path'）",
        )
    if len(text) > 5000:
        return _error(400, "INVALID_INPUT", f"文本超长（{len(text)} > 5000 字符）")

    # ---- 公共参数（共享契约字段） ----
    voice: str = str(params.get("voice", "default"))
    try:
        speed: float = float(params.get("speed", 1.0))
    except (TypeError, ValueError):
        speed = 1.0
    try:
        sample_rate: int = int(params.get("sample_rate", 24000))
    except (TypeError, ValueError):
        sample_rate = 24000
    speed = max(0.5, min(2.0, speed))
    if sample_rate not in (8000, 16000, 22050, 24000, 44100, 48000):
        sample_rate = 24000
    try:
        language = _normalize_language(params.get("language", "auto"))
        params["__language"] = language
    except ValueError as exc:
        return _error(400, "INVALID_PARAM", str(exc))

    inference_cap = cap

    # ---- 能力专属参数 ----
    if cap == "clone_voice":
        ref_audio = str(params.get("ref_audio", "") or "").strip()
        path = Path(ref_audio)
        if not ref_audio:
            return _error(
                400, "INVALID_PARAM",
                "缺少 ref_audio（参考音频服务器路径，经 /api/upload/input 上传后回填）；"
                f"所需变体：" + ", ".join(_CAP_VARIANT_DIRS["clone_voice"]),
            )
        if not path.is_file():
            return _error(
                400, "REF_AUDIO_NOT_FOUND",
                f"参考音频文件不存在: {ref_audio}",
            )
        params["__ref_audio"] = str(path.resolve())
        params["ref_text"] = str(params.get("ref_text", "") or "").strip()
    elif cap == "custom_voice":
        try:
            params["__speaker"] = _validate_speaker(str(params.get("spk_id", "Vivian")))
        except ValueError as exc:
            return _error(400, "INVALID_PARAM", str(exc))
        params["instruct"] = str(params.get("instruct", "") or "").strip()
    else:  # synthesize：voice 音色映射（CustomVoice 模型生效）
        params["__speaker"] = _map_speaker(voice)
        params["instruct"] = str(params.get("instruct", "") or "").strip()

    # ---- 输出路径：params.output_path（模块产物协议注入）优先，否则 workspace ----
    injected = params.get("output_path")
    if injected:
        output_path = Path(str(injected))
        output_path.parent.mkdir(parents=True, exist_ok=True)
    else:
        WORKSPACE.mkdir(parents=True, exist_ok=True)
        filename = f"tts_{cap}_{int(time.time())}_{uuid.uuid4().hex[:8]}.wav"
        output_path = WORKSPACE / filename

    # ---- 执行 ----
    t0 = time.time()
    progress = _EpProgress()
    fallback_note: Optional[str] = None
    sr = sample_rate
    try:
        if inference_cap == "synthesize" and _engine == "edge-tts":
            # 旧行为：仅 synthesize 支持 edge-tts 在线降级
            duration = await _synthesize_edge_tts(text, voice, sample_rate, output_path)
        else:
            err = await _ensure_capability_model(inference_cap)
            if err is not None:
                return _error(503, "MODEL_NOT_LOADED", err)
            progress.report(40, f"generate {inference_cap} ({len(text)} chars)")
            try:
                duration, sr = await asyncio.get_event_loop().run_in_executor(
                    None, _sync_generate,
                    inference_cap, text, params, output_path,
                )
            except CapabilityMismatchError as cm_exc:
                # 契约回退：clone_voice 失败/模型异常 → 降级 synthesize（VoiceDesign/
                # CustomVoice 模型）重试并提示；Base 模型上二者皆不可则明确报错
                if inference_cap == "clone_voice" and _qwen_model_type(_model) in _CAP_REQUIRED_TYPES.get("synthesize", ()):
                    logger.warning("clone_voice 能力回退 synthesize：%s", cm_exc)
                    fallback_note = f"clone_voice 不可用（{cm_exc}），已回退 synthesize"
                    params.pop("__ref_audio", None)
                    params["__speaker"] = _map_speaker(str(params.get("voice", "default")))
                    params["instruct"] = str(params.get("instruct", "") or "").strip()
                    duration, sr = await asyncio.get_event_loop().run_in_executor(
                        None, _sync_generate,
                        "synthesize", text, params, output_path,
                    )
                elif inference_cap == "clone_voice":
                    return _error(
                        400, "UNSUPPORTED_CAPABILITY",
                        f"{cm_exc}；clone_voice 需激活 tts-base-clone 变体"
                        "（Base 模型）并导入权重，或换用 synthesize/custom_voice",
                    )
                else:
                    return _error(400, "UNSUPPORTED_CAPABILITY", str(cm_exc))

        progress.report(95, "synthesis done")

        # edge-tts 无法转 WAV 时可能降级输出同名 .mp3
        if not output_path.exists():
            alt = output_path.with_suffix(".mp3")
            if alt.exists():
                output_path = alt
            else:
                raise RuntimeError("合成完成但输出文件不存在")

        progress.report(100, f"{cap} done")
        elapsed = round(time.time() - t0, 3)
        metadata: dict[str, Any] = {
            "engine": _engine,
            "capability": inference_cap,
            "sample_rate": sr,
            "duration_secs": round(duration, 3),
            "language": language,
            "model_type": _qwen_model_type(_model) if _engine == "qwen3-tts" else "edge-tts",
        }
        if cap == "synthesize":
            metadata["voice"] = voice
        elif cap == "custom_voice":
            metadata["speaker"] = params["__speaker"]
        if fallback_note:
            metadata["note"] = fallback_note
        return {
            "status": "completed",
            "output_type": "file",
            "result": str(output_path.resolve()),
            "output_path": str(output_path.resolve()),
            "metadata": metadata,
            "elapsed_seconds": elapsed,
        }
    except Exception as exc:
        logger.exception("%s 语音合成失败", cap)
        if output_path.exists():
            try:
                output_path.unlink()
            except OSError:
                pass
        return _error(500, "INFERENCE_ERROR", f"{cap} 语音合成失败: {exc}")


def _make_predict_endpoint(cap: str):
    async def _predict_endpoint(
        request: Request,
        file: Optional[UploadFile] = File(None),
        params_form: Optional[str] = Form(None, alias="params"),
    ):
        return await _predict(request, file, params_form, cap)

    _predict_endpoint.__name__ = f"predict_{cap}"
    return _predict_endpoint


app.post("/predict/synthesize")(_make_predict_endpoint("synthesize"))
app.post("/predict/clone_voice")(_make_predict_endpoint("clone_voice"))
app.post("/predict/custom_voice")(_make_predict_endpoint("custom_voice"))


@app.get("/health")
async def health():
    return {
        "status": "ok" if _engine != "none" else "degraded",
        "engine": _engine,
        "model_id": MODEL_ID,
        "model_type": _qwen_model_type(_model) if _engine == "qwen3-tts" else None,
        "device": DEVICE,
        "backend": BACKEND,
    }


@app.get("/info")
async def info():
    return {
        "module_id": "qwen3-tts",
        "name": "Qwen3-TTS 语音合成",
        "version": "1.1.0",
        "engine": _engine,
        "model_id": MODEL_ID,
        "model_dir": str(MODEL_DIR),
        "models_root": str(MODELS_ROOT or MODEL_DIR),
        "device": DEVICE,
        "backend": BACKEND,
        "workspace": str(WORKSPACE),
        "capabilities": ["synthesize", "clone_voice", "custom_voice"],
        "languages": ["auto", *_SUPPORTED_LANGUAGES],
        "speakers": list(_SPEAKERS),
        "load_error": _load_error,
    }


# ---------------------------------------------------------------------------
# 自检
# ---------------------------------------------------------------------------
def _selftest() -> int:
    """模块内自测（纯标准库，无需加载模型/重依赖）：
      1) 语言枚举归一（12 值 + 非法值）；
      2) 音色规则映射；
      3) 模型目录解析（EP_MODEL_ID → target_dir / config.json 直指）。
    用法：python adapter.py --selftest
    """
    failures: list[str] = []

    if _normalize_language("auto") != "Auto":
        failures.append("language auto → Auto")
    if _normalize_language("Chinese") != "Chinese":
        failures.append("language Chinese")
    if _normalize_language("japanese") != "Japanese":
        failures.append("language japanese")
    for lang in _SUPPORTED_LANGUAGES:
        if _normalize_language(lang).lower() != lang:
            failures.append(f"language {lang}")
    try:
        _normalize_language("klingon")
        failures.append("language klingon 应报错")
    except ValueError:
        pass

    if _map_speaker("") != "Vivian":
        failures.append("speaker default")
    if _validate_speaker("sohee") != "Sohee":
        failures.append("speaker Sohee")
    try:
        _validate_speaker("klingon")
        failures.append("speaker klingon 应报错")
    except ValueError:
        pass

    active = _resolve_active_model_path()
    print(f"[selftest] EP_MODEL_DIR={MODEL_DIR}")
    print(f"[selftest] EP_MODELS_ROOT={MODELS_ROOT}")
    print(f"[selftest] 激活模型目录: {active}")
    print(f"[selftest] capabilities -> variant dirs:")
    for cap, dirs in _CAP_VARIANT_DIRS.items():
        found = _resolve_capability_dir(cap)
        print(f"  {cap}: {'发现 ' + str(found) if found else '未落盘 (' + ', '.join(dirs) + ')'}")

    if failures:
        print(f"[selftest] FAIL: {failures}")
        return 1
    print("[selftest] OK")
    return 0


# ---------------------------------------------------------------------------
# 启动
# ---------------------------------------------------------------------------
@app.on_event("startup")
async def on_startup():
    logger.info(
        "Qwen3-TTS adapter 启动 | MODEL_DIR=%s | MODELS_ROOT=%s | WORKSPACE=%s | "
        "DEVICE=%s | BACKEND=%s | MODEL_ID=%s",
        MODEL_DIR, MODELS_ROOT or "-", WORKSPACE, DEVICE, BACKEND, MODEL_ID,
    )
    _init_engine()


if __name__ == "__main__":
    if "--selftest" in sys.argv:
        sys.exit(_selftest())
    uvicorn.run(
        app,
        # 绑定地址读 EP_HOST（daemon 注入，缺省回环）——硬编码 0.0.0.0 会触发
        # Windows 防火墙弹窗，见 ep-core process.rs build_module_env（EP_HOST=127.0.0.1）
        host=os.getenv("EP_HOST", "127.0.0.1"),
        port=PORT,
        log_level="info",
    )
