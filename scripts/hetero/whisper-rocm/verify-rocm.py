#!/usr/bin/env python3
"""verify-rocm.py — E1 验证脚本：faster-whisper × CTranslate2-HIP wheel × AMD GPU（gfx1100）

行为：
  1) 运行时诊断：ctranslate2 版本、CUDA(=HIP) 设备数、cuda 支持的算精度；
     HIP 轮子加载失败时给出结构化诊断（缺 libamdhip64 / libomp 等）
  2) 全链路（默认）：加载 faster-whisper large-v3（device="cuda"，HIP 透明映射），
     用 numpy 合成一段变频正弦 wav（无外部素材）转写，打印 segments 数与首段文本

用法：
  verify-rocm.py [--dry] [MODEL_DIR]
  MODEL_DIR 缺省顺序：argv > 环境变量 EP_MODEL_DIR > <repo>/models/faster-whisper-large-v3

退出码：0 成功；2 模型目录无效；3 ctranslate2(HIP) 不可用；5 推理失败
调研依据：scripts/hetero/whisper-rocm/README.md
"""

import argparse
import os
import sys
import tempfile
import time
import wave
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent          # scripts/hetero/whisper-rocm
ROOT_DIR = SCRIPT_DIR.parents[2]                      # 仓库根
DEFAULT_MODEL_DIR = ROOT_DIR / "models" / "faster-whisper-large-v3"
ROCM_PATH = Path(os.environ.get("ROCM_PATH", "/opt/rocm"))


def ensure_rocm_loader_paths() -> None:
    """HIP 库可能只在 $ROCM_PATH/lib 且未进 ldconfig；LD_LIBRARY_PATH 必须在进程启动前生效 → re-exec。"""
    if sys.platform == "win32" or os.environ.get("_EP_VERIFY_ROCM_REEXEC") == "1":
        return
    candidates = [str(ROCM_PATH / "lib"), str(ROCM_PATH / "lib" / "llvm" / "lib")]
    current = os.environ.get("LD_LIBRARY_PATH", "")
    existing = current.split(os.pathsep) if current else []
    add = [p for p in candidates if os.path.isdir(p) and p not in existing]
    if not add:
        return
    env = dict(os.environ)
    env["LD_LIBRARY_PATH"] = os.pathsep.join(add + existing)
    env["_EP_VERIFY_ROCM_REEXEC"] = "1"
    os.execve(sys.executable, [sys.executable] + sys.argv, env)


def diagnose_hip_runtime() -> None:
    hip_lib = ROCM_PATH / "lib"
    amdhip = sorted(hip_lib.glob("libamdhip64.so*"))
    omp = ROCM_PATH / "lib" / "llvm" / "lib" / "libomp.so"
    print(f"[diagnose] ROCM_PATH           : {ROCM_PATH}")
    print(f"[diagnose] libamdhip64.so*     : {amdhip if amdhip else 'NOT FOUND'}")
    print(f"[diagnose] llvm libomp.so      : {'found' if omp.is_file() else 'NOT FOUND'}")
    print("[diagnose] CT2 官方 ROCm wheel 需宿主机提供 HIP runtime；"
          "修复示例：sudo apt install hip-runtime-amd（详见 scripts/hetero/whisper-rocm/README.md §5）")


def resolve_model_dir(cli_value: str | None) -> Path:
    raw = cli_value or os.environ.get("EP_MODEL_DIR") or str(DEFAULT_MODEL_DIR)
    return Path(raw).expanduser().resolve()


def make_sine_wav(path: Path, sample_rate: int = 16000, seconds: float = 6.0) -> float:
    """生成变频正弦 + 能量包络的合成语音状音频，返回时长。"""
    import numpy as np

    t = np.arange(int(sample_rate * seconds), dtype=np.float64) / sample_rate
    # 220→880Hz 对数扫频，模拟音高变化；慢速能量包络制造响度起伏便于分段
    freq = 220.0 * (2.0 ** ((t % 1.5) / 1.5))
    envelope = 0.55 + 0.45 * np.sin(2 * np.pi * 0.8 * t - np.pi / 2) ** 2
    signal = (0.35 * np.sin(2 * np.pi * np.cumsum(freq) / sample_rate) * envelope).astype(np.float32)
    pcm16 = (np.clip(signal, -1.0, 1.0) * 32767.0).astype("<i2")
    with wave.open(str(path), "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(sample_rate)
        wf.writeframes(pcm16.tobytes())
    return seconds


def main() -> int:
    parser = argparse.ArgumentParser(description="E1: faster-whisper on CTranslate2-HIP smoke test")
    parser.add_argument("model_dir", nargs="?", default=None,
                        help=f"模型目录（缺省 {DEFAULT_MODEL_DIR}，或 EP_MODEL_DIR）")
    parser.add_argument("--dry", action="store_true",
                        help="仅做运行时诊断（ct2 版本/设备/算精度），不加载模型不推理")
    args = parser.parse_args()

    ensure_rocm_loader_paths()

    try:
        import ctranslate2 as ct2
    except Exception as exc:
        print(f"[FAIL] cannot import ctranslate2 (HIP wheel unusable): {exc}")
        diagnose_hip_runtime()
        return 3

    print("[runtime] ctranslate2 version :", getattr(ct2, "__version__", "unknown"))
    device_count = ct2.get_cuda_device_count()
    print("[runtime] cuda(HIP) devices   :", device_count)
    try:
        print("[runtime] compute types(cuda) :", ct2.get_supported_compute_types("cuda"))
    except Exception as exc:
        print("[runtime] compute types(cuda) : unavailable ->", exc)

    if args.dry:
        print("[dry] runtime diagnostics done; skip model load & inference")
        return 0

    if device_count < 1:
        print("[FAIL] no CUDA(HIP) device visible to ctranslate2; check driver/HIP runtime")
        diagnose_hip_runtime()
        return 3

    model_dir = resolve_model_dir(args.model_dir)
    if not (model_dir / "model.bin").is_file():
        print(f"[FAIL] not a valid faster-whisper model dir (model.bin missing): {model_dir}")
        return 4
    print("[model] loading from          :", model_dir)

    from faster_whisper import WhisperModel

    t0 = time.perf_counter()
    model = WhisperModel(str(model_dir), device="cuda", compute_type="float16")
    print(f"[model] loaded in             : {time.perf_counter() - t0:.1f}s")

    inner = getattr(model, "model", None)
    reported_device = None
    for attr in ("device", "device_index"):
        val = getattr(inner, attr, None)
        if val is not None:
            reported_device = f"{attr}={val}"
            break
    print("[device] requested            : cuda (HIP transparent mapping)")
    print("[device] ct2-reported         :", reported_device or "n/a (API 未暴露；以推理成功为准)")

    with tempfile.TemporaryDirectory(prefix="ws-b-rocm-") as tmp:
        wav_path = Path(tmp) / "synthetic.wav"
        dur = make_sine_wav(wav_path)
        t0 = time.perf_counter()
        try:
            segments_iter, info = model.transcribe(
                str(wav_path), beam_size=5, vad_filter=False,
            )
            segments = list(segments_iter)
        except Exception as exc:
            print(f"[FAIL] transcription failed: {exc}")
            return 5
        elapsed = time.perf_counter() - t0

    first_text = segments[0].text.strip() if segments else "<no segments>"
    print(f"[result] audio duration       : {dur:.1f}s | language detected: {info.language}")
    print(f"[result] segments             : {len(segments)}")
    print(f"[result] first segment text   : {first_text!r}")
    print(f"[result] transcribe wall time : {elapsed:.2f}s "
          f"({dur / elapsed if elapsed else float('inf'):.1f}x realtime)")
    print("[PASS] full pipeline OK on HIP path")
    return 0


if __name__ == "__main__":
    sys.exit(main())
