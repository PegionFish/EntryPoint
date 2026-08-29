"""comfy_client.py 单测（子代理 A，文件域独占）。

- 不依赖 pytest-asyncio 插件：全部为同步测试函数，内部用 ``asyncio.run()`` 驱动。
- 测试桩为本文件内自带的极简 http.server（ThreadingHTTPServer，端口 0 随机分配），
  行为对齐 CONTRACT.md §6 mock 行为矩阵的子集；不 import tests/mock_comfyui
  （C 代理文件域，避免交叉）。
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import re
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlparse

import pytest
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from comfy_client import ComfyClient, ComfyClientError  # noqa: E402


# ---------------------------------------------------------------------------
# 极简 ComfyUI 行为桩（仅覆盖本测试所需路径）
# ---------------------------------------------------------------------------


class _StubComfyUIHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # 静音访问日志
        pass

    # -- 输出辅助 --

    def _send_json(self, code: int, payload) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_bytes(self, code: int, body: bytes, ctype: str) -> None:
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    # -- GET --

    def do_GET(self):
        url = urlparse(self.path)
        path = unquote(url.path)
        qs = parse_qs(url.query)
        srv = self.server
        try:
            if path == "/system_stats":
                self._send_json(200, {"system": {"comfyui_version": "stub-0.3"}})
            elif path == "/queue":
                self._send_json(200, {
                    "queue_running": list(srv.queue_running),
                    "queue_pending": list(srv.queue_pending),
                })
            elif path.startswith("/history/"):
                self._handle_history(path[len("/history/"):])
            elif path == "/view":
                filename = (qs.get("filename") or [None])[0]
                if not filename:
                    self._send_json(400, {"error": "missing filename"})
                    return
                self._send_bytes(200, b"MOCK-PNG-BYTES:" + filename.encode("utf-8"),
                                 "application/octet-stream")
            else:
                self._send_json(404, {"error": f"not found: {path}"})
        except (BrokenPipeError, ConnectionResetError):
            self.close_connection = True

    def _handle_history(self, pid: str):
        srv = self.server
        srv.history_counts[pid] = srv.history_counts.get(pid, 0) + 1
        count = srv.history_counts[pid]
        rounds = srv.history_rounds.get(pid, srv.default_rounds)
        if count < rounds:  # §6：空对象 = 未完成
            self._send_json(200, {})
            return
        if srv.fail_execution:
            entry = {
                "prompt": [],
                "outputs": {},
                "status": {
                    "status_str": "error",
                    "completed": False,
                    "messages": [["execution_error",
                                  {"node_type": "KSampler", "exception_message": "mock boom"}]],
                },
            }
        else:
            entry = {
                "prompt": [],
                "outputs": {
                    "9": {"images": [{"filename": f"out_{pid}_0.png",
                                      "subfolder": "", "type": "output"}]},
                },
                "status": {"status_str": "success", "completed": True,
                           "messages": [["execution_success", {"prompt_id": pid}]]},
            }
        self._send_json(200, {pid: entry})

    # -- POST --

    def do_POST(self):
        path = unquote(urlparse(self.path).path)
        srv = self.server
        try:
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length) if length > 0 else b""
            if path == "/prompt":
                self._handle_prompt(body)
            elif path == "/upload/image":
                match = re.search(rb'filename="([^"]+)"', body)
                filename = match.group(1).decode("utf-8") if match else "upload.bin"
                srv.uploaded.append(filename)
                match_type = re.search(rb'name="type"\r\n\r\n([^\r\n]*)', body)
                srv.upload_types.append(match_type.group(1).decode() if match_type else None)
                self._send_json(200, {"name": filename, "subfolder": "", "type": "input"})
            elif path == "/interrupt":
                srv.interrupt_count += 1
                self._send_json(200, {"ok": True})
            else:
                self._send_json(404, {"error": f"not found: {path}"})
        except (BrokenPipeError, ConnectionResetError):
            self.close_connection = True

    def _handle_prompt(self, body: bytes):
        srv = self.server
        if srv.reject_prompt:
            self._send_json(400, {
                "error": {"type": "prompt_outputs_failed_validation",
                          "message": "Prompt outputs failed validation"},
                "node_errors": {"3": {"errors": [
                    {"type": "value_not_in_list", "message": "value not in list",
                     "details": "ckpt_name"}]}},
            })
            return
        try:
            payload = json.loads(body)
            prompt = payload["prompt"]
            if not isinstance(prompt, dict):
                raise ValueError("prompt is not an object")
        except Exception:
            self._send_json(400, {"error": "prompt missing or invalid"})
            return
        srv.prompt_counter += 1
        pid = f"stub-{srv.prompt_counter:04x}"
        srv.history_rounds.setdefault(pid, srv.default_rounds)
        srv.history_counts[pid] = 0
        srv.prompts.append(prompt)
        self._send_json(200, {"prompt_id": pid, "number": srv.prompt_counter})


@contextlib.contextmanager
def _stub_comfyui(**overrides):
    """启动一次性行为桩，yield (base_url, server)；退出时干净关闭。"""
    srv = ThreadingHTTPServer(("127.0.0.1", 0), _StubComfyUIHandler)
    srv.prompt_counter = 0
    srv.interrupt_count = 0
    srv.default_rounds = 2
    srv.history_rounds = {}
    srv.history_counts = {}
    srv.reject_prompt = False
    srv.fail_execution = False
    srv.queue_running = []
    srv.queue_pending = []
    srv.uploaded = []
    srv.upload_types = []
    srv.prompts = []
    for key, value in overrides.items():
        setattr(srv, key, value)
    thread = threading.Thread(target=srv.serve_forever,
                              kwargs={"poll_interval": 0.02}, daemon=True)
    thread.start()
    base_url = f"http://127.0.0.1:{srv.server_address[1]}"
    try:
        yield base_url, srv
    finally:
        srv.shutdown()
        srv.server_close()
        thread.join(timeout=5)


def _dead_port_url() -> str:
    """取一个当前无人监听的回环端口（连接必被拒绝）。"""
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return f"http://127.0.0.1:{port}"


# ---------------------------------------------------------------------------
# 测试
# ---------------------------------------------------------------------------


def test_system_stats_ok():
    with _stub_comfyui() as (base, _srv):
        async def main():
            async with ComfyClient(base) as client:
                return await client.system_stats()
        stats = asyncio.run(main())
    assert stats["system"]["comfyui_version"] == "stub-0.3"


def test_system_stats_connect_error():
    async def main():
        async with ComfyClient(_dead_port_url(), timeout=5.0) as client:
            await client.system_stats()
    with pytest.raises(ComfyClientError) as exc_info:
        asyncio.run(main())
    assert exc_info.value.kind == "connect"


def test_queue_info_counts():
    with _stub_comfyui(queue_running=[{"x": 1}], queue_pending=[{"a": 1}, {"b": 2}]) as (base, _srv):
        async def main():
            async with ComfyClient(base) as client:
                return await client.queue_info()
        info = asyncio.run(main())
    assert info == {"running": 1, "pending": 2}


def test_upload_image_returns_filename():
    with _stub_comfyui() as (base, srv):
        async def main():
            async with ComfyClient(base) as client:
                return await client.upload_image(b"PNG-DATA", "input_01.png")
        name = asyncio.run(main())
    assert name == "input_01.png"
    assert srv.uploaded == ["input_01.png"]
    # multipart 附加字段 type=input
    assert srv.upload_types == ["input"]


def test_submit_prompt_ok():
    workflow = {"1": {"class_type": "KSampler", "inputs": {"seed": 42}}}
    with _stub_comfyui() as (base, srv):
        async def main():
            async with ComfyClient(base) as client:
                return await client.submit_prompt(workflow)
        prompt_id = asyncio.run(main())
    assert prompt_id.startswith("stub-")
    assert srv.prompts == [workflow]


def test_submit_prompt_rejected_on_400():
    with _stub_comfyui(reject_prompt=True) as (base, _srv):
        async def main():
            async with ComfyClient(base) as client:
                await client.submit_prompt({"1": {"class_type": "KSampler"}})
        with pytest.raises(ComfyClientError) as exc_info:
            asyncio.run(main())
    assert exc_info.value.kind == "rejected"
    # 错误信息含 node_errors 摘要（§4.2 / §5）
    assert "node 3" in exc_info.value.message


def test_poll_history_completes_on_nth_poll():
    with _stub_comfyui(default_rounds=3) as (base, srv):
        async def main():
            pcts = []
            async with ComfyClient(base) as client:
                pid = await client.submit_prompt({"1": {"class_type": "SaveImage"}})
                entry = await client.poll_history(
                    pid, interval0=0.05, interval_max=0.1, on_progress=pcts.append)
            return pid, entry, pcts, dict(srv.history_counts)
        pid, entry, pcts, counts = asyncio.run(main())
    # 恰在第 N（=3）次轮询取到完成条目
    assert counts[pid] == 3
    assert pcts, "on_progress 未被调用"
    assert pcts[-1] == 100
    assert all(isinstance(p, int) for p in pcts)
    assert entry["status"]["status_str"] == "success"
    assert entry["outputs"]["9"]["images"][0]["filename"] == f"out_{pid}_0.png"


def test_poll_history_execution_error():
    with _stub_comfyui(default_rounds=2, fail_execution=True) as (base, _srv):
        async def main():
            async with ComfyClient(base) as client:
                pid = await client.submit_prompt({"1": {"class_type": "KSampler"}})
                await client.poll_history(pid, interval0=0.05, interval_max=0.1)
        with pytest.raises(ComfyClientError) as exc_info:
            asyncio.run(main())
    assert exc_info.value.kind == "execution"
    assert "KSampler" in exc_info.value.message
    assert "mock boom" in exc_info.value.message


def test_poll_history_timeout():
    with _stub_comfyui(default_rounds=10 ** 9) as (base, _srv):  # 永不完成
        async def main():
            async with ComfyClient(base) as client:
                pid = await client.submit_prompt({"1": {"class_type": "KSampler"}})
                # 极短 timeout 参数触发 timeout 分类
                await client.poll_history(pid, interval0=0.05, interval_max=0.1, timeout=0.3)
        with pytest.raises(ComfyClientError) as exc_info:
            asyncio.run(main())
    assert exc_info.value.kind == "timeout"
    assert "timeout" in exc_info.value.message


def test_fetch_output_writes_file(tmp_path: Path):
    with _stub_comfyui() as (base, _srv):
        async def main():
            async with ComfyClient(base) as client:
                return await client.fetch_output("out_abc_0.png", dest_dir=tmp_path)
        dest = asyncio.run(main())
    assert dest == tmp_path / "out_abc_0.png"
    assert dest.is_file()
    assert dest.read_bytes() == b"MOCK-PNG-BYTES:out_abc_0.png"


def test_fetch_output_error_kind(tmp_path: Path):
    with _stub_comfyui() as (base, _srv):
        async def main():
            async with ComfyClient(base) as client:
                # 空 filename → 桩返回 400 → 'output' 分类
                return await client.fetch_output("", dest_dir=tmp_path)
        with pytest.raises(ComfyClientError) as exc_info:
            asyncio.run(main())
    assert exc_info.value.kind == "output"


def test_interrupt_ok_and_best_effort():
    with _stub_comfyui() as (base, srv):
        async def main():
            async with ComfyClient(base) as client:
                await client.interrupt()
        asyncio.run(main())
        assert srv.interrupt_count == 1
    # ComfyUI 不可达时 interrupt 也不抛（best-effort 吞连接错误）
    async def main_down():
        async with ComfyClient(_dead_port_url(), timeout=2.0) as client:
            await client.interrupt()
    asyncio.run(main_down())  # 不应抛出任何异常


def test_loopback_bypasses_proxy(monkeypatch):
    # 回环地址必须 trust_env=False：即便设置了（不可用的）代理环境变量，
    # 对 127.0.0.1 的请求也应直连成功。
    bogus = "http://10.255.255.1:9"
    for var in ("HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"):
        monkeypatch.setenv(var, bogus)
    with _stub_comfyui() as (base, _srv):
        assert base.startswith("http://127.0.0.1:")
        async def main():
            async with ComfyClient(base) as client:
                assert client.trust_env is False
                return await client.system_stats()
        stats = asyncio.run(main())
    assert stats["system"]["comfyui_version"] == "stub-0.3"


def test_trust_env_policy_flags():
    assert ComfyClient("http://127.0.0.1:8188").trust_env is False
    assert ComfyClient("http://localhost:8188").trust_env is False
    assert ComfyClient("http://[::1]:8188").trust_env is False
    # 远程尊重环境代理
    assert ComfyClient("http://192.168.1.10:8188").trust_env is True
    assert ComfyClient("http://comfy.example.com:8188").trust_env is True
