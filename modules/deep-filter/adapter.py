"""
DeepFilter 音频降噪模块 — EntryPoint adapter
基于 DeepFilterNet 的深度学习语音增强/降噪，scipy 频谱减法作为 fallback。
"""

from __future__ import annotations

import logging
import os
import shutil
import tempfile
import time
import uuid
from pathlib import Path
from typing import Optional

import numpy as np
import soundfile as sf
import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import JSONResponse
from pydantic import BaseModel

# ---------------------------------------------------------------------------
# 环境变量
# ---------------------------------------------------------------------------
EP_PORT = int(os.environ.get("EP_PORT", "8900"))
EP_MODEL_DIR = os.environ.get("EP_MODEL_DIR", ".")
# EP_DEVICE 形如 "cuda:0"/"cpu"（ep-core process.rs build_module_env）；
# 设备序号单独走 EP_DEVICE_INDEX（CPU 设备时为空串）。
EP_DEVICE = os.environ.get("EP_DEVICE", "0")
EP_DEVICE_INDEX = os.environ.get("EP_DEVICE_INDEX", "")
EP_BACKEND = os.environ.get("EP_BACKEND", "cpu")
EP_WORKSPACE = os.environ.get("EP_WORKSPACE", "")
EP_MODULE_ID = os.environ.get("EP_MODULE_ID", "deep-filter")

logging.basicConfig(
    level=logging.INFO,
    format=f"[{EP_MODULE_ID}] %(asctime)s %(levelname)s %(message)s",
)
log = logging.getLogger(EP_MODULE_ID)

# ---------------------------------------------------------------------------
# 模型状态
# ---------------------------------------------------------------------------
_model = None
_df_state = None
_sr: int = 48000
_backend_used: str = "none"  # "deepfilternet" | "scipy" | "none"
_model_load_error: Optional[str] = None


def _load_model() -> None:
    """尝试加载 DeepFilterNet 模型；失败则标记 scipy fallback。"""
    global _model, _df_state, _sr, _backend_used, _model_load_error

    # 如果指定了 cuda 后端，尝试设置 torch 设备
    if EP_BACKEND == "cuda":
        try:
            import torch
            if torch.cuda.is_available():
                # 旧实现 int(EP_DEVICE) 对 "cuda:0" 恒抛 ValueError → 静默回退 CPU。
                # 设备序号改读 EP_DEVICE_INDEX（daemon 注入；缺省/空串按 0 处理）。
                device_index = int(EP_DEVICE_INDEX or 0)
                torch.cuda.set_device(device_index)
                log.info(
                    "CUDA 设备 %d (%s) 可用", device_index, torch.cuda.get_device_name()
                )
            else:
                log.warning("CUDA 不可用，回退到 CPU")
        except Exception as exc:
            log.warning("CUDA 初始化失败: %s，回退到 CPU", exc)

    try:
        # deepfilterlib 0.5.6 的 df/io.py 硬导入 torchaudio.backend.common.AudioMetaData，
        # 该路径在新版 torchaudio（≥2.6）已移除；注入同名 shim 模块后再导入 df，
        # 避免 ModuleNotFoundError（adapter 自身 I/O 走 soundfile，shim 仅满足导入链）。
        import sys as _sys
        import types as _types
        try:
            from torchaudio.backend.common import AudioMetaData  # noqa: F401
        except ImportError:
            import torchaudio as _ta
            _backend = _types.ModuleType("torchaudio.backend")
            _common = _types.ModuleType("torchaudio.backend.common")
            _common.AudioMetaData = getattr(_ta, "AudioMetaData", type(
                "AudioMetaData", (), {}
            ))
            _backend.common = _common
            _sys.modules.setdefault("torchaudio.backend", _backend)
            _sys.modules["torchaudio.backend.common"] = _common

        from df.enhance import init_df

        model_path = None
        # 检查 EP_MODEL_DIR 下是否有解压后的模型
        model_dir = Path(EP_MODEL_DIR)

        # daemon 布局：EP_MODEL_DIR 直指模型目录（ep-core build_module_env
        # MODEL_DIR = models/<target_dir>，即 models/deep-filter-df3）。
        # tar.gz 解压后可能带嵌套前缀（如 tmp/export/），rglob 定位到含
        # enc.onnx 的真实目录。
        if (model_dir / "enc.onnx").exists():
            candidate = model_dir
        else:
            hits = sorted(model_dir.rglob("enc.onnx"))
            candidate = hits[0].parent if hits else None

        if candidate is None:
            # 兼容旧布局：EP_MODEL_DIR 为 models 根，模型在 <root>/deep-filter-df3 子目录
            legacy = model_dir / "deep-filter-df3"
            if legacy.is_dir():
                if (legacy / "enc.onnx").exists():
                    candidate = legacy
                else:
                    hits = sorted(legacy.rglob("enc.onnx"))
                    candidate = hits[0].parent if hits else None

        if candidate is not None:
            # DeepFilterNet init_df 接受模型目录或 .tar.gz 路径
            model_path = str(candidate)
            log.info("使用本地模型目录: %s", model_path)

        _model, _df_state, _suffix = init_df(model_path)
        _backend_used = "deepfilternet"
        log.info("DeepFilterNet 模型加载成功 (model=%s)", _suffix)
        return
    except Exception as exc:
        log.warning("DeepFilterNet 加载失败: %s", exc)

    # Fallback: scipy 频谱减法
    try:
        import scipy.signal  # noqa: F401
        _backend_used = "scipy"
        _model_load_error = None
        log.info("使用 scipy 频谱减法 fallback")
    except ImportError:
        _backend_used = "none"
        _model_load_error = "DeepFilterNet 和 scipy 均不可用"
        log.error(_model_load_error)


# ---------------------------------------------------------------------------
# 音频处理
# ---------------------------------------------------------------------------

def _denoise_deepfilternet(
    audio: np.ndarray, sr: int, attenuation: int, min_db: float
) -> np.ndarray:
    """使用 DeepFilterNet 进行降噪。"""
    import torch
    from df.enhance import enhance

    # DeepFilterNet 期望 48 kHz
    if sr != _sr:
        audio = _resample(audio, sr, _sr)
        sr = _sr

    # 确保是 float32, shape (channels, samples) 或 (samples,)
    audio_f32 = audio.astype(np.float32)
    if audio_f32.ndim == 1:
        tensor = torch.from_numpy(audio_f32).unsqueeze(0)  # (1, samples)
    else:
        tensor = torch.from_numpy(audio_f32.T)  # (channels, samples)

    with torch.no_grad():
        enhanced = enhance(
            _model,
            _df_state,
            tensor,
            atten_lim_db=float(attenuation),
            min_db=min_db,
        )

    result = enhanced.squeeze(0).cpu().numpy()
    return result


def _denoise_scipy(
    audio: np.ndarray, sr: int, attenuation: int, min_db: float
) -> np.ndarray:
    """简单的 scipy 频谱减法降噪 (fallback)。"""
    from scipy.signal import stft, istft

    # 将 attenuation (0-100) 映射为降噪因子
    alpha = 1.0 + (attenuation / 100.0) * 4.0  # 1.0 ~ 5.0
    # min_db 映射为频谱下限
    floor = 10.0 ** (min_db / 20.0)

    if audio.ndim == 1:
        channels = [audio]
    else:
        channels = [audio[:, ch] for ch in range(audio.shape[1])]

    processed = []
    for ch_audio in channels:
        nperseg = min(2048, len(ch_audio))
        if nperseg < 64:
            processed.append(ch_audio)
            continue

        f, t, Zxx = stft(ch_audio, fs=sr, nperseg=nperseg, noverlap=nperseg // 2)
        magnitude = np.abs(Zxx)
        phase = np.angle(Zxx)

        # 估计噪声：取前 0.5 秒（或前 10 帧）的均值
        noise_frames = max(1, min(10, int(0.5 * sr / (nperseg // 2))))
        noise_mag = np.mean(magnitude[:, :noise_frames], axis=1, keepdims=True)

        # 频谱减法
        enhanced_mag = magnitude - alpha * noise_mag
        enhanced_mag = np.maximum(enhanced_mag, floor * magnitude)

        Zxx_enhanced = enhanced_mag * np.exp(1j * phase)
        _, result = istft(Zxx_enhanced, fs=sr, nperseg=nperseg, noverlap=nperseg // 2)

        # istft 输出长度可能与输入略有差异，对齐
        if len(result) > len(ch_audio):
            result = result[: len(ch_audio)]
        elif len(result) < len(ch_audio):
            result = np.pad(result, (0, len(ch_audio) - len(result)))

        processed.append(result)

    if audio.ndim == 1:
        return processed[0].astype(np.float32)
    return np.stack(processed, axis=1).astype(np.float32)


def _resample(audio: np.ndarray, orig_sr: int, target_sr: int) -> np.ndarray:
    """简单重采样。"""
    if orig_sr == target_sr:
        return audio
    try:
        from scipy.signal import resample_poly
        from math import gcd

        g = gcd(orig_sr, target_sr)
        up = target_sr // g
        down = orig_sr // g
        if audio.ndim == 1:
            return resample_poly(audio, up, down).astype(np.float32)
        return np.stack(
            [resample_poly(audio[:, ch], up, down) for ch in range(audio.shape[1])],
            axis=1,
        ).astype(np.float32)
    except ImportError:
        # 最简线性插值 fallback
        ratio = target_sr / orig_sr
        n_out = int(len(audio) * ratio)
        indices = np.linspace(0, len(audio) - 1, n_out)
        if audio.ndim == 1:
            return np.interp(indices, np.arange(len(audio)), audio).astype(np.float32)
        return np.stack(
            [
                np.interp(indices, np.arange(len(audio)), audio[:, ch])
                for ch in range(audio.shape[1])
            ],
            axis=1,
        ).astype(np.float32)


def _get_output_dir() -> Path:
    """获取输出目录。"""
    if EP_WORKSPACE:
        out = Path(EP_WORKSPACE)
        out.mkdir(parents=True, exist_ok=True)
        return out
    return Path(tempfile.gettempdir())


# ---------------------------------------------------------------------------
# FastAPI 应用
# ---------------------------------------------------------------------------
app = FastAPI(title=f"EntryPoint Module: {EP_MODULE_ID}", version="0.5.6")


class DenoiseRequest(BaseModel):
    input_path: str
    attenuation: int = 100
    min_db: float = -60.0


class DenoiseResponse(BaseModel):
    status: str
    output_path: str
    backend: str
    sample_rate: int
    duration_secs: float


@app.on_event("startup")
async def startup_event():
    log.info("正在加载模型 (backend=%s, device=%s) ...", EP_BACKEND, EP_DEVICE)
    _load_model()
    if _backend_used == "none":
        log.error("无可用降噪后端！服务将以降级模式运行。")
    else:
        log.info("模块就绪，后端: %s", _backend_used)


@app.get("/health")
async def health():
    if _backend_used == "none":
        return JSONResponse(
            status_code=503,
            content={
                "status": "not_ready",
                "error": _model_load_error or "模型未加载",
            },
        )
    return {"status": "ok", "backend": _backend_used}


@app.get("/info")
async def info():
    return {
        "module_id": EP_MODULE_ID,
        "name": "DeepFilter 音频降噪",
        "version": "0.5.6",
        "backend": _backend_used,
        "sample_rate": _sr,
        "device": EP_DEVICE,
        "compute_backend": EP_BACKEND,
        "capabilities": ["denoise"],
    }


@app.post("/predict/denoise", response_model=DenoiseResponse)
async def predict_denoise(
    file: Optional[UploadFile] = File(None),
    input_path: Optional[str] = Form(None),
    attenuation: int = Form(100),
    min_db: float = Form(-60.0),
):
    """
    音频降噪。支持两种输入：
    1. multipart file 上传
    2. input_path 指定本地文件路径
    """
    if _backend_used == "none":
        raise HTTPException(status_code=503, detail="降噪后端不可用")

    # 参数校验
    attenuation = max(0, min(100, attenuation))
    min_db = max(-100.0, min(0.0, min_db))

    tmp_input: Optional[Path] = None
    try:
        # 确定输入文件
        if file is not None:
            # multipart 上传
            suffix = Path(file.filename or "audio.wav").suffix or ".wav"
            tmp_input = Path(tempfile.mkdtemp()) / f"upload_{uuid.uuid4().hex[:8]}{suffix}"
            content = await file.read()
            tmp_input.write_bytes(content)
            src_path = tmp_input
        elif input_path:
            src_path = Path(input_path)
            if not src_path.is_file():
                raise HTTPException(status_code=400, detail=f"文件不存在: {input_path}")
        else:
            raise HTTPException(
                status_code=400,
                detail="请提供 file (multipart) 或 input_path 参数",
            )

        # 读取音频
        try:
            audio, sr = sf.read(str(src_path), dtype="float32")
        except Exception as exc:
            raise HTTPException(
                status_code=400, detail=f"无法读取音频文件: {exc}"
            )

        duration = len(audio) / sr if audio.ndim == 1 else len(audio[:, 0]) / sr

        # 降噪
        t0 = time.time()
        if _backend_used == "deepfilternet":
            enhanced = _denoise_deepfilternet(audio, sr, attenuation, min_db)
        else:
            enhanced = _denoise_scipy(audio, sr, attenuation, min_db)
        elapsed = time.time() - t0
        log.info("降噪完成: %.2fs 音频, 耗时 %.2fs", duration, elapsed)

        # 写入输出
        out_dir = _get_output_dir()
        out_name = f"denoised_{uuid.uuid4().hex[:8]}.wav"
        out_path = out_dir / out_name
        sf.write(str(out_path), enhanced, sr if _backend_used == "scipy" else _sr)

        return DenoiseResponse(
            status="ok",
            output_path=str(out_path),
            backend=_backend_used,
            sample_rate=sr if _backend_used == "scipy" else _sr,
            duration_secs=round(duration, 3),
        )

    except HTTPException:
        raise
    except Exception as exc:
        log.exception("降噪处理失败")
        raise HTTPException(status_code=500, detail=f"降噪处理失败: {exc}")
    finally:
        # 清理临时上传文件
        if tmp_input and tmp_input.exists():
            try:
                shutil.rmtree(tmp_input.parent, ignore_errors=True)
            except Exception:
                pass


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    log.info("启动 %s 服务, 端口 %d", EP_MODULE_ID, EP_PORT)
    # 绑定地址读 EP_HOST（daemon 注入，缺省回环）——硬编码 0.0.0.0 会触发
    # Windows 防火墙弹窗，见 ep-core process.rs build_module_env（EP_HOST=127.0.0.1）
    uvicorn.run(app, host=os.getenv("EP_HOST", "127.0.0.1"), port=EP_PORT, log_level="info")
