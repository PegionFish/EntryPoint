"""test_inject.py — inject 引擎纯函数单测（CONTRACT.md §3；集成门禁 V2）。

覆盖：多组注入 / 多上游 $input.<id> / 文本注入 / 字面量 / 非法键报错（错误信息
含可用节点清单）/ 未映射字段保留默认值 / 输出节点选取解析 / 文件名清洗 /
工作流格式校验。

本文件不依赖 comfy_client.py 与 mock 服务器 —— adapter 对 comfy_client 的
import 全部为函数内惰性执行，模块缺失不影响本文件运行（无网络、无 IO 副作用）。
"""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

import pytest

# 兜底路径（conftest.py 已做同样处理，此处幂等）：保证可 import adapter
_TESTS_DIR = Path(__file__).resolve().parent
_MODULE_DIR = _TESTS_DIR.parent
for _p in (str(_TESTS_DIR), str(_MODULE_DIR)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import adapter  # noqa: E402


def _workflow() -> dict:
    """API 格式测试工作流：1 LoadImage → 3 ImageScale → 4 SaveImage，
    另含 7 CLIPTextEncode（文本注入点）与 9 KSampler（常量注入点）。"""
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
        "7": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": "default prompt", "clip": ["0", 1]},
        },
        "9": {
            "class_type": "KSampler",
            "inputs": {"seed": 0, "steps": 20, "cfg": 8.0},
        },
    }


def _stub_resolver(items: list[tuple[str, str, object]] | None = None):
    """按 §3 语义的最小解析桩（resolve_fn(expr, ctx)）。

    items: 有序 [(字段名, "file"|"text", 载荷)]；
    - "$input"        → 首个输入（文件 → "UP:<载荷>"，文本 → 文本内容）
    - "$input.<ref>"  → 按字段名定向引用
    - 其他            → 字面量原样
    """

    def resolve(expr, ctx):
        assert isinstance(ctx, dict) and "available_nodes" in ctx
        if isinstance(expr, str) and expr.startswith("$input"):
            if expr == "$input":
                if not items:
                    raise ValueError("no upstream input provided")
                item = items[0]
            else:
                ref = expr[len("$input."):]
                item = next((i for i in items if i[0] == ref), None)
                if item is None:
                    available = ", ".join(i[0] for i in items) or "(none)"
                    raise ValueError(f'upstream input "{ref}" not found; available: [{available}]')
            _field, kind, payload = item
            if kind == "text":
                return payload
            return f"UP:{payload}"
        return expr

    return resolve


# ── parse_inject ────────────────────────────────────────────────────


def test_parse_inject_accepts_json_string_dict_and_none():
    assert adapter.parse_inject(None) == {}
    assert adapter.parse_inject('{"9.inputs.seed": 42}') == {"9.inputs.seed": 42}
    mapping = {"3.inputs.image": "$input", "9.inputs.seed": 42}
    assert adapter.parse_inject(mapping) is mapping  # dict 原样透传


def test_parse_inject_rejects_bad_json_and_non_object():
    with pytest.raises(ValueError, match="invalid inject mapping"):
        adapter.parse_inject("{not json")
    with pytest.raises(ValueError, match="invalid inject mapping"):
        adapter.parse_inject("[1, 2, 3]")
    with pytest.raises(ValueError, match="invalid inject mapping"):
        adapter.parse_inject('"just a string"')
    with pytest.raises(ValueError, match="invalid inject mapping"):
        adapter.parse_inject(42)


def test_parse_inject_rejects_unsupported_value_types():
    with pytest.raises(ValueError, match="unsupported value type"):
        adapter.parse_inject({"k": {"nested": 1}})
    with pytest.raises(ValueError, match="unsupported value type"):
        adapter.parse_inject({"k": ["arr"]})
    # 标量字面量与 null 均合法
    ok = {"a": 1, "b": 1.5, "c": True, "d": "s", "e": None}
    assert adapter.parse_inject(json.dumps(ok)) == ok


# ── validate_inject_keys ────────────────────────────────────────────


def test_validate_inject_keys_ok_for_valid_nodes():
    mapping = {
        "3.inputs.image": "$input",
        "7.inputs.text": "$input.prompt",
        "9.inputs.seed": 42,
    }
    assert adapter.validate_inject_keys(_workflow(), mapping) == []


def test_validate_inject_keys_missing_node_error_lists_available_nodes():
    mapping = {"8.inputs.image": "$input", "3.inputs.width": 512}
    errors = adapter.validate_inject_keys(_workflow(), mapping)
    assert len(errors) == 1
    message = errors[0]
    assert '"8"' in message and "not found in workflow" in message
    assert "available nodes" in message
    # 错误信息必须列出模板可用节点清单（D6）
    for node_id in ("1", "3", "4", "7", "9"):
        assert node_id in message
    # 合法键不产生报错
    assert "3.inputs.width" not in message


def test_validate_inject_keys_bad_shapes():
    errors = adapter.validate_inject_keys(
        _workflow(), {"9": 42, "a.b.c.d": 1, "3.inputs.": 1, ".inputs.x": 1}
    )
    assert len(errors) == 4
    assert all("invalid inject key" in e for e in errors)


# ── apply_inject ────────────────────────────────────────────────────


def test_apply_inject_literals_and_unmapped_fields_keep_defaults():
    wf_src = _workflow()
    mapping = {
        "9.inputs.seed": 42,          # int 字面量
        "9.inputs.steps": 28,         # int 字面量
        "7.inputs.text": "hello world",  # 字符串字面量（非 $input 前缀）
        "4.inputs.filename_prefix": "e2e",
    }
    wf = adapter.apply_inject(wf_src, mapping, _stub_resolver())
    assert wf["9"]["inputs"]["seed"] == 42
    assert wf["9"]["inputs"]["steps"] == 28
    assert wf["7"]["inputs"]["text"] == "hello world"
    assert wf["4"]["inputs"]["filename_prefix"] == "e2e"
    # 未映射字段保留模板默认值（§3 规则 3）
    assert wf["9"]["inputs"]["cfg"] == 8.0
    assert wf["3"]["inputs"]["width"] == 1024
    assert wf["3"]["inputs"]["image"] == ["1", 0]
    # 原工作流不被就地修改
    assert wf_src["9"]["inputs"]["seed"] == 0
    assert wf_src["7"]["inputs"]["text"] == "default prompt"


def test_apply_inject_multi_upstream_file_and_text():
    mapping = {
        "3.inputs.image": "$input.img",   # 定向文件上游
        "7.inputs.text": "$input.prompt",  # 定向文本上游（txt2img 场景）
        "1.inputs.image": "$input",        # 首个上游产物（同为 img 文件）
        "9.inputs.seed": 7,
        "9.inputs.steps": 30,
    }
    resolver = _stub_resolver(
        [("img", "file", "ref.png"), ("prompt", "text", "a cartoon cat")]
    )
    wf = adapter.apply_inject(_workflow(), mapping, resolver)
    assert wf["3"]["inputs"]["image"] == "UP:ref.png"
    assert wf["7"]["inputs"]["text"] == "a cartoon cat"
    assert wf["1"]["inputs"]["image"] == "UP:ref.png"
    assert wf["9"]["inputs"]["seed"] == 7
    assert wf["9"]["inputs"]["steps"] == 30


def test_apply_inject_two_file_upstreams_resolved_independently():
    mapping = {"3.inputs.image": "$input.a", "1.inputs.image": "$input.b"}
    resolver = _stub_resolver(
        [("a", "file", "first.png"), ("b", "file", "second.png")]
    )
    wf = adapter.apply_inject(_workflow(), mapping, resolver)
    assert wf["3"]["inputs"]["image"] == "UP:first.png"
    assert wf["1"]["inputs"]["image"] == "UP:second.png"


def test_apply_inject_missing_field_is_created():
    wf = adapter.apply_inject(
        _workflow(), {"3.inputs.new_field": "v"}, _stub_resolver()
    )
    assert wf["3"]["inputs"]["new_field"] == "v"  # 缺失字段允许（写入新键）


def test_apply_inject_invalid_key_raises_with_available_nodes():
    with pytest.raises(ValueError) as excinfo:
        adapter.apply_inject(
            _workflow(), {"99.inputs.image": "$input"}, _stub_resolver()
        )
    message = str(excinfo.value)
    assert "invalid inject mapping" in message
    assert '"99"' in message
    assert "available nodes" in message
    for node_id in ("1", "3", "4", "7", "9"):
        assert node_id in message


def test_apply_inject_two_part_key_alias_supported():
    # 兼容 "<节点id>.<字段名>" 两段式
    wf = adapter.apply_inject(_workflow(), {"9.seed": 1}, _stub_resolver())
    assert wf["9"]["inputs"]["seed"] == 1


def test_apply_inject_supports_async_resolver():
    base = _stub_resolver([("img", "file", "a.png")])

    async def resolve(expr, ctx):
        return base(expr, ctx)

    wf = adapter.apply_inject(_workflow(), {"3.inputs.image": "$input.img"}, resolve)
    assert wf["3"]["inputs"]["image"] == "UP:a.png"


def test_apply_inject_async_variant():
    base = _stub_resolver([("img", "file", "a.png")])

    async def resolve(expr, ctx):
        return base(expr, ctx)

    wf = asyncio.run(
        adapter.apply_inject_async(_workflow(), {"3.inputs.image": "$input.img"}, resolve)
    )
    assert wf["3"]["inputs"]["image"] == "UP:a.png"


def test_apply_inject_sync_resolver_in_async_variant():
    wf = asyncio.run(
        adapter.apply_inject_async(_workflow(), {"9.inputs.seed": 5}, _stub_resolver())
    )
    assert wf["9"]["inputs"]["seed"] == 5


# ── resolve_output_nodes ────────────────────────────────────────────


def _img(filename: str) -> dict:
    return {"filename": filename, "subfolder": "", "type": "output"}


def test_resolve_output_nodes_default_follows_workflow_order_with_images():
    workflow = {
        "10": {"class_type": "SaveImage"},
        "3": {"class_type": "ImageScale"},
        "7": {"class_type": "SaveImage"},
    }
    outputs = {
        "7": {"images": [_img("b.png")]},
        "10": {"images": [_img("a.png")]},
    }
    # 缺省取全部有 images 的节点，顺序跟随工作流（10 在 7 之前）
    assert adapter.resolve_output_nodes(workflow, outputs) == ["10", "7"]


def test_resolve_output_nodes_default_none_without_images():
    assert adapter.resolve_output_nodes({"4": {"class_type": "SaveImage"}}, {}) == []


def test_resolve_output_nodes_explicit_preserves_order_and_strips_whitespace():
    workflow = {"4": {}, "6": {}, "9": {}}
    assert adapter.resolve_output_nodes(workflow, {}, " 9 ,4, ") == ["9", "4"]


def test_resolve_output_nodes_unknown_node_error_lists_available():
    with pytest.raises(ValueError) as excinfo:
        adapter.resolve_output_nodes({"4": {}, "6": {}}, {}, "8")
    message = str(excinfo.value)
    assert '"8"' in message and "available nodes" in message
    assert "4" in message and "6" in message


def test_resolve_output_nodes_empty_or_none_param_uses_default():
    workflow = {"5": {"class_type": "SaveImage"}}
    outputs = {"5": {"images": [_img("x.png")]}}
    assert adapter.resolve_output_nodes(workflow, outputs, None) == ["5"]
    assert adapter.resolve_output_nodes(workflow, outputs, "") == ["5"]
    assert adapter.resolve_output_nodes(workflow, outputs, "  ") == ["5"]


# ── clean_workflow_filename / validate_workflow_format（§2.2） ──────


def test_clean_workflow_filename_strips_traversal_and_unsafe_chars():
    assert adapter.clean_workflow_filename("../../etc/passwd") == "passwd"
    assert adapter.clean_workflow_filename("..\\..\\win_evil.JSON") == "win_evil"
    assert adapter.clean_workflow_filename("/abs/path/style.json") == "style"
    assert adapter.clean_workflow_filename("  my workflow!!.json ") == "myworkflow"
    assert adapter.clean_workflow_filename("upscale_4x.api.json") == "upscale_4x.api"


def test_clean_workflow_filename_empty_after_sanitization_rejected():
    for bad in ("   ", "../../", "中文工作流", ".json", ""):
        with pytest.raises(ValueError, match="empty after sanitization"):
            adapter.clean_workflow_filename(bad)


def test_validate_workflow_format_api_shape():
    assert adapter.validate_workflow_format(_workflow()) is None
    assert adapter.validate_workflow_format({"1": {"class_type": "X", "inputs": {}}}) is None
    assert "top level" in adapter.validate_workflow_format([1, 2])
    assert "top level" in adapter.validate_workflow_format("nope")
    assert 'node "1"' in adapter.validate_workflow_format({"1": {"inputs": {}}})
    assert 'node "1"' in adapter.validate_workflow_format({"1": {"class_type": "X"}})
    assert 'node "1"' in adapter.validate_workflow_format({"1": {"class_type": "", "inputs": {}}})
    assert 'node "1"' in adapter.validate_workflow_format({"1": "oops"})
