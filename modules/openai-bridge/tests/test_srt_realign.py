"""test_srt_realign.py — SRT 就地重组纯函数核心单测（零 IO、零网络）。

核心证明（T-23 验收「SRT 入 → SRT 出时间轴逐条相等」的解析器半边）：
- round-trip：parse → render 与输入逐字节一致（LF/CRLF/BOM/缺序号/多行正文）；
- 重组不变量：仅替换正文后，时间轴行与序号行原文逐条相等；
- 标签保护（三原则 3）：{\\an8} 前缀标签与整条包裹标签剥离/回填原样；
- ##N## 协议：完整映射 / 缺失检测 / 哨兵还原 / 乱序与重复容忍。
"""

from __future__ import annotations

import srt_realign as sr


def _timeline_lines(text: str) -> list[str]:
    """提取时间轴行原文（含行尾空白）——「时间轴不变」的判定口径。"""
    return [line for line in text.splitlines() if "-->" in line]


def _index_lines(text: str) -> list[str]:
    """提取序号行原文 ——「序号不变」的判定口径。"""
    out = []
    for block in text.strip().split("\n\n"):
        lines = block.split("\n")
        if lines and lines[0].strip().isdigit():
            out.append(lines[0].strip())
    return out


# ── looks_like_srt ──────────────────────────────────────────────────────


def test_looks_like_srt_positive_and_negative():
    srt = "1\n00:00:01,000 --> 00:00:02,000\nhi\n"
    assert sr.looks_like_srt(srt)
    assert not sr.looks_like_srt("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhi\n")  # VTT 点毫秒
    assert not sr.looks_like_srt("plain text")
    assert not sr.looks_like_srt("")


# ── round-trip：parse → render 逐字节一致 ───────────────────────────────


def test_roundtrip_lf_basic():
    text = (
        "1\n00:00:01,000 --> 00:00:02,500\nHello\n\n"
        "2\n00:00:02,500 --> 00:00:04,000\nWorld\n"
    )
    assert sr.render_srt(sr.parse_srt(text)) == text


def test_roundtrip_crlf_preserved():
    text = (
        "1\r\n00:00:01,000 --> 00:00:02,500\r\nHello\r\n\r\n"
        "2\r\n00:00:02,500 --> 00:00:04,000\r\nWorld\r\n"
    )
    assert sr.render_srt(sr.parse_srt(text)) == text


def test_roundtrip_bom_and_no_trailing_newline():
    text = "1\n00:00:01,000 --> 00:00:02,000\nHi"
    doc = sr.parse_srt("\ufeff" + text)
    assert doc.has_bom
    rendered = sr.render_srt(doc)
    assert rendered == "\ufeff" + text  # 无结尾换行保真


def test_roundtrip_missing_index_and_multiline_text():
    text = (
        "00:00:01,000 --> 00:00:02,000\n"
        "line one\n"
        "line two\n\n"
        "2\n00:00:02,000 --> 00:00:03,000\nafter\n"
    )
    assert sr.render_srt(sr.parse_srt(text)) == text


def test_parse_rejects_block_without_timeline():
    import pytest

    with pytest.raises(ValueError, match="no timeline line"):
        sr.parse_srt("1\njust text no timeline\n\n2\n00:00:01,000 --> 00:00:02,000\nok\n")
    with pytest.raises(ValueError, match="no subtitle entries"):
        sr.parse_srt("  \n\n  \n")


# ── 重组不变量：仅换正文，时间轴/序号逐条相等 ───────────────────────────


def test_reassembled_output_keeps_timeline_and_indices_exactly():
    """T-23 验收核心断言（纯函数半边）：变换正文后时间轴与序号逐条相等。"""
    original = (
        "1\n00:00:01,000 --> 00:00:02,500\nhello\n\n"
        "2\n00:00:02,500 --> 00:00:04,000\nbig world\n\n"
        "3\n00:00:04,000 --> 00:00:05,250\n bye\n"
    )
    doc = sr.parse_srt(original)
    for entry in doc.entries:
        protected = sr.extract_protected("\n".join(entry.text_lines))
        new_core = protected.core.upper()
        entry.text_lines = sr.restore_protected(protected, new_core).split("\n")
    out = sr.render_srt(doc)

    assert _timeline_lines(out) == _timeline_lines(original)  # 时间轴逐条相等（原文级）
    assert _index_lines(out) == _index_lines(original)  # 序号逐条相等
    assert "HELLO" in out and "BIG WORLD" in out and " BYE" in out.upper()
    # CRLF 风格按输入保留
    crlf_doc = sr.parse_srt(original.replace("\n", "\r\n"))
    assert crlf_doc.eol == "\r\n"


# ── 标签保护（三原则 3） ────────────────────────────────────────────────


def test_extract_protected_position_tag_and_wrap_tags():
    protected = sr.extract_protected("{\\an8}<i>hello <b>world</b></i>")
    assert protected.prefix_overrides == "{\\an8}"
    # 只有 <i> 是「整条包裹」可剥离；<b> 起点在正文中间 → 随正文直送（best-effort）
    assert protected.open_tags == ["<i>"]
    assert protected.close_tags == ["</i>"]
    assert protected.core == "hello <b>world</b>"
    restored = sr.restore_protected(protected, "HELLO <b>WORLD</b>")
    assert restored == "{\\an8}<i>HELLO <b>WORLD</b></i>"


def test_restore_protected_exact_nesting():
    protected = sr.extract_protected("{\\an8}<i><b>hello</b></i>")
    assert sr.restore_protected(protected, "HI") == "{\\an8}<i><b>HI</b></i>"


def test_extract_protected_font_tag_with_attrs():
    protected = sr.extract_protected('<font color="#ff0000">red text</font>')
    assert protected.open_tags == ['<font color="#ff0000">']
    assert protected.close_tags == ["</font>"]
    assert protected.core == "red text"


def test_extract_protected_leaves_partial_wraps_in_core():
    # <i>a</i> b：非整条包裹 → 不剥离，随正文直送（best-effort 语义）
    protected = sr.extract_protected("<i>hello</i> world")
    assert protected.open_tags == []
    assert protected.core == "<i>hello</i> world"
    # 嵌套中间出现同名闭标签 → 放弃外层剥离
    protected2 = sr.extract_protected("<i>a</i>middle</i>")
    assert protected2.open_tags == []


def test_extract_protected_plain_text_untouched():
    protected = sr.extract_protected("plain text")
    assert protected.core == "plain text"
    assert protected.prefix_overrides == ""


# ── ##N## 序号协议 ──────────────────────────────────────────────────────


def test_build_and_parse_full_map_with_sentinel():
    cores = ["first line", "multi\nline entry", "third"]
    payload = sr.build_numbered_payload(cores)
    assert payload == "##1## first line\n##2## multi\\nline entry\n##3## third"

    reply = "noise before\n##1## FIRST\n##2## MULTI\\nLINE ENTRY\n##3## THIRD\n"
    mapping, missing = sr.parse_numbered_response(reply, 3)
    assert missing == set()
    assert mapping == {1: "FIRST", 2: "MULTI\nLINE ENTRY", 3: "THIRD"}


def test_parse_continuation_line_is_ambiguous_not_appended():
    """折行/尾随闲聊不并入正文（防数据污染/丢失歧义）→ 计入缺失，交逐条补调。"""
    reply = "##1## ONE\n##3## THIRD\ntrailing chatter"
    mapping, missing = sr.parse_numbered_response(reply, 3)
    assert mapping == {1: "ONE"}
    assert missing == {2, 3}  # 2 本来就缺；3 有续行 → 歧义缺失


def test_parse_inline_marker_terminates_previous_segment():
    """审查 P2-1 复核（实测不成立）：内嵌 ##N## 作为后续标记必然终结前段
    （finditer 切分），first_line 不可能含标记——首行各段干净解析。
    代价：字幕正文含字面 ##N##（协议碰撞）会被截断（见 CONTRACT 边界）。"""
    reply = "##1## A ##2## B\n##3## THIRD"
    mapping, missing = sr.parse_numbered_response(reply, 3)
    assert mapping == {1: "A", 2: "B", 3: "THIRD"}
    assert missing == set()
    assert "##" not in mapping[1] and "##" not in mapping[2]


def test_parse_detects_missing_and_duplicate_prefers_first():
    reply = "##1## ONE\n##3## THREE\n##3## THREE-FAKE"
    mapping, missing = sr.parse_numbered_response(reply, 3)
    assert mapping == {1: "ONE", 3: "THREE"}  # 缺 2；重复取首次
    assert missing == {2}


def test_parse_empty_transform_counts_as_missing():
    reply = "##1##\n##2## TWO"
    mapping, missing = sr.parse_numbered_response(reply, 2)
    assert mapping == {2: "TWO"}
    assert missing == {1}


def test_parse_garbage_maps_nothing():
    mapping, missing = sr.parse_numbered_response("no markers at all", 3)
    assert mapping == {}
    assert missing == {1, 2, 3}


def test_realign_system_prompt_composition():
    prompt = sr.realign_system_prompt("Translate to Chinese.", "[Reference Context]\nglossary")
    assert prompt.startswith("Translate to Chinese.")
    assert "glossary" in prompt
    assert "##N##" in prompt  # 协议块在末尾
    default = sr.realign_system_prompt(None, None)
    assert "Correct spelling" in default  # 缺省任务 = 校对润色
