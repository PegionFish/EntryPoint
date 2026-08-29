"""ComfyUI HTTP 客户端封装 —— CONTRACT.md §5 冻结签名（F1）。

封装 ComfyUI REST 端点（§4.1）：
GET /system_stats · GET /queue · POST /upload/image · POST /prompt ·
GET /history/{prompt_id}（退避轮询）· GET /view（产物下载）· POST /interrupt。

代理策略（§5 末尾）：base_url 为回环地址（127.0.0.1 / localhost / ::1）时对
httpx 显式 trust_env=False 绕过本机代理；远程地址尊重环境代理（HTTP_PROXY 等）。

错误分类（ComfyClientError.kind）：
- connect   ComfyUI 不可达 / 传输层失败 / 意外的服务端 5xx
- rejected  ComfyUI 以 4xx 明确拒绝（提交 /prompt、上传被拒等）
- execution 工作流在 ComfyUI 侧执行失败（history status_str=error / execution_error）
- timeout   poll_history 轮询超时
- output    产物下载 / 落盘失败
"""

from __future__ import annotations

import asyncio
import ipaddress
from pathlib import Path
from urllib.parse import urlparse

import httpx

__all__ = ["ComfyClientError", "ComfyClient"]

KINDS = ("connect", "rejected", "execution", "timeout", "output")


class ComfyClientError(Exception):
    """comfy_client 分类错误，kind ∈ connect / rejected / execution / timeout / output。

    ``message`` 为面向 adapter 错误映射（§4.2）的技术信息（英文）。
    """

    def __init__(self, kind: str, message: str = ""):
        self.kind = kind
        self.message = message
        super().__init__(message or kind)


def _is_loopback(base_url: str) -> bool:
    """判断 base_url 的主机部分是否为回环地址。"""
    try:
        host = urlparse(base_url).hostname or ""
    except ValueError:  # 畸形 IPv6 等
        return False
    if not host:
        return False
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:  # 主机名（非 IP）按远程处理
        return False


def _summarize_prompt_rejection(resp: httpx.Response) -> str:
    """从 POST /prompt 的 4xx 响应体提取 node_errors 摘要（§4.2）。"""
    try:
        payload = resp.json()
    except ValueError:
        return f"comfyui rejected prompt: HTTP {resp.status_code}: {resp.text[:200]}"
    parts: list[str] = []
    err = payload.get("error") if isinstance(payload, dict) else None
    if isinstance(err, dict):
        msg = err.get("message") or err.get("type")
        if msg:
            parts.append(str(msg))
    elif isinstance(err, str):
        parts.append(err)
    node_errors = payload.get("node_errors") if isinstance(payload, dict) else None
    if isinstance(node_errors, dict) and node_errors:
        node_parts = []
        for node_id, info in list(node_errors.items())[:5]:
            first = None
            if isinstance(info, dict):
                errors = info.get("errors") or []
                if errors and isinstance(errors[0], dict):
                    first = errors[0].get("message") or errors[0].get("type")
                first = first or info.get("details") or "error"
            else:
                first = str(info)
            node_parts.append(f"node {node_id}: {first}")
        parts.append("; ".join(node_parts))
    summary = "; ".join(p for p in parts if p)
    if summary:
        return f"comfyui rejected prompt: {summary}"
    return f"comfyui rejected prompt: HTTP {resp.status_code}"


def _execution_error_summary(entry: dict) -> str:
    """从 history 条目的 status 中提取 execution_error 摘要。"""
    status = entry.get("status")
    if not isinstance(status, dict):
        return "unknown execution error"
    for msg in status.get("messages") or []:
        if isinstance(msg, (list, tuple)) and msg and msg[0] == "execution_error":
            detail = msg[1] if len(msg) > 1 and isinstance(msg[1], dict) else {}
            node_type = detail.get("node_type", "?")
            exception = detail.get("exception_message") or detail.get("exception_type") or "unknown error"
            return f"{node_type}: {exception}"
    return "status_str=error"


class ComfyClient:
    """ComfyUI REST 异步客户端（httpx.AsyncClient）。

    用法::

        async with ComfyClient("http://127.0.0.1:8188") as client:
            stats = await client.system_stats()
    """

    def __init__(self, base_url: str, timeout: float = 30.0):
        self.base_url = base_url.rstrip("/")
        self.timeout = float(timeout)
        # 回环地址绕过本机代理；远程尊重环境代理（CONTRACT.md §5 末尾）
        self.trust_env = not _is_loopback(base_url)
        self._client: httpx.AsyncClient | None = None

    # ---------- 生命周期（契约之外的辅助方法，只增不改） ----------

    def _http(self) -> httpx.AsyncClient:
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(trust_env=self.trust_env, timeout=self.timeout)
        return self._client

    async def aclose(self) -> None:
        if self._client is not None and not self._client.is_closed:
            await self._client.aclose()
        self._client = None

    async def __aenter__(self) -> "ComfyClient":
        self._http()
        return self

    async def __aexit__(self, *exc_info) -> None:
        await self.aclose()

    # ---------- §5 冻结签名表 ----------

    async def system_stats(self) -> dict:
        """GET /system_stats；不通抛 ComfyClientError('connect')。"""
        client = self._http()
        try:
            resp = await client.get(f"{self.base_url}/system_stats")
        except httpx.HTTPError as exc:
            raise ComfyClientError("connect", f"comfyui unreachable: {exc}") from exc
        if resp.status_code != 200:
            raise ComfyClientError("connect", f"comfyui unreachable: HTTP {resp.status_code}")
        try:
            return resp.json()
        except ValueError as exc:
            raise ComfyClientError("connect", f"system_stats returned invalid JSON: {exc}") from exc

    async def queue_info(self) -> dict:
        """GET /queue → {'running': int, 'pending': int}。"""
        client = self._http()
        try:
            resp = await client.get(f"{self.base_url}/queue")
        except httpx.HTTPError as exc:
            raise ComfyClientError("connect", f"comfyui unreachable: {exc}") from exc
        if resp.status_code != 200:
            raise ComfyClientError("connect", f"failed to get queue: HTTP {resp.status_code}")
        try:
            data = resp.json()
        except ValueError as exc:
            raise ComfyClientError("connect", f"queue returned invalid JSON: {exc}") from exc
        running = data.get("queue_running") if isinstance(data, dict) else None
        pending = data.get("queue_pending") if isinstance(data, dict) else None
        return {
            "running": len(running) if isinstance(running, (list, tuple)) else 0,
            "pending": len(pending) if isinstance(pending, (list, tuple)) else 0,
        }

    async def upload_image(self, file_bytes: bytes, filename: str) -> str:
        """POST /upload/image（multipart 字段 image，附加 type=input）→ 返回服务器侧文件名。"""
        client = self._http()
        files = {"image": (filename, file_bytes)}
        data = {"type": "input", "overwrite": "true"}
        try:
            resp = await client.post(f"{self.base_url}/upload/image", files=files, data=data)
        except httpx.HTTPError as exc:
            raise ComfyClientError("connect", f"upload failed: {exc}") from exc
        if resp.status_code != 200:
            raise ComfyClientError("rejected", f"upload rejected: HTTP {resp.status_code}: {resp.text[:200]}")
        try:
            name = resp.json()["name"]
        except (ValueError, KeyError, TypeError) as exc:
            raise ComfyClientError("output", f"unexpected upload response (missing name): {exc}") from exc
        return str(name)

    async def submit_prompt(self, workflow: dict) -> str:
        """POST /prompt {'prompt': workflow} → prompt_id；4xx 抛 'rejected'（含 node_errors 摘要）。"""
        client = self._http()
        try:
            resp = await client.post(f"{self.base_url}/prompt", json={"prompt": workflow})
        except httpx.HTTPError as exc:
            raise ComfyClientError("connect", f"comfyui unreachable: {exc}") from exc
        if resp.status_code == 200:
            try:
                prompt_id = resp.json()["prompt_id"]
            except (ValueError, KeyError, TypeError) as exc:
                raise ComfyClientError("output", f"unexpected /prompt response (missing prompt_id): {exc}") from exc
            return str(prompt_id)
        if 400 <= resp.status_code < 500:
            raise ComfyClientError("rejected", _summarize_prompt_rejection(resp))
        raise ComfyClientError("connect", f"comfyui /prompt server error: HTTP {resp.status_code}")

    async def poll_history(self, prompt_id: str, *, interval0: float = 1.0,
                           interval_max: float = 5.0, timeout: float = 1800.0,
                           on_progress=None) -> dict:
        """轮询 GET /history/{prompt_id} 至终态。

        - 空 history（空对象 / 无该 prompt_id 条目）= 未完成；
        - status.status_str == "error" 或 messages 含 execution_error →
          抛 ComfyClientError('execution')；
        - 超过 timeout 秒仍无终态 → 抛 ComfyClientError('timeout')；
        - 轮询间隔自 interval0 起指数退避（×2）至 interval_max 封顶；
        - 每轮通过 on_progress(pct: int) 上报心跳（5%~95% 按已用时长/timeout 估算），
          完成时回调 on_progress(100)；
        - 返回该 prompt_id 的 history 条目（含 outputs/status）。
        """
        client = self._http()
        loop = asyncio.get_running_loop()
        start = loop.time()
        deadline = start + max(timeout, 0.0)
        interval = max(interval0, 0.01)
        cap = max(interval_max, interval)

        while True:
            try:
                resp = await client.get(f"{self.base_url}/history/{prompt_id}")
            except httpx.HTTPError as exc:
                raise ComfyClientError(
                    "connect", f"comfyui unreachable while polling history: {exc}") from exc
            if resp.status_code != 200:
                raise ComfyClientError("connect", f"history poll failed: HTTP {resp.status_code}")
            try:
                data = resp.json()
            except ValueError as exc:
                raise ComfyClientError("connect", f"history poll returned invalid JSON: {exc}") from exc

            entry = data.get(prompt_id) if isinstance(data, dict) else None
            if isinstance(entry, dict) and entry:
                status = entry.get("status")
                status_str = status.get("status_str") if isinstance(status, dict) else None
                has_exec_error = isinstance(status, dict) and any(
                    isinstance(m, (list, tuple)) and m and m[0] == "execution_error"
                    for m in (status.get("messages") or [])
                )
                if status_str == "error" or has_exec_error:
                    raise ComfyClientError(
                        "execution",
                        f"comfyui execution error (prompt {prompt_id}): "
                        f"{_execution_error_summary(entry)}",
                    )
                if on_progress is not None:
                    on_progress(100)
                return entry

            # 未完成：心跳 + 超时判定
            now = loop.time()
            if on_progress is not None:
                if timeout > 0:
                    pct = min(95, max(5, int(95 * (now - start) / timeout)))
                else:
                    pct = 5
                on_progress(pct)
            if now >= deadline:
                raise ComfyClientError(
                    "timeout",
                    f"comfyui generation timeout after {now - start:.1f}s (prompt {prompt_id})",
                )
            await asyncio.sleep(min(interval, deadline - now))
            interval = min(interval * 2, cap)

    async def fetch_output(self, filename: str, subfolder: str = "", *,
                           dest_dir: Path) -> Path:
        """GET /view 下载到 dest_dir（保留原名）→ 返回落盘路径。

        下载失败或写盘失败抛 ComfyClientError('output')。
        重名覆盖策略由调用方处理（§2.3 第 6 步由 adapter 决定加序号）。
        """
        client = self._http()
        params = {"filename": filename, "type": "output"}
        if subfolder:
            params["subfolder"] = subfolder
        try:
            resp = await client.get(f"{self.base_url}/view", params=params)
        except httpx.HTTPError as exc:
            raise ComfyClientError("output", f"failed to fetch output {filename}: {exc}") from exc
        if resp.status_code != 200:
            raise ComfyClientError(
                "output", f"failed to fetch output {filename}: HTTP {resp.status_code}")
        dest_dir = Path(dest_dir)
        try:
            dest_dir.mkdir(parents=True, exist_ok=True)
            dest = dest_dir / Path(filename).name
            dest.write_bytes(resp.content)
        except OSError as exc:
            raise ComfyClientError("output", f"failed to write output {filename}: {exc}") from exc
        return dest

    async def interrupt(self) -> None:
        """POST /interrupt，best-effort：吞掉连接类错误，永不向调用方抛出。"""
        client = self._http()
        try:
            await client.post(f"{self.base_url}/interrupt")
        except httpx.HTTPError:
            pass  # best-effort：ComfyUI 不可达/中断失败均静默
