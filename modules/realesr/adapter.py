"""adapter.py — EntryPoint realesr 模块适配器（W1 脚手架，实验 E6/E7/E8 载体）

视频超分统一 REST 服务：ffmpeg 抽帧 → 按 EP_BACKEND 分派推理引擎 → 回封 mp4。

运行时分层（HETERO_DIST_PLAN §3.1，厂商栈优先）：
  cuda / rocm / cpu : torch + 官方 .pth（懒导入守卫；栈未装时返回 501 EXPERIMENTAL）
  openvino          : onnxruntime OpenVINO EP（ORT-OV）；E7 已落地——
                      权重 = scripts/export_onnx.py 自建 dynamic-shape ONNX，
                      缺权重时按契约返回 501 EXPERIMENTAL
  vulkan            : nihui/xinntao 上游 ncnn 引擎子进程（bin/<os>-<arch>/ 由用户放置）

产物协议：MODULE_SPEC §5 —— output_type="file"，result=输出绝对路径；
params.output_path 注入时优先写入。
"""

from __future__ import annotations

import json
import logging
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
EP_PORT = int(os.getenv("EP_PORT", "8920"))
EP_WORKSPACE = os.getenv("EP_WORKSPACE", os.path.join(os.getcwd(), "workspace"))
EP_MODEL_DIR = os.getenv("EP_MODEL_DIR", "")
EP_MODELS_ROOT = os.getenv("EP_MODELS_ROOT", "")
EP_MODEL_ID = os.getenv("EP_MODEL_ID", "")
EP_BACKEND = os.getenv("EP_BACKEND", "cuda").strip().lower()
EP_DEVICE = os.getenv("EP_DEVICE", "cuda:0")
EP_DEVICE_INDEX = os.getenv("EP_DEVICE_INDEX", "0")
EP_MODULE_ID = os.getenv("EP_MODULE_ID", "realesr")
EP_LOG_LEVEL = os.getenv("EP_LOG_LEVEL", "INFO")

MODULE_DIR = Path(os.getenv("EP_MODULE_DIR", Path(__file__).resolve().parent))

# 与 module.toml [runtime.binaries] 键位一致（平台词表 <os>-<arch>）
PLATFORM_KEY = {
    ("windows", "amd64"): "windows-x86_64",
    ("linux", "x86_64"): "linux-x86_64",
}.get((platform.system().lower(), platform.machine().lower()), "linux-x86_64")
NCNN_BINARY_NAMES = {
    "windows-x86_64": "realesrgan-ncnn-vulkan.exe",
    "linux-x86_64": "realesrgan-ncnn-vulkan",
}
# [[models]] id → target_dir（与 module.toml 保持一致；params.model 变体覆盖解析用）
MODEL_TARGET_DIRS = {
    "realesr-animevideov3-pth": "realesr-animevideov3-x4-pth",
    "realesrgan-x4plus-pth": "realesrgan-x4plus-pth",
    "realesrgan-animevideov3-x4-ncnn": "realesr-animevideov3-x4-ncnn",
    "realesr-animevideov3-onnx": "realesr-animevideov3-onnx",
}

logging.basicConfig(
    level=getattr(logging, EP_LOG_LEVEL.upper(), logging.INFO),
    format="%(asctime)s [realesr] %(levelname)s %(message)s",
)
logger = logging.getLogger("realesr")

app = FastAPI(title="EntryPoint realesr adapter", version="0.1.0")


class ExperimentalError(RuntimeError):
    """分支结构就绪但依赖物缺失（权重/依赖栈）：按契约返回 501 EXPERIMENTAL。"""

    def __init__(self, reason: str):
        super().__init__(reason)
        self.reason = reason


# ---------------------------------------------------------------------------
# 通用工具
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
    """执行外部命令，非零退出抛 RuntimeError（携带 stderr 尾部）。"""
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if proc.returncode != 0:
        tail = (proc.stderr or proc.stdout or "")[-1500:]
        raise RuntimeError(f"command failed ({proc.returncode}): {' '.join(map(str, cmd))}\n{tail}")
    return proc.stdout


def _probe_video(path: Path) -> dict:
    """ffprobe 提取帧率/时长/音轨信息。"""
    out = _run([
        "ffprobe", "-v", "error", "-select_streams", "v:0",
        "-show_entries", "stream=r_frame_rate,width,height,nb_frames",
        "-show_entries", "format=duration",
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
    return {
        "fps": fps,
        "width": int(stream.get("width") or 0),
        "height": int(stream.get("height") or 0),
        "has_audio": has_audio,
    }


def _extract_frames(video: Path, frames_dir: Path) -> int:
    """无损抽帧为 %08d.png，返回总帧数。"""
    frames_dir.mkdir(parents=True, exist_ok=True)
    # ffmpeg>=7 移除 -vsync，改用 -fps_mode passthrough（等价 vsync=0）
    _run([
        "ffmpeg", "-hide_banner", "-y", "-i", str(video),
        "-fps_mode", "passthrough", "-start_number", "0",
        str(frames_dir / "%08d.png"),
    ], timeout=None)
    count = len(list(frames_dir.glob("*.png")))
    if count == 0:
        raise RuntimeError("ffmpeg produced no frames")
    return count


def _mux_video(frames_dir: Path, fps: float, audio_src: Optional[Path], out_path: Path) -> None:
    """帧序列回封 mp4：libx264 crf17 yuv420p；存在音轨则 copy。"""
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


# ---------------------------------------------------------------------------
# 模型目录解析（params.model 变体覆盖，参照 rembg adapter 思路）
# ---------------------------------------------------------------------------
def resolve_model_dir(model_id: str) -> tuple[Path, str]:
    """返回 (模型目录, 解析说明)；优先 EP_MODELS_ROOT 下变体子目录，回退激活目录。"""
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
        + ". Download it via the platform model manager."
    )


def find_ncnn_model_dir(model_dir: Path) -> Optional[Path]:
    """定位含 ncnn param+bin 的模型子目录（整包解压后位于 models/ 或包根）。"""
    if any(model_dir.glob("*.param")):
        return model_dir
    for sub in sorted(model_dir.rglob("*.param")):
        return sub.parent
    return None


def find_weight_file(model_dir: Path) -> Optional[Path]:
    """定位 .pth / .onnx 权重文件。"""
    for pattern in ("*.pth", "*.onnx"):
        hits = sorted(model_dir.glob(pattern)) or sorted(model_dir.rglob(pattern))
        if hits:
            return hits[0]
    return None


def resolve_ncnn_engine() -> Path:
    """解析 ncnn 引擎二进制（module.toml [runtime.binaries] 相对路径约定）。"""
    env_override = os.getenv("EP_NCNN_ENGINE")
    if env_override and Path(env_override).is_file():
        return Path(env_override)
    name = NCNN_BINARY_NAMES.get(PLATFORM_KEY, "realesrgan-ncnn-vulkan")
    candidate = MODULE_DIR / "bin" / PLATFORM_KEY / name
    if candidate.is_file():
        return candidate
    raise ExperimentalError(
        f"ncnn engine binary not found at {candidate}. Fetch the official portable "
        "package from xinntao/Real-ESRGAN releases (see README '引擎二进制放置') "
        "and place the executable there."
    )


# ---------------------------------------------------------------------------
# 分支实现：vulkan → ncnn 子进程
# ---------------------------------------------------------------------------
def upscale_frames_ncnn(frames_in: Path, frames_out: Path, model_dir: Path,
                        scale: int, tile: int) -> None:
    """调用 realesrgan-ncnn-vulkan 目录模式逐帧超分（结构完整，E8 真机联调）。"""
    engine = resolve_ncnn_engine()
    ncnn_model = find_ncnn_model_dir(model_dir)
    if ncnn_model is None:
        raise ExperimentalError(
            f"no ncnn param/bin models under {model_dir}; the upstream package is a zip "
            "bundle — extract it first (platform URL downloads do not auto-extract zip)."
        )
    frames_out.mkdir(parents=True, exist_ok=True)
    cmd = [
        str(engine), "-i", str(frames_in), "-o", str(frames_out),
        "-m", str(ncnn_model), "-s", str(scale),
        "-t", str(tile if tile > 0 else 0), "-f", "png",
    ]
    logger.info("ncnn exec: %s", " ".join(cmd))
    _run(cmd)


# ---------------------------------------------------------------------------
# 分支实现：cuda / rocm / cpu → torch（懒导入守卫）
# ---------------------------------------------------------------------------
_TORCH_STATE = {"checked": False}


def upscale_frames_torch(frames_in: Path, frames_out: Path, weight: Path,
                         scale: int, tile: int, fp32: bool) -> None:
    """torch 推理路径：懒导入守卫 → realesrgan 包 RealESRGANer 逐帧处理。

    栈缺失（torch / realesrgan 未安装）时抛 ExperimentalError → 501，
    注明需安装 requirements-torch.txt（M2 消费前手动）。
    """
    try:
        import torch  # noqa: F401  懒导入守卫
    except ImportError as exc:
        raise ExperimentalError(
            f"torch runtime not installed in this venv ({exc}); install "
            "requirements-torch.txt (CUDA/ROCm wheel per platform) — pending M2 "
            "requirements_by_backend consumption"
        ) from exc
    try:
        # basicsr 1.4.2 引用的 functional_tensor 在 torchvision>=0.17 已移除，
        # 以别名 shim 兜底（post-install 钩子的导入自检同款逻辑）
        import sys as _sys, types as _types
        import torchvision.transforms.functional as _tvf
        _shim = _types.ModuleType("torchvision.transforms.functional_tensor")
        _shim.rgb_to_grayscale = _tvf.rgb_to_grayscale
        _sys.modules.setdefault("torchvision.transforms.functional_tensor", _shim)
        from basicsr.archs.rrdbnet_arch import RRDBNet
        from realesrgan import RealESRGANer
    except ImportError as exc:
        raise ExperimentalError(
            f"'realesrgan'/'basicsr' packages missing ({exc}); install "
            "requirements-torch.txt to enable the torch branch"
        ) from exc

    device = "cpu" if EP_BACKEND == "cpu" else "cuda"
    model_name = weight.stem.lower()
    # 架构预设按权重文件名判定（官方 release 实证：键集+形状全匹配）：
    #   realesr-animevideov3 → SRVGG num_conv=16, upscale=4
    #   v2 xsx2              → SRVGG num_conv=16, upscale=2
    #   v2 xsx4              → SRVGG num_conv=32, upscale=4
    #   x4plus(_anime)       → RRDBNet（23/6 blocks）
    def _srvgg_preset(stem: str) -> tuple[int, int]:
        if "xsx4" in stem or "animevideov3" in stem:
            return 32 if "xsx4" in stem else 16, 4
        return 16, 2  # xsx2

    if "animevideov3" in model_name or "xsx" in model_name:
        # compact SRVGGNetCompact 架构（animevideov3 / v2 xs 系列）
        from realesrgan.archs.srvgg_arch import SRVGGNetCompact
        num_conv, netscale = _srvgg_preset(model_name)
        net = SRVGGNetCompact(num_in_ch=3, num_out_ch=3, num_feat=64, num_conv=num_conv,
                              upscale=netscale, act_type="prelu")
    elif "x4plus_anime" in model_name:
        net = RRDBNet(num_in_ch=3, num_out_ch=3, num_feat=64, num_block=6,
                      num_grow_ch=32, scale=4)
        netscale = 4
    else:  # x4plus / x2plus RRDBNet 23 blocks
        net = RRDBNet(num_in_ch=3, num_out_ch=3, num_feat=64, num_block=23,
                      num_grow_ch=32, scale=max(2, scale))
        netscale = max(2, scale)

    upsampler = RealESRGANer(
        scale=netscale, model_path=str(weight), model=net, tile=tile,
        tile_pad=10, pre_pad=0, half=(not fp32 and device == "cuda"), device=device,
    )
    import cv2  # realesrgan 依赖链自带 opencv-python-headless? 见 requirements-torch.txt
    frames_out.mkdir(parents=True, exist_ok=True)
    for img_path in sorted(frames_in.glob("*.png")):
        img = cv2.imread(str(img_path), cv2.IMREAD_UNCHANGED)
        output, _ = upsampler.enhance(img, outscale=scale)
        cv2.imwrite(str(frames_out / img_path.name), output)


# ---------------------------------------------------------------------------
# 分支实现：openvino → ORT-OV（复用 onnx-matting 同款 provider 选择思路）
# ---------------------------------------------------------------------------
def upscale_frames_openvino(frames_in: Path, frames_out: Path, weight: Path,
                            tile: int) -> None:
    """ORT-OV 路径：OpenVINOExecutionProvider 优先（OPENVINO_DEVICE 指定
    设备类型），CPUExecutionProvider 兜底。权重为自建 dynamic-shape ONNX
    （scripts/export_onnx.py，E7）；预处理与 torch RealESRGANer 同口径：
    BGR→RGB /255 CHW fp32，输出 RGB→BGR 回写。"""
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
    use_gpu = False
    dev = os.getenv("OPENVINO_DEVICE", "GPU")
    if "OpenVINOExecutionProvider" in avail:
        providers.append(("OpenVINOExecutionProvider", {"device_type": dev.split(".")[0]}))
        use_gpu = not dev.upper().startswith("CPU")
    if "CPUExecutionProvider" in avail:
        providers.append("CPUExecutionProvider")

    def _make_infer():
        """构造 infer(img_bgr)->img_bgr。

        GPU + 动态 shape 模型：ORT 的 OV EP 在部分 iGPU 驱动上对动态输入报
        clEnqueueMapBuffer(-30)，且 ORT 无法加载 OV IR——故该组合直接用
        openvino runtime 按首帧尺寸 reshape 后内存编译（E7 排障实证）；
        其余组合维持 ORT 会话。"""
        if use_gpu:
            try:
                import openvino as ov

                core = ov.Core()
                model = core.read_model(str(weight))
                if not model.inputs[0].partial_shape.is_static:
                    import cv2 as _cv2p

                    probe = _cv2p.imread(str(min(frames_in.glob("*.png"))), _cv2p.IMREAD_COLOR)
                    h0, w0 = probe.shape[:2]
                    model.reshape(f"[1,3,{h0},{w0}]")
                    logger.info("OV-GPU 按首帧尺寸 reshape: %dx%d", w0, h0)
                compiled = core.compile_model(model, dev.split(".")[0])
                in_node = compiled.input(0)

                def infer_ov(img_bgr):
                    blob = cv2.dnn.blobFromImage(img_bgr, 1.0 / 255.0, swapRB=True)
                    out = compiled({in_node: blob})[compiled.output(0)]
                    out = np.clip(out[0].transpose(1, 2, 0) * 255.0, 0, 255).astype("uint8")
                    return cv2.cvtColor(out, cv2.COLOR_RGB2BGR)

                logger.info("openvino 直连路线就绪 (device=%s)", dev)
                return infer_ov
            except Exception as exc:  # noqa: BLE001 —— 回退 ORT（含 CPU 兜底）
                logger.warning("openvino 直连不可用，回退 ORT 会话: %s", exc)

        session = ort.InferenceSession(str(weight), providers=providers or None)
        input_name = session.get_inputs()[0].name

        def infer_ort(img_bgr):
            blob = cv2.dnn.blobFromImage(img_bgr, 1.0 / 255.0, swapRB=True)
            out = session.run(None, {input_name: blob})[0]
            out = np.clip(out[0].transpose(1, 2, 0) * 255.0, 0, 255).astype("uint8")
            return cv2.cvtColor(out, cv2.COLOR_RGB2BGR)

        return infer_ort

    infer = _make_infer()

    frames_out.mkdir(parents=True, exist_ok=True)
    for img_path in sorted(frames_in.glob("*.png")):
        img = cv2.imread(str(img_path), cv2.IMREAD_COLOR)
        h, w = img.shape[:2]
        if tile > 0 and max(h, w) > tile * 2:
            result = _tile_infer(img, tile, infer)
        else:
            result = infer(img)
        cv2.imwrite(str(frames_out / img_path.name), result)


def _tile_infer(img, tile: int, infer):
    """固定 shape ONNX（如 128px 社区版）的简单网格 tile 推理拼接。"""
    import cv2
    import numpy as np

    h, w = img.shape[:2]
    pad_h = (tile - h % tile) % tile
    pad_w = (tile - w % tile) % tile
    padded = cv2.copyMakeBorder(img, 0, pad_h, 0, pad_w, cv2.BORDER_REFLECT)
    ph, pw = padded.shape[:2]
    canvas = np.zeros_like(padded)
    for y in range(0, ph, tile):
        for x in range(0, pw, tile):
            patch = padded[y:y + tile, x:x + tile]
            canvas[y:y + tile, x:x + tile] = infer(patch)[:tile, :tile]
    return canvas[:h, :w]


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------
def run_upscale(input_path: Path, out_path: Path, params: dict) -> dict:
    probe = _probe_video(input_path)
    model_id = str(params.get("model") or EP_MODEL_ID or "realesr-animevideov3-pth")
    model_dir, how = resolve_model_dir(model_id)
    scale = int(params.get("scale_factor") or params.get("scale") or 4)
    preset = str(params.get("target_preset") or "balanced").lower()
    tile = int(params.get("tile_size") or (0 if preset == "fast" else 256))
    fp32 = preset == "quality"

    # 帧序列落位：params.staging_dir（平台 RAM 暂存区，ep-core::staging 注入）
    # 优先，缺省回退 workspace（第三方直连/旧平台兼容语义不变）
    staging_root = (params.get("staging_dir") or "").strip() if isinstance(params, dict) else ""
    work_base = staging_root or (EP_WORKSPACE or None)
    work_root = Path(tempfile.mkdtemp(prefix="ep-vups-", dir=work_base))
    frames_in = work_root / "frames_in"
    try:
        n_frames = _extract_frames(input_path, frames_in)
        logger.info("extracted %d frames (%dx%d @%.3ffps)", n_frames,
                    probe["width"], probe["height"], probe["fps"])

        frames_out = work_root / "frames_out"
        if EP_BACKEND == "vulkan":
            upscale_frames_ncnn(frames_in, frames_out, model_dir, scale, tile)
        elif EP_BACKEND in ("cuda", "rocm", "cpu"):
            weight = find_weight_file(model_dir)
            if weight is None or weight.suffix != ".pth":
                raise ExperimentalError(
                    f"no .pth weights under {model_dir} for backend '{EP_BACKEND}'"
                )
            upscale_frames_torch(frames_in, frames_out, weight, scale, tile, fp32)
        elif EP_BACKEND == "openvino":
            weight = find_weight_file(model_dir)
            if weight is None or weight.suffix != ".onnx":
                raise ExperimentalError(
                    "OpenVINO route requires an .onnx artifact but the manifest ONNX "
                    "variant is a placeholder (see module.toml / ws-f-engine-choice.md); "
                    "self-converted dynamic-shape ONNX will be published to fill this slot"
                )
            upscale_frames_openvino(frames_in, frames_out, weight, tile)
        else:
            raise ExperimentalError(f"backend '{EP_BACKEND}' has no implementation branch")

        done = len(list(frames_out.glob("*.png")))
        if done != n_frames:
            raise RuntimeError(f"frame count mismatch after inference: {done} != {n_frames}")
        _mux_video(frames_out, probe["fps"], input_path if probe["has_audio"] else None, out_path)
        return {"frames": done, "model": model_id, "model_source": how,
                "backend": EP_BACKEND, "preset": preset, "scale": scale}
    finally:
        shutil.rmtree(work_root, ignore_errors=True)


# ---------------------------------------------------------------------------
# 标准端点
# ---------------------------------------------------------------------------
@app.get("/health")
async def health():
    # 懒加载设计：服务即就绪；分支级依赖在 predict 时校验
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
        "name": "Video Upscale 视频超分",
        "version": "0.1.0",
        "model_id": EP_MODEL_ID,
        "device": EP_DEVICE,
        "backend": EP_BACKEND,
        "experimental": True,
        "capabilities": [{
            "name": "upscale",
            "input_type": "video",
            "output_type": "file",
            "params": {
                "scale_factor": {"type": "integer", "default": 4},
                "target_preset": {"type": "select", "options": ["fast", "balanced", "quality"],
                                  "default": "balanced"},
                "tile_size": {"type": "integer", "default": 256},
            },
        }],
    }


@app.post("/predict/upscale")
async def predict_upscale(
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
            out_path = out_dir / f"{src.stem}_upscaled_{os.urandom(3).hex()}.mp4"

        meta = run_upscale(src, out_path, params)
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
    logger.info("starting realesr adapter on %s:%d (backend=%s)",
                EP_HOST, EP_PORT, EP_BACKEND)
    uvicorn.run(app, host=EP_HOST, port=EP_PORT, log_level=EP_LOG_LEVEL.lower())
