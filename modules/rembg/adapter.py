"""
RemBG 智能去背景 — EntryPoint adapter
基于 rembg 库的 HTTP 服务，提供图像背景移除能力。
"""

from __future__ import annotations

import io
import json
import logging
import os
import time
import uuid
from pathlib import Path
from typing import Optional

import uvicorn
from fastapi import FastAPI, File, Form, Request, UploadFile
from fastapi.responses import JSONResponse

# ---------------------------------------------------------------------------
# 环境变量
# ---------------------------------------------------------------------------
EP_HOST: str = os.getenv("EP_HOST", "127.0.0.1")
EP_PORT: int = int(os.getenv("EP_PORT", "8900"))
EP_WORKSPACE: str = os.getenv("EP_WORKSPACE", os.path.join(os.getcwd(), "workspace"))
# daemon 注入的模型键名是 EP_MODEL_ID（ep-core process.rs build_module_env，
# 取激活变体 id，如 "u2net"）；旧实现读 EP_MODEL_NAME（daemon 从不注入该键），
# 导致变体切换静默失效、恒用默认值。
EP_MODEL_NAME: str = os.getenv("EP_MODEL_ID", os.getenv("EP_MODEL_NAME", "u2net"))
EP_DEVICE_INDEX: str = os.getenv("EP_DEVICE_INDEX", "0")
EP_LOG_LEVEL: str = os.getenv("EP_LOG_LEVEL", "INFO")

# daemon 将模型下载到 EP_MODEL_DIR（models/<target_dir>/<model>.onnx）。
# rembg 按 <model>.onnx 文件名在其模型主目录查找/下载；将该目录指向
# EP_MODEL_DIR，使 daemon 预下载的模型被真正消费、变体切换端到端生效。
EP_MODEL_DIR: str = os.getenv("EP_MODEL_DIR", "")
# 模型缓存根目录（缺陷 #4）：EP_MODEL_DIR 恒指激活变体目录，params.model
# 覆盖为非激活变体时 rembg 在激活目录找不到权重会静默联网下载。据
# EP_MODELS_ROOT 按下方映射解析变体子目录，命中本地权重则直接用。
EP_MODELS_ROOT: str = os.getenv("EP_MODELS_ROOT", "")

# 与 module.toml [[models]] 的 id → target_dir 约定对齐（rembg 会话名即 model id，
# 权重文件名为 <model id>.onnx，见 rembg sessions 各 download_models 实现）。
MODEL_TARGET_DIRS: dict[str, str] = {
    "u2net": "rembg-u2net",
    "isnet-general-use": "rembg-isnet",
    "birefnet-general": "rembg-birefnet",
}


class ModelLocalMissingError(RuntimeError):
    """请求的模型本地权重缺失：明确报错而非让 rembg 静默联网下载。"""


def resolve_local_model_dir(model_name: str) -> Optional[Path]:
    """解析 model_name 的本地权重目录（目录内须有 <model_name>.onnx）。

    优先级：EP_MODELS_ROOT 下按 target_dir 映射解析的变体目录 →
    激活变体目录 EP_MODEL_DIR（仅当请求的就是激活模型）。均未命中返回 None。
    """
    expected = f"{model_name}.onnx"
    candidates: list[Path] = []
    target = MODEL_TARGET_DIRS.get(model_name)
    if target and EP_MODELS_ROOT:
        candidates.append(Path(EP_MODELS_ROOT) / target)
    if EP_MODEL_DIR:
        candidates.append(Path(EP_MODEL_DIR))
    for d in candidates:
        if (d / expected).is_file():
            return d
    return None


def _activate_model_dir(model_name: str) -> Path:
    """定位并激活模型的本地权重目录；缺失时报错指出目录与获取方式。"""
    found = resolve_local_model_dir(model_name)
    if found is None:
        target = MODEL_TARGET_DIRS.get(model_name, f"rembg-{model_name}")
        expected_dir = (
            Path(EP_MODELS_ROOT) / target
            if EP_MODELS_ROOT
            else Path("<EP_MODELS_ROOT>") / target
        )
        raise ModelLocalMissingError(
            f"Local weights for model '{model_name}' not found: expected "
            f"{expected_dir / f'{model_name}.onnx'}. Obtain it via the platform model "
            f"manager, or switch the active variant with "
            f"PUT /api/models/rembg/{model_name}/variant and restart the module."
        )
    # rembg sessions 统一读 U2NET_HOME 定位 <model>.onnx
    os.environ["U2NET_HOME"] = str(found)
    logger.info("Using local weights dir for '%s': %s", model_name, found)
    return found


# 启动时先将激活变体目录设为 U2NET_HOME（保持旧行为：预加载走激活变体）；
# 请求级变体覆盖时再按需切换（见 _activate_model_dir）。
if EP_MODEL_DIR:
    os.environ.setdefault("U2NET_HOME", EP_MODEL_DIR)

# ---------------------------------------------------------------------------
# 日志
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=getattr(logging, EP_LOG_LEVEL.upper(), logging.INFO),
    format="%(asctime)s [rembg-adapter] %(levelname)s %(message)s",
)
logger = logging.getLogger("rembg-adapter")

# ---------------------------------------------------------------------------
# 全局状态
# ---------------------------------------------------------------------------
_session = None
_session_model: Optional[str] = None
_ready = False

app = FastAPI(
    title="RemBG Adapter",
    version="2.0.50",
    description="EntryPoint rembg 图像去背景模块",
)


# ---------------------------------------------------------------------------
# 模型加载
# ---------------------------------------------------------------------------
def _load_session(model_name: str):
    """懒加载 rembg session，按需切换模型。

    加载前先解析本地权重目录并将 U2NET_HOME 指向它（缺陷 #4）：
    缺失时抛 ModelLocalMissingError，绝不让 rembg 静默联网下载。
    """
    global _session, _session_model, _ready
    if _session is not None and _session_model == model_name:
        return _session

    local_dir = _activate_model_dir(model_name)
    logger.info("Loading rembg session: model=%s (weights=%s) ...", model_name, local_dir)
    t0 = time.time()
    try:
        from rembg import new_session

        _session = new_session(model_name)
        _session_model = model_name
        _ready = True
        logger.info("Session loaded in %.1fs", time.time() - t0)
    except Exception as exc:
        _ready = False
        logger.exception("Failed to load session: %s", exc)
        raise
    return _session


# ---------------------------------------------------------------------------
# 启动时预加载
# ---------------------------------------------------------------------------
@app.on_event("startup")
async def _startup():
    try:
        _load_session(EP_MODEL_NAME)
    except ModelLocalMissingError as exc:
        # 激活变体权重缺失：保持 not ready，待请求时给出明确错误
        logger.warning("Preload skipped (local weights missing): %s", exc)
    except Exception:
        logger.warning("Preload failed; will retry on first request.")


# ---------------------------------------------------------------------------
# 健康检查 & 信息
# ---------------------------------------------------------------------------
@app.get("/health")
async def health():
    return JSONResponse(
        content={
            "status": "ok" if _ready else "loading",
            "model": _session_model,
        },
        status_code=200 if _ready else 503,
    )


@app.get("/info")
async def info():
    return {
        "module": "rembg",
        "version": "2.0.50",
        "model": _session_model,
        "ready": _ready,
        "capabilities": ["remove_bg"],
        # 与 module.toml [compute].backends 保持一致：rembg[cpu] 栈仅 CPU
        "backends": ["cpu"],
    }


# ---------------------------------------------------------------------------
# 核心推理
# ---------------------------------------------------------------------------
def _run_remove_bg(
    input_bytes: bytes,
    model_name: str,
    alpha_matting: bool = False,
    post_process: bool = True,
) -> bytes:
    """调用 rembg 执行背景移除，返回 PNG bytes。"""
    from rembg import remove

    session = _load_session(model_name)
    output_bytes = remove(
        input_bytes,
        session=session,
        alpha_matting=alpha_matting,
        post_process_mask=post_process,
    )
    return output_bytes


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


def _parse_bool(value, default: bool) -> bool:
    """宽容解析布尔参数（JSON bool / 字符串 "true"/"false"）。"""
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        v = value.strip().lower()
        if v in ("true", "1", "yes"):
            return True
        if v in ("false", "0", "no"):
            return False
    return default


@app.post("/predict/remove_bg")
async def predict_remove_bg(
    request: Request,
    file: Optional[UploadFile] = File(None),
    input_path_form: Optional[str] = Form(None, alias="input_path"),
    params_form: Optional[str] = Form(None, alias="params"),
    model_form: Optional[str] = Form(None, alias="model"),
    alpha_matting_form: Optional[str] = Form(None, alias="alpha_matting"),
    post_process_form: Optional[str] = Form(None, alias="post_process"),
):
    """
    移除图片背景，输出透明 PNG。

    支持三种输入方式：
    - multipart file 上传（ep-core executor 文件类产物路径）
    - multipart/JSON 的 input_path 指定服务器端文件路径
    - JSON body {"input_path": ...}（ADAPTER_API.md 格式 B）

    参数来源：`params` 字段（JSON 对象/字符串，executor 契约）优先，
    兼容旧版平铺表单字段（model/alpha_matting/post_process）。

    响应契约（ep-core executor 权威）：
    {status:"completed", output_type:"file", result:<输出路径>,
     output_path:<输出路径>, metadata:{...}, elapsed_seconds:<float>}
    """
    t0 = time.time()

    # ---- 解析输入与参数 ----
    input_bytes: bytes = b""
    source_name: str = ""
    params: dict = {}
    model_override: Optional[str] = None
    alpha_matting = False
    post_process = True

    content_type = request.headers.get("content-type", "")
    try:
        if "application/json" in content_type:
            # JSON body：路径输入（格式 B）
            body = await request.json()
            params = body.get("params") or {}
            if not isinstance(params, dict):
                params = {}
            input_path = body.get("input_path")
            if not input_path:
                return _error(
                    400, "INVALID_INPUT",
                    "No input provided (need 'input_path' in JSON body or 'file' in multipart)",
                )
            p = Path(input_path)
            if not p.is_file():
                return _error(400, "FILE_NOT_FOUND", f"input_path not found: {input_path}")
            input_bytes = p.read_bytes()
            source_name = p.name
        else:
            # multipart/form-data：params 为 JSON 字符串（executor 契约）
            if params_form:
                try:
                    parsed = json.loads(params_form)
                    if isinstance(parsed, dict):
                        params = parsed
                except json.JSONDecodeError:
                    pass
            input_path = input_path_form
            model_override = model_form
            alpha_matting = _parse_bool(alpha_matting_form, False)
            post_process = _parse_bool(post_process_form, True)

            if file is not None and file.filename:
                input_bytes = await file.read()
                source_name = file.filename
            elif input_path:
                p = Path(input_path)
                if not p.is_file():
                    return _error(400, "FILE_NOT_FOUND", f"input_path not found: {input_path}")
                input_bytes = p.read_bytes()
                source_name = p.name
            else:
                return _error(
                    400, "INVALID_INPUT",
                    "Provide either a multipart 'file' or an 'input_path' field.",
                )
    except Exception as exc:
        logger.exception("Failed to parse request")
        return _error(400, "INVALID_INPUT", f"Failed to parse request: {exc}")

    # params（executor 注入/用户参数）覆盖平铺表单默认值
    if "model" in params:
        model_override = str(params.get("model") or model_override)
    if "alpha_matting" in params:
        alpha_matting = _parse_bool(params.get("alpha_matting"), False)
    if "post_process" in params:
        post_process = _parse_bool(params.get("post_process"), True)

    if not input_bytes:
        return _error(400, "INVALID_INPUT", "Empty input image.")

    # 大小限制 50 MB
    if len(input_bytes) > 50 * 1024 * 1024:
        return _error(413, "INVALID_INPUT", "Input exceeds 50 MB limit.")

    model_name = model_override or EP_MODEL_NAME

    # ---- 推理 ----
    try:
        output_bytes = _run_remove_bg(
            input_bytes,
            model_name=model_name,
            alpha_matting=alpha_matting,
            post_process=post_process,
        )
    except ModelLocalMissingError as exc:
        # 本地权重缺失：按契约 MODEL_NOT_LOADED（503）明确报错（缺失目录 +
        # 获取方式），绝不让 rembg 静默联网下载
        logger.error("Model weights missing: %s", exc)
        return _error(503, "MODEL_NOT_LOADED", str(exc))
    except Exception as exc:
        logger.exception("Inference failed")
        return _error(500, "INFERENCE_ERROR", f"Inference error: {exc}")

    # ---- 写出：params.output_path（模块产物协议注入）优先，否则 workspace ----
    try:
        injected = params.get("output_path")
        if injected:
            out_path = Path(str(injected))
            out_path.parent.mkdir(parents=True, exist_ok=True)
        else:
            ws = Path(EP_WORKSPACE)
            ws.mkdir(parents=True, exist_ok=True)
            stem = Path(source_name).stem or "output"
            out_path = ws / f"{stem}_{uuid.uuid4().hex[:8]}.png"
        out_path.write_bytes(output_bytes)
    except Exception as exc:
        logger.exception("Failed to write output")
        return _error(500, "INTERNAL_ERROR", f"Output write error: {exc}")

    elapsed = round(time.time() - t0, 3)
    logger.info("Done: %s -> %s (%d bytes, %.2fs)", source_name, out_path, len(output_bytes), elapsed)

    return {
        "status": "completed",
        "output_type": "file",
        "result": str(out_path),
        "output_path": str(out_path),
        "metadata": {
            "model": model_name,
            "alpha_matting": alpha_matting,
            "post_process": post_process,
            "output_size_bytes": len(output_bytes),
        },
        "elapsed_seconds": elapsed,
    }


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    logger.info(
        "Starting rembg adapter on %s:%d (model=%s, workspace=%s)",
        EP_HOST,
        EP_PORT,
        EP_MODEL_NAME,
        EP_WORKSPACE,
    )
    uvicorn.run(
        app,
        host=EP_HOST,
        port=EP_PORT,
        log_level=EP_LOG_LEVEL.lower(),
    )
