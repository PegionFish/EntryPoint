"""ComfyUI 桥接测试脚手架（pytest fixtures）。

提供（供 A/B 代理的测试模块使用）：
- mock_comfy:      启动 mock ComfyUI（127.0.0.1:0 自动分配端口），
                   yield MockComfyHandle(base_url, server)；teardown shutdown+close。
                   函数级作用域 —— 每个测试获得独立计数器与干净状态。
- sample_workflow: 合法 API 格式工作流 dict（LoadImage -> ImageScale -> SaveImage）。

mock_comfyui 模块接口与端点行为见 CONTRACT.md §6；
错误注入开关可在测试内运行期翻转：
    handle.server.reject_prompt = True    # /prompt 恒 400
    handle.server.fail_execution = True   # history 终态 status_str="error"
    handle.server.history_rounds = 3      # 轮询 N 次才完成
"""

import sys
import threading
from pathlib import Path
from typing import Any, Dict, NamedTuple

import pytest

# 保证测试模块可直接 import mock_comfyui / adapter / comfy_client
# （tests/ 与模块根目录均无 __init__.py，双路径兜底）
_TESTS_DIR = Path(__file__).resolve().parent
_MODULE_DIR = _TESTS_DIR.parent
for _p in (str(_TESTS_DIR), str(_MODULE_DIR)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

from mock_comfyui import create_server  # noqa: E402


class MockComfyHandle(NamedTuple):
    """mock_comfy fixture 产物：既可元组解包，也可按属性访问。

    字段：
        base_url: 如 "http://127.0.0.1:<port>"
        server:   MockComfyUIServer 句柄（错误注入开关 / interrupt_count /
                  uploaded_files / server_port 等）
    """

    base_url: str
    server: Any


@pytest.fixture()
def mock_comfy():
    """启动 mock ComfyUI 服务器，yield (base_url, server) 句柄。

    - 绑定 127.0.0.1:0（内核自动分配空闲端口，base_url 与 server.server_port 一致）
    - serve_forever 运行于守护线程
    - teardown: shutdown() + server_close()，无端口/线程泄漏
    """
    server = create_server("127.0.0.1", 0)
    thread = threading.Thread(
        target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True
    )
    thread.start()
    try:
        yield MockComfyHandle(
            base_url=f"http://127.0.0.1:{server.server_port}", server=server
        )
    finally:
        server.shutdown()
        server.server_close()


@pytest.fixture()
def sample_workflow() -> Dict[str, Any]:
    """合法 ComfyUI API 格式工作流（每次测试独立副本，可自由改动）。

    节点图：1 LoadImage -> 3 ImageScale -> 4 SaveImage
    常用注入点示例：
        "3.inputs.image"          -> "$input"（文件类，先 POST /upload/image）
        "4.inputs.filename_prefix" -> "ep"（字面量）
    """
    return {
        "1": {
            "class_type": "LoadImage",
            "inputs": {"image": "example.png", "upload": "image"},
        },
        "3": {
            "class_type": "ImageScale",
            "inputs": {
                "upscale_method": "nearest-exact",
                "width": 1024,
                "height": 1024,
                "crop": "disabled",
                "image": ["1", 0],
            },
        },
        "4": {
            "class_type": "SaveImage",
            "inputs": {"images": ["3", 0], "filename_prefix": "ep"},
        },
    }
