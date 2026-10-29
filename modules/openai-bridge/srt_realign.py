"""srt_realign.py — SRT 就地重组核心（纯函数层：零 IO、零网络、零第三方依赖）。

格式保真三原则（reports/audit/2026-10-25-master-report.md 计划6补充「出口逻辑三原则」②③；
reports/plan/2026-10-25-wave2-dispatch.md T-23 验收）：

  1. 时间轴不变 —— 输出 SRT 每条的起止时间与输入对应条目完全一致
     （时间轴行原文逐字保留，不做任何重排/重算）；
  2. 序号不变 —— 输出沿用输入的序号行原文（缺序号的容错条目输出同样缺序号）；
  3. 非正文标记/样式标签原样保留 —— ``{\\an8}`` 位置标签与「整条包裹」标签
     （<i> / <b> / <u> / <font ...>）由 extract_protected 剥离、restore_protected
     在重组后原样回填（代码级保证）；正文行内其余标签随正文送上游，经系统
     提示词要求原样保留（best-effort，见 CONTRACT.md 局限声明）。

重组协议（防时间轴漂移的关键）：上游 LLM 不接触时间轴 —— 只见
``##N## <单行文本>`` 协议行；响应按标记回映射到原条目；映射不齐时由
adapter 侧逐条补调（见 adapter._realign_missing_entries）。

边界（诚实声明）：
- 仅支持毫秒逗号时间戳（``HH:MM:SS,mmm``）的 SRT；WebTT/ASS（点毫秒）不做重组；
- 跨条目延续的包裹标签（如 ``<i>`` 在下一条才闭合）不识别为可剥离标签；
- 输入内混合换行符按主导风格归一（\\r\\n 或 \\n 二选一）。
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

# ── 识别与解析 ──────────────────────────────────────────────────────────

BOM = "\ufeff"

# SRT 时间轴行（毫秒逗号）：HH:MM:SS,mmm --> HH:MM:SS,mmm
_TIMELINE_RE = re.compile(
    r"^\s*\d{1,2}:\d{2}:\d{2},\d{1,3}\s*-->\s*\d{1,2}:\d{2}:\d{2},\d{1,3}.*$"
)
_TIMELINE_ANY_RE = re.compile(
    r"\d{1,2}:\d{2}:\d{2},\d{1,3}\s*-->\s*\d{1,2}:\d{2}:\d{2},\d{1,3}"
)
_INDEX_RE = re.compile(r"^\s*\d+\s*$")


def looks_like_srt(text: str) -> bool:
    """廉价判定文本是否为 SRT（含毫秒逗号时间轴行）。VTT/ASS 纯文本不命中。"""
    return bool(_TIMELINE_ANY_RE.search(text or ""))


@dataclass
class SrtEntry:
    """单条字幕：序号行与时间轴行【原文】保留，仅 text_lines 会被替换。"""

    index_raw: str  # 序号行原文（如 "1"）；缺序号条目为 ""
    timeline_raw: str  # 时间轴行原文（含行尾空白与 --> 右侧坐标等）
    text_lines: list[str] = field(default_factory=list)


@dataclass
class SrtDocument:
    """解析结果：entries + 换行风格 + BOM/结尾换行保真信息。"""

    entries: list[SrtEntry] = field(default_factory=list)
    eol: str = "\n"  # 输入的主导换行风格（"\r\n" 或 "\n"）
    has_bom: bool = False
    ends_with_newline: bool = True


def parse_srt(text: str) -> SrtDocument:
    """SRT 文本 → SrtDocument。

    - 按空行分块；块 = [可选序号行] + 时间轴行 + 1..N 正文行；
    - 缺序号行容忍（index_raw=""）；块内无时间轴行 → ValueError（非法 SRT，
      上层转 400，不静默错位重组）；
    - 换行按主导风格归一（混合 CRLF/LF 时以含 "\r\n" 与否定夺）。
    """
    if not isinstance(text, str):
        raise ValueError("invalid SRT: input must be a string")
    has_bom = text.startswith(BOM)
    if has_bom:
        text = text[len(BOM):]
    eol = "\r\n" if "\r\n" in text else "\n"
    lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")

    # 按空行分块（跳过首尾空行）
    blocks: list[list[str]] = []
    current: list[str] = []
    for line in lines:
        if line.strip() == "":
            if current:
                blocks.append(current)
                current = []
        else:
            current.append(line)
    if current:
        blocks.append(current)

    entries: list[SrtEntry] = []
    for pos, block in enumerate(blocks, start=1):
        if not _TIMELINE_RE.match(block[0]):
            # 首行是纯数字 → 缺时间轴行；否则缺序号+时间轴行
            if _INDEX_RE.match(block[0]) and len(block) >= 2 and _TIMELINE_RE.match(block[1]):
                entry = SrtEntry(index_raw=block[0], timeline_raw=block[1], text_lines=block[2:])
            else:
                raise ValueError(
                    f"invalid SRT: entry {pos} has no timeline line "
                    "(expected 'HH:MM:SS,mmm --> HH:MM:SS,mmm')"
                )
        else:
            entry = SrtEntry(index_raw="", timeline_raw=block[0], text_lines=block[1:])
        entries.append(entry)

    if not entries:
        raise ValueError("invalid SRT: no subtitle entries found")
    return SrtDocument(
        entries=entries,
        eol=eol,
        has_bom=has_bom,
        ends_with_newline=text.endswith("\n"),
    )


def render_srt(doc: SrtDocument) -> str:
    """SrtDocument → SRT 文本（未替换 text_lines 时与输入逐字节一致，见 round-trip 测试）。

    条目之间以一个空行分隔；结尾按输入是否有换行还原（多个连续结尾空行
    归一为一个，属空白保真豁免项，见模块 docstring 边界）。
    """
    blocks = []
    for entry in doc.entries:
        lines = ([entry.index_raw] if entry.index_raw else []) + [
            entry.timeline_raw
        ] + list(entry.text_lines)
        blocks.append(doc.eol.join(lines))
    body = (doc.eol + doc.eol).join(blocks)
    if doc.ends_with_newline:
        body += doc.eol
    return (BOM if doc.has_bom else "") + body


# ── 标签保护（三原则 3） ────────────────────────────────────────────────

# 行首连续的 {...} 覆盖标签（ASS 派生位置标签，如 {\an8}；LLM 极易丢弃/挪动 → 代码级保真）
_PREFIX_OVERRIDES_RE = re.compile(r"^(\s*(?:\{[^{}]*\})+)", re.DOTALL)
# 整条包裹开标签：<i> / <b> / <u> / <font color="#fff"> 等
_WRAP_OPEN_RE = re.compile(r"^\s*<([A-Za-z][A-Za-z0-9]*)\b[^<>]*>")


@dataclass
class Protected:
    """从条目正文中剥离出的保护片段。core 才送上游。"""

    prefix_overrides: str  # 行首 {...} 串（原文，含前导空白）
    open_tags: list[str] = field(default_factory=list)  # 包裹开标签原文（外→内）
    close_tags: list[str] = field(default_factory=list)  # 对应闭标签原文（外→内）
    core: str = ""


def extract_protected(text: str) -> Protected:
    """剥离行首 {...} 覆盖标签与整条包裹标签，返回 (保护片段, 核心正文)。

    整条包裹判定：开标签之后、末尾闭标签之前不得出现同名闭标签（否则
    ``<i>a</i> b`` / ``<i>a</i><b>x</b>`` 这类非整条包裹不剥离，随正文直送）。
    """
    text = text or ""
    prefix = ""
    m = _PREFIX_OVERRIDES_RE.match(text)
    if m:
        prefix = m.group(1)
        text = text[m.end():]

    open_tags: list[str] = []
    close_tags: list[str] = []
    core = text
    while True:
        wm = _WRAP_OPEN_RE.match(core)
        if not wm:
            break
        tag = wm.group(1)
        close = f"</{tag}>"
        close_pos = core.lower().rfind(close.lower())
        if close_pos < 0:
            break
        end_pos = close_pos + len(close)
        if core[end_pos:].strip():
            break  # 闭标签后还有正文 → 非整条包裹
        inner = core[wm.end():close_pos]
        if close.lower() in inner.lower():
            break  # 内部同名闭标签 → 非整条包裹（外层放弃，避免错剥）
        open_tags.append(core[: wm.end()])
        close_tags.append(core[close_pos:end_pos])
        core = inner
    return Protected(prefix_overrides=prefix, open_tags=open_tags, close_tags=close_tags, core=core)


def restore_protected(protected: Protected, new_core: str) -> str:
    """把变换后的核心正文回填进保护片段（前缀 {...} 在最前，包裹标签按原嵌套序复原）。"""
    parts: list[str] = [protected.prefix_overrides]
    parts.extend(protected.open_tags)
    parts.append(new_core)
    parts.extend(reversed(protected.close_tags))
    return "".join(parts)


# ── ##N## 序号协议（LLM 只见文本，不见时间轴） ─────────────────────────

NEWLINE_SENTINEL = "\\n"  # 条目内换行在协议行中转义为字面量 \n（字幕正文几乎不含反斜杠，见 CONTRACT 局限）

_REALIGN_PROTOCOL_PROMPT = (
    "PROTOCOL (must follow exactly): The user message contains one line per subtitle, "
    "formatted as '##N## text'. Transform ONLY the text after the '##N## ' marker. "
    "Return EXACTLY one output line per input line with the SAME '##N##' marker, "
    "the SAME order and the SAME count. Never merge, split, add, drop, or renumber lines. "
    "Never add explanations, quotes, or blank lines. "
    "Preserve literal '\\n' escape sequences and any inline markup tags unchanged."
)

DEFAULT_TASK_PROMPT = (
    "Correct spelling and punctuation of the subtitle text. "
    "Keep the original language and meaning."
)


def build_numbered_payload(cores: list[str]) -> str:
    """核心正文列表 → 协议载荷（每条一行：##N## <换行转义后的正文>）。"""
    lines = []
    for i, core in enumerate(cores, start=1):
        lines.append(f"##{i}## {core.replace(chr(10), NEWLINE_SENTINEL)}")
    return "\n".join(lines)


def realign_system_prompt(task_prompt: str | None, context_block: str | None) -> str:
    """SRT 重组调用的系统提示词：任务指令 + 协议块（+ 可选参考上下文）。"""
    parts = [(task_prompt or "").strip() or DEFAULT_TASK_PROMPT]
    if context_block:
        parts.append(context_block)
    parts.append(_REALIGN_PROTOCOL_PROMPT)
    return "\n\n".join(p for p in parts if p)


_MARK_RE = re.compile(r"##(\d+)##")


def parse_numbered_response(content: str, count: int) -> tuple[dict[int, str], set[int]]:
    """协议响应 → ({序号: 新正文(已还原换行)}, 缺失序号集合)。

    - 协议要求一条一行（条目内换行由哨兵携带）：标记后只取**首行**；
    - 首行之外还有续行（模型折行 / 尾随闲聊）→ 该条视为【歧义】，计入缺失，
      交由逐条补调拿干净文本 —— 宁可补调，不猜测（不静默错位）；
    - 空变换结果视为缺失；
    - 同序号重复出现取首次（防尾部幻觉覆盖正解）。
    """
    mapping: dict[int, str] = {}
    matches = list(_MARK_RE.finditer(content or ""))
    for k, m in enumerate(matches):
        n = int(m.group(1))
        if n in mapping:
            continue
        end = matches[k + 1].start() if k + 1 < len(matches) else len(content)
        seg = content[m.end():end]
        first_line, _, rest = seg.partition("\n")
        first_line = first_line.strip()
        if first_line and not rest.strip():
            mapping[n] = first_line.replace(NEWLINE_SENTINEL, "\n")
    missing = {i for i in range(1, count + 1) if i not in mapping}
    return mapping, missing
