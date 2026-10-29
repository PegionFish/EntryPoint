"""adapter.py — OpenAI 兼容桥接模块适配器（EntryPoint 策略 C：桥接外部 HTTP 服务）。

编程依据：
    modules/openai-bridge/CONTRACT.md（本模块冻结契约）；
    reports/plan/2026-10-25-wave2-dispatch.md T-23（M1 范围）；
    reports/audit/2026-10-25-master-report.md 计划6补充（能力映射 / 端口语义 /
    出口逻辑三原则）；docs/ADAPTER_API.md §8（桥接模式）；docs/MODULE_SPEC.md
    §5（模块产物协议）§10（桥接模板）。

三能力 → 上游端点映射（计划6补充，M1 固定三能力）：
    chat        POST {base_url}/chat/completions     文本/SRT → 文本/SRT
    transcribe  POST {base_url}/audio/transcriptions 纯音频 → 文本/SRT/JSON
                （输入为 SRT 时：逐段文本变换经 chat/completions，
                  时间轴逐条对齐回填 —— 见 CONTRACT.md §4）
    speak       POST {base_url}/audio/speech         文本 → 音频文件

环境变量：
    EP_PORT / EP_HOST   监听端口与绑定地址（平台注入；恒回环）
    MODULE_DIR          模块目录（缺省 output 目录基准）
    EP_OUTPUT_DIR       产物落盘目录（平台引擎注入；params.output_path 优先）
    EP_WORKSPACE        任务工作目录（usage.json 侧产物优先落点）
    OPENAI_BASE_URL     缺省上游地址（params.base_url 优先；[compute.env] 注入）
    OPENAI_API_KEY      缺省密钥环境变量名（params.key_env 可指向任意变量名）

密钥纪律（T-23 验收：模块文件与导出零明文）：
    模块不存储、不落盘、不回显密钥；只接受运行时注入 ——
    params.api_key（运行时值）> params.key_env（环境变量名引用）> env OPENAI_API_KEY。
    所有错误信息经 _scrub 脱敏后才返回。
"""

from __future__ import annotations

import ipaddress
import json
import logging
import mimetypes
import os
import re
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

import httpx
from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from starlette.datastructures import UploadFile
from starlette.exceptions import HTTPException as StarletteHTTPException

import srt_realign as sr

logger = logging.getLogger("openai-bridge")

MODULE_ID = "openai-bridge"
VERSION = "0.1.0"
DEFAULT_PORT = 8186
DEFAULT_HOST = "127.0.0.1"
DEFAULT_OPENAI_BASE_URL = "https://api.openai.com/v1"
DEFAULT_TIMEOUT_SECS = 120.0
DEFAULT_SPEECH_FORMAT = "mp3"
_MAX_PATCH_CALLS = 32  # SRT 逐条补调上限（超出 → 明确报错，不静默错位）
_PROGRESS_TAG = "EP-PROGRESS"  # 前端解析约定（comfyui-bridge 同款格式）

app = FastAPI(title=MODULE_ID, version=VERSION, docs_url=None, redoc_url=None)

# ── 错误响应统一 {"error": ...}（ADAPTER_API §8 / comfyui-bridge §4.2 先例） ──


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


# ── 环境与目录（每次调用读 env，便于测试 monkeypatch 隔离） ─────────────


def _module_dir() -> Path:
    return Path(os.environ.get("MODULE_DIR") or Path(__file__).resolve().parent)


def _env_base_url() -> str:
    return os.environ.get("OPENAI_BASE_URL") or DEFAULT_OPENAI_BASE_URL


def _resolve_output_dir(params_output_path: str | None = None) -> Path:
    """产物/侧产物落盘基准：EP_WORKSPACE（任务工作区，usage.json 优先落点）
    不作 output 基准 —— 文件产物目录遵循：EP_OUTPUT_DIR > params.output_path
    父目录 > MODULE_DIR/output（comfyui-bridge 同序）。"""
    candidate = (
        os.environ.get("EP_OUTPUT_DIR")
        or (str(Path(params_output_path).expanduser().resolve().parent)
            if params_output_path else None)
        or str(_module_dir() / "output")
    )
    return Path(candidate).expanduser()


# ── 请求解包（multipart 与 JSON body 双格式，ADAPTER_API §2.3 A/B/C） ────

_KNOWN_PARAM_KEYS = (
    "model", "system_prompt", "temperature", "base_url", "api_key", "key_env",
    "timeout", "output_format", "output_path", "staging_dir", "context_in",
    "language", "prompt", "response_format", "voice", "speed",
)


@dataclass
class InputBundle:
    """一次 /predict 调用的解包结果。"""

    params: dict[str, Any] = field(default_factory=dict)
    files: list[tuple[str, str, bytes]] = field(default_factory=list)  # (字段名, 文件名, 内容)
    values: dict[str, str] = field(default_factory=dict)  # 非文件字段（含 JSON body 的 input/context_in）


async def _read_bundle(request: Request) -> InputBundle:
    """multipart → params JSON ∪ 平铺字段（平铺优先，comfyui-bridge 同款）；
    JSON body → params 对象 + 保留键（input/input_text/input_path/context_in/context）。
    """
    bundle = InputBundle()
    content_type = request.headers.get("content-type", "")
    if "multipart/form-data" in content_type:
        form = await request.form()
        raw_params = form.get("params")
        if isinstance(raw_params, str) and raw_params.strip():
            try:
                parsed = json.loads(raw_params)
            except ValueError as exc:
                raise StarletteHTTPException(400, f"invalid params JSON: {exc}")
            if not isinstance(parsed, dict):
                raise StarletteHTTPException(400, "invalid params: expected a JSON object")
            bundle.params.update(parsed)
        for key in _KNOWN_PARAM_KEYS:
            value = form.get(key)
            if isinstance(value, str) and value != "":
                bundle.params[key] = value
        for field_name, value in form.multi_items():
            if isinstance(value, UploadFile):
                bundle.files.append(
                    (field_name, value.filename or f"{field_name}.bin", await value.read())
                )
            elif isinstance(value, str) and field_name not in ("params",):
                bundle.values[field_name] = value
        return bundle

    try:
        body = await request.json()
    except Exception:
        raise StarletteHTTPException(400, "invalid request: body is not valid JSON")
    if not isinstance(body, dict):
        raise StarletteHTTPException(400, "invalid request: body must be a JSON object")
    params = body.get("params")
    if isinstance(params, dict):
        bundle.params.update(params)
    for key, value in body.items():
        if key == "params":
            continue
        if key in ("context_in", "context") and isinstance(value, list):
            # 列表上下文立即拍平为分块文本，避免与 JSON 可解析字符串歧义
            bundle.values[key] = "\n\n---\n\n".join(
                str(item) for item in value if str(item).strip()
            )
        elif isinstance(value, (str, int, float, bool)):
            bundle.values[key] = str(value)
        elif isinstance(value, (dict, list)):
            bundle.values[key] = json.dumps(value, ensure_ascii=False)
        else:
            bundle.values[key] = str(value)
    return bundle


def _param(bundle: InputBundle, key: str, default: Any = None) -> Any:
    value = bundle.params.get(key, default)
    return default if value is None or value == "" else value


# ── 密钥解析与脱敏（零明文落盘） ────────────────────────────────────────


def resolve_api_key(params: dict[str, Any]) -> str | None:
    """params.api_key（运行时值）> params.key_env（环境变量名引用）> env OPENAI_API_KEY。

    返回 None 表示未配置密钥（本地 vLLM/Ollama 等免鉴权实例的合法状态）。
    """
    direct = params.get("api_key")
    if isinstance(direct, str) and direct.strip():
        return direct.strip()
    env_name = params.get("key_env")
    if isinstance(env_name, str) and is_valid_env_name(env_name):
        value = os.environ.get(env_name, "")
        if value.strip():
            return value.strip()
    fallback = os.environ.get("OPENAI_API_KEY", "")
    return fallback.strip() or None


def is_valid_env_name(name: str) -> bool:
    """环境变量名合法性（防注入怪字符；仅允许常见 env 命名字符）。"""
    return bool(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name))


def _scrub(text: str, secrets: list[str | None]) -> str:
    """错误信息出站前脱敏：密钥值绝不回显（零明文纪律）。"""
    for secret in secrets:
        if secret:
            text = text.replace(secret, "***")
    return text


# ── 上游 HTTP（httpx；错误分类与映射对齐 comfyui-bridge §4.2） ───────────


class UpstreamError(Exception):
    """kind: connect | timeout | rejected | malformed"""

    def __init__(self, kind: str, message: str):
        super().__init__(message)
        self.kind = kind
        self.message = message


def _is_private_host(hostname: str | None) -> bool:
    """回环/私网判定：私网实例（vLLM/Ollama/LM Studio/ComfyUI 式本地服务）→ True。
    用于 /health 代理探测决策与 httpx trust_env（回环绕本机代理，comfyui-bridge §5）。"""
    if not hostname:
        return False
    if hostname.lower() == "localhost":
        return True
    try:
        ip = ipaddress.ip_address(hostname)
    except ValueError:
        return False
    return ip.is_private or ip.is_loopback


def _join_url(base_url: str, path: str) -> str:
    return base_url.rstrip("/") + path


def _make_client(base_url: str, timeout: float) -> httpx.AsyncClient:
    trust_env = not _is_private_host(urlparse(base_url).hostname)
    return httpx.AsyncClient(
        timeout=httpx.Timeout(timeout), trust_env=trust_env, follow_redirects=True
    )


def _map_upstream_error(exc: UpstreamError) -> StarletteHTTPException:
    status = 504 if exc.kind == "timeout" else 502
    prefix = {
        "connect": "openai unreachable",
        "timeout": "openai request timeout",
        "rejected": "openai rejected request",
        "malformed": "openai malformed response",
    }.get(exc.kind, "openai error")
    return StarletteHTTPException(status, f"{prefix}: {exc.message}")


@app.exception_handler(UpstreamError)
async def _upstream_error_handler(request: Request, exc: UpstreamError):
    """上游错误统一映射（CONTRACT §6）：connect/rejected/malformed → 502，timeout → 504。"""
    mapped = _map_upstream_error(exc)
    logger.warning("upstream error (%s): %s", exc.kind, exc.message)
    return JSONResponse({"error": mapped.detail}, status_code=mapped.status_code)


async def _upstream_request(
    client: httpx.AsyncClient,
    method: str,
    url: str,
    *,
    api_key: str | None,
    secrets: list[str | None],
    **kwargs: Any,
) -> httpx.Response:
    headers = dict(kwargs.pop("headers", {}))
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    try:
        response = await client.request(method, url, headers=headers, **kwargs)
    except httpx.ConnectError as exc:
        raise UpstreamError("connect", _scrub(str(exc), secrets))
    except (httpx.ConnectTimeout, httpx.ReadTimeout, httpx.WriteTimeout, httpx.PoolTimeout):
        raise UpstreamError("timeout", f"upstream did not answer within timeout ({url})")
    except httpx.TransportError as exc:  # 其余传输层异常按不可达处理
        raise UpstreamError("connect", _scrub(str(exc), secrets))
    if response.status_code >= 400:
        body = response.text[:500]
        raise UpstreamError(
            "rejected",
            f"HTTP {response.status_code}: {_scrub(body, secrets)}",
        )
    return response


# ── usage 记账（M1 收敛为模块侧产物；任务元数据记账留 M2） ───────────────


@dataclass
class UsageRecorder:
    """汇总本次 /predict 的上游 token 用量；每次上游调用 add() 一次。"""

    calls: list[dict[str, Any]] = field(default_factory=list)

    def add(self, endpoint: str, model: str | None, usage: Any) -> None:
        entry: dict[str, Any] = {"endpoint": endpoint, "model": model}
        if isinstance(usage, dict):
            entry["prompt_tokens"] = usage.get("prompt_tokens")
            entry["completion_tokens"] = usage.get("completion_tokens")
            entry["total_tokens"] = usage.get("total_tokens")
        else:
            entry["prompt_tokens"] = None  # 上游未返回 usage（如 /audio/speech）
            entry["completion_tokens"] = None
            entry["total_tokens"] = None
        self.calls.append(entry)

    def payload(self, capability: str, model: str | None) -> dict[str, Any]:
        totals = {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
        seen_any = False
        for call in self.calls:
            for key in totals:
                value = call.get(key)
                if isinstance(value, (int, float)):
                    totals[key] += int(value)
                    seen_any = True
        if not seen_any:
            totals = {k: None for k in totals}
        return {
            "module": MODULE_ID,
            "capability": capability,
            "model": model,
            "upstream_calls": len(self.calls),
            "calls": self.calls,
            "totals": totals,
        }


def _write_usage_json(
    bundle: InputBundle, payload: dict[str, Any]
) -> None:
    """usage.json 侧产物（不阻塞主产物；写入失败仅告警）。

    落点优先级：EP_WORKSPACE（任务工作目录，随任务归档）>
    params.output_path 同目录（文件产物模式）> MODULE_DIR/output/。
    内容仅含端点路径/模型名/token 数 —— 无密钥、无完整 URL（零明文纪律）。
    """
    dest_dir = (
        Path(os.environ["EP_WORKSPACE"])
        if os.environ.get("EP_WORKSPACE")
        else _resolve_output_dir(_param(bundle, "output_path"))
    )
    try:
        dest_dir.mkdir(parents=True, exist_ok=True)
        (dest_dir / "usage.json").write_text(
            json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
    except OSError as exc:
        logger.warning("usage.json side-artifact write failed: %s", exc)


def _completed(
    output_type: str,
    result: Any,
    *,
    output_path: str | None = None,
    elapsed: float | None = None,
    usage: dict[str, Any] | None = None,
) -> dict[str, Any]:
    body: dict[str, Any] = {
        "status": "completed",
        "output_type": output_type,
        "result": result,
        "output_path": output_path,
    }
    if elapsed is not None:
        body["elapsed_seconds"] = round(elapsed, 3)
    if usage is not None:
        body["usage"] = usage
    return body


def _progress(pct: int) -> None:
    print(f"{_PROGRESS_TAG}:{max(0, min(100, pct))}%", flush=True)


# ── 输入提取（chat/speak 文本；transcribe 音频或 SRT） ──────────────────

_CONTEXT_FIELD_NAMES = ("context_in", "context")


def _is_context_file(field_name: str, filename: str) -> bool:
    if field_name in _CONTEXT_FIELD_NAMES:
        return True
    return Path(filename or "").name.lower().startswith("context")


def _decode_text(data: bytes, filename: str) -> str:
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise StarletteHTTPException(
            400, f"invalid input: {filename} is not valid UTF-8 text ({exc})"
        )


def _collect_contexts(bundle: InputBundle) -> list[str]:
    """context_in 端口语义（计划6补充：可选参考上下文 → 注入 system 上下文）。

    接受：JSON body `context_in`/`context`（字符串或字符串数组）；
    multipart 名为 context_in 的字段；文件名以 context 开头的文件部件
    （现引擎多文件统一发 "file" 字段，按文件名甄别）；params.context_in。
    """
    contexts: list[str] = []
    for name in _CONTEXT_FIELD_NAMES:
        raw = bundle.values.get(name)
        if raw is not None and str(raw).strip():
            contexts.append(str(raw))
    for field_name, filename, content in bundle.files:
        if _is_context_file(field_name, filename):
            contexts.append(_decode_text(content, filename))
    param_ctx = bundle.params.get("context_in")
    if isinstance(param_ctx, str) and param_ctx.strip():
        contexts.append(param_ctx)
    return [c for c in contexts if c and c.strip()]


def _context_block(contexts: list[str]) -> str | None:
    if not contexts:
        return None
    joined = "\n\n---\n\n".join(contexts)
    return f"[Reference Context]\n{joined}"


def _main_text(bundle: InputBundle, *, capability: str) -> str:
    """主载荷文本：非 context 文件部件 > input/input_text > input_path。"""
    for field_name, filename, content in bundle.files:
        if not _is_context_file(field_name, filename):
            return _decode_text(content, filename)
    for name in ("input", "input_text"):
        if name in bundle.values and str(bundle.values[name]).strip():
            return str(bundle.values[name])
    input_path = bundle.values.get("input_path") or _param(bundle, "input_path")
    if input_path:
        path = Path(str(input_path)).expanduser()
        if not path.is_file():
            raise StarletteHTTPException(400, f"FILE_NOT_FOUND: input_path does not exist: {input_path}")
        return _decode_text(path.read_bytes(), path.name)
    raise StarletteHTTPException(
        400, f"missing input for capability '{capability}': "
        "provide multipart file, JSON 'input'/'input_text', or 'input_path'"
    )


# ── 上游调用封装 ────────────────────────────────────────────────────────


async def _chat_completions(
    client: httpx.AsyncClient,
    *,
    base_url: str,
    api_key: str | None,
    secrets: list[str | None],
    model: str,
    messages: list[dict[str, str]],
    temperature: float | None,
    response_format: str | None,
    recorder: UsageRecorder,
) -> str:
    body: dict[str, Any] = {"model": model, "messages": messages}
    if temperature is not None:
        body["temperature"] = temperature
    if response_format:
        body["response_format"] = response_format
    response = await _upstream_request(
        client, "POST", _join_url(base_url, "/chat/completions"),
        api_key=api_key, secrets=secrets, json=body,
    )
    try:
        data = response.json()
        content = data["choices"][0]["message"]["content"]
    except (ValueError, KeyError, IndexError, TypeError) as exc:
        raise UpstreamError(
            "malformed", f"chat/completions response missing choices[0].message.content ({exc})"
        )
    recorder.add("chat/completions", model, data.get("usage") if isinstance(data, dict) else None)
    if not isinstance(content, str):
        raise UpstreamError("malformed", "chat/completions content is not a string")
    return content


def _build_srt_from_segments(segments: list[dict[str, Any]]) -> str:
    """上游 verbose_json segments → SRT（faster-whisper 同款时间戳格式）。"""
    def ts(seconds: Any) -> str:
        ms = max(0, int(round(float(seconds or 0) * 1000)))
        h, ms = divmod(ms, 3600_000)
        m, ms = divmod(ms, 60_000)
        s, ms = divmod(ms, 1000)
        return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"

    lines: list[str] = []
    for idx, seg in enumerate(segments, start=1):
        lines.append(str(idx))
        lines.append(f"{ts(seg.get('start'))} --> {ts(seg.get('end'))}")
        lines.append(str(seg.get("text", "")).strip())
        lines.append("")
    return "\n".join(lines)


# ── SRT 就地重组编排（chat 与 transcribe-SRT 共用） ─────────────────────


async def _realign_via_chat(
    client: httpx.AsyncClient,
    *,
    base_url: str,
    api_key: str | None,
    secrets: list[str | None],
    model: str,
    temperature: float | None,
    cores: list[str],
    task_prompt: str | None,
    context_block: str | None,
    recorder: UsageRecorder,
) -> list[str]:
    """核心正文列表 → 上游变换后的核心正文列表（与输入一一对应）。

    主路径：一次批量调用（##N## 协议，保上下文连贯、省调用数）；
    兜底：映射缺失的序号逐条补调（上限 _MAX_PATCH_CALLS，超出明错不静默错位）。
    """
    system = sr.realign_system_prompt(task_prompt, context_block)
    content = await _chat_completions(
        client, base_url=base_url, api_key=api_key, secrets=secrets, model=model,
        messages=[
            {"role": "system", "content": system},
            {"role": "user", "content": sr.build_numbered_payload(cores)},
        ],
        temperature=temperature, response_format=None, recorder=recorder,
    )
    mapping, missing = sr.parse_numbered_response(content, len(cores))
    if missing:
        if len(missing) > _MAX_PATCH_CALLS:
            raise UpstreamError(
                "malformed",
                f"SRT realignment failed: response mapped {len(cores) - len(missing)}/"
                f"{len(cores)} entries; {len(missing)} missing exceeds patch limit "
                f"{_MAX_PATCH_CALLS} — refusing to misalign timeline",
            )
        patch_system = (task_prompt or "").strip() or sr.DEFAULT_TASK_PROMPT
        for index in sorted(missing):
            single = await _chat_completions(
                client, base_url=base_url, api_key=api_key, secrets=secrets, model=model,
                messages=[
                    {"role": "system", "content": patch_system},
                    {"role": "user", "content": cores[index - 1]},
                ],
                temperature=temperature, response_format=None, recorder=recorder,
            )
            single = single.strip()
            if not single:
                raise UpstreamError(
                    "malformed",
                    f"SRT realignment failed: empty transformation for entry {index}",
                )
            mapping[index] = single
    return [mapping[i] for i in range(1, len(cores) + 1)]


async def _realign_srt_text(
    bundle: InputBundle,
    srt_text: str,
    *,
    base_url: str,
    api_key: str | None,
    secrets: list[str | None],
    model: str,
    recorder: UsageRecorder,
) -> str:
    """SRT 就地重组总入口：解析 → 保护标签剥离 → 批量变换 → 逐条回填 → 渲染。

    时间轴行与序号行原文保留（三原则 1/2）；整条包裹标签与行首 {...} 由
    代码级保护（三原则 3）；正文行内标签随正文直送（best-effort）。
    """
    try:
        doc = sr.parse_srt(srt_text)
    except ValueError as exc:
        raise StarletteHTTPException(400, str(exc))

    protected_entries = []
    cores: list[str] = []
    for entry in doc.entries:
        protected = sr.extract_protected("\n".join(entry.text_lines))
        if not protected.core.strip():
            # 纯标签条目（如只有 {\an8}）：无需上游变换，原样保留
            protected_entries.append(None)
            cores.append("")
        else:
            protected_entries.append(protected)
            cores.append(protected.core)

    active = [i for i, core in enumerate(cores) if core]
    temperature = _temperature_param(bundle)
    if active:
        client_timeout = _timeout_param(bundle)
        async with _make_client(base_url, client_timeout) as client:
            task_prompt = _param(bundle, "system_prompt")
            transformed = await _realign_via_chat(
                client, base_url=base_url, api_key=api_key, secrets=secrets, model=model,
                temperature=temperature,
                cores=[cores[i] for i in active],
                task_prompt=task_prompt,
                context_block=_context_block(_collect_contexts(bundle)),
                recorder=recorder,
            )
        for pos, index in enumerate(active):
            cores[index] = transformed[pos]

    for index, entry in enumerate(doc.entries):
        protected = protected_entries[index]
        if protected is None:
            continue  # 纯标签条目（如只有 {\an8}）：原样保留，不经上游
        restored = sr.restore_protected(protected, cores[index])
        entry.text_lines = restored.split("\n")
    return sr.render_srt(doc)


# ── 通用参数解析 ────────────────────────────────────────────────────────


def _require_model(bundle: InputBundle) -> str:
    model = _param(bundle, "model")
    if not model:
        raise StarletteHTTPException(
            400, "missing parameter: model (upstream model name, e.g. deepseek-chat / whisper-1 / tts-1)"
        )
    return str(model)


def _resolve_base_url(bundle: InputBundle) -> str:
    return str(_param(bundle, "base_url") or _env_base_url())


def _temperature_param(bundle: InputBundle) -> float | None:
    raw = bundle.params.get("temperature")
    if raw is None or raw == "":
        return None
    try:
        value = float(raw)
    except (TypeError, ValueError):
        raise StarletteHTTPException(400, f"invalid params: temperature must be a number, got {raw!r}")
    if not 0.0 <= value <= 2.0:
        raise StarletteHTTPException(400, f"invalid params: temperature must be within [0, 2], got {value}")
    return value


def _timeout_param(bundle: InputBundle) -> float:
    raw = bundle.params.get("timeout")
    if raw is None or raw == "":
        return DEFAULT_TIMEOUT_SECS
    try:
        value = float(raw)
    except (TypeError, ValueError):
        raise StarletteHTTPException(400, f"invalid params: timeout must be a number, got {raw!r}")
    if value <= 0:
        raise StarletteHTTPException(400, f"invalid params: timeout must be positive, got {value}")
    return value


def _write_file_output(
    bundle: InputBundle, content: str | bytes, *, fallback_ext: str
) -> Path:
    """文件产物落盘（MODULE_SPEC §5 产物协议）：params.output_path 优先。"""
    output_path = _param(bundle, "output_path")
    if output_path:
        path = Path(str(output_path)).expanduser()
    else:
        path = _resolve_output_dir(None) / f"output.{fallback_ext}"
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(content, str):
        path.write_text(content, encoding="utf-8")
    else:
        path.write_bytes(content)
    return path


# ── 三能力端点 ──────────────────────────────────────────────────────────


@app.post("/predict/chat")
async def predict_chat(request: Request):
    bundle = await _read_bundle(request)
    model = _require_model(bundle)
    base_url = _resolve_base_url(bundle)
    api_key = resolve_api_key(bundle.params)
    secrets = [api_key]
    recorder = UsageRecorder()
    output_format = str(_param(bundle, "output_format") or "").lower()
    _progress(5)
    t0 = time.perf_counter()

    main_text = _main_text(bundle, capability="chat")

    if sr.looks_like_srt(main_text):
        # 格式就地保持（三原则 2）：SRT 入 → SRT 出，时间轴逐条回填
        srt_out = await _realign_srt_text(
            bundle, main_text, base_url=base_url, api_key=api_key,
            secrets=secrets, model=model, recorder=recorder,
        )
        usage_payload = recorder.payload("chat", model)
        _write_usage_json(bundle, usage_payload)
        _progress(100)
        if _param(bundle, "output_path") or output_format in ("srt", "txt"):
            written = _write_file_output(bundle, srt_out, fallback_ext=output_format or "srt")
            return _completed("file", str(written.resolve()), output_path=str(written.resolve()),
                              elapsed=time.perf_counter() - t0, usage=usage_payload)
        return _completed("text", srt_out, elapsed=time.perf_counter() - t0, usage=usage_payload)

    # 普通文本：原始保真（三原则 3）—— 响应全文原样作为主产物
    # context_in 端口语义：参考上下文注入 system 上下文（计划6补充）
    system_parts: list[str] = []
    system_prompt = str(_param(bundle, "system_prompt") or "").strip()
    if system_prompt:
        system_parts.append(system_prompt)
    context_block = _context_block(_collect_contexts(bundle))
    if context_block:
        system_parts.append(context_block)
    messages: list[dict[str, str]] = []
    if system_parts:
        messages.append({"role": "system", "content": "\n\n".join(system_parts)})
    messages.append({"role": "user", "content": main_text})

    client_timeout = _timeout_param(bundle)
    async with _make_client(base_url, client_timeout) as client:
        content = await _chat_completions(
            client, base_url=base_url, api_key=api_key, secrets=secrets, model=model,
            messages=messages, temperature=_temperature_param(bundle),
            response_format=_param(bundle, "response_format"), recorder=recorder,
        )
    usage_payload = recorder.payload("chat", model)
    _write_usage_json(bundle, usage_payload)
    _progress(100)
    if output_format not in ("", "json") and _param(bundle, "output_path"):
        written = _write_file_output(bundle, content, fallback_ext=output_format or "txt")
        return _completed("file", str(written.resolve()), output_path=str(written.resolve()),
                          elapsed=time.perf_counter() - t0, usage=usage_payload)
    return _completed("text", content, elapsed=time.perf_counter() - t0, usage=usage_payload)


@app.post("/predict/transcribe")
async def predict_transcribe(request: Request):
    bundle = await _read_bundle(request)
    model = _require_model(bundle)
    base_url = _resolve_base_url(bundle)
    api_key = resolve_api_key(bundle.params)
    secrets = [api_key]
    recorder = UsageRecorder()
    output_format = str(_param(bundle, "output_format") or "").lower()
    _progress(5)
    t0 = time.perf_counter()

    # ── 输入甄别：SRT（就地重组）或纯音频（上游转写） ──────────────────
    srt_text: str | None = None
    audio: tuple[str, bytes] | None = None
    for field_name, filename, content in bundle.files:
        if _is_context_file(field_name, filename):
            continue
        if filename.lower().endswith(".srt"):
            srt_text = _decode_text(content, filename)
        else:
            audio = (filename, content)
        break  # 引擎多文件统一 "file" 字段：首个非 context 部件即主输入
    if srt_text is None and audio is None:
        input_path = bundle.values.get("input_path") or _param(bundle, "input_path")
        raw_input = bundle.values.get("input")
        if input_path and str(input_path).lower().endswith(".srt"):
            path = Path(str(input_path)).expanduser()
            if not path.is_file():
                raise StarletteHTTPException(400, f"FILE_NOT_FOUND: input_path does not exist: {input_path}")
            srt_text = _decode_text(path.read_bytes(), path.name)
        elif raw_input and sr.looks_like_srt(str(raw_input)):
            srt_text = str(raw_input)
        elif input_path:
            path = Path(str(input_path)).expanduser()
            if not path.is_file():
                raise StarletteHTTPException(400, f"FILE_NOT_FOUND: input_path does not exist: {input_path}")
            audio = (path.name, path.read_bytes())
        else:
            raise StarletteHTTPException(
                400, "missing input for capability 'transcribe': "
                "provide an audio file (multipart/JSON input_path) or SRT input"
            )

    if srt_text is not None:
        # SRT 入 → SRT 出（逐段文本变换经上游 chat 端点；时间轴逐条对齐回填）
        srt_out = await _realign_srt_text(
            bundle, srt_text, base_url=base_url, api_key=api_key,
            secrets=secrets, model=model, recorder=recorder,
        )
        usage_payload = recorder.payload("transcribe", model)
        _write_usage_json(bundle, usage_payload)
        _progress(100)
        if _param(bundle, "output_path") or output_format in ("srt", "txt"):
            written = _write_file_output(bundle, srt_out, fallback_ext=output_format or "srt")
            return _completed("file", str(written.resolve()), output_path=str(written.resolve()),
                              elapsed=time.perf_counter() - t0, usage=usage_payload)
        return _completed("text", srt_out, elapsed=time.perf_counter() - t0, usage=usage_payload)

    # ── 纯音频：POST {base}/audio/transcriptions（透传上游文本/SRT） ────
    filename, content = audio
    response_format = str(_param(bundle, "response_format") or "").lower() or None
    if response_format is None and output_format == "srt":
        response_format = "srt"  # 上游直接产 SRT —— 输出上游返回的 SRT（T-23 语义）
    if response_format is None:
        response_format = "json"

    mime = mimetypes.guess_type(filename)[0] or "application/octet-stream"
    upstream_files = {"file": (filename, content, mime)}
    upstream_data: dict[str, str] = {"model": model, "response_format": response_format}
    language = _param(bundle, "language")
    if language and str(language).lower() != "auto":
        upstream_data["language"] = str(language)
    prompt = _param(bundle, "prompt")
    if prompt:
        upstream_data["prompt"] = str(prompt)
    temperature = _temperature_param(bundle)
    if temperature is not None:
        upstream_data["temperature"] = str(temperature)

    client_timeout = _timeout_param(bundle)
    async with _make_client(base_url, client_timeout) as client:
        response = await _upstream_request(
            client, "POST", _join_url(base_url, "/audio/transcriptions"),
            api_key=api_key, secrets=secrets, files=upstream_files, data=upstream_data,
        )

    if response_format in ("srt", "vtt", "text"):
        text = response.text
        recorder.add("audio/transcriptions", model, None)
        usage_payload = recorder.payload("transcribe", model)
        _write_usage_json(bundle, usage_payload)
        _progress(100)
        if output_format not in ("", "json"):
            written = _write_file_output(bundle, text, fallback_ext=output_format or "srt")
            return _completed("file", str(written.resolve()), output_path=str(written.resolve()),
                              elapsed=time.perf_counter() - t0, usage=usage_payload)
        return _completed("text", text, elapsed=time.perf_counter() - t0, usage=usage_payload)

    try:
        data = response.json()
    except ValueError as exc:
        raise UpstreamError("malformed", f"audio/transcriptions response is not JSON ({exc})")
    recorder.add("audio/transcriptions", model, data.get("usage") if isinstance(data, dict) else None)
    text = str(data.get("text", ""))
    segments = data.get("segments") if isinstance(data, dict) else None

    if output_format == "srt":
        if isinstance(segments, list) and segments:
            srt_out = _build_srt_from_segments(segments)
        else:
            raise UpstreamError(
                "malformed",
                "upstream returned no segments; cannot build SRT "
                "(set params.response_format='srt' to let the upstream produce the timeline)",
            )
        usage_payload = recorder.payload("transcribe", model)
        _write_usage_json(bundle, usage_payload)
        _progress(100)
        written = _write_file_output(bundle, srt_out, fallback_ext="srt")
        return _completed("file", str(written.resolve()), output_path=str(written.resolve()),
                          elapsed=time.perf_counter() - t0, usage=usage_payload)

    usage_payload = recorder.payload("transcribe", model)
    _write_usage_json(bundle, usage_payload)
    _progress(100)
    output_format_txt = output_format if output_format in ("txt", "vtt") else ""
    if output_format_txt:
        written = _write_file_output(bundle, text, fallback_ext=output_format_txt)
        return _completed("file", str(written.resolve()), output_path=str(written.resolve()),
                          elapsed=time.perf_counter() - t0, usage=usage_payload)
    result: dict[str, Any] = {"text": text}
    if isinstance(segments, list):
        result["segments"] = segments
    if isinstance(data, dict):
        for key in ("language", "duration"):
            if key in data:
                result[key] = data[key]
    return _completed("json", result, elapsed=time.perf_counter() - t0, usage=usage_payload)


@app.post("/predict/speak")
async def predict_speak(request: Request):
    bundle = await _read_bundle(request)
    model = _require_model(bundle)
    base_url = _resolve_base_url(bundle)
    api_key = resolve_api_key(bundle.params)
    secrets = [api_key]
    recorder = UsageRecorder()
    _progress(5)
    t0 = time.perf_counter()

    text = _main_text(bundle, capability="speak")
    voice = str(_param(bundle, "voice") or "alloy")
    speech_format = str(_param(bundle, "response_format") or DEFAULT_SPEECH_FORMAT).lower()
    body: dict[str, Any] = {"model": model, "input": text, "voice": voice,
                            "response_format": speech_format}
    speed = bundle.params.get("speed")
    if speed not in (None, ""):
        try:
            body["speed"] = float(speed)
        except (TypeError, ValueError):
            raise StarletteHTTPException(400, f"invalid params: speed must be a number, got {speed!r}")

    client_timeout = _timeout_param(bundle)
    async with _make_client(base_url, client_timeout) as client:
        response = await _upstream_request(
            client, "POST", _join_url(base_url, "/audio/speech"),
            api_key=api_key, secrets=secrets, json=body,
        )
    audio_bytes = response.content
    if not audio_bytes:
        raise UpstreamError("malformed", "audio/speech returned an empty body")
    recorder.add("audio/speech", model, None)  # speech 端点不返回 token usage
    usage_payload = recorder.payload("speak", model)
    _write_usage_json(bundle, usage_payload)
    _progress(100)
    written = _write_file_output(bundle, audio_bytes, fallback_ext=speech_format)
    return _completed("file", str(written.resolve()), output_path=str(written.resolve()),
                      elapsed=time.perf_counter() - t0, usage=usage_payload)


# ── 平台标准端点 ────────────────────────────────────────────────────────


@app.get("/health")
async def health():
    """桥接模块健康语义（ADAPTER_API §8.3）：

    - 私网/回环上游（vLLM/Ollama/LM Studio 等用户自启实例）：代理探测
      GET {base}/models，不可达 → 503 {"status":"unavailable"}（失败语义直白）；
    - 公网云上游：不探测（健康检查不得依赖外网/消耗配额）→ 200，
      上游可用性在调用时以明确 502/504 暴露。
    """
    base_url = _env_base_url()
    if _is_private_host(urlparse(base_url).hostname):
        try:
            async with _make_client(base_url, 5.0) as client:
                # 任意 HTTP 响应（含 401/404）= 实例可达；鉴权问题在调用时暴露
                await client.get(_join_url(base_url, "/models"))
        except httpx.TransportError:
            return JSONResponse(
                {"status": "unavailable", "openai": "unreachable", "base_url": base_url},
                status_code=503,
            )
        return {"status": "ok", "openai": "reachable", "base_url": base_url}
    return {"status": "ok", "openai": "skipped-public-probe", "base_url": base_url}


@app.get("/info")
async def info():
    return {
        "module_id": MODULE_ID,
        "name": "OpenAI 兼容桥接",
        "version": VERSION,
        "device": "cpu",
        "backend": "cpu",
        "capabilities": [
            {
                "name": "chat",
                "input_type": "text",
                "output_type": "text",
                "params": {
                    "model": {"type": "string", "description": "上游模型名（如 deepseek-chat）"},
                    "system_prompt": {"type": "string", "description": "任务指令（如“翻译成简体中文”）"},
                    "context_in": {"type": "string", "description": "可选参考上下文（context_in 端口的 params 注入形式）"},
                    "temperature": {"type": "float", "min": 0.0, "max": 2.0, "description": "采样温度"},
                    "base_url": {"type": "string", "description": "覆盖 OPENAI_BASE_URL（须含版本段，如 …/v1）"},
                    "api_key": {"type": "string", "description": "运行时注入密钥（勿落盘，优先用 key_env）"},
                    "key_env": {"type": "string", "description": "密钥环境变量名引用（key_ref 形态）"},
                    "timeout": {"type": "float", "min": 1, "description": "上游超时秒数"},
                },
            },
            {
                "name": "transcribe",
                "input_type": "audio",
                "output_type": "text",
                "params": {
                    "model": {"type": "string", "description": "上游 ASR 模型名（如 whisper-1）"},
                    "language": {"type": "string", "description": "语言代码或 auto"},
                    "prompt": {"type": "string", "description": "转写提示词（领域词引导）"},
                    "response_format": {"type": "select", "options": ["json", "verbose_json", "srt", "vtt", "text"]},
                    "base_url": {"type": "string", "description": "覆盖 OPENAI_BASE_URL"},
                    "api_key": {"type": "string", "description": "运行时注入密钥（勿落盘）"},
                    "key_env": {"type": "string", "description": "密钥环境变量名引用"},
                    "timeout": {"type": "float", "min": 1, "description": "上游超时秒数"},
                },
            },
            {
                "name": "speak",
                "input_type": "text",
                "output_type": "audio",
                "params": {
                    "model": {"type": "string", "description": "上游 TTS 模型名（如 tts-1）"},
                    "voice": {"type": "string", "description": "音色（如 alloy）"},
                    "response_format": {"type": "select", "options": ["mp3", "opus", "aac", "flac", "wav", "pcm"]},
                    "speed": {"type": "float", "min": 0.25, "max": 4.0, "description": "语速"},
                    "base_url": {"type": "string", "description": "覆盖 OPENAI_BASE_URL"},
                    "api_key": {"type": "string", "description": "运行时注入密钥（勿落盘）"},
                    "key_env": {"type": "string", "description": "密钥环境变量名引用"},
                    "timeout": {"type": "float", "min": 1, "description": "上游超时秒数"},
                },
            },
        ],
    }


if __name__ == "__main__":
    import uvicorn

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
    )
    host = os.environ.get("HOST") or os.environ.get("EP_HOST") or DEFAULT_HOST
    port = int(os.environ.get("EP_PORT") or DEFAULT_PORT)
    uvicorn.run(app, host=host, port=port, log_level="info")
