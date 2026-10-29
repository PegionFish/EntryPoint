"""mock_openai.py — OpenAI 兼容 API 仿真服务器（纯标准库实现）。

契约依据：modules/openai-bridge/CONTRACT.md §7「mock 上游行为矩阵」。
本模块不依赖 pytest / fastapi / httpx，可独立运行（`python mock_openai.py --help`）。
测试永久不访问真实外网（MODULE_SPEC §10.4 桥接测试纪律）。

被消费端点：
    POST /v1/chat/completions      按 server.chat_mode 变换用户消息文本；
                                   返回 OpenAI 形状响应 + usage（token 数按文本长度模拟）
    POST /v1/audio/transcriptions  按 server.transcribe_mode 返回 json / verbose_json / srt
    POST /v1/audio/speech          返回固定伪音频字节 b"MOCK-AUDIO-BYTES:" + voice
    GET  /v1/models                固定模型清单（/health 私网探测用）

行为开关（服务器实例属性，测试运行期可翻转）：
    chat_mode        "upper"(默认，按 ##N## 协议转大写) | "drop"(丢弃第 2 条，触发
                     adapter 逐条补调) | "garbage"(无标记自由文本) | "echo"(原样回显)
    transcribe_mode  "json"(默认，text+segments) | "verbose_json" | "srt" | "vtt"
    fail_chat        True → /chat/completions 恒 500
    fail_auth        True → 无 Authorization 头时 401，且错误正文回显收到的
                     Authorization 值（供 adapter 密钥脱敏断言）
    require_auth     True → 无 Authorization 头一律 401（不回显）

并发安全：ThreadingHTTPServer + threading.Lock 保护全部计数器与记录。
"""

from __future__ import annotations

import argparse
import json
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

MOCK_VERSION = "mock-openai-0.3"

_MARK_RE = re.compile(r"##(\d+)##(.*)$")


class MockOpenAIServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, *args: Any, **kwargs: Any):
        super().__init__(*args, **kwargs)
        self.lock = threading.Lock()
        # ── 行为开关（运行期可翻转） ──
        self.chat_mode = "upper"
        self.transcribe_mode = "json"
        self.fail_chat = False
        self.fail_auth = False
        self.require_auth = False
        # ── 请求记录（供测试断言） ──
        self.chat_requests: list[dict] = []
        self.transcribe_requests: list[dict] = []
        self.speech_requests: list[dict] = []
        self.auth_headers: list[str | None] = []

    # ── 记录辅助 ──
    def record_chat(self, body: dict, auth: str | None) -> None:
        with self.lock:
            self.chat_requests.append(body)
            self.auth_headers.append(auth)

    def record_transcribe(self, meta: dict, auth: str | None) -> None:
        with self.lock:
            self.transcribe_requests.append(meta)
            self.auth_headers.append(auth)

    def record_speech(self, body: dict, auth: str | None) -> None:
        with self.lock:
            self.speech_requests.append(body)
            self.auth_headers.append(auth)

    # ── 变换逻辑（chat_mode 分派） ──
    def transform_chat(self, user_content: str) -> str:
        mode = self.chat_mode
        if mode == "echo":
            return user_content
        if mode == "garbage":
            # 批量（含 ##N## 标记）→ 无标记自由文本（映射全缺）；逐条补调 → 空
            #（两者叠加逼出 adapter 的「拒绝静默错位」报错路径）
            if "##" in user_content:
                return "I processed your subtitles but lost the numbering, sorry."
            return ""
        lines = user_content.split("\n")
        out: list[str] = []
        matched = False
        for line in lines:
            m = _MARK_RE.match(line)
            if not m:
                continue
            matched = True
            index, text = m.group(1), m.group(2)
            if mode == "drop" and index == "2":
                continue  # 模拟 LLM 丢行 → 触发 adapter 逐条补调
            out.append(f"##{index}## {text.upper()}")
        if not matched:
            # 无标记（普通 chat / 逐条补调）→ 确定性大写变换
            return user_content.upper()
        return "\n".join(out)

    @staticmethod
    def fake_usage(text: str) -> dict:
        # 确定性伪 token 计数：字符数取整到 4 的倍数
        p = max(1, (len(text) + 3) // 4)
        c = max(1, p // 2)
        return {"prompt_tokens": p, "completion_tokens": c, "total_tokens": p + c}


class _Handler(BaseHTTPRequestHandler):
    server: MockOpenAIServer  # 类型收窄

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
        pass  # 测试静默

    # ── 工具 ──
    def _auth(self) -> str | None:
        header = self.headers.get("Authorization")
        if header is None:
            return None
        return header.removeprefix("Bearer ").strip() or None

    def _auth_gate(self) -> bool:
        """True = 已放行；False = 已回 401。"""
        auth = self.headers.get("Authorization")
        if self.server.require_auth and not auth:
            self._json_error(401, "missing api key")
            return False
        if self.server.fail_auth:
            # 恒 401，且错误正文回显收到的 Authorization 值（供 adapter 密钥
            # 脱敏断言：适配器必须把密钥从错误信息中洗掉，零明文出站）
            self._json_error(401, f"invalid api key: {auth!r}")
            return False
        return True

    def _json(self, status: int, payload: dict) -> None:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _json_error(self, status: int, message: str) -> None:
        self._json(status, {"error": {"message": message, "type": "mock_error"}})

    def _read_body(self) -> bytes:
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def _parse_multipart(self, body: bytes, content_type: str) -> dict[str, dict]:
        """multipart/form-data → {name: {filename, content}}（email 包解析，纯标准库）。"""
        from email.parser import BytesParser
        from email.policy import HTTP

        boundary = ""
        for part in content_type.split(";"):
            part = part.strip()
            if part.startswith("boundary="):
                boundary = part[len("boundary="):].strip('"')
        raw = (
            f"Content-Type: multipart/form-data; boundary={boundary}\r\nMIME-Version: 1.0\r\n\r\n"
        ).encode("utf-8") + body
        message = BytesParser(policy=HTTP).parsebytes(raw)
        fields: dict[str, dict] = {}
        for part in message.iter_parts():  # type: ignore[attr-defined]
            name = part.get_param("name", header="content-disposition")
            if name is None:
                continue
            filename = part.get_filename()
            payload = part.get_payload(decode=True) or b""
            fields[str(name)] = {
                "filename": str(filename) if filename else None,
                "content": payload,
                "text": payload.decode("utf-8", errors="replace"),
            }
        return fields

    # ── 路由 ──
    def do_GET(self) -> None:  # noqa: N802
        if self.path.rstrip("/").endswith("/models"):
            self._json(
                200,
                {"object": "list", "data": [{"id": "mock-chat", "object": "model"}]},
            )
            return
        self._json_error(404, f"unknown path: {self.path}")

    def do_POST(self) -> None:  # noqa: N802
        path = self.path.rstrip("/")
        body = self._read_body()
        if path.endswith("/chat/completions"):
            self._handle_chat(body)
        elif path.endswith("/audio/transcriptions"):
            self._handle_transcribe(body)
        elif path.endswith("/audio/speech"):
            self._handle_speech(body)
        else:
            self._json_error(404, f"unknown path: {self.path}")

    def _handle_chat(self, body: bytes) -> None:
        if not self._auth_gate():
            return
        if self.server.fail_chat:
            self._json_error(500, "mock chat boom")
            return
        try:
            payload = json.loads(body.decode("utf-8"))
        except ValueError:
            self._json_error(400, "invalid JSON body")
            return
        messages = payload.get("messages")
        if not isinstance(messages, list) or not messages:
            self._json_error(400, "messages must be a non-empty array")
            return
        self.server.record_chat(payload, self._auth())
        user_content = ""
        for message in reversed(messages):
            if message.get("role") == "user":
                user_content = str(message.get("content", ""))
                break
        content = self.server.transform_chat(user_content)
        self._json(
            200,
            {
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": payload.get("model"),
                "choices": [
                    {"index": 0, "message": {"role": "assistant", "content": content},
                     "finish_reason": "stop"}
                ],
                "usage": self.server.fake_usage(user_content),
            },
        )

    def _handle_transcribe(self, body: bytes) -> None:
        if not self._auth_gate():
            return
        fields = self._parse_multipart(body, self.headers.get("Content-Type", ""))
        file_part = fields.get("file") or {}
        meta = {
            "filename": file_part.get("filename"),
            "model": (fields.get("model") or {}).get("text"),
            "response_format": (fields.get("response_format") or {}).get("text"),
            "language": (fields.get("language") or {}).get("text"),
        }
        self.server.record_transcribe(meta, self._auth())
        fmt = meta["response_format"] or self.server.transcribe_mode
        if fmt in ("srt", "vtt"):
            sep = "," if fmt == "srt" else "."
            text = (
                "1\n"
                f"00:00:01{sep}000 --> 00:00:03{sep}500\n"
                "hello world\n\n"
                "2\n"
                f"00:00:03{sep}500 --> 00:00:05{sep}000\n"
                "mock speech\n"
            )
            self._send_text(200, text)
            return
        segments = [
            {"start": 0.0, "end": 1.2, "text": " hello"},
            {"start": 1.2, "end": 2.5, "text": " world"},
        ]
        payload: dict[str, Any] = {
            "text": "hello world",
            "usage": self.server.fake_usage(file_part.get("text", "") or "x"),
        }
        if fmt == "verbose_json":
            payload["segments"] = segments
            payload["language"] = "en"
            payload["duration"] = 2.5
        self._json(200, payload)

    def _handle_speech(self, body: bytes) -> None:
        if not self._auth_gate():
            return
        try:
            payload = json.loads(body.decode("utf-8"))
        except ValueError:
            self._json_error(400, "invalid JSON body")
            return
        if not payload.get("input") or not payload.get("voice"):
            self._json_error(400, "input and voice are required")
            return
        self.server.record_speech(payload, self._auth())
        data = b"MOCK-AUDIO-BYTES:" + str(payload.get("voice")).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", f"audio/{payload.get('response_format', 'mpeg')}")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _send_text(self, status: int, text: str) -> None:
        data = text.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def create_server(host: str, port: int) -> MockOpenAIServer:
    """绑定监听（port=0 由内核分配空闲端口）；serve_forever 由调用方驱动。"""
    return MockOpenAIServer((host, port), _Handler)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Mock OpenAI-compatible upstream")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8199)
    parser.add_argument("--chat-mode", default="upper",
                        choices=["upper", "drop", "garbage", "echo"])
    parser.add_argument("--require-auth", action="store_true")
    args = parser.parse_args()
    server = create_server(args.host, args.port)
    server.chat_mode = args.chat_mode
    server.require_auth = args.require_auth
    print(f"mock openai listening on http://{args.host}:{args.port}/v1 (chat_mode={args.chat_mode})")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
