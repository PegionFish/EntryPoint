"""test_adapter_flow.py — adapter HTTP 层对 mock ComfyUI 的流程测试（V3 门禁）。

- mock ComfyUI：tests.mock_comfyui（conftest.mock_comfy fixture，127.0.0.1:0
  自动分配端口），错误注入开关经 handle.server 运行期翻转（CONTRACT.md §6）。
- adapter 经 httpx ASGITransport 进程内直发（不起 uvicorn）。
- 每个测试用独立 tmp_path 作 MODULE_DIR（workflows/ 与 output/ 完全隔离）。

覆盖：完整 generate 成功落盘（result 路径存在且内容以 MOCK-PNG-BYTES 开头）、
workflow 不存在 400、inject 非法键 400、reject_prompt → 502、fail_execution → 502、
/workflows 非法 JSON 400、路径穿越文件名清洗、DELETE 不存在 404，
以及 /health 200/503、/info、文本上游注入、output_nodes 选取等补充用例。
"""

from __future__ import annotations

import asyncio
import json
import os
import socket
import sys
from pathlib import Path

import httpx
import pytest

# 兜底路径（conftest.py 已做同样处理，此处幂等）：保证可 import adapter / mock_comfyui
_TESTS_DIR = Path(__file__).resolve().parent
_MODULE_DIR = _TESTS_DIR.parent
for _p in (str(_TESTS_DIR), str(_MODULE_DIR)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import adapter  # noqa: E402

ADAPTER_BASE = "http://adapter.test"
MOCK_PNG_PREFIX = b"MOCK-PNG-BYTES:"


def _run(coro):
    return asyncio.run(coro)


def _client() -> httpx.AsyncClient:
    """进程内 ASGI 客户端（不占端口、不起 uvicorn）。"""
    return httpx.AsyncClient(
        transport=httpx.ASGITransport(app=adapter.app, raise_app_exceptions=False),
        base_url=ADAPTER_BASE,
        timeout=30.0,
    )


def _post_generate(
    client: httpx.AsyncClient,
    *,
    workflow: str | None = None,
    inject: dict | None = None,
    base_url: str | None = None,
    output_nodes: str | None = None,
    output_path: str | None = None,
    files: list | None = None,
):
    data: dict[str, str] = {}
    if workflow is not None:
        data["workflow"] = workflow
    if inject is not None:
        data["inject"] = json.dumps(inject)
    if base_url is not None:
        data["base_url"] = base_url
    if output_nodes is not None:
        data["output_nodes"] = output_nodes
    if output_path is not None:
        data["output_path"] = output_path
    return client.post("/predict/generate", data=data, files=files or [])


def _upload_workflow(client: httpx.AsyncClient, name: str, workflow: dict):
    return client.post(
        "/workflows",
        files={"file": (name, json.dumps(workflow).encode("utf-8"), "application/json")},
    )


def _last_submitted_prompt(server) -> dict:
    """从 mock 的 history 快照取最后提交的 prompt 工作流（断言注入生效）。"""
    history = server.completed_history()
    assert history, "mock history is empty — prompt was never submitted"
    entry = list(history.values())[-1]
    return entry["prompt"][2]


def _dead_base_url() -> str:
    """取得一个当前无监听的 127.0.0.1 端口（连接立即被拒）。"""
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return f"http://127.0.0.1:{sock.getsockname()[1]}"


@pytest.fixture()
def module_env(monkeypatch, tmp_path) -> Path:
    """隔离 MODULE_DIR（workflows/ 与缺省 output/ 基准）并清掉 EP_OUTPUT_DIR。"""
    monkeypatch.setenv("MODULE_DIR", str(tmp_path))
    monkeypatch.delenv("EP_OUTPUT_DIR", raising=False)
    return tmp_path


# ── /health 与 /info ────────────────────────────────────────────────


def test_health_ok_when_comfyui_reachable(mock_comfy, module_env, monkeypatch):
    monkeypatch.setenv("COMFYUI_URL", mock_comfy.base_url)

    async def _t():
        async with _client() as client:
            return await client.get("/health")

    resp = _run(_t())
    assert resp.status_code == 200
    assert resp.json() == {"status": "ok", "comfyui": "reachable"}


def test_health_503_when_comfyui_unreachable(module_env, monkeypatch):
    monkeypatch.setenv("COMFYUI_URL", _dead_base_url())

    async def _t():
        async with _client() as client:
            return await client.get("/health")

    resp = _run(_t())
    assert resp.status_code == 503
    assert resp.json() == {"status": "unavailable", "comfyui": "unreachable"}


def test_info_reports_comfyui_version_and_queue(mock_comfy, module_env, monkeypatch):
    monkeypatch.setenv("COMFYUI_URL", mock_comfy.base_url)

    async def _t():
        async with _client() as client:
            return await client.get("/info")

    resp = _run(_t())
    assert resp.status_code == 200
    body = resp.json()
    assert body["module"] == "comfyui-bridge"
    assert body["version"] == "0.1.0"
    assert body["comfyui"] == {"reachable": True, "version": "mock-0.3"}
    assert body["queue"] == {"running": 0, "pending": 0}


def test_info_degrades_when_comfyui_unreachable(module_env, monkeypatch):
    monkeypatch.setenv("COMFYUI_URL", _dead_base_url())

    async def _t():
        async with _client() as client:
            return await client.get("/info")

    resp = _run(_t())
    assert resp.status_code == 200
    body = resp.json()
    assert body["comfyui"]["reachable"] is False
    assert body["comfyui"]["version"] is None
    assert body["queue"] == {"running": 0, "pending": 0}


# ── 工作流管理端点（§2.2） ──────────────────────────────────────────


def test_workflow_upload_list_replace_delete_roundtrip(module_env, sample_workflow):
    async def _t():
        async with _client() as client:
            up1 = await _upload_workflow(client, "demo.json", sample_workflow)
            listed = await client.get("/workflows")
            up2 = await _upload_workflow(client, "demo.json", sample_workflow)  # 重名覆盖
            deleted = await client.delete("/workflows/demo")
            listed_after = await client.get("/workflows")
            return up1, listed, up2, deleted, listed_after

    up1, listed, up2, deleted, listed_after = _run(_t())
    assert up1.status_code == 200
    assert up1.json() == {"name": "demo", "replaced": False}
    assert listed.status_code == 200
    items = listed.json()
    assert len(items) == 1
    assert items[0]["name"] == "demo"
    assert items[0]["size_bytes"] > 0
    assert items[0]["mtime"] > 0
    assert up2.status_code == 200
    assert up2.json() == {"name": "demo", "replaced": True}
    assert deleted.status_code == 200
    assert deleted.json() == {"ok": True}
    assert listed_after.json() == []


def test_workflow_upload_invalid_json_rejected_400(module_env):
    async def _t():
        async with _client() as client:
            return await client.post(
                "/workflows",
                files={"file": ("bad.json", b"{not json", "application/json")},
            )

    resp = _run(_t())
    assert resp.status_code == 400
    assert "invalid workflow JSON" in resp.json()["error"]


def test_workflow_upload_invalid_api_format_rejected_400(module_env):
    bad_bodies = [
        b'"just a string"',  # 顶层非对象
        json.dumps({"1": "oops"}).encode(),  # 节点值非对象
        json.dumps({"1": {"inputs": {}}}).encode(),  # 缺 class_type
        json.dumps({"1": {"class_type": "X"}}).encode(),  # 缺 inputs
    ]

    async def _t():
        results = []
        async with _client() as client:
            for body in bad_bodies:
                results.append(
                    await client.post(
                        "/workflows",
                        files={"file": ("bad.json", body, "application/json")},
                    )
                )
        return results

    for resp in _run(_t()):
        assert resp.status_code == 400
        assert "invalid workflow format" in resp.json()["error"]


def test_workflow_upload_path_traversal_filename_sanitized(module_env, sample_workflow):
    async def _t():
        async with _client() as client:
            r1 = await client.post(
                "/workflows",
                files={
                    "file": (
                        "../../pwned.json",
                        json.dumps(sample_workflow).encode(),
                        "application/json",
                    )
                },
            )
            r2 = await client.post(
                "/workflows",
                files={
                    "file": (
                        "..\\..\\evil.JSON",
                        json.dumps(sample_workflow).encode(),
                        "application/json",
                    )
                },
            )
            return r1, r2

    r1, r2 = _run(_t())
    assert r1.status_code == 200
    assert r1.json() == {"name": "pwned", "replaced": False}
    assert r2.status_code == 200
    assert r2.json() == {"name": "evil", "replaced": False}
    # 文件落在清洗后的名字上，未逃出 workflows/ 目录
    assert (module_env / "workflows" / "pwned.json").is_file()
    assert (module_env / "workflows" / "evil.json").is_file()
    assert not (module_env.parent / "pwned.json").exists()
    assert not (module_env / "pwned.json").exists()


def test_workflow_upload_missing_file_field_400(module_env):
    async def _t():
        async with _client() as client:
            return await client.post("/workflows", data={"name": "x"})

    resp = _run(_t())
    assert resp.status_code == 400
    assert "error" in resp.json()


def test_workflow_delete_missing_404(module_env):
    async def _t():
        async with _client() as client:
            return await client.delete("/workflows/ghost")

    resp = _run(_t())
    assert resp.status_code == 404
    assert 'workflow "ghost" not found' in resp.json()["error"]


# ── generate 全流程（§2.3） ─────────────────────────────────────────


def test_generate_full_flow_success(mock_comfy, module_env, sample_workflow, capsys):
    png_bytes = b"\x89PNG\r\n\x1a\nfake-image"
    inject = {"3.inputs.image": "$input", "4.inputs.filename_prefix": "e2e"}

    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200, up.text
            return await _post_generate(
                client,
                workflow="demo",
                inject=inject,
                base_url=mock_comfy.base_url,
                files=[("3", ("input.png", png_bytes, "image/png"))],
            )

    resp = _run(_t())
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["status"] == "completed"
    assert body["output_type"] == "file"
    result = Path(body["result"])
    assert result.is_absolute()
    assert result.is_file(), f"result not on disk: {result}"
    assert result.read_bytes().startswith(MOCK_PNG_PREFIX)
    # 缺省产物目录 = MODULE_DIR/output
    assert result.parent == (module_env / "output").resolve()
    # inject 生效：提交给 mock 的工作流含上传文件名与字面量
    submitted = _last_submitted_prompt(mock_comfy.server)
    assert submitted["3"]["inputs"]["image"] == "input.png"  # 上传后的服务器侧名
    assert submitted["4"]["inputs"]["filename_prefix"] == "e2e"
    # 未注入字段保留模板默认值
    assert submitted["3"]["inputs"]["width"] == 1024
    # 轮询期间打印 EP-PROGRESS:NN%
    assert "EP-PROGRESS:" in capsys.readouterr().out
    # 文件确实上传给了 ComfyUI
    assert "input.png" in mock_comfy.server.uploaded_files


def test_generate_accepts_single_input_alias_file_field(
    mock_comfy, module_env, sample_workflow
):
    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            # 单输入也接受字段名 file；$input → 首个上游产物
            return await _post_generate(
                client,
                workflow="demo",
                inject={"3.inputs.image": "$input"},
                base_url=mock_comfy.base_url,
                files=[("file", ("alias.png", b"PNGDATA", "image/png"))],
            )

    resp = _run(_t())
    assert resp.status_code == 200, resp.text
    submitted = _last_submitted_prompt(mock_comfy.server)
    assert submitted["3"]["inputs"]["image"] == "alias.png"


def test_generate_text_upstream_injection_txt2img(mock_comfy, module_env):
    txt2img = {
        "5": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": "default prompt", "clip": ["0", 1]},
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": {"images": ["5", 0], "filename_prefix": "t2i"},
        },
    }

    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "txt2img.json", txt2img)
            assert up.status_code == 200
            return await _post_generate(
                client,
                workflow="txt2img",
                inject={"5.inputs.text": "$input.prompt"},
                base_url=mock_comfy.base_url,
                files=[("prompt", ("prompt.txt", "a lovely cat".encode(), "text/plain"))],
            )

    resp = _run(_t())
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert Path(body["result"]).read_bytes().startswith(MOCK_PNG_PREFIX)
    submitted = _last_submitted_prompt(mock_comfy.server)
    # 文本上游内容直接注入字符串字段（未上传文件）
    assert submitted["5"]["inputs"]["text"] == "a lovely cat"
    assert mock_comfy.server.uploaded_files == set()


def test_generate_workflow_not_found_400(mock_comfy, module_env, sample_workflow):
    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            return await _post_generate(client, workflow="ghost", base_url=mock_comfy.base_url)

    resp = _run(_t())
    assert resp.status_code == 400
    message = resp.json()["error"]
    assert 'workflow "ghost" not found' in message
    assert "available" in message and "demo" in message


def test_generate_missing_workflow_param_400(mock_comfy, module_env):
    async def _t():
        async with _client() as client:
            return await _post_generate(client, base_url=mock_comfy.base_url)

    resp = _run(_t())
    assert resp.status_code == 400
    assert "missing parameter: workflow" in resp.json()["error"]


def test_generate_invalid_inject_key_400_lists_available_nodes(
    mock_comfy, module_env, sample_workflow
):
    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            # 非法键：节点 99 不在工作流（校验在提交 ComfyUI 之前，不触网）
            return await _post_generate(
                client,
                workflow="demo",
                inject={"99.inputs.image": "$input"},
                base_url=mock_comfy.base_url,
                files=[("3", ("in.png", b"PNGDATA", "image/png"))],
            )

    resp = _run(_t())
    assert resp.status_code == 400
    message = resp.json()["error"]
    assert "invalid inject mapping" in message
    assert '"99"' in message
    assert "available nodes" in message
    for node_id in ("1", "3", "4"):
        assert node_id in message
    # 校验失败不应触碰 ComfyUI（无上传、无提交）
    assert mock_comfy.server.uploaded_files == set()
    assert mock_comfy.server.completed_history() == {}


def test_generate_reject_prompt_502(mock_comfy, module_env, sample_workflow):
    mock_comfy.server.reject_prompt = True  # §6 错误注入开关

    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            return await _post_generate(
                client,
                workflow="demo",
                inject={"4.inputs.filename_prefix": "x"},
                base_url=mock_comfy.base_url,
            )

    resp = _run(_t())
    assert resp.status_code == 502
    assert resp.json()["error"].startswith("comfyui rejected prompt")


def test_generate_fail_execution_502(mock_comfy, module_env, sample_workflow):
    mock_comfy.server.fail_execution = True  # §6 错误注入开关

    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            return await _post_generate(
                client,
                workflow="demo",
                inject={"4.inputs.filename_prefix": "x"},
                base_url=mock_comfy.base_url,
            )

    resp = _run(_t())
    assert resp.status_code == 502
    message = resp.json()["error"]
    assert "comfyui execution error" in message
    assert "KSampler" in message and "mock boom" in message


def test_generate_comfyui_unreachable_502(mock_comfy, module_env, sample_workflow):
    # inject 合法但 $input 文件上传阶段 ComfyUI 不可达（connect → 502）
    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            return await _post_generate(
                client,
                workflow="demo",
                inject={"3.inputs.image": "$input"},
                base_url=_dead_base_url(),
                files=[("3", ("in.png", b"PNGDATA", "image/png"))],
            )

    resp = _run(_t())
    assert resp.status_code == 502
    assert "comfyui unreachable" in resp.json()["error"]


def test_generate_output_nodes_selects_requested_node(mock_comfy, module_env):
    two_saves = {
        "1": {"class_type": "LoadImage", "inputs": {"image": "a.png"}},
        "4": {
            "class_type": "SaveImage",
            "inputs": {"images": ["1", 0], "filename_prefix": "A"},
        },
        "6": {
            "class_type": "SaveImage",
            "inputs": {"images": ["1", 0], "filename_prefix": "B"},
        },
    }

    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "twosave.json", two_saves)
            assert up.status_code == 200
            default = await _post_generate(
                client, workflow="twosave", base_url=mock_comfy.base_url
            )
            picked = await _post_generate(
                client,
                workflow="twosave",
                base_url=mock_comfy.base_url,
                output_nodes="6",
            )
            return default, picked

    default, picked = _run(_t())
    assert default.status_code == 200, default.text
    assert picked.status_code == 200, picked.text
    # 缺省主产物 = 工作流顺序第一个有 images 的节点（4 → out_<id>_0.png）
    assert Path(default.json()["result"]).name.endswith("_0.png")
    # output_nodes="6" → 主产物取节点 6 的输出（out_<id>_1.png）
    picked_path = Path(picked.json()["result"])
    assert picked_path.name.endswith("_1.png")
    assert picked_path.read_bytes().startswith(MOCK_PNG_PREFIX)


def test_generate_output_nodes_unknown_node_400(mock_comfy, module_env, sample_workflow):
    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            return await _post_generate(
                client,
                workflow="demo",
                base_url=mock_comfy.base_url,
                output_nodes="42",
            )

    resp = _run(_t())
    assert resp.status_code == 400
    message = resp.json()["error"]
    assert '"42"' in message and "available nodes" in message


def test_generate_params_json_field_supported(mock_comfy, module_env, sample_workflow):
    """平台标准传参：multipart 'params' JSON 字段（ADAPTER_API §2.3 格式 A）。"""

    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            params = json.dumps(
                {
                    "workflow": "demo",
                    "inject": {"3.inputs.image": "$input"},
                    "base_url": mock_comfy.base_url,
                }
            )
            return await client.post(
                "/predict/generate",
                data={"params": params},
                files=[("3", ("p.json.png", b"PNGDATA2", "image/png"))],
            )

    resp = _run(_t())
    assert resp.status_code == 200, resp.text
    submitted = _last_submitted_prompt(mock_comfy.server)
    assert submitted["3"]["inputs"]["image"] == "p.json.png"


def test_generate_params_output_path_overrides_default_dir(
    mock_comfy, module_env, sample_workflow
):
    alt_dir = module_env / "artifacts"

    async def _t():
        async with _client() as client:
            up = await _upload_workflow(client, "demo.json", sample_workflow)
            assert up.status_code == 200
            return await _post_generate(
                client,
                workflow="demo",
                inject={"4.inputs.filename_prefix": "alt"},
                base_url=mock_comfy.base_url,
                output_path=str(alt_dir),
            )

    resp = _run(_t())
    assert resp.status_code == 200, resp.text
    result = Path(resp.json()["result"])
    assert result.parent == alt_dir.resolve()
    assert result.read_bytes().startswith(MOCK_PNG_PREFIX)


@pytest.mark.skipif(os.geteuid() == 0, reason="os.access 恒真，无法以权限模拟不可写")
def test_generate_output_dir_not_writable_500(mock_comfy, module_env, sample_workflow):
    ro_dir = module_env / "readonly"
    ro_dir.mkdir()
    ro_dir.chmod(0o555)
    try:

        async def _t():
            async with _client() as client:
                up = await _upload_workflow(client, "demo.json", sample_workflow)
                assert up.status_code == 200
                return await _post_generate(
                    client,
                    workflow="demo",
                    inject={"4.inputs.filename_prefix": "x"},
                    base_url=mock_comfy.base_url,
                    output_path=str(ro_dir),
                )

        resp = _run(_t())
        assert resp.status_code == 500
        assert "output dir not writable" in resp.json()["error"]
    finally:
        ro_dir.chmod(0o755)
