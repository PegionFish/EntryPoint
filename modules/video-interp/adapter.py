"""adapter.py — EntryPoint video-interp 模块适配器（W1 脚手架，实验 E6/E7/E8 载体）

视频插帧统一 REST 服务：ffmpeg 抽帧 → 按 EP_BACKEND 分派 RIFE 引擎生成中间帧 →
原帧/中间帧交错重组 → 按新帧率回封 mp4（音频流 copy）。

运行时分层（HETERO_DIST_PLAN §3.1，厂商栈优先）：
  cuda / rocm / cpu : torch + flownet.pkl（懒导入守卫；需 vendor Practical-RIFE
                      推理模块，就绪前返回 501 EXPERIMENTAL）
  openvino          : onnxruntime OpenVINO EP；社区 ONNX 无权威源、manifest 占位，
                      权重落位前恒返回 501 EXPERIMENTAL
  vulkan            : nihui rife-ncnn-vulkan 子进程（bin/<os>-<arch>/ 由用户放置）

注意：rife-ncnn-vulkan 目录模式只输出「相邻两帧之间的中间帧」（N 帧入 → N-1 帧
出），交错重组由本 adapter 完成；倍数 >2 时按 2 的幂逐 pass 递归，非 2 的幂再均匀
抽帧对齐目标帧数。

产物协议：MODULE_SPEC §5 —— output_type="file"，result=输出绝对路径。
"""

from __future__ import annotations

import json
import logging
import math
import os
import platform
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Optional

import uvicorn
from fastapi import FastAPI, File, Form, Request, UploadFile
from fastapi.responses import JSONResponse

# ---------------------------------------------------------------------------
# 环境变量（MODULE_SPEC §4 契约）
# ---------------------------------------------------------------------------
EP_HOST = os.getenv("EP_HOST", "127.0.0.1")
EP_PORT = int(os.getenv("EP_PORT", "8921"))
EP_WORKSPACE = os.getenv("EP_WORKSPACE", os.path.join(os.getcwd(), "workspace"))
EP_MODEL_DIR = os.getenv("EP_MODEL_DIR", "")
EP_MODELS_ROOT = os.getenv("EP_MODELS_ROOT", "")
EP_MODEL_ID = os.getenv("EP_MODEL_ID", "")
EP_BACKEND = os.getenv("EP_BACKEND", "cuda").strip().lower()
EP_DEVICE = os.getenv("EP_DEVICE", "cuda:0")
EP_DEVICE_INDEX = os.getenv("EP_DEVICE_INDEX", "0")
EP_MODULE_ID = os.getenv("EP_MODULE_ID", "video-interp")
EP_LOG_LEVEL = os.getenv("EP_LOG_LEVEL", "INFO")

MODULE_DIR = Path(os.getenv("EP_MODULE_DIR", Path(__file__).resolve().parent))

PLATFORM_KEY = {
    ("windows", "amd64"): "windows-x86_64",
    ("linux", "x86_64"): "linux-x86_64",
}.get((platform.system().lower(), platform.machine().lower()), "linux-x86_64")
NCNN_BINARY_NAMES = {
    "windows-x86_64": "rife-ncnn-vulkan.exe",
    "linux-x86_64": "rife-ncnn-vulkan",
}
MODEL_TARGET_DIRS = {
    "rife-v4.6-ncnn": "video-interp-rife-ncnn",
    "rife-v4.26-pkl": "video-interp-rife-v426-pkl",
    "rife-v4.25-lite-pkl": "video-interp-rife-v425lite-pkl",
}
# nihui 整包内可选模型目录（params.model_name 选择，无需重复下载）
NCNN_MODEL_SUBDIRS = ["rife-v4.6", "rife-v4", "rife-anime", "rife-UHD", "rife-HD",
                      "rife-v4.4", "rife-v2.4", "rife-v2.3"]

logging.basicConfig(
    level=getattr(logging, EP_LOG_LEVEL.upper(), logging.INFO),
    format="%(asctime)s [video-interp] %(levelname)s %(message)s",
)
logger = logging.getLogger("video-interp")

app = FastAPI(title="EntryPoint video-interp adapter", version="0.1.0")


class ExperimentalError(RuntimeError):
    """分支结构就绪但依赖物缺失：按契约返回 501 EXPERIMENTAL。"""

    def __init__(self, reason: str):
        super().__init__(reason)
        self.reason = reason


# ---------------------------------------------------------------------------
# 通用工具（与 video-upscale 同构）
# ---------------------------------------------------------------------------
def _error(status_code: int, error_code: str, message: str, detail: Optional[str] = None):
    return JSONResponse(
        status_code=status_code,
        content={
            "status": "error",
            "error_code": error_code,
            "message": message,
            "detail": detail,
        },
    )


def _run(cmd: list[str], timeout: Optional[int] = None) -> str:
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if proc.returncode != 0:
        tail = (proc.stderr or proc.stdout or "")[-1500:]
        raise RuntimeError(f"command failed ({proc.returncode}): {' '.join(map(str, cmd))}\n{tail}")
    return proc.stdout


def _probe_video(path: Path) -> dict:
    out = _run([
        "ffprobe", "-v", "error", "-select_streams", "v:0",
        "-show_entries", "stream=r_frame_rate,width,height",
        "-of", "json", str(path),
    ])
    info = json.loads(out or "{}")
    stream = (info.get("streams") or [{}])[0]
    num, _, den = (stream.get("r_frame_rate") or "25/1").partition("/")
    try:
        fps = float(num) / float(den or 1)
    except (ValueError, ZeroDivisionError):
        fps = 25.0
    has_audio = _run([
        "ffprobe", "-v", "error", "-select_streams", "a",
        "-show_entries", "stream=index", "-of", "csv=p=0", str(path),
    ]).strip() != ""
    return {"fps": fps, "width": int(stream.get("width") or 0),
            "height": int(stream.get("height") or 0), "has_audio": has_audio}


def _extract_frames(video: Path, frames_dir: Path) -> int:
    frames_dir.mkdir(parents=True, exist_ok=True)
    _run([
        "ffmpeg", "-hide_banner", "-y", "-i", str(video),
        "-vsync", "0", "-start_number", "0",
        str(frames_dir / "%08d.png"),
    ])
    count = len(list(frames_dir.glob("*.png")))
    if count == 0:
        raise RuntimeError("ffmpeg produced no frames")
    return count


def _mux_video(frames_dir: Path, fps: float, audio_src: Optional[Path], out_path: Path) -> None:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    cmd = [
        "ffmpeg", "-hide_banner", "-y",
        "-framerate", f"{fps:.6f}",
        "-i", str(frames_dir / "%08d.png"),
    ]
    if audio_src is not None:
        cmd += ["-i", str(audio_src), "-map", "0:v:0", "-map", "1:a:0?", "-c:a", "copy"]
    cmd += ["-c:v", "libx264", "-crf", "17", "-preset", "medium", "-pix_fmt", "yuv420p", str(out_path)]
    _run(cmd)


def resolve_model_dir(model_id: str) -> tuple[Path, str]:
    candidates: list[tuple[Path, str]] = []
    target = MODEL_TARGET_DIRS.get(model_id)
    if target and EP_MODELS_ROOT:
        candidates.append((Path(EP_MODELS_ROOT) / target, "variant override via EP_MODELS_ROOT"))
    if EP_MODEL_DIR:
        candidates.append((Path(EP_MODEL_DIR), "active variant EP_MODEL_DIR"))
    for path, how in candidates:
        if path.is_dir():
            return path, how
    raise ExperimentalError(
        f"model '{model_id}' not present locally; expected one of: "
        + ", ".join(str(p) for p, _ in candidates)
        + ". Download it via the platform model manager and extract the zip manually."
    )


def find_ncnn_model_dir(model_dir: Path, model_name: str) -> Optional[Path]:
    """定位含 rife param+bin 的模型目录：整包解压后为 models/<name>/ 子目录。"""
    wanted = model_name.strip().lower()
    # 显式指定（params.model_name）且命中已知子目录 → 直接用
    if wanted:
        for base in (model_dir, model_dir / "models"):
            cand = base / wanted
            if cand.is_dir() and any(cand.glob("*.param")):
                return cand
    if any(model_dir.glob("*.param")):  # 目录本身即模型
        return model_dir
    for sub in sorted(model_dir.rglob("*.param")):  # 否则取第一个含 param 的目录
        return sub.parent
    return None


def resolve_ncnn_engine() -> Path:
    env_override = os.getenv("EP_NCNN_ENGINE")
    if env_override and Path(env_override).is_file():
        return Path(env_override)
    name = NCNN_BINARY_NAMES.get(PLATFORM_KEY, "rife-ncnn-vulkan")
    candidate = MODULE_DIR / "bin" / PLATFORM_KEY / name
    if candidate.is_file():
        return candidate
    raise ExperimentalError(
        f"ncnn engine binary not found at {candidate}. Fetch the official portable "
        "package from nihui/rife-ncnn-vulkan releases (see README '引擎二进制放置')."
    )


# ---------------------------------------------------------------------------
# 分支实现：vulkan → ncnn 子进程（目录模式只出中间帧 → 本地交错重组）
# ---------------------------------------------------------------------------
def interp_frames_ncnn(frames_in: Path, model_dir: Path, model_name: str,
                       passes: int) -> tuple[int, Path]:
    """执行 passes 轮 2x 插帧；返回 (最终帧数, 最终帧目录)。"""
    engine = resolve_ncnn_engine()
    ncnn_model = find_ncnn_model_dir(model_dir, model_name)
    if ncnn_model is None:
        raise ExperimentalError(
            f"no ncnn param/bin models under {model_dir}; the upstream package is a zip "
            "bundle — extract it first (models live under models/<rife-vX.Y>/)."
        )

    current, cur_count = frames_in, len(list(frames_in.glob("*.png")))
    work_root = current.parent
    for step in range(passes):
        mid_dir = work_root / f"mids_{step}"
        out_dir = work_root / f"pass_{step + 1}"
        mid_dir.mkdir(parents=True, exist_ok=True)
        cmd = [str(engine), "-i", str(current), "-o", str(mid_dir),
               "-m", str(ncnn_model)]
        logger.info("ncnn exec (pass %d/%d): %s", step + 1, passes, " ".join(cmd))
        _run(cmd)

        mids = sorted(mid_dir.glob("*.png"))
        if len(mids) != max(cur_count - 1, 0):
            raise RuntimeError(
                f"engine produced {len(mids)} intermediate frames, expected {cur_count - 1}"
            )
        # 原帧/中间帧交错：out[2i]=orig[i], out[2i+1]=mid[i]
        out_dir.mkdir(parents=True, exist_ok=True)
        origs = sorted(current.glob("*.png"))
        idx = 0
        for i, orig in enumerate(origs):
            shutil.copy(orig, out_dir / f"{idx:08d}.png")
            if i < len(mids):
                shutil.copy(mids[i], out_dir / f"{idx + 1:08d}.png")
            idx += 2
        current, cur_count = out_dir, idx

    return cur_count, current


def thin_frames(frames_dir: Path, keep: int, out_dir: Path) -> int:
    """非 2 的幂倍数时按等距抽样保留 keep 帧。"""
    frames = sorted(frames_dir.glob("*.png"))
    if len(frames) <= keep:
        return len(frames)
    out_dir.mkdir(parents=True, exist_ok=True)
    stride = (len(frames) - 1) / max(keep - 1, 1)
    picked = [frames[min(len(frames) - 1, round(i * stride))] for i in range(keep)]
    for j, f in enumerate(picked):
        shutil.copy(f, out_dir / f"{j:08d}.png")
    return keep


# ---------------------------------------------------------------------------
# 分支实现：cuda / rocm / cpu → torch（懒导入守卫）
# ---------------------------------------------------------------------------
def interp_frames_torch(frames_in: Path, frames_out: Path, weight_hint: Path,
                        passes: int) -> None:
    """torch 推理路径（E6 rocm 实验载体）。

    链路：懒导入 torch → 校验 flownet.pkl 可载 → IFNet 前向逐对合成中间帧。
    当前状态：IFNet_HDv3 网络定义与 warplayer 工具尚未 vendor 入库（Practical-RIFE
    的 rife_ifnet.py / rife_warplayer.py，MIT 允许入库，W2 补齐），因此完成权重
    加载校验后即抛 EXPERIMENTAL 占位——避免静默假装成功。
    """
    try:
        import torch
    except ImportError as exc:
        raise ExperimentalError(
            f"torch runtime not installed in this venv ({exc}); install "
            "requirements-torch.txt (CUDA/ROCm wheel per platform)"
        ) from exc

    pkl = weight_hint if weight_hint.is_file() else next(
        iter(sorted(weight_hint.rglob("flownet.pkl"))), None)
    if pkl is None:
        raise ExperimentalError(
            f"flownet.pkl not found under {weight_hint}; the upstream asset is a zip "
            "(train_log layout) — extract it first"
        )
    try:
        state = torch.load(str(pkl), map_location="cpu", weights_only=True)
    except Exception as exc:  # noqa: BLE001 — 权重损坏需如实上报
        raise ExperimentalError(f"failed to load {pkl}: {exc}") from exc
    if not isinstance(state, dict) or not state:
        raise ExperimentalError(f"unexpected checkpoint layout in {pkl}")

    raise ExperimentalError(
        "torch VFI core not vendored yet: Practical-RIFE inference modules "
        "(rife_ifnet.py / rife_warplayer.py, MIT) are required to run the loaded "
        "checkpoint; tracked for W2 before experiment E6"
    )


# ---------------------------------------------------------------------------
# 分支实现：openvino → ORT-OV
# ---------------------------------------------------------------------------
def interp_frames_openvino(frames_in: Path, frames_out: Path, weight: Path,
                           passes: int) -> None:
    """ORT-OV 路线：provider 选择同 onnx-matting 思路。

    社区 RIFE ONNX 无权威一手源（yuvraj108c/rife-onnx 等许可标注不全），manifest
    不出直链声明；权重落位前任何请求在此命中 501 占位提示。
    """
    try:
        import numpy as np
        import onnxruntime as ort
        import cv2
    except ImportError as exc:
        raise ExperimentalError(
            f"onnxruntime/cv2 missing ({exc}); install requirements-openvino.txt"
        ) from exc

    providers: list = []
    avail = ort.get_available_providers()
    if "OpenVINOExecutionProvider" in avail:
        dev = os.getenv("OPENVINO_DEVICE", "GPU")
        providers.append(("OpenVINOExecutionProvider", {"device_type": dev.split(".")[0]}))
    if "CPUExecutionProvider" in avail:
        providers.append("CPUExecutionProvider")
    session = ort.InferenceSession(str(weight), providers=providers or None)

    def infer_pair(img0, img1, timestep=0.5):
        b0 = cv2.dnn.blobFromImage(img0, 1.0 / 255.0, swapRB=True)
        b1 = cv2.dnn.blobFromImage(img1, 1.0 / 255.0, swapRB=True)
        feeds: dict = {}
        inputs = session.get_inputs()
        names = [i.name for i in inputs]
        vals = [b0, b1]
        if len(names) >= 3:  # 双输入 + timestep（契约反馈见 README）
            vals = [b0, b1, np.array([timestep], dtype="float32")]
        for n, v in zip(names, vals):
            feeds[n] = v
        out = session.run(None, feeds)[0]
        out = np.clip(out[0].transpose(1, 2, 0) * 255.0, 0, 255).astype("uint8")
        return cv2.cvtColor(out, cv2.COLOR_RGB2BGR)

    frames_out.mkdir(parents=True, exist_ok=True)
    origs = sorted(frames_in.glob("*.png"))
    imgs = [cv2.imread(str(p), cv2.IMREAD_COLOR) for p in origs]
    idx = 0
    for i, img in enumerate(imgs):
        cv2.imwrite(str(frames_out / f"{idx:08d}.png"), img)
        if i + 1 < len(imgs):
            cv2.imwrite(str(frames_out / f"{idx + 1:08d}.png"), infer_pair(img, imgs[i + 1]))
        idx += 2


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------
def run_interpolate(input_path: Path, out_path: Path, params: dict) -> dict:
    probe = _probe_video(input_path)
    src_fps = probe["fps"]
    target_fps = int(params.get("target_fps") or 0)
    multiplier = int(params.get("multiplier") or 2)

    if target_fps > 0:
        multiplier = max(1, round(target_fps / src_fps)) if src_fps > 0 else multiplier
    out_fps = src_fps * multiplier if target_fps <= 0 else float(target_fps)
    passes = max(int(math.ceil(math.log2(max(multiplier, 1)))), 1)

    model_id = str(params.get("model") or EP_MODEL_ID or "rife-v4.6-ncnn")
    model_dir, how = resolve_model_dir(model_id)
    model_name = str(params.get("model_name") or "")

    work_root = Path(tempfile.mkdtemp(prefix="ep-vint-", dir=EP_WORKSPACE or None))
    frames_in = work_root / "frames_in"
    try:
        n_src = _extract_frames(input_path, frames_in)
        logger.info("extracted %d source frames (%dx%d @%.3ffps) mult=%d passes=%d",
                    n_src, probe["width"], probe["height"], src_fps, multiplier, passes)

        final_dir = work_root / "final"
        if EP_BACKEND == "vulkan":
            _, current = interp_frames_ncnn(frames_in, model_dir, model_name, passes)
            total_now = len(list(current.glob("*.png")))
            want = min(n_src * multiplier, total_now)
            if want == total_now:
                final_dir = current
            else:
                thin_frames(current, want, final_dir)
        elif EP_BACKEND in ("cuda", "rocm", "cpu"):
            interp_frames_torch(frames_in, final_dir, model_dir, passes)
        elif EP_BACKEND == "openvino":
            weight = next(iter(sorted(model_dir.glob("*.onnx"))), None)
            if weight is None:
                raise ExperimentalError(
                    "OpenVINO route requires an .onnx artifact but no authoritative "
                    "RIFE ONNX distribution exists yet (see ws-f-engine-choice.md §1.2); "
                    "manifest slot intentionally unfilled"
                )
            interp_frames_openvino(frames_in, final_dir, weight, passes)
        else:
            raise ExperimentalError(f"backend '{EP_BACKEND}' has no implementation branch")

        done = len(list(final_dir.glob("*.png")))
        _mux_video(final_dir, out_fps, input_path if probe["has_audio"] else None, out_path)
        return {"src_frames": n_src, "out_frames": done, "src_fps": round(src_fps, 3),
                "target_fps": round(out_fps, 3), "multiplier_effective": multiplier,
                "passes": passes, "model": model_id, "model_source": how,
                "backend": EP_BACKEND}
    finally:
        shutil.rmtree(work_root, ignore_errors=True)


# ---------------------------------------------------------------------------
# 标准端点
# ---------------------------------------------------------------------------
@app.get("/health")
async def health():
    ffmpeg_ok = shutil.which("ffmpeg") is not None and shutil.which("ffprobe") is not None
    return JSONResponse(
        content={"status": "ok" if ffmpeg_ok else "degraded", "ffmpeg": ffmpeg_ok,
                 "backend": EP_BACKEND},
        status_code=200 if ffmpeg_ok else 503,
    )


@app.get("/info")
async def info():
    return {
        "module_id": EP_MODULE_ID,
        "name": "Video Interp 视频插帧",
        "version": "0.1.0",
        "model_id": EP_MODEL_ID,
        "device": EP_DEVICE,
        "backend": EP_BACKEND,
        "experimental": True,
        "capabilities": [{
            "name": "interpolate",
            "input_type": "video",
            "output_type": "file",
            "params": {
                "multiplier": {"type": "integer", "default": 2},
                "target_fps": {"type": "integer", "default": 0},
            },
        }],
    }


@app.post("/predict/interpolate")
async def predict_interpolate(
    request: Request,
    file: Optional[UploadFile] = File(None),
    input_path_form: Optional[str] = Form(None, alias="input_path"),
    params_form: Optional[str] = Form(None, alias="params"),
):
    t0 = time.time()
    content_type = request.headers.get("content-type", "")

    params: dict = {}
    src: Optional[Path] = None
    try:
        if "application/json" in content_type:
            body = await request.json()
            params = body.get("params") or {}
            raw = body.get("input_path")
            if not raw:
                return _error(400, "INVALID_INPUT",
                              "JSON body requires 'input_path' (or use multipart 'file')")
            src = Path(raw)
        else:
            if params_form:
                try:
                    parsed = json.loads(params_form)
                    if isinstance(parsed, dict):
                        params = parsed
                except json.JSONDecodeError:
                    pass
            if file is not None and file.filename:
                ws = Path(EP_WORKSPACE)
                ws.mkdir(parents=True, exist_ok=True)
                src = ws / f"upload-{time.time_ns()}-{Path(file.filename).name}"
                src.write_bytes(await file.read())
            elif input_path_form:
                src = Path(input_path_form)

        if src is None:
            return _error(400, "INVALID_INPUT",
                          "No input provided (need 'file' or 'input_path')")
        if not src.is_file():
            return _error(400, "FILE_NOT_FOUND", f"input file not found: {src}")

        injected = params.get("output_path")
        if injected:
            out_path = Path(str(injected))
            out_path.parent.mkdir(parents=True, exist_ok=True)
        else:
            out_dir = Path(EP_WORKSPACE) if EP_WORKSPACE else Path(tempfile.gettempdir())
            out_dir.mkdir(parents=True, exist_ok=True)
            out_path = out_dir / f"{src.stem}_interp_{os.urandom(3).hex()}.mp4"

        meta = run_interpolate(src, out_path, params)
    except ExperimentalError as exc:
        logger.warning("EXPERIMENTAL branch blocked: %s", exc.reason)
        return _error(501, "EXPERIMENTAL_NOT_IMPLEMENTED",
                      f"{EP_BACKEND} branch not usable yet: {exc.reason}", exc.reason)
    except FileNotFoundError as exc:
        return _error(500, "INTERNAL_ERROR", f"external tool missing: {exc}")
    except Exception as exc:
        logger.exception("inference failed")
        return _error(500, "INFERENCE_ERROR", f"Inference error: {exc}")

    elapsed = round(time.time() - t0, 3)
    logger.info("done: %s -> %s (%.2fs)", src, out_path, elapsed)
    return {
        "status": "completed",
        "output_type": "file",
        "result": str(out_path),
        "output_path": str(out_path),
        "metadata": meta,
        "elapsed_seconds": elapsed,
    }


if __name__ == "__main__":
    logger.info("starting video-interp adapter on %s:%d (backend=%s)",
                EP_HOST, EP_PORT, EP_BACKEND)
    uvicorn.run(app, host=EP_HOST, port=EP_PORT, log_level=EP_LOG_LEVEL.lower())
