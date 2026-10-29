"""openai-bridge 测试脚手架（pytest fixtures）。

提供：
- mock_openai:   启动 mock OpenAI 兼容上游（127.0.0.1:0 自动分配端口），
                 yield MockOpenAIHandle(base_url, server)；teardown shutdown+close。
                 函数级作用域 —— 每个测试获得干净状态与独立开关。
- module_env:    隔离 MODULE_DIR / 清空 EP_OUTPUT_DIR / EP_WORKSPACE /
                 OPENAI_API_KEY / OPENAI_BASE_URL（防真实外网与本机配置渗漏）。
- asgi_client:   httpx ASGITransport 进程内客户端（不起 uvicorn、不占端口）。

mock 行为矩阵与开关见 tests/mock_openai.py 模块 docstring（CONTRACT.md §7）。
"""

from __future__ import annotations

import sys
import threading
from pathlib import Path
from typing import Any, NamedTuple

import httpx
import pytest

# 保证测试模块可直接 import mock_openai / adapter / srt_realign
# （tests/ 与模块根目录均无 __init__.py，双路径兜底，comfyui-bridge 同款）
_TESTS_DIR = Path(__file__).resolve().parent
_MODULE_DIR = _TESTS_DIR.parent
for _p in (str(_TESTS_DIR), str(_MODULE_DIR)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

from mock_openai import MockOpenAIServer, create_server  # noqa: E402


class MockOpenAIHandle(NamedTuple):
    """mock_openai fixture 产物：既可元组解包，也可按属性访问。"""

    base_url: str  # 形如 http://127.0.0.1:<port>/v1
    server: MockOpenAIServer


@pytest.fixture()
def mock_openai():
    """启动 mock 上游；base_url 已含 /v1 版本段（与真实 provider 习惯一致）。"""
    raw = create_server("127.0.0.1", 0)
    thread = threading.Thread(
        target=raw.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True
    )
    thread.start()
    try:
        yield MockOpenAIHandle(base_url=f"http://127.0.0.1:{raw.server_port}/v1", server=raw)
    finally:
        raw.shutdown()
        raw.server_close()


@pytest.fixture()
def module_env(monkeypatch, tmp_path) -> Path:
    """隔离模块目录与全部外联环境变量（测试零真实外网、零本机配置渗漏）。"""
    monkeypatch.setenv("MODULE_DIR", str(tmp_path))
    for name in ("EP_OUTPUT_DIR", "EP_WORKSPACE", "OPENAI_API_KEY", "OPENAI_BASE_URL"):
        monkeypatch.delenv(name, raising=False)
    return tmp_path


class _SyncASGIFacade:
    """同步门面：get/post 返回协程（供 run() 驱动），每次请求独立 client+事件循环。

    httpx.AsyncClient 只支持 async with；测试普遍以 `with asgi_client() as c` +
    `run(c.get(...))` 风格书写，本门面把两条路径都接通（进程内 ASGITransport，
    不起 uvicorn、不占端口）。
    """

    def __init__(self, app: Any) -> None:
        self._app = app

    def _make(self) -> httpx.AsyncClient:
        return httpx.AsyncClient(
            transport=httpx.ASGITransport(app=self._app, raise_app_exceptions=False),
            base_url="http://adapter.test",
            timeout=30.0,
        )

    def get(self, path: str) -> Any:
        async def _t():
            async with self._make() as client:
                return await client.get(path)

        return _t()

    def post(self, path: str, **kwargs: Any) -> Any:
        async def _t():
            async with self._make() as client:
                return await client.post(path, **kwargs)

        return _t()

    def __enter__(self) -> "_SyncASGIFacade":
        return self

    def __exit__(self, *exc: Any) -> None:
        return None


@pytest.fixture()
def asgi_client():
    """进程内 ASGI 客户端工厂（同步门面，见 _SyncASGIFacade；每次调用新实例）。"""
    import adapter

    def _make() -> _SyncASGIFacade:
        return _SyncASGIFacade(adapter.app)

    return _make


def run(coro: Any) -> Any:
    """同步驱动协程（测试无 asyncio marker 依赖）。"""
    import asyncio

    return asyncio.run(coro)
