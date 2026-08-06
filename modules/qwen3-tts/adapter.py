"""
Qwen3-TTS 语音合成模块 — EntryPoint adapter
=============================================

提供 HTTP API 将文本合成为 WAV 音频文件。
优先使用 Qwen3-TTS 本地模型；模型不可用时自动降级为 edge-tts。
"""

from __future__ import annotations

import asyncio
import logging
import os
import time
import uuid
import wave
from pathlib import Path
from typing import Any, Optional

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

# ---------------------------------------------------------------------------
# 环境变量
# ---------------------------------------------------------------------------
MODULE_DIR = Path(os.environ.get("EP_MODULE_DIR", Path(__file__).resolve().parent))
MODEL_DIR = Path(os.environ.get("EP_MODEL_DIR", MODULE_DIR / "models"))
WORKSPACE = Path(os.environ.get("EP_WORKSPACE", "."))
PORT = int(os.environ.get("EP_PORT", "8000"))
DEVICE = os.environ.get("EP_DEVICE", "cuda").lower()
MODEL_ID = os.environ.get("EP_MODEL_ID", "1.7b")

# 模型 ID → 子目录名映射
MODEL_DIR_MAP: dict[str, str] = {
    "1.7b": "qwen3-tts-1.7b",
    "0.6b": "qwen3-tts-0.6b",
}

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("qwen3-tts")

# ---------------------------------------------------------------------------
# 全局状态
# ---------------------------------------------------------------------------
_model: Any = None
_tokenizer: Any = None
_engine: str = "none"  # "qwen3-tts" | "edge-tts" | "none"
_load_error: Optional[str] = None


# ---------------------------------------------------------------------------
# 模型加载
# ---------------------------------------------------------------------------
def _resolve_model_path() -> Optional[Path]:
    """根据 EP_MODEL_ID 定位模型目录。"""
    subdir = MODEL_DIR_MAP.get(MODEL_ID)
    if subdir:
        candidate = MODEL_DIR / subdir
        if candidate.is_dir():
            return candidate
    # 回退：扫描 MODEL_DIR 下任何包含 config.json 的子目录
    if MODEL_DIR.is_dir():
        for child in sorted(MODEL_DIR.iterdir()):
            if child.is_dir() and (child / "config.json").exists():
                return child
    return None


def _load_qwen3_tts() -> bool:
    """尝试加载 Qwen3-TTS 模型，成功返回 True。"""
    global _model, _tokenizer, _engine, _load_error

    model_path = _resolve_model_path()
    if model_path is None:
        _load_error = f"模型目录未找到 (MODEL_DIR={MODEL_DIR}, MODEL_ID={MODEL_ID})"
        logger.warning("Qwen3-TTS 模型不可用: %s", _load_error)
        return False

    try:
        import torch
        from transformers import AutoModel, AutoTokenizer

        device_map = "auto" if DEVICE == "cuda" and torch.cuda.is_available() else "cpu"
        dtype = torch.float16 if device_map != "cpu" else torch.float32

        logger.info("正在加载 Qwen3-TTS 模型: %s (device=%s, dtype=%s)", model_path, device_map, dtype)
        _tokenizer = AutoTokenizer.from_pretrained(str(model_path), trust_remote_code=True)
        _model = AutoModel.from_pretrained(
            str(model_path),
            trust_remote_code=True,
            torch_dtype=dtype,
            device_map=device_map,
        )
        _model.eval()
        _engine = "qwen3-tts"
        logger.info("Qwen3-TTS 模型加载完成")
        return True
    except Exception as exc:
        _load_error = f"Qwen3-TTS 加载失败: {exc}"
        logger.warning("%s", _load_error)
        _model = None
        _tokenizer = None
        return False


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

    if _load_qwen3_tts():
        return

    if _check_edge_tts():
        _engine = "edge-tts"
        logger.info("使用 edge-tts 作为 fallback 引擎")
        return

    _engine = "none"
    _load_error = "Qwen3-TTS 和 edge-tts 均不可用"
    logger.error("%s", _load_error)


# ---------------------------------------------------------------------------
# 音频合成
# ---------------------------------------------------------------------------
def _synthesize_qwen3(
    text: str,
    voice: str,
    speed: float,
    sample_rate: int,
    output_path: Path,
) -> float:
    """使用 Qwen3-TTS 模型合成语音，返回时长（秒）。"""
    import numpy as np
    import soundfile as sf

    # Qwen3-TTS 的 synthesize 接口（trust_remote_code 模型自定义方法）
    result = _model.synthesize(text, voice=voice, speed=speed)

    # 处理不同返回格式
    if isinstance(result, dict):
        audio_data = result.get("audio", result.get("waveform", result.get("output")))
        sr = result.get("sample_rate", result.get("sampling_rate", sample_rate))
    elif isinstance(result, (tuple, list)) and len(result) >= 2:
        audio_data, sr = result[0], result[1]
    else:
        audio_data = result
        sr = sample_rate

    # 转为 numpy 数组
    if not isinstance(audio_data, np.ndarray):
        try:
            import torch
            if isinstance(audio_data, torch.Tensor):
                audio_data = audio_data.detach().cpu().numpy()
            else:
                audio_data = np.array(audio_data)
        except ImportError:
            audio_data = np.array(audio_data)

    # 确保一维
    if audio_data.ndim > 1:
        audio_data = audio_data.squeeze()

    # 归一化到 [-1, 1]（如果是整数格式）
    if audio_data.dtype in (np.int16, np.int32):
        max_val = np.iinfo(audio_data.dtype).max
        audio_data = audio_data.astype(np.float32) / max_val

    sf.write(str(output_path), audio_data, sr)
    duration = len(audio_data) / sr
    return duration


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

        duration = _get_wav_duration(output_path)
        return duration
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
    description="基于 Qwen3 的高质量多语言语音合成 — EntryPoint 模块",
    version="1.0.0",
)


class SynthesizeRequest(BaseModel):
    input_text: str = Field(..., min_length=1, max_length=5000, description="待合成文本")
    params: dict[str, Any] = Field(default_factory=dict, description="合成参数")


class SynthesizeResponse(BaseModel):
    status: str
    output_path: str
    sample_rate: int
    duration_secs: float
    engine: str


@app.get("/health")
async def health():
    return {
        "status": "ok" if _engine != "none" else "degraded",
        "engine": _engine,
        "model_id": MODEL_ID,
        "device": DEVICE,
    }


@app.get("/info")
async def info():
    return {
        "module_id": "qwen3-tts",
        "name": "Qwen3-TTS 语音合成",
        "version": "1.0.0",
        "engine": _engine,
        "model_id": MODEL_ID,
        "model_dir": str(MODEL_DIR),
        "device": DEVICE,
        "workspace": str(WORKSPACE),
        "capabilities": ["synthesize"],
        "load_error": _load_error,
    }


@app.post("/predict/synthesize", response_model=SynthesizeResponse)
async def predict_synthesize(req: SynthesizeRequest):
    if _engine == "none":
        raise HTTPException(
            status_code=503,
            detail=f"无可用合成引擎: {_load_error or '未知错误'}",
        )

    text = req.input_text.strip()
    if not text:
        raise HTTPException(status_code=400, detail="input_text 不能为空")

    # 解析参数
    params = req.params or {}
    voice: str = str(params.get("voice", "default"))
    speed: float = float(params.get("speed", 1.0))
    sample_rate: int = int(params.get("sample_rate", 24000))

    # 参数校验
    speed = max(0.5, min(2.0, speed))
    if sample_rate not in (8000, 16000, 22050, 24000, 44100, 48000):
        sample_rate = 24000

    # 准备输出路径
    WORKSPACE.mkdir(parents=True, exist_ok=True)
    filename = f"tts_{int(time.time())}_{uuid.uuid4().hex[:8]}.wav"
    output_path = WORKSPACE / filename

    try:
        if _engine == "qwen3-tts":
            duration = await asyncio.get_event_loop().run_in_executor(
                None,
                _synthesize_qwen3,
                text, voice, speed, sample_rate, output_path,
            )
        else:
            duration = await _synthesize_edge_tts(text, voice, sample_rate, output_path)

        if not output_path.exists():
            raise RuntimeError("合成完成但输出文件不存在")

        return SynthesizeResponse(
            status="ok",
            output_path=str(output_path.resolve()),
            sample_rate=sample_rate,
            duration_secs=round(duration, 3),
            engine=_engine,
        )
    except HTTPException:
        raise
    except Exception as exc:
        logger.exception("语音合成失败")
        # 清理可能的残留文件
        if output_path.exists():
            try:
                output_path.unlink()
            except OSError:
                pass
        raise HTTPException(status_code=500, detail=f"语音合成失败: {exc}") from exc


# ---------------------------------------------------------------------------
# 启动
# ---------------------------------------------------------------------------
@app.on_event("startup")
async def on_startup():
    logger.info(
        "Qwen3-TTS adapter 启动 | MODEL_DIR=%s | WORKSPACE=%s | DEVICE=%s | MODEL_ID=%s",
        MODEL_DIR, WORKSPACE, DEVICE, MODEL_ID,
    )
    _init_engine()


if __name__ == "__main__":
    uvicorn.run(
        app,
        # 绑定地址读 EP_HOST（daemon 注入，缺省回环）——硬编码 0.0.0.0 会触发
        # Windows 防火墙弹窗，见 ep-core process.rs build_module_env（EP_HOST=127.0.0.1）
        host=os.getenv("EP_HOST", "127.0.0.1"),
        port=PORT,
        log_level="info",
    )
