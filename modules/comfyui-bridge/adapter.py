"""adapter.py — ComfyUI 桥接模块适配器（EntryPoint 策略 C：桥接外部 HTTP 服务）。

编程依据（冻结契约）：modules/comfyui-bridge/CONTRACT.md §1/§2/§3/§4.2/§5；
背景：docs/COMFYUI_BRIDGE_PLAN.md §3.2/§3.3。

端点一览：
    GET    /health              探测上游 ComfyUI GET /system_stats（不通 → 503）
    GET    /info                模块信息 + ComfyUI 可达性/版本 + 队列摘要
    POST   /predict/generate    同步阻塞全流程：收上游产物 → inject 注入 →
                                上传 → POST /prompt → 轮询 history（打印
                                EP-PROGRESS:NN%）→ GET /view 下载产物落盘
    GET    /workflows           工作流列表 [{name, size_bytes, mtime}]
    POST   /workflows           上传 API 格式工作流 JSON（multipart file 字段）
    DELETE /workflows/{name}    删除工作流

环境变量（CONTRACT.md §2.4）：
    EP_PORT        监听端口（默认 8180）
    HOST           绑定地址（默认 127.0.0.1；兼容平台注入的 EP_HOST）
    COMFYUI_URL    默认 ComfyUI 地址（params.base_url 优先）
    MODULE_DIR     模块目录（workflows/ 与缺省 output 目录的解析基准）
    EP_OUTPUT_DIR  产物落盘目录（平台引擎注入；亦可用 params.output_path，
                   两者皆缺 → MODULE_DIR/output/）

结构约束：comfy_client.py 由并行代理交付 —— 对它的 import 一律放在函数内
（_load_comfy_module），保证 comfy_client 缺席时本文件仍可被 import/单测。
"""

from __future__ import annotations

import asyncio
import contextlib
import copy
import inspect
import json
import logging
import os
import re
import shutil
import tempfile
from pathlib import Path
from typing import Any, Callable, NamedTuple

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from starlette.datastructures import UploadFile
from starlette.exceptions import HTTPException as StarletteHTTPException

logger = logging.getLogger("comfyui-bridge")

MODULE_ID = "comfyui-bridge"
VERSION = "0.1.0"
DEFAULT_COMFYUI_URL = "http://127.0.0.1:8188"
DEFAULT_PORT = 8180
DEFAULT_HOST = "127.0.0.1"

app = FastAPI(title=MODULE_ID, version=VERSION, docs_url=None, redoc_url=None)

# ── 错误响应统一 {"error": ...}（§4.2） ──────────────────────────────


@app.exception_handler(StarletteHTTPException)
async def _http_exception_handler(request: Request, exc: StarletteHTTPException):
    detail = exc.detail
    if not isinstance(detail, str):
        try:
            detail = json.dumps(detail, ensure_ascii=False)
        except (TypeError, ValueError):
            detail = str(detail)
    return JSONResponse(
        {"error": detail}, status_code=exc.status_code, headers=exc.headers
    )


@app.exception_handler(RequestValidationError)
async def _request_validation_handler(request: Request, exc: RequestValidationError):
    return JSONResponse({"error": f"invalid request: {exc}"}, status_code=400)


@app.exception_handler(Exception)
async def _unhandled_error_handler(request: Request, exc: Exception):
    logger.exception("unhandled error: %s", exc)
    return JSONResponse({"error": f"internal error: {exc}"}, status_code=500)


# ── inject 引擎（CONTRACT.md §3，顶层纯函数，便于单测） ─────────────


def _node_sort_key(node_id: str):
    """节点 id 排序键：纯数字按数值序在前，其余按字符串序。"""
    try:
        return (0, int(node_id), "")
    except (TypeError, ValueError):
        return (1, 0, str(node_id))


def _available_nodes_text(workflow: dict) -> str:
    """工作流可用节点清单文本（用于错误信息，D6）。"""
    if not isinstance(workflow, dict):
        return "(none)"
    ids = sorted((str(k) for k in workflow.keys()), key=_node_sort_key)
    return ", ".join(ids) if ids else "(none)"


def parse_inject(mapping_json: Any) -> dict:
    """解析 inject 参数（JSON 字符串/bytes/dict）→ dict。

    非法（坏 JSON / 非对象 / 值类型不支持）→ ValueError，
    消息以 "invalid inject mapping" 开头（§4.2）。
    值允许："$input" 表达式字符串与标量字面量（str/int/float/bool/null）。
    """
    if mapping_json is None:
        return {}
    if isinstance(mapping_json, dict):
        mapping = mapping_json
    elif isinstance(mapping_json, (str, bytes, bytearray)):
        try:
            mapping = json.loads(mapping_json)
        except (ValueError, UnicodeDecodeError) as exc:
            raise ValueError(f"invalid inject mapping: invalid JSON ({exc})") from exc
    else:
        raise ValueError("invalid inject mapping: expected a JSON object")
    if not isinstance(mapping, dict):
        raise ValueError("invalid inject mapping: expected a JSON object")
    for key, value in mapping.items():
        if value is not None and not isinstance(value, (str, int, float, bool)):
            raise ValueError(
                f"invalid inject mapping: key {key!r}: unsupported value type "
                f"{type(value).__name__} (expected $input expression or scalar literal)"
            )
    return mapping


def _split_inject_key(key: Any) -> tuple[str | None, str | None]:
    """键 → (node_id, field)；形状非法 → (None, None)。

    契约键形 "<节点id>.inputs.<字段名>"；兼容两段式 "<节点id>.<字段名>"。
    """
    if not isinstance(key, str):
        return None, None
    parts = key.split(".")
    if len(parts) == 3 and parts[1] == "inputs":
        node_id, field = parts[0], parts[2]
    elif len(parts) == 2:
        node_id, field = parts
    else:
        return None, None
    if not node_id or not field:
        return None, None
    return node_id, field


def validate_inject_keys(workflow: dict, mapping: dict) -> list[str]:
    """注入前逐项校验键存在性（D6）：键的节点 id 必须在工作流中。

    返回错误列表（空列表 = 全部合法）；每条错误附带可用节点清单。
    """
    if not isinstance(workflow, dict):
        return ["invalid inject mapping: workflow must be a JSON object"]
    errors: list[str] = []
    available = _available_nodes_text(workflow)
    for key in mapping:
        node_id, _field = _split_inject_key(key)
        if node_id is None:
            errors.append(
                f"key {key!r}: invalid inject key, expected "
                f'"<node_id>.inputs.<field>" (available nodes: [{available}])'
            )
        elif str(node_id) not in workflow:
            errors.append(
                f'key {key!r}: node "{node_id}" not found in workflow; '
                f"available nodes: [{available}]"
            )
    return errors


# resolve_fn(expr, ctx) -> 注入值；expr 为映射原始值（表达式或字面量）
ResolveFn = Callable[[Any, dict], Any]


def _set_field(workflow: dict, node_id: str, field: str, value: Any) -> None:
    entry = workflow[node_id]
    if not isinstance(entry, dict):
        raise ValueError(f'invalid inject mapping: node "{node_id}" is not an object')
    # 缺失字段允许（写入新键）；未触碰字段保留模板默认值（§3 规则 3）
    entry.setdefault("inputs", {})[field] = value


def _resolve_sync(value: Any) -> Any:
    """同步路径下消化 resolve_fn 返回的 coroutine。

    仅当当前线程没有运行中的事件循环时可行（单测/线程池场景）；
    在事件循环内请使用 apply_inject_async。
    """
    if not inspect.isawaitable(value):
        return value
    try:
        asyncio.get_running_loop()
    except RuntimeError:
        return asyncio.run(value)
    raise TypeError(
        "resolve_fn returned an awaitable inside a running event loop; "
        "use apply_inject_async() instead"
    )


def apply_inject(workflow: dict, mapping: dict, resolve_fn: ResolveFn) -> dict:
    """把 inject 映射应用到工作流【副本】（同步版；resolve_fn 可同步或返回 coroutine）。

    - 注入前逐项校验键存在性，非法 → ValueError（含可用节点清单，D6）；
    - resolve_fn(expr, ctx) 负责把来源表达式解析为待写入值
      （adapter 内：文件类先 POST /upload/image，文本/字面量原样）；
    - 未映射字段保留模板默认值；返回修改后的深拷贝，不改原对象。
    """
    errors = validate_inject_keys(workflow, mapping)
    if errors:
        raise ValueError("invalid inject mapping: " + "; ".join(errors))
    wf = copy.deepcopy(workflow)
    ctx = {"available_nodes": _available_nodes_text(wf), "keys": list(mapping.keys())}
    for key, expr in mapping.items():
        node_id, field = _split_inject_key(key)
        if node_id is None or node_id not in wf:  # 防御：入口已校验
            raise ValueError(f"invalid inject mapping: key {key!r}")
        value = _resolve_sync(resolve_fn(expr, ctx))
        _set_field(wf, node_id, field, value)
    return wf


async def apply_inject_async(
    workflow: dict, mapping: dict, resolve_fn: ResolveFn
) -> dict:
    """apply_inject 的异步版：resolve_fn 可返回值或 Awaitable（adapter 主路径）。"""
    errors = validate_inject_keys(workflow, mapping)
    if errors:
        raise ValueError("invalid inject mapping: " + "; ".join(errors))
    wf = copy.deepcopy(workflow)
    ctx = {"available_nodes": _available_nodes_text(wf), "keys": list(mapping.keys())}
    for key, expr in mapping.items():
        node_id, field = _split_inject_key(key)
        if node_id is None or node_id not in wf:  # 防御：入口已校验
            raise ValueError(f"invalid inject mapping: key {key!r}")
        value = resolve_fn(expr, ctx)
        if inspect.isawaitable(value):
            value = await value
        _set_field(wf, node_id, field, value)
    return wf


def resolve_output_nodes(
    workflow: dict, outputs: dict, output_nodes_param: str | None = None
) -> list[str]:
    """解析取回哪些输出节点的产物（§2.3 第 6 步）。

    - output_nodes_param 非空：逗号分隔的 Save 节点 id，按给定顺序返回；
      节点不在工作流 → ValueError（含可用节点清单）。
    - 缺省：按工作流节点顺序返回 history outputs 中含 images 的节点。
    """
    outputs = outputs if isinstance(outputs, dict) else {}
    if output_nodes_param is not None and str(output_nodes_param).strip():
        requested = [p.strip() for p in str(output_nodes_param).split(",") if p.strip()]
        available = _available_nodes_text(workflow)
        for node_id in requested:
            if node_id not in workflow:
                raise ValueError(
                    f'invalid output_nodes: node "{node_id}" not found in workflow; '
                    f"available nodes: [{available}]"
                )
        return requested
    selected: list[str] = []
    if isinstance(workflow, dict):
        for node_id in workflow.keys():
            info = outputs.get(node_id)
            if isinstance(info, dict) and info.get("images"):
                selected.append(node_id)
    return selected


# ── 工作流文件名清洗与格式校验（§2.2） ──────────────────────────────

_SAFE_NAME_RE = re.compile(r"[^A-Za-z0-9._-]")


def clean_workflow_filename(filename: str) -> str:
    """§2.2 文件名清洗：剥路径分量（防穿越）→ 仅保留 [A-Za-z0-9._-] →
    剥 ".json" 后缀 → 返回工作流名（不含 .json）。

    清洗后为空 → ValueError（端点层转 400）。
    """
    raw = str(filename or "").replace("\\", "/")
    raw = raw.rsplit("/", 1)[-1]  # 剥路径分量（含 ../ 穿越）
    raw = _SAFE_NAME_RE.sub("", raw)  # 仅保留白名单字符
    if raw.lower().endswith(".json"):
        raw = raw[:-5]
    if not raw:
        raise ValueError("invalid workflow filename: empty after sanitization")
    return raw


def validate_workflow_format(data: Any) -> str | None:
    """校验 ComfyUI API 格式（§2.2）：顶层对象、每个值是对象且含
    class_type 与 inputs。返回错误描述，合法返回 None。"""
    if not isinstance(data, dict):
        return "top level must be a JSON object"
    for node_id, node in data.items():
        if not isinstance(node, dict):
            return f'node "{node_id}" must be a JSON object'
        class_type = node.get("class_type")
        if not isinstance(class_type, str) or not class_type.strip():
            return f'node "{node_id}": missing or invalid "class_type"'
        if not isinstance(node.get("inputs"), dict):
            return f'node "{node_id}": missing or invalid "inputs" object'
    return None


# ── 目录与环境（§2.4；每次调用读 env，便于测试用 monkeypatch 隔离） ──


def _module_dir() -> Path:
    return Path(os.environ.get("MODULE_DIR") or Path(__file__).resolve().parent)


def _workflows_dir() -> Path:
    return _module_dir() / "workflows"


def _workflow_path(name: str) -> Path:
    """逻辑名 → 落盘文件：用户上传存 `<名>.json`；随包示例模板为
    `<名>.api.json`（ComfyUI API 导出命名习惯），优先前者、回退后者。"""
    stem = clean_workflow_filename(name)
    canonical = _workflows_dir() / f"{stem}.json"
    if canonical.is_file():
        return canonical
    shipped = _workflows_dir() / f"{stem}.api.json"
    if shipped.is_file():
        return shipped
    return canonical


def _logical_workflow_name(path: Path) -> str:
    """文件名 → 逻辑名：剥 `.json` 与随包模板的 `.api.json` 后缀。"""
    stem = path.stem
    if stem.lower().endswith(".api"):
        stem = stem[:-4]
    return stem


def _list_workflow_names() -> list[str]:
    directory = _workflows_dir()
    if not directory.is_dir():
        return []
    names = (_logical_workflow_name(p) for p in directory.glob("*.json"))
    return sorted(names, key=_node_sort_key)


def _resolve_output_dir(params_output_path: str | None = None) -> Path:
    """产物落盘目录（§2.4）：EP_OUTPUT_DIR > params.output_path > MODULE_DIR/output。"""
    candidate = (
        os.environ.get("EP_OUTPUT_DIR")
        or params_output_path
        or str(_module_dir() / "output")
    )
    return Path(candidate).expanduser()


def _env_comfyui_url() -> str:
    return os.environ.get("COMFYUI_URL") or DEFAULT_COMFYUI_URL


def _load_comfy_module():
    """惰性 import comfy_client（并行代理 A 交付；缺席时抛 ImportError）。"""
    from comfy_client import ComfyClient, ComfyClientError  # noqa: PLC0415

    return ComfyClient, ComfyClientError


async def _safe_aclose(client: Any) -> None:
    """尽力释放客户端连接（兼容无 aclose 的实现）。"""
    closer = getattr(client, "aclose", None) or getattr(client, "close", None)
    if callable(closer):
        with contextlib.suppress(Exception):
            result = closer()
            if inspect.isawaitable(result):
                await result


# ── ComfyUI 错误映射（§4.2） ────────────────────────────────────────

_ERROR_TEMPLATES = {
    "connect": "comfyui unreachable: {}",
    "rejected": "comfyui rejected prompt: {}",
    "execution": "comfyui execution error: {}",
    "timeout": "comfyui generation timeout: {}",
    "output": "failed to fetch output: {}",
}

_KNOWN_MESSAGE_PREFIXES = (
    "comfyui unreachable",
    "comfyui rejected prompt",
    "comfyui execution error",
    "comfyui generation timeout",
    "failed to fetch output",
)


def _client_error_message(exc: Any) -> str:
    """comfy_client 已按 §4.2 模板给出技术信息时原样透传，否则按 kind 补模板。"""
    kind = getattr(exc, "kind", "") or "connect"
    message = (getattr(exc, "message", "") or str(exc) or "").strip()
    if message.startswith(_KNOWN_MESSAGE_PREFIXES):
        return message
    template = _ERROR_TEMPLATES.get(kind, "comfyui error: {}")
    return template.format(message or kind)


def _map_client_error(exc: Any) -> StarletteHTTPException:
    """connect/rejected/execution/output → 502；timeout → 504。"""
    kind = getattr(exc, "kind", "") or "connect"
    status = 504 if kind == "timeout" else 502
    return StarletteHTTPException(status, _client_error_message(exc))


def _print_progress(pct: Any) -> None:
    """契约 §2.3：轮询期间打印 EP-PROGRESS:NN%（前端解析约定，D3）。"""
    try:
        value = int(pct)
    except (TypeError, ValueError):
        return
    value = max(0, min(100, value))
    print(f"EP-PROGRESS:{value}%", flush=True)


# ── 上游产物收集与 inject 解析器 ────────────────────────────────────


class _Upstream(NamedTuple):
    """一个上游产物（multipart 字段名 = 上游节点 id；单输入可别名 file/input）。"""

    field: str  # 表单字段名
    filename: str
    content: bytes
    is_text: bool  # .txt 上游 → 文本注入候选（§2.3 第 1 步）
    text: str | None


async def _collect_upstream(form: Any) -> list[_Upstream]:
    upstream: list[_Upstream] = []
    for field_name, value in form.multi_items():
        if not isinstance(value, UploadFile):
            continue
        filename = value.filename or f"{field_name}.bin"
        content = await value.read()
        is_text = filename.lower().endswith(".txt")
        text = content.decode("utf-8", errors="replace") if is_text else None
        upstream.append(
            _Upstream(
                field=field_name,
                filename=filename,
                content=content,
                is_text=is_text,
                text=text,
            )
        )
    return upstream


def _make_resolver(upstream: list[_Upstream], client: Any) -> ResolveFn:
    """构造 inject 解析器（§3 规则 1）：文件类来源先 POST /upload/image
    再把返回的服务器侧文件名写入字段；文本/字面量原样写入。"""

    async def resolve(expr: Any, ctx: dict) -> Any:
        if isinstance(expr, str) and expr.startswith("$input"):
            if expr == "$input":  # 首个上游产物
                if not upstream:
                    raise ValueError(
                        "invalid inject mapping: $input referenced but no upstream input provided"
                    )
                item = upstream[0]
            elif expr.startswith("$input."):  # 定向引用 $input.<上游节点id>
                ref = expr[len("$input."):]
                item = next((u for u in upstream if u.field == ref), None)
                if item is None:
                    available = ", ".join(u.field for u in upstream) or "(none)"
                    raise ValueError(
                        f'invalid inject mapping: upstream input "{ref}" not found; '
                        f"available inputs: [{available}]"
                    )
            else:
                raise ValueError(
                    f"invalid inject mapping: unsupported source expression {expr!r}"
                )
            if item.is_text:
                return item.text
            return await client.upload_image(item.content, item.filename)
        return expr  # 字面量

    return resolve


# ── 生成参数提取（平台经 multipart 'params' JSON 传参，兼容平铺字段） ──

_GENERATE_PARAM_KEYS = ("workflow", "inject", "base_url", "output_nodes", "output_path")


def _extract_generate_params(form: Any) -> dict[str, str]:
    """params JSON 字段（ADAPTER_API §2.3 格式 A）∪ 平铺表单字段（平铺优先）。"""
    params: dict[str, str] = {}
    raw = form.get("params")
    if isinstance(raw, str) and raw.strip():
        try:
            parsed = json.loads(raw)
        except ValueError as exc:
            raise StarletteHTTPException(400, f"invalid params JSON: {exc}")
        if not isinstance(parsed, dict):
            raise StarletteHTTPException(400, "invalid params: expected a JSON object")
        for key in _GENERATE_PARAM_KEYS:
            value = parsed.get(key)
            if value is None:
                continue
            if isinstance(value, str):
                params[key] = value
            elif isinstance(value, (dict, list)):  # 如 inject 内嵌 JSON 对象
                params[key] = json.dumps(value, ensure_ascii=False)
            else:
                params[key] = str(value)
    for key in _GENERATE_PARAM_KEYS:
        value = form.get(key)
        if isinstance(value, str) and value.strip():
            params[key] = value
    return params


# ── 平台标准端点（§2.1） ────────────────────────────────────────────


@app.get("/health")
async def health():
    """探测上游 ComfyUI /system_stats；不通 → 503（外部服务未就绪语义直白）。"""
    try:
        ComfyClient, _ComfyClientError = _load_comfy_module()
    except ImportError as exc:
        logger.error("comfy_client unavailable: %s", exc)
        return JSONResponse(
            {"status": "unavailable", "comfyui": "unreachable"}, status_code=503
        )
    client = ComfyClient(_env_comfyui_url())
    try:
        await client.system_stats()
    except Exception:  # 连接失败/超时等 → 一律视为不可达
        return JSONResponse(
            {"status": "unavailable", "comfyui": "unreachable"}, status_code=503
        )
    finally:
        await _safe_aclose(client)
    return {"status": "ok", "comfyui": "reachable"}


@app.get("/info")
async def info():
    """模块信息 + ComfyUI 可达性/版本 + 队列摘要（§2.1）。"""
    try:
        ComfyClient, _ComfyClientError = _load_comfy_module()
    except ImportError as exc:
        raise StarletteHTTPException(503, f"comfyui client unavailable: {exc}")
    client = ComfyClient(_env_comfyui_url())
    reachable = False
    comfy_version: str | None = None
    running = pending = 0
    try:
        stats = await client.system_stats()
        reachable = True
        system = stats.get("system") if isinstance(stats, dict) else None
        if isinstance(system, dict):
            comfy_version = system.get("comfyui_version")
        queue = await client.queue_info()
        running = int(queue.get("running", 0) or 0)
        pending = int(queue.get("pending", 0) or 0)
    except Exception:
        pass  # 不可达 → reachable=False / 版本 null / 队列 0
    finally:
        await _safe_aclose(client)
    return {
        "module": MODULE_ID,
        "version": VERSION,
        "comfyui": {"reachable": reachable, "version": comfy_version},
        "queue": {"running": running, "pending": pending},
    }


# ── 工作流管理端点（§2.2） ──────────────────────────────────────────


@app.get("/workflows")
async def list_workflows():
    directory = _workflows_dir()
    items: list[dict[str, Any]] = []
    if directory.is_dir():
        for path in directory.glob("*.json"):
            try:
                stat = path.stat()
            except OSError:
                continue
            items.append(
                {
                    "name": _logical_workflow_name(path),
                    "size_bytes": int(stat.st_size),
                    "mtime": int(stat.st_mtime),
                }
            )
    items.sort(key=lambda item: _node_sort_key(item["name"]))
    return items


@app.post("/workflows")
async def upload_workflow(request: Request):
    form = await request.form()
    upload = form.get("file")
    if upload is None or not isinstance(upload, UploadFile):
        raise StarletteHTTPException(400, "missing multipart field 'file'")
    data = await upload.read()
    try:
        stem = clean_workflow_filename(upload.filename or "")
    except ValueError as exc:
        raise StarletteHTTPException(400, str(exc))
    try:
        workflow = json.loads(data.decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as exc:
        raise StarletteHTTPException(400, f"invalid workflow JSON: {exc}")
    problem = validate_workflow_format(workflow)
    if problem:
        raise StarletteHTTPException(400, f"invalid workflow format: {problem}")
    directory = _workflows_dir()
    try:
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / f"{stem}.json"
        replaced = path.exists()
        path.write_text(
            json.dumps(workflow, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
    except OSError as exc:
        raise StarletteHTTPException(500, f"failed to store workflow: {exc}")
    return {"name": stem, "replaced": replaced}


@app.delete("/workflows/{name}")
async def delete_workflow(name: str):
    try:
        path = _workflow_path(name)
    except ValueError as exc:
        raise StarletteHTTPException(400, str(exc))
    if not path.is_file():
        available = ", ".join(_list_workflow_names()) or "(none)"
        raise StarletteHTTPException(
            404, f'workflow "{name}" not found; available: [{available}]'
        )
    try:
        path.unlink()
    except OSError as exc:
        raise StarletteHTTPException(500, f"failed to delete workflow: {exc}")
    return {"ok": True}


# ── generate 全流程（§2.3） ─────────────────────────────────────────


async def _download_outputs(client: Any, images: list[dict], dest_dir: Path) -> list[Path]:
    """按序经 GET /view 下载全部产物（§2.3 第 6 步）。

    保留原名；本 run 内重名自动加序号（fetch_output 重名覆盖策略由调用方处理），
    第一个落盘的即主产物。
    """
    paths: list[Path] = []
    used: set[str] = set()
    for image in images:
        original = Path(str(image.get("filename") or "")).name
        if not original:
            raise StarletteHTTPException(
                502, "failed to fetch output: missing filename in history outputs"
            )
        subfolder = str(image.get("subfolder") or "")
        name = original
        if name in used:
            stem, suffix = os.path.splitext(name)
            seq = 1
            while True:
                candidate = f"{stem}_{seq}{suffix}"
                if candidate not in used:
                    name = candidate
                    break
                seq += 1
        if name == original:
            paths.append(await client.fetch_output(original, subfolder, dest_dir=dest_dir))
        else:
            # 重名：先落暂存目录再改为序号名，避免覆盖先下载的产物
            with tempfile.TemporaryDirectory(prefix="ep-bridge-", dir=dest_dir) as staging:
                staged = await client.fetch_output(original, subfolder, dest_dir=Path(staging))
                final = dest_dir / name
                shutil.move(str(staged), final)
                paths.append(final)
        used.add(name)
    return paths


def _collect_output_images(outputs: dict, selected_nodes: list[str]) -> list[dict]:
    """从 history outputs 收集待下载 image 描述（type=output）。"""
    images: list[dict] = []
    for node_id in selected_nodes:
        info = outputs.get(node_id)
        node_images = info.get("images") if isinstance(info, dict) else None
        for image in node_images or []:
            if isinstance(image, dict) and str(image.get("type", "output")) == "output":
                images.append(image)
    return images


@app.post("/predict/generate")
async def generate(request: Request):
    form = await request.form()
    params = _extract_generate_params(form)

    # 1) 参数与工作流模板
    workflow_name = params.get("workflow") or ""
    if not workflow_name:
        raise StarletteHTTPException(400, "missing parameter: workflow")
    try:
        mapping = parse_inject(params.get("inject"))
    except ValueError as exc:
        raise StarletteHTTPException(400, str(exc))
    try:
        wf_path = _workflow_path(workflow_name)
    except ValueError:
        wf_path = None
    if wf_path is None or not wf_path.is_file():
        available = ", ".join(_list_workflow_names()) or "(none)"
        raise StarletteHTTPException(
            400, f'workflow "{workflow_name}" not found; available: [{available}]'
        )
    try:
        workflow = json.loads(wf_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, ValueError) as exc:
        raise StarletteHTTPException(500, f"failed to load workflow file: {exc}")
    if not isinstance(workflow, dict):
        raise StarletteHTTPException(
            400, "invalid workflow format: top level must be a JSON object"
        )

    # 2) 注入前逐项校验键存在性（D6：不合法不提交给 ComfyUI）
    key_errors = validate_inject_keys(workflow, mapping)
    if key_errors:
        raise StarletteHTTPException(400, "invalid inject mapping: " + "; ".join(key_errors))

    # 3) 上游产物与落盘目录
    upstream = await _collect_upstream(form)
    base_url = params.get("base_url") or _env_comfyui_url()
    out_dir = _resolve_output_dir(params.get("output_path"))
    try:
        out_dir.mkdir(parents=True, exist_ok=True)
        if not os.access(out_dir, os.W_OK):
            raise OSError("directory is not writable")
    except OSError as exc:
        raise StarletteHTTPException(500, f"output dir not writable: {out_dir} ({exc})")

    try:
        ComfyClient, ComfyClientError = _load_comfy_module()
    except ImportError as exc:
        raise StarletteHTTPException(500, f"comfyui client unavailable: {exc}")

    client = ComfyClient(base_url)
    disconnected = [False]

    async def _watch_disconnect() -> None:
        # 客户端断开探测（§2.1：尽力 POST /interrupt）
        while True:
            if await request.is_disconnected():
                disconnected[0] = True
                return
            await asyncio.sleep(1.0)

    watcher = asyncio.create_task(_watch_disconnect())
    try:
        # 4) inject 解析 + 注入（文件类先上传）
        resolver = _make_resolver(upstream, client)
        try:
            workflow = await apply_inject_async(workflow, mapping, resolver)
        except ValueError as exc:
            raise StarletteHTTPException(400, str(exc))
        except ComfyClientError as exc:  # 文件上传阶段上游不可达/被拒等
            raise _map_client_error(exc)

        # 5) POST /prompt 提交
        try:
            prompt_id = await client.submit_prompt(workflow)
        except ComfyClientError as exc:
            raise _map_client_error(exc)

        # 6) 轮询 history（on_progress → EP-PROGRESS:NN%；断开 → 尽力 interrupt）
        poll_task = asyncio.create_task(
            client.poll_history(prompt_id, on_progress=_print_progress)
        )
        done, _pending = await asyncio.wait(
            {poll_task, watcher}, return_when=asyncio.FIRST_COMPLETED
        )
        if watcher in done and disconnected[0]:
            poll_task.cancel()
            with contextlib.suppress(BaseException):
                await poll_task
            await client.interrupt()  # best-effort
            raise StarletteHTTPException(499, "client disconnected; interrupt requested")
        try:
            entry = poll_task.result()
        except ComfyClientError as exc:
            raise _map_client_error(exc)

        # 7) 选取输出节点并下载全部产物
        outputs = entry.get("outputs") if isinstance(entry, dict) else None
        if not isinstance(outputs, dict):
            outputs = {}
        try:
            selected_nodes = resolve_output_nodes(
                workflow, outputs, params.get("output_nodes")
            )
        except ValueError as exc:
            raise StarletteHTTPException(400, str(exc))
        images = _collect_output_images(outputs, selected_nodes)
        if not images:
            raise StarletteHTTPException(502, "comfyui returned no output images")
        try:
            paths = await _download_outputs(client, images, out_dir)
        except ComfyClientError as exc:
            raise _map_client_error(exc)

        _print_progress(100)
        # 8) 主产物（第一个）绝对路径返回下游
        return {
            "status": "completed",
            "output_type": "file",
            "result": str(paths[0].resolve()),
        }
    finally:
        watcher.cancel()
        with contextlib.suppress(BaseException):
            await watcher
        await _safe_aclose(client)


if __name__ == "__main__":
    import uvicorn

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
    )
    host = os.environ.get("HOST") or os.environ.get("EP_HOST") or DEFAULT_HOST
    port = int(os.environ.get("EP_PORT") or DEFAULT_PORT)
    uvicorn.run(app, host=host, port=port, log_level="info")
