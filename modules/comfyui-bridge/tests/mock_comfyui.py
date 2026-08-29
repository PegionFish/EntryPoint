"""mock_comfyui.py — ComfyUI REST API 仿真服务器（纯标准库实现）。

契约依据：modules/comfyui-bridge/CONTRACT.md §6「mock_comfyui 行为矩阵」。
本模块不依赖 pytest / fastapi / httpx，可独立运行（`python3 mock_comfyui.py --help`）。

行为矩阵（§6 逐格对齐）：
    GET  /system_stats   -> 200 {"system":{"comfyui_version":"mock-0.3"}}
    GET  /queue          -> 200 {"queue_running":[],"queue_pending":[]}
    POST /upload/image   -> 200 {"name":"<filename>","subfolder":"","type":"input"}；
                            文件名记入 server.uploaded_files
    POST /prompt         -> 校验 body JSON 含 "prompt" 对象 -> 生成自增十六进制
                            prompt_id -> 200 {"prompt_id":...,"number":n}；
                            prompt 缺失/非对象 -> 400 {"error":...}；
                            注入开关 reject_prompt=1 -> 恒 400
    GET  /history/{id}   -> 第 1..N-1 次 -> 200 {}（空对象=未完成）；
                            第 N 次起 -> 200 完整条目（粘滞）：
                            {"<id>":{"prompt":[...],
                              "outputs":{"<save_node>":{"images":[
                                {"filename":"out_<id>_0.png","subfolder":"","type":"output"}]}},
                              "status":{"status_str":"success","completed":true}}}
                            N 默认 server.history_rounds（提交时可经 ?rounds=N
                            或 body "mock_rounds":N 覆盖）；
                            注入开关 fail_execution=1 -> 第 N 次起
                            status_str="error" + completed=false +
                            messages:[["execution_error",
                              {"node_type":"KSampler","exception_message":"mock boom"}]]
    GET  /view           -> 200 固定字节 b"MOCK-PNG-BYTES:" + filename；
                            缺 filename 参数 -> 400
    POST /interrupt      -> 200 {"ok":true}；server.interrupt_count 自增

错误注入开关（服务器实例属性，可运行期翻转）：
    reject_prompt / fail_execution / history_rounds

并发安全：ThreadingHTTPServer（每请求一线程）+ threading.Lock 保护全部计数器
与状态（prompt 序号、interrupt 计数、上传集合、history 记录）。

无 SaveImage 类节点的兜底：outputs 挂到排序后的最后一个节点 id 上，
保证 history 条目始终含 images（供"缺省取全部有 images 的节点"路径测试）。
"""

from __future__ import annotations

import argparse
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Dict, List, Optional, Tuple
from urllib.parse import parse_qs, unquote, urlparse

MOCK_VERSION = "mock-0.3"

# fail_execution 注入时返回的固定 messages（契约 §6）
EXECUTION_ERROR_MESSAGES: List[List[Any]] = [
    ["execution_error", {"node_type": "KSampler", "exception_message": "mock boom"}]
]


def _sort_node_ids(ids: List[str]) -> List[str]:
    """节点 id 排序：纯数字按数值序在前，其余按字符串序，保证确定性。"""

    def key(nid: str) -> Tuple[int, int, str]:
        try:
            return (0, int(nid), "")
        except ValueError:
            return (1, 0, str(nid))

    return sorted(ids, key=key)


def _save_nodes(prompt: Dict[str, Any]) -> List[str]:
    """找出工作流中的 Save 类节点 id（class_type 含 "save"，大小写不敏感）。

    无 Save 类节点时回退为最后一个节点 id（排序后），保证 history 始终有 outputs。
    """
    if not isinstance(prompt, dict) or not prompt:
        return []
    save_ids = [
        nid
        for nid, node in prompt.items()
        if isinstance(node, dict) and "save" in str(node.get("class_type", "")).lower()
    ]
    if save_ids:
        return _sort_node_ids([str(nid) for nid in save_ids])
    return _sort_node_ids([str(nid) for nid in prompt.keys()])[-1:]


def _parse_multipart(
    body: bytes, content_type: str
) -> List[Tuple[Optional[str], Optional[str], bytes]]:
    """极简 multipart/form-data 解析（仅标准库，mock 场景足够）。

    返回 [(field_name, filename, value_bytes), ...]；
    非 multipart 或缺 boundary 时返回空列表。
    """
    if not content_type or "multipart" not in content_type.lower():
        return []
    boundary = None
    for segment in content_type.split(";"):
        segment = segment.strip()
        if segment.lower().startswith("boundary="):
            boundary = segment[len("boundary="):].strip().strip('"')
            break
    if not boundary:
        return []
    delimiter = b"--" + boundary.encode("latin-1", "replace")
    parts: List[Tuple[Optional[str], Optional[str], bytes]] = []
    for chunk in body.split(delimiter):
        if chunk in (b"", b"--", b"--\r\n"):
            continue  # 首尾空段 / 终止标记
        chunk = chunk.lstrip(b"\r\n")  # 仅去分隔符框架 CRLF，不触碰值内容
        head, sep, value = chunk.partition(b"\r\n\r\n")
        if not sep:
            continue
        if value.endswith(b"\r\n"):
            value = value[:-2]  # 值末尾的 CRLF 属于框架，而非内容
        name: Optional[str] = None
        filename: Optional[str] = None
        for line in head.split(b"\r\n"):
            if b":" not in line:
                continue
            key, _, val = line.partition(b":")
            if key.strip().lower() != b"content-disposition":
                continue
            for item in val.split(b";"):
                item = item.strip()
                if item.startswith(b"name="):
                    name = item[5:].strip().strip(b'"').decode("latin-1", "replace")
                elif item.startswith(b"filename="):
                    filename = item[9:].strip().strip(b'"').decode("latin-1", "replace")
        parts.append((name, filename, value))
    return parts


class MockComfyUIServer(ThreadingHTTPServer):
    """§6 行为矩阵的载体。create_server() 工厂创建，serve_forever() 启动。"""

    daemon_threads = True
    allow_reuse_address = True

    def __init__(
        self,
        address: Tuple[str, int],
        handler_cls: type = None,  # type: ignore[assignment]
        *,
        history_rounds: int = 2,
        reject_prompt: bool = False,
        fail_execution: bool = False,
        verbose: bool = False,
    ) -> None:
        if handler_cls is None:
            handler_cls = MockComfyUIHandler
        super().__init__(address, handler_cls)
        # ---- 错误注入开关：实例属性，可运行期翻转（赋值在 GIL 下原子） ----
        self.history_rounds = max(1, int(history_rounds))
        self.reject_prompt = bool(reject_prompt)
        self.fail_execution = bool(fail_execution)
        self.verbose = bool(verbose)
        # ---- 锁保护的可观测状态 ----
        self._lock = threading.Lock()
        self._seq = 0
        self.interrupt_count = 0
        self.uploaded_files: set = set()
        # prompt_id -> {"number", "prompt", "rounds", "hits"}
        self._pending: Dict[str, Dict[str, Any]] = {}
        # 完成条目粘滞缓存：prompt_id -> history entry
        self._completed: Dict[str, Dict[str, Any]] = {}

    # ---- 状态推进（全部加锁） ----

    def next_prompt(self) -> Tuple[str, int]:
        """生成自增十六进制 prompt_id 与队列序号 number。"""
        with self._lock:
            self._seq += 1
            return f"{self._seq:08x}", self._seq

    def register_prompt(self, prompt_id: str, number: int, prompt: dict, rounds: int) -> None:
        with self._lock:
            self._pending[prompt_id] = {
                "number": number,
                "prompt": prompt,
                "rounds": max(1, int(rounds)),
                "hits": 0,
            }

    def record_upload(self, filename: str) -> None:
        with self._lock:
            self.uploaded_files.add(filename)

    def record_interrupt(self) -> None:
        with self._lock:
            self.interrupt_count += 1

    def history_snapshot(self, prompt_id: str, fail_execution: bool) -> Dict[str, Any]:
        """GET /history/{id} 的响应体；第 N 次起完整条目并粘滞。"""
        with self._lock:
            if prompt_id in self._completed:
                return {prompt_id: self._completed[prompt_id]}
            rec = self._pending.get(prompt_id)
            if rec is None:
                return {}  # 未知 id：永远未完成（与真实 ComfyUI 一致，可用于超时路径）
            rec["hits"] += 1
            if rec["hits"] < rec["rounds"]:
                return {}
            entry = self._build_entry_locked(prompt_id, rec, fail_execution)
            self._completed[prompt_id] = entry
            return {prompt_id: entry}

    def completed_history(self) -> Dict[str, Any]:
        """GET /history（无 id）：返回全部已完成条目。"""
        with self._lock:
            return json.loads(json.dumps(self._completed))  # 深拷贝快照

    def _build_entry_locked(
        self, prompt_id: str, rec: Dict[str, Any], fail_execution: bool
    ) -> Dict[str, Any]:
        outputs: Dict[str, Any] = {}
        for idx, node_id in enumerate(_save_nodes(rec["prompt"])):
            outputs[node_id] = {
                "images": [
                    {"filename": f"out_{prompt_id}_{idx}.png", "subfolder": "", "type": "output"}
                ]
            }
        if fail_execution:
            status = {
                "status_str": "error",
                "completed": False,
                "messages": json.loads(json.dumps(EXECUTION_ERROR_MESSAGES)),
            }
        else:
            status = {"status_str": "success", "completed": True}
        return {
            "prompt": [rec["number"], prompt_id, rec["prompt"]],
            "outputs": outputs,
            "status": status,
        }


class MockComfyUIHandler(BaseHTTPRequestHandler):
    """七端点路由实现。状态全部经由 self.server（MockComfyUIServer）。"""

    protocol_version = "HTTP/1.1"
    server_version = "MockComfyUI/" + MOCK_VERSION

    # ---- 基础设施 ----

    def log_message(self, fmt: str, *args: Any) -> None:  # noqa: A003
        if getattr(self.server, "verbose", False):
            super().log_message(fmt, *args)

    def _send_json(self, status: int, obj: Any) -> None:
        body = json.dumps(obj).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _send_bytes(self, status: int, data: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _read_body(self) -> bytes:
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            length = 0
        if length <= 0:
            return b""
        return self.rfile.read(length)

    def do_GET(self) -> None:  # noqa: N802
        try:
            self._route_get()
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as exc:  # 防御：解析异常转 500，不令线程崩溃
            try:
                self._send_json(500, {"error": f"mock internal error: {exc}"})
            except OSError:
                pass

    def do_POST(self) -> None:  # noqa: N802
        try:
            body = self._read_body()  # 先排水，保证 HTTP/1.1 keep-alive 不串流
            self._route_post(body)
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as exc:
            try:
                self._send_json(500, {"error": f"mock internal error: {exc}"})
            except OSError:
                pass

    # ---- GET 路由 ----

    def _route_get(self) -> None:
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/") or "/"
        if path == "/system_stats":
            self._send_json(200, {"system": {"comfyui_version": MOCK_VERSION}})
        elif path == "/queue":
            self._send_json(200, {"queue_running": [], "queue_pending": []})
        elif path == "/history":
            self._send_json(200, self.server.completed_history())
        elif path.startswith("/history/"):
            prompt_id = unquote(path[len("/history/"):])
            self._send_json(
                200,
                self.server.history_snapshot(prompt_id, bool(self.server.fail_execution)),
            )
        elif path == "/view":
            self._handle_view(parse_qs(parsed.query))
        else:
            self._send_json(404, {"error": f"mock: no route for GET {path}"})

    def _handle_view(self, query: Dict[str, List[str]]) -> None:
        filename = (query.get("filename") or [""])[0]
        if not filename:
            self._send_json(400, {"error": "missing 'filename' parameter"})
            return
        self._send_bytes(200, b"MOCK-PNG-BYTES:" + filename.encode("utf-8"), "image/png")

    # ---- POST 路由 ----

    def _route_post(self, body: bytes) -> None:
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/") or "/"
        if path == "/upload/image":
            self._handle_upload(body)
        elif path == "/prompt":
            self._handle_prompt(body, parse_qs(parsed.query))
        elif path == "/interrupt":
            self.server.record_interrupt()
            self._send_json(200, {"ok": True})
        else:
            self._send_json(404, {"error": f"mock: no route for POST {path}"})

    def _handle_upload(self, body: bytes) -> None:
        parts = _parse_multipart(body, self.headers.get("Content-Type", ""))
        filename: Optional[str] = None
        fallback_value: Optional[bytes] = None
        for field_name, part_filename, value in parts:
            if part_filename:
                filename = part_filename
                break
            if field_name == "image" and fallback_value is None:
                fallback_value = value
        if not filename:
            if fallback_value:
                filename = "uploaded_input.png"  # 无文件名部件的确定性兜底名
            else:
                self._send_json(
                    400, {"error": "mock: upload requires a multipart 'image' file part"}
                )
                return
        filename = os.path.basename(filename.replace("\\", "/"))
        if not filename:
            self._send_json(400, {"error": "mock: upload filename is empty"})
            return
        self.server.record_upload(filename)
        self._send_json(200, {"name": filename, "subfolder": "", "type": "input"})

    def _handle_prompt(self, body: bytes, query: Dict[str, List[str]]) -> None:
        if self.server.reject_prompt:  # 注入开关：恒 400
            self._send_json(400, {"error": "mock: prompt rejected (reject_prompt=1)"})
            return
        try:
            data = json.loads(body.decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            self._send_json(400, {"error": "mock: request body is not valid JSON"})
            return
        prompt = data.get("prompt") if isinstance(data, dict) else None
        if not isinstance(prompt, dict):
            self._send_json(400, {"error": "mock: missing or invalid 'prompt' object"})
            return
        prompt_id, number = self.server.next_prompt()
        rounds = self.server.history_rounds
        # 单次提交可覆盖轮询次数：?rounds=N 或 body "mock_rounds": N
        raw_rounds: Any = (query.get("rounds") or [None])[0]
        if raw_rounds is None:
            body_rounds = data.get("mock_rounds")
            if isinstance(body_rounds, int) and not isinstance(body_rounds, bool):
                raw_rounds = body_rounds
        if raw_rounds is not None:
            try:
                rounds = max(1, int(raw_rounds))
            except (TypeError, ValueError):
                pass
        self.server.register_prompt(prompt_id, number, prompt, rounds)
        self._send_json(200, {"prompt_id": prompt_id, "number": number})


def create_server(
    host: str,
    port: int,
    *,
    history_rounds: int = 2,
    reject_prompt: bool = False,
    fail_execution: bool = False,
    verbose: bool = False,
) -> MockComfyUIServer:
    """创建未启动的 mock ComfyUI 服务器（§6 行为矩阵）。

    - port=0 由内核自动分配空闲端口，实际端口读 server.server_port。
    - 调用方自行 server.serve_forever()（通常放守护线程），
      结束时 server.shutdown() + server.server_close()。
    - 错误注入开关 reject_prompt / fail_execution / history_rounds
      为服务器实例属性，可运行期翻转。
    - verbose=True 时向 stderr 打印访问日志（默认静默）。
    """
    return MockComfyUIServer(
        (host, port),
        MockComfyUIHandler,
        history_rounds=history_rounds,
        reject_prompt=reject_prompt,
        fail_execution=fail_execution,
        verbose=verbose,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Mock ComfyUI server (CONTRACT.md §6 behavior matrix)"
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8199, help="0 = auto assign")
    parser.add_argument("--history-rounds", type=int, default=2)
    parser.add_argument("--reject-prompt", action="store_true")
    parser.add_argument("--fail-execution", action="store_true")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    _server = create_server(
        args.host,
        args.port,
        history_rounds=args.history_rounds,
        reject_prompt=args.reject_prompt,
        fail_execution=args.fail_execution,
        verbose=args.verbose,
    )
    print(f"mock comfyui listening on http://{args.host}:{_server.server_port}", flush=True)
    try:
        _server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        _server.shutdown()
        _server.server_close()
