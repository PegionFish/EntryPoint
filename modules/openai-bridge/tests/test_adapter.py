"""test_adapter.py — adapter HTTP 层对 mock OpenAI 兼容上游的流程测试。

- mock 上游：tests.mock_openai（conftest.mock_openai fixture，127.0.0.1:0 自动
  分配端口），行为开关经 handle.server 运行期翻转（CONTRACT.md §7）。
- adapter 经 httpx ASGITransport 进程内直发（不起 uvicorn、不访问真实外网）。
- 每个测试用独立 tmp_path 作 MODULE_DIR（output/ 完全隔离）。

覆盖：三能力 e2e、SRT 入 → SRT 出时间轴逐条相等（T-23 验收核心）、
context_in 端口语义、密钥注入（api_key / key_env / 环境变量）与零明文出站、
usage.json 侧产物、params 校验与错误映射（上游 4xx/5xx/超时/不可达）。
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import httpx

# 兜底路径（conftest.py 已做同样处理，此处幂等）
_TESTS_DIR = Path(__file__).resolve().parent
_MODULE_DIR = _TESTS_DIR.parent
for _p in (str(_TESTS_DIR), str(_MODULE_DIR)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import srt_realign as sr  # noqa: E402

from conftest import run  # noqa: E402

ADAPTER_BASE = "http://adapter.test"
MOCK_AUDIO_PREFIX = b"MOCK-AUDIO-BYTES:"

# 三条字幕：含多行正文、{\an8} 位置标签、整条包裹标签（三原则 3 素材）
SRT_INPUT = (
    "1\n00:00:01,000 --> 00:00:02,500\nhello world\n\n"
    "2\n00:00:02,500 --> 00:00:04,000\nsecond line\n\n"
    "3\n00:00:04,000 --> 00:00:05,250\n{\\an8}<i>fancy entry</i>\n"
)


def _timeline_lines(text: str) -> list[str]:
    return [line for line in text.splitlines() if "-->" in line]


def _index_lines(text: str) -> list[str]:
    out = []
    for block in text.strip().split("\n\n"):
        lines = block.split("\n")
        if lines and lines[0].strip().isdigit():
            out.append(lines[0].strip())
    return out


def _chat_params(mock_openai, **overrides) -> dict:
    params = {
        "model": "mock-chat",
        "base_url": mock_openai.base_url,
        "api_key": "sk-test-secret-123",
    }
    params.update(overrides)
    return params


async def _post_json(client: httpx.AsyncClient, path: str, body: dict):
    return await client.post(path, json=body)


# ── /health 与 /info ────────────────────────────────────────────────────


def test_health_skips_probe_for_public_host(module_env, asgi_client):
    with asgi_client() as client:
        resp = run(client.get("/health"))
    assert resp.status_code == 200
    body = resp.json()
    assert body["status"] == "ok"
    assert body["openai"] == "skipped-public-probe"  # 不触外网


def test_health_probes_private_upstream(mock_openai, module_env, monkeypatch, asgi_client):
    monkeypatch.setenv("OPENAI_BASE_URL", mock_openai.base_url)
    with asgi_client() as client:
        ok = run(client.get("/health"))
    assert ok.status_code == 200
    assert ok.json()["openai"] == "reachable"


def test_health_503_when_private_upstream_dead(module_env, monkeypatch, asgi_client):
    import socket

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        dead = f"http://127.0.0.1:{sock.getsockname()[1]}/v1"
    monkeypatch.setenv("OPENAI_BASE_URL", dead)
    with asgi_client() as client:
        resp = run(client.get("/health"))
    assert resp.status_code == 503
    assert resp.json()["status"] == "unavailable"


def test_info_lists_three_capabilities(module_env, asgi_client):
    with asgi_client() as client:
        resp = run(client.get("/info"))
    assert resp.status_code == 200
    body = resp.json()
    assert body["module_id"] == "openai-bridge"
    assert [cap["name"] for cap in body["capabilities"]] == ["chat", "transcribe", "speak"]
    chat = body["capabilities"][0]
    assert chat["input_type"] == "text" and chat["output_type"] == "text"


# ── chat：普通文本 ──────────────────────────────────────────────────────


def test_chat_plain_text_and_usage_json(module_env, mock_openai, asgi_client):
    with asgi_client() as client:
        resp = run(
            _post_json(client, "/predict/chat", {"input": "hello world", "params": _chat_params(mock_openai)})
        )
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["status"] == "completed"
    assert body["output_type"] == "text"
    assert body["result"] == "HELLO WORLD"  # mock 确定性变换
    # 密钥经 Bearer 头转发
    assert mock_openai.server.auth_headers[-1] == "sk-test-secret-123"
    # usage.json 侧产物（缺省落 MODULE_DIR/output）
    usage_path = module_env / "output" / "usage.json"
    assert usage_path.is_file()
    usage = json.loads(usage_path.read_text(encoding="utf-8"))
    assert usage["module"] == "openai-bridge"
    assert usage["capability"] == "chat"
    assert usage["upstream_calls"] == 1
    assert usage["totals"]["total_tokens"] > 0
    assert "sk-test-secret-123" not in usage_path.read_text(encoding="utf-8")  # 零明文
    # 响应内 usage 与侧产物一致
    assert body["usage"]["upstream_calls"] == 1


def test_chat_key_env_reference(monkeypatch, module_env, mock_openai, asgi_client):
    monkeypatch.setenv("MY_PROVIDER_KEY", "sk-env-secret-456")
    params = {"model": "mock-chat", "base_url": mock_openai.base_url, "key_env": "MY_PROVIDER_KEY"}
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": "hi", "params": params}))
    assert resp.status_code == 200, resp.text
    assert mock_openai.server.auth_headers[-1] == "sk-env-secret-456"
    out = json.dumps(resp.json())
    assert "sk-env-secret-456" not in out  # 密钥不回显


def test_chat_missing_model_400(module_env, mock_openai, asgi_client):
    with asgi_client() as client:
        resp = run(
            _post_json(client, "/predict/chat", {"input": "hi", "params": {"base_url": mock_openai.base_url}})
        )
    assert resp.status_code == 400
    assert "missing parameter: model" in resp.json()["error"]


def test_chat_missing_input_400(module_env, mock_openai, asgi_client):
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"params": _chat_params(mock_openai)}))
    assert resp.status_code == 400
    assert "missing input" in resp.json()["error"]


def test_chat_context_in_injected_into_system(module_env, mock_openai, asgi_client):
    """context_in 端口语义：参考上下文注入 system 上下文。"""
    with asgi_client() as client:
        resp = run(
            _post_json(
                client,
                "/predict/chat",
                {
                    "input": "translate this",
                    "context_in": "glossary:EntryPoint=入口",
                    "params": _chat_params(mock_openai, system_prompt="Translate to Chinese."),
                },
            )
        )
    assert resp.status_code == 200, resp.text
    sent = mock_openai.server.chat_requests[-1]
    roles = [m["role"] for m in sent["messages"]]
    assert roles == ["system", "user"]
    assert "Translate to Chinese." in sent["messages"][0]["content"]
    assert "[Reference Context]" in sent["messages"][0]["content"]
    assert "glossary:EntryPoint=入口" in sent["messages"][0]["content"]
    assert sent["messages"][1]["content"] == "translate this"


def test_chat_context_in_list_and_multipart_file(module_env, mock_openai, asgi_client):
    """context_in 列表拍平 + multipart context 文件部件（文件名 context* 甄别）。"""
    with asgi_client() as client:
        resp = run(
            client.post(
                "/predict/chat",
                data={"params": json.dumps(_chat_params(mock_openai))},
                files=[
                    ("file", ("main.txt", b"main payload", "text/plain")),
                    ("file", ("context_terms.txt", b"term A\nterm B", "text/plain")),
                ],
            )
        )
    assert resp.status_code == 200, resp.text
    sent = mock_openai.server.chat_requests[-1]
    assert sent["messages"][-1]["content"] == "main payload"
    system_content = sent["messages"][0]["content"]
    assert "[Reference Context]" in system_content
    assert "term A" in system_content and "term B" in system_content


def test_chat_upstream_500_maps_502(module_env, mock_openai, asgi_client):
    mock_openai.server.fail_chat = True
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": "hi", "params": _chat_params(mock_openai)}))
    assert resp.status_code == 502
    assert "openai rejected request" in resp.json()["error"]


def test_chat_upstream_401_key_scrubbed(module_env, mock_openai, asgi_client):
    """上游错误正文回显密钥 → adapter 出站错误必须已脱敏（零明文）。"""
    mock_openai.server.fail_auth = True
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": "hi", "params": _chat_params(mock_openai)}))
    assert resp.status_code == 502
    error = resp.json()["error"]
    assert "sk-test-secret-123" not in error
    assert "***" in error


def test_chat_upstream_unreachable_502(module_env, asgi_client):
    import socket

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        dead = f"http://127.0.0.1:{sock.getsockname()[1]}/v1"
    params = {"model": "mock-chat", "base_url": dead, "api_key": "k"}
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": "hi", "params": params}))
    assert resp.status_code == 502
    assert "openai unreachable" in resp.json()["error"]


def test_chat_invalid_temperature_400(module_env, mock_openai, asgi_client):
    params = _chat_params(mock_openai, temperature=9.5)
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": "hi", "params": params}))
    assert resp.status_code == 400
    assert "temperature" in resp.json()["error"]


def test_chat_engine_multipart_params_json_field(module_env, mock_openai, asgi_client):
    """引擎格式 A：multipart 'params' JSON 字段（executor send_module_request 形状）。"""
    with asgi_client() as client:
        resp = run(
            client.post(
                "/predict/chat",
                data={"params": json.dumps(_chat_params(mock_openai))},
                files=[("file", ("input.txt", b"engine shape", "text/plain"))],
            )
        )
    assert resp.status_code == 200, resp.text
    assert resp.json()["result"] == "ENGINE SHAPE"


# ── chat：SRT 就地重组（T-23 验收核心） ─────────────────────────────────


def test_chat_srt_in_srt_out_timeline_identical(module_env, mock_openai, asgi_client):
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": SRT_INPUT, "params": _chat_params(mock_openai)}))
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["output_type"] == "text"
    out = body["result"]
    # 时间轴逐条相等（原文级）、序号不变
    assert _timeline_lines(out) == _timeline_lines(SRT_INPUT)
    assert _index_lines(out) == _index_lines(SRT_INPUT)
    # 正文被变换，样式标签/位置标签原样保留（代码级保护）
    assert "HELLO WORLD" in out
    assert "{\\an8}<i>FANCY ENTRY</i>" in out


def test_chat_srt_multipart_file_to_output_path(module_env, mock_openai, asgi_client, tmp_path):
    out_path = tmp_path / "out.srt"
    params = _chat_params(mock_openai, output_format="srt", output_path=str(out_path))
    with asgi_client() as client:
        resp = run(
            client.post(
                "/predict/chat",
                data={"params": json.dumps(params)},
                files=[("file", ("sub.srt", SRT_INPUT.encode("utf-8"), "application/x-subrip"))],
            )
        )
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["output_type"] == "file"
    result = Path(body["result"])
    assert result.is_file() and result == out_path.resolve()
    content = result.read_text(encoding="utf-8")
    assert _timeline_lines(content) == _timeline_lines(SRT_INPUT)
    assert _index_lines(content) == _index_lines(SRT_INPUT)


def test_chat_srt_crlf_roundtrip(module_env, mock_openai, asgi_client):
    crlf_input = SRT_INPUT.replace("\n", "\r\n")
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": crlf_input, "params": _chat_params(mock_openai)}))
    assert resp.status_code == 200, resp.text
    out = resp.json()["result"]
    assert "\r\n" in out  # 换行风格保真
    assert _timeline_lines(out) == _timeline_lines(crlf_input)


def test_chat_srt_drop_mode_fills_missing_entries(module_env, mock_openai, asgi_client):
    """模拟 LLM 丢行 → 逐条补调兜底，最终输出仍然逐条齐整。"""
    mock_openai.server.chat_mode = "drop"
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": SRT_INPUT, "params": _chat_params(mock_openai)}))
    assert resp.status_code == 200, resp.text
    out = resp.json()["result"]
    assert _timeline_lines(out) == _timeline_lines(SRT_INPUT)
    assert _index_lines(out) == _index_lines(SRT_INPUT)
    assert "HELLO WORLD" in out and "SECOND LINE" in out and "FANCY ENTRY" in out
    # 批量 1 次 + 补调 1 次 = 2 次上游调用；usage 记账如实反映
    usage = json.loads((module_env / "output" / "usage.json").read_text(encoding="utf-8"))
    assert usage["upstream_calls"] == 2


def test_chat_srt_garbage_mode_rejects_misalignment(module_env, mock_openai, asgi_client):
    """映射全缺且补调拿不到文本 → 明确报错，绝不静默错位。"""
    mock_openai.server.chat_mode = "garbage"
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": SRT_INPUT, "params": _chat_params(mock_openai)}))
    assert resp.status_code == 502
    assert "SRT realignment failed" in resp.json()["error"]


def test_chat_srt_exceeds_patch_limit_errors(module_env, mock_openai, asgi_client):
    """批量映射全缺且缺失数超补调上限（40 条 > 32）→ 快速失败。"""
    mock_openai.server.chat_mode = "garbage"

    def _ts(total_ms: int) -> str:
        h, rem = divmod(total_ms, 3_600_000)
        m, rem = divmod(rem, 60_000)
        s, ms = divmod(rem, 1000)
        return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"

    big_srt = "\n".join(
        f"{i}\n{_ts(i * 250)} --> {_ts(i * 250 + 200)}\ntext {i}\n" for i in range(1, 41)
    )
    assert sr.looks_like_srt(big_srt)
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": big_srt, "params": _chat_params(mock_openai)}))
    assert resp.status_code == 502
    assert "exceeds patch limit" in resp.json()["error"]


def test_chat_srt_invalid_srt_400(module_env, mock_openai, asgi_client):
    # 含合法时间轴行（命中 SRT 检测）+ 缺时间轴的坏块 → 解析层 400，不静默重组
    bad = "1\n00:00:01,000 --> 00:00:02,000\nok\n\n2\nthis block has no timeline\n"
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", {"input": bad, "params": _chat_params(mock_openai)}))
    assert resp.status_code == 400
    assert "no timeline line" in resp.json()["error"]


def test_chat_srt_system_prompt_and_context_reach_upstream(module_env, mock_openai, asgi_client):
    """SRT 模式：任务指令 + context_in 进入系统提示词，上游只见 ##N## 文本（不见时间轴）。"""
    params = _chat_params(mock_openai, system_prompt="Translate to Chinese.")
    body = {"input": SRT_INPUT, "context_in": "VI style guide", "params": params}
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/chat", body))
    assert resp.status_code == 200, resp.text
    sent = mock_openai.server.chat_requests[-1]
    system_content = sent["messages"][0]["content"]
    assert "Translate to Chinese." in system_content
    assert "VI style guide" in system_content
    user_content = sent["messages"][-1]["content"]
    assert "00:00:01,000" not in user_content  # 时间轴绝不外送
    assert "##1## hello world" in user_content


# ── transcribe ──────────────────────────────────────────────────────────


def test_transcribe_audio_json_result(module_env, mock_openai, asgi_client):
    params = _chat_params(mock_openai, model="whisper-mock", language="zh")
    with asgi_client() as client:
        resp = run(
            client.post(
                "/predict/transcribe",
                data={"params": json.dumps(params)},
                files=[("file", ("audio.wav", b"RIFFfake", "audio/wav"))],
            )
        )
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["output_type"] == "json"
    assert body["result"]["text"] == "hello world"
    meta = mock_openai.server.transcribe_requests[-1]
    assert meta["model"] == "whisper-mock"
    assert meta["language"] == "zh"
    assert meta["filename"] == "audio.wav"
    assert mock_openai.server.auth_headers[-1] == "sk-test-secret-123"


def test_transcribe_output_format_srt_uses_upstream_srt(module_env, mock_openai, asgi_client, tmp_path):
    """output_format=srt（未显式给 response_format）→ 上游 response_format=srt，
    输出上游返回的 SRT（T-23：输入纯音频时输出上游返回的文本/SRT）。"""
    out_path = tmp_path / "node_output.srt"
    params = _chat_params(
        mock_openai, model="whisper-mock", output_format="srt", output_path=str(out_path)
    )
    with asgi_client() as client:
        resp = run(
            client.post(
                "/predict/transcribe",
                data={"params": json.dumps(params)},
                files=[("file", ("audio.wav", b"RIFFfake", "audio/wav"))],
            )
        )
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["output_type"] == "file"
    content = Path(body["result"]).read_text(encoding="utf-8")
    assert "00:00:01,000 --> 00:00:03,500" in content  # mock 上游 SRT 原文
    assert mock_openai.server.transcribe_requests[-1]["response_format"] == "srt"


def test_transcribe_verbose_json_builds_srt_from_segments(module_env, mock_openai, asgi_client, tmp_path):
    out_path = tmp_path / "seg.srt"
    params = _chat_params(
        mock_openai, model="whisper-mock", response_format="verbose_json",
        output_format="srt", output_path=str(out_path),
    )
    with asgi_client() as client:
        resp = run(
            client.post(
                "/predict/transcribe",
                data={"params": json.dumps(params)},
                files=[("file", ("audio.wav", b"RIFFfake", "audio/wav"))],
            )
        )
    assert resp.status_code == 200, resp.text
    content = Path(resp.json()["result"]).read_text(encoding="utf-8")
    assert "1\n00:00:00,000 --> 00:00:01,200\nhello" in content
    assert "2\n00:00:01,200 --> 00:00:02,500\nworld" in content


def test_transcribe_srt_input_realigns_via_chat(module_env, mock_openai, asgi_client):
    """transcribe 的 SRT 输入：逐段文本变换走上游 chat 端点，时间轴逐条回填。"""
    with asgi_client() as client:
        resp = run(
            client.post(
                "/predict/transcribe",
                data={"params": json.dumps(_chat_params(mock_openai, model="mock-chat"))},
                files=[("file", ("subtitle.srt", SRT_INPUT.encode("utf-8"), "application/x-subrip"))],
            )
        )
    assert resp.status_code == 200, resp.text
    body = resp.json()
    out = body["result"]
    assert _timeline_lines(out) == _timeline_lines(SRT_INPUT)
    assert _index_lines(out) == _index_lines(SRT_INPUT)
    assert "HELLO WORLD" in out
    assert mock_openai.server.transcribe_requests == []  # 未触 /audio/transcriptions
    assert len(mock_openai.server.chat_requests) == 1


def test_transcribe_missing_input_400(module_env, mock_openai, asgi_client):
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/transcribe", {"params": _chat_params(mock_openai, model="w")}))
    assert resp.status_code == 400
    assert "missing input" in resp.json()["error"]


def test_transcribe_json_body_input_path(module_env, mock_openai, asgi_client, tmp_path):
    audio = tmp_path / "clip.wav"
    audio.write_bytes(b"RIFFfake")
    params = _chat_params(mock_openai, model="whisper-mock")
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/transcribe", {"input_path": str(audio), "params": params}))
    assert resp.status_code == 200, resp.text
    assert mock_openai.server.transcribe_requests[-1]["filename"] == "clip.wav"


def test_transcribe_input_path_srt_file(module_env, mock_openai, asgi_client, tmp_path):
    srt_file = tmp_path / "in.srt"
    srt_file.write_text(SRT_INPUT, encoding="utf-8")
    params = _chat_params(mock_openai, model="mock-chat")
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/transcribe", {"input_path": str(srt_file), "params": params}))
    assert resp.status_code == 200, resp.text
    out = resp.json()["result"]
    assert _timeline_lines(out) == _timeline_lines(SRT_INPUT)


# ── speak ───────────────────────────────────────────────────────────────


def test_speak_produces_audio_file(module_env, mock_openai, asgi_client):
    params = _chat_params(mock_openai, model="tts-mock", voice="alloy", response_format="mp3")
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/speak", {"input": "你好世界", "params": params}))
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["output_type"] == "file"
    result = Path(body["result"])
    assert result.is_file()
    assert result.suffix == ".mp3"
    assert result.read_bytes().startswith(MOCK_AUDIO_PREFIX)
    sent = mock_openai.server.speech_requests[-1]
    assert sent["model"] == "tts-mock"
    assert sent["voice"] == "alloy"
    assert sent["input"] == "你好世界"
    assert mock_openai.server.auth_headers[-1] == "sk-test-secret-123"
    # speech 端点无 token usage → 如实记 null
    usage = json.loads((module_env / "output" / "usage.json").read_text(encoding="utf-8"))
    assert usage["capability"] == "speak"
    assert usage["totals"]["prompt_tokens"] is None


def test_speak_output_path_and_wav(module_env, mock_openai, asgi_client, tmp_path):
    out_path = tmp_path / "voice.wav"
    params = _chat_params(
        mock_openai, model="tts-mock", voice="nova", response_format="wav",
        output_format="wav", output_path=str(out_path),
    )
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/speak", {"input": "text", "params": params}))
    assert resp.status_code == 200, resp.text
    result = Path(resp.json()["result"])
    assert result == out_path.resolve()
    assert result.read_bytes().endswith(b"nova")


def test_speak_missing_voice_defaults_alloy(monkeypatch, module_env, mock_openai, asgi_client):
    monkeypatch.setenv("OPENAI_API_KEY_FOR_TEST", "sk-default-789")
    params = _chat_params(mock_openai, model="tts-mock")
    del params["api_key"]
    params["key_env"] = "OPENAI_API_KEY_FOR_TEST"
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/speak", {"input": "text", "params": params}))
    assert resp.status_code == 200, resp.text
    assert mock_openai.server.speech_requests[-1]["voice"] == "alloy"


def test_speak_missing_input_400(module_env, mock_openai, asgi_client):
    with asgi_client() as client:
        resp = run(_post_json(client, "/predict/speak", {"params": _chat_params(mock_openai, model="t")}))
    assert resp.status_code == 400


# ── unknown capability ──────────────────────────────────────────────────


def test_unknown_capability_404(module_env, asgi_client):
    with asgi_client() as client:
        resp = run(client.post("/predict/embed", json={"input": "x", "params": {}}))
    assert resp.status_code == 404
