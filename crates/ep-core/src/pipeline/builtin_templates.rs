//! 内置 ffmpeg 节点模板库（T-13：零模型成本扩管线能力）。
//!
//! 设计目标：**模板 = 数据**。新增一个内置 ffmpeg 节点只需在本文件的
//! [`TEMPLATES`] 静态表追加一条记录——侧栏清单（`GET /api/builtins`）、
//! DAG 端口类型登记、执行器命令拼装共用同一数据源，Rust/前端代码零改动。
//!
//! 节点在管线 spec 中的形态（与既有 builtin 节点一致，仅多一个 `template` 参数）：
//!
//! ```json
//! {
//!   "kind": "builtin",
//!   "builtin": "ffmpeg_template",
//!   "params": { "template": "transcode", "codec": "h264", "crf": 23 }
//! }
//! ```
//!
//! 命令拼装规则（[`render_template_args`]）：
//! - 模板 `args` 中的占位符在运行时替换：
//!   - `{input}` / `{input2}`  → 第 1/2 个上游文件产物（多入口按边顺序），
//!   - `{input_filter}` / `{input2_filter}` → 同上，但按 ffmpeg 滤镜参数转义
//!     （Windows 盘符 `C:\…` → `C\:/…`，供 `subtitles=` 等滤镜使用），
//!   - `{output}` → 本节点解析出的输出路径，
//!   - `{param:<name>}` → 参数值；`select` 参数可经 `select_exprs` 映射为
//!     ffmpeg 表达式（如 `h264 → libx264`、水印位置 → overlay 坐标表达式），
//!     映射表达式内可继续嵌套 `{param:*}`（最多 4 轮展开）。
//! - 参数缺省时取模板默认值；数值参数做范围校验、`select` 参数做枚举校验。
//! - 声明 `omit_when_empty` 的参数渲染为空串时，该 arg 连同紧邻的前一个
//!   ffmpeg 选项 flag（如 `-vf`）一并省略——实现「可选滤镜/可选输出参数」。

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde_json::{json, Map, Value};

use crate::pipeline::dag::PortType;

/// 内置模板节点的 builtin 标识（`kind = "builtin"` 时的 `builtin` 字段值）。
pub const FFMPEG_TEMPLATE_KIND: &str = "ffmpeg_template";

/// 目录型产物（抽帧序列）manifest 的类型标识。
///
/// executor 对 `output_is_dir` 模板不再返回 `Artifact::File(目录)`（目录无法
/// 下载、`size` 恒 0），而是返回带此标识的 [`Artifact::Json`] manifest，
/// 由 daemon 侧展开为逐个可下载文件。两端共用本常量作契约。
pub const FRAME_MANIFEST_KIND: &str = "ffmpeg_template_frames";

/// 单节点展开登记的最大文件数：防一次抽帧数千帧把任务记录/产物列表撑爆。
pub const MAX_EXPANDED_FRAME_FILES: usize = 200;

/// 模板分类（侧栏分组维度；前端 i18n 映射为分组标题）。
pub const CATEGORY_VIDEO: &str = "video";
pub const CATEGORY_AUDIO: &str = "audio";
pub const CATEGORY_SUBTITLE: &str = "subtitle";

/// 参数类型（复用能力参数 schema 形态：select/int/bool + min/max/default）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateParamKind {
    /// 整数（min/max 闭区间）
    Integer,
    /// 浮点数（min/max 闭区间）
    Number,
    /// 枚举（`options` 白名单）
    Select,
    /// 布尔（true/false）
    Bool,
    /// 自由字符串
    String,
}

impl TemplateParamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Select => "select",
            Self::Bool => "bool",
            Self::String => "string",
        }
    }
}

/// 模板参数 schema（纯数据；默认值以字符串存储，渲染时按 [`kind`](Self::kind) 转换）。
#[derive(Debug, Clone, Copy)]
pub struct TemplateParam {
    pub name: &'static str,
    pub kind: TemplateParamKind,
    /// 默认值（字符串形态；`Integer`/`Number` 在渲染时解析为数字）
    pub default: &'static str,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// `Select` 的合法取值（其他类型为空切片）
    pub options: &'static [&'static str],
    /// `Select` 取值 → 渲染表达式（缺省时渲染取值本身）
    pub select_exprs: &'static [(&'static str, &'static str)],
    /// true：参数值为空串时省略对应的 ffmpeg arg（含前一个选项 flag）
    pub omit_when_empty: bool,
}

impl TemplateParam {
    /// 类型化默认值（`GET /api/builtins` 下发 / 前端建节点时写入 params）。
    pub fn default_value(&self) -> Value {
        match self.kind {
            TemplateParamKind::Integer => self
                .default
                .parse::<i64>()
                .map(Value::from)
                .unwrap_or_else(|_| Value::String(self.default.to_string())),
            TemplateParamKind::Number => self
                .default
                .parse::<f64>()
                .map(Value::from)
                .unwrap_or_else(|_| Value::String(self.default.to_string())),
            TemplateParamKind::Bool => Value::Bool(self.default.eq_ignore_ascii_case("true")),
            TemplateParamKind::Select | TemplateParamKind::String => {
                Value::String(self.default.to_string())
            }
        }
    }
}

/// 内置模板的完整定义（静态表元素）。
#[derive(Debug, Clone, Copy)]
pub struct BuiltinTemplate {
    /// 模板 id（管线节点 `params.template` 的取值）
    pub id: &'static str,
    /// 固定为 [`FFMPEG_TEMPLATE_KIND`]
    pub kind: &'static str,
    pub display_name: &'static str,
    pub display_name_en: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub description_en: &'static str,
    pub params: &'static [TemplateParam],
    /// 命名入口 + 端口类型（写入 DAG 校验；首个为 `{input}`，第二个为 `{input2}`）
    pub input_ports: &'static [(&'static str, PortType)],
    /// 产物端口类型（输出端口名固定 `output`）
    pub output_port: PortType,
    /// 命令构建描述
    pub ffmpeg_build: FfmpegBuild,
}

/// ffmpeg 命令构建描述。
#[derive(Debug, Clone, Copy)]
pub struct FfmpegBuild {
    /// 参数模板（含 `{input}` / `{input2}` / `{output}` / `{param:*}` 占位符）
    pub args: &'static [&'static str],
    /// 产物扩展名（不含点）；目录产物为空串
    pub output_extension: &'static str,
    /// 扩展名由该参数值决定（如 subtitles 的 `to_format`），优先于上面的固定值
    pub extension_param: Option<&'static str>,
    /// true：产物是目录（抽帧序列）而非单文件
    pub output_is_dir: bool,
}

// ─── 参数构造辅助（const，供静态表使用） ────────────────────────────────────

// 静态表构造器：字段全部显式命名（比每行写完整结构体字面量更易读），
// 8 个参数是有意形态，故豁免 clippy 参数个数检查。
#[allow(clippy::too_many_arguments)]
const fn param(
    name: &'static str,
    kind: TemplateParamKind,
    default: &'static str,
    min: Option<f64>,
    max: Option<f64>,
    options: &'static [&'static str],
    select_exprs: &'static [(&'static str, &'static str)],
    omit_when_empty: bool,
) -> TemplateParam {
    TemplateParam {
        name,
        kind,
        default,
        min,
        max,
        options,
        select_exprs,
        omit_when_empty,
    }
}

const fn int_param(
    name: &'static str,
    default: &'static str,
    min: f64,
    max: f64,
) -> TemplateParam {
    param(
        name,
        TemplateParamKind::Integer,
        default,
        Some(min),
        Some(max),
        &[],
        &[],
        false,
    )
}

const fn num_param(
    name: &'static str,
    default: &'static str,
    min: f64,
    max: f64,
) -> TemplateParam {
    param(
        name,
        TemplateParamKind::Number,
        default,
        Some(min),
        Some(max),
        &[],
        &[],
        false,
    )
}

const fn select_param(
    name: &'static str,
    default: &'static str,
    options: &'static [&'static str],
) -> TemplateParam {
    param(
        name,
        TemplateParamKind::Select,
        default,
        None,
        None,
        options,
        &[],
        false,
    )
}

const fn mapped_select_param(
    name: &'static str,
    default: &'static str,
    options: &'static [&'static str],
    exprs: &'static [(&'static str, &'static str)],
) -> TemplateParam {
    param(
        name,
        TemplateParamKind::Select,
        default,
        None,
        None,
        options,
        exprs,
        false,
    )
}

/// 可选字符串参数：留空时省略对应 arg（含前一个 flag）。
const fn optional_text_param(name: &'static str) -> TemplateParam {
    param(
        name,
        TemplateParamKind::String,
        "",
        None,
        None,
        &[],
        &[],
        true,
    )
}

// ─── 选项常量 ──────────────────────────────────────────────────────────────

const CODECS: &[&str] = &["h264", "hevc"];
/// 编码器取值 → ffmpeg 编码器名
const CODEC_EXPRS: &[(&str, &str)] = &[("h264", "libx264"), ("hevc", "libx265")];
const PRESETS: &[&str] = &["ultrafast", "fast", "medium", "slow", "veryslow"];
const FRAME_FORMATS: &[&str] = &["png", "jpg"];
const SUBTITLE_FORMATS: &[&str] = &["srt", "vtt", "ass"];
const WATERMARK_POSITIONS: &[&str] = &[
    "top-left",
    "top-right",
    "bottom-left",
    "bottom-right",
    "center",
];
/// 水印位置 → overlay 坐标表达式（`{param:margin}` 二次展开）
const WATERMARK_POSITION_EXPRS: &[(&str, &str)] = &[
    ("top-left", "{param:margin}:{param:margin}"),
    ("top-right", "W-w-{param:margin}:{param:margin}"),
    ("bottom-left", "{param:margin}:H-h-{param:margin}"),
    ("bottom-right", "W-w-{param:margin}:H-h-{param:margin}"),
    ("center", "(W-w)/2:(H-h)/2"),
];

// ─── 端口常量 ──────────────────────────────────────────────────────────────

const VIDEO_IN: &[(&str, PortType)] = &[("input", PortType::Video)];
const AUDIO_IN: &[(&str, PortType)] = &[("input", PortType::Audio)];
const SUBTITLE_IN: &[(&str, PortType)] = &[("input", PortType::File)];
const VIDEO_PLUS_VIDEO_IN: &[(&str, PortType)] = &[
    ("input", PortType::Video),
    ("input2", PortType::Video),
];
const VIDEO_PLUS_SUBTITLE_IN: &[(&str, PortType)] = &[
    ("input", PortType::Video),
    ("input2", PortType::File),
];
const VIDEO_PLUS_IMAGE_IN: &[(&str, PortType)] = &[
    ("input", PortType::Video),
    ("input2", PortType::Image),
];

// ─── 静态模板表（唯一数据源） ──────────────────────────────────────────────

static TEMPLATES: &[BuiltinTemplate] = &[
    BuiltinTemplate {
        id: "transcode",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "转码",
        display_name_en: "Transcode",
        category: CATEGORY_VIDEO,
        description: "统一编码格式，可调 CRF / 预设，可选缩放",
        description_en: "Re-encode with codec, CRF and preset; optional scaling",
        params: &[
            mapped_select_param("codec", "h264", CODECS, CODEC_EXPRS),
            int_param("crf", "23", 0.0, 51.0),
            select_param("preset", "medium", PRESETS),
            optional_text_param("scale"),
        ],
        input_ports: VIDEO_IN,
        output_port: PortType::Video,
        ffmpeg_build: FfmpegBuild {
            args: &[
                "-i",
                "{input}",
                "-c:v",
                "{param:codec}",
                "-crf",
                "{param:crf}",
                "-preset",
                "{param:preset}",
                "-c:a",
                "copy",
                "-vf",
                "scale={param:scale}",
                "{output}",
            ],
            output_extension: "mp4",
            extension_param: None,
            output_is_dir: false,
        },
    },
    BuiltinTemplate {
        id: "concat",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "拼接",
        display_name_en: "Concat",
        category: CATEGORY_VIDEO,
        description: "两个视频顺序拼接（分辨率需一致）",
        description_en: "Concatenate two videos (same resolution required)",
        params: &[],
        input_ports: VIDEO_PLUS_VIDEO_IN,
        output_port: PortType::Video,
        ffmpeg_build: FfmpegBuild {
            args: &[
                "-i",
                "{input}",
                "-i",
                "{input2}",
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                "-map",
                "[v]",
                "-c:v",
                "libx264",
                "-crf",
                "20",
                "{output}",
            ],
            output_extension: "mp4",
            extension_param: None,
            output_is_dir: false,
        },
    },
    BuiltinTemplate {
        id: "extract-frames",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "抽帧",
        display_name_en: "Extract Frames",
        category: CATEGORY_VIDEO,
        description: "按帧率抽帧为图片序列（产物为目录）",
        description_en: "Extract frames at a fixed rate into an image sequence (directory output)",
        params: &[
            int_param("fps", "1", 1.0, 60.0),
            select_param("format", "png", FRAME_FORMATS),
            num_param("start", "0", 0.0, 86400.0),
        ],
        input_ports: VIDEO_IN,
        output_port: PortType::Image,
        ffmpeg_build: FfmpegBuild {
            args: &[
                "-ss",
                "{param:start}",
                "-i",
                "{input}",
                "-vf",
                "fps={param:fps}",
                "{output}/frame_%04d.{param:format}",
            ],
            output_extension: "",
            extension_param: None,
            output_is_dir: true,
        },
    },
    BuiltinTemplate {
        id: "clip",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "裁剪片段",
        display_name_en: "Clip",
        category: CATEGORY_VIDEO,
        description: "按起止秒裁剪片段（流复制，不重编码）",
        description_en: "Cut a segment by start/end seconds (stream copy, no re-encode)",
        params: &[
            num_param("start", "0", 0.0, 86400.0),
            num_param("end", "10", 0.0, 86400.0),
        ],
        input_ports: VIDEO_IN,
        output_port: PortType::Video,
        ffmpeg_build: FfmpegBuild {
            args: &[
                "-i",
                "{input}",
                "-ss",
                "{param:start}",
                "-to",
                "{param:end}",
                "-c",
                "copy",
                "{output}",
            ],
            output_extension: "mp4",
            extension_param: None,
            output_is_dir: false,
        },
    },
    BuiltinTemplate {
        id: "speed",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "变速",
        display_name_en: "Speed",
        category: CATEGORY_VIDEO,
        description: "视频整体变速（倍率，音频丢弃）",
        description_en: "Change video speed by a factor (audio is dropped)",
        params: &[num_param("factor", "2", 0.25, 4.0)],
        input_ports: VIDEO_IN,
        output_port: PortType::Video,
        ffmpeg_build: FfmpegBuild {
            args: &[
                "-i",
                "{input}",
                "-filter_complex",
                "[0:v]setpts=PTS/{param:factor}[v]",
                "-map",
                "[v]",
                "-c:v",
                "libx264",
                "-crf",
                "20",
                "{output}",
            ],
            output_extension: "mp4",
            extension_param: None,
            output_is_dir: false,
        },
    },
    BuiltinTemplate {
        id: "volume-normalize",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "音量标准化",
        display_name_en: "Normalize Volume",
        category: CATEGORY_AUDIO,
        description: "EBU R128 响度标准化（单遍 loudnorm）",
        description_en: "EBU R128 loudness normalization (single-pass loudnorm)",
        params: &[num_param("target", "-16", -70.0, -5.0)],
        input_ports: AUDIO_IN,
        output_port: PortType::Audio,
        ffmpeg_build: FfmpegBuild {
            args: &[
                "-i",
                "{input}",
                "-af",
                "loudnorm=I={param:target}:TP=-1.5:LRA=11",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-vn",
                "{output}",
            ],
            output_extension: "m4a",
            extension_param: None,
            output_is_dir: false,
        },
    },
    BuiltinTemplate {
        id: "subtitle-convert",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "字幕转换",
        display_name_en: "Convert Subtitles",
        category: CATEGORY_SUBTITLE,
        description: "srt / vtt / ass 字幕互转",
        description_en: "Convert subtitles between srt / vtt / ass",
        params: &[select_param("to_format", "srt", SUBTITLE_FORMATS)],
        input_ports: SUBTITLE_IN,
        output_port: PortType::File,
        ffmpeg_build: FfmpegBuild {
            args: &["-i", "{input}", "{output}"],
            output_extension: "srt",
            extension_param: Some("to_format"),
            output_is_dir: false,
        },
    },
    BuiltinTemplate {
        id: "burn-in",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "字幕硬烧",
        display_name_en: "Burn-in Subtitles",
        category: CATEGORY_SUBTITLE,
        description: "把字幕文件烧进视频画面（libx264 重编码）",
        description_en: "Burn a subtitle file into the video picture (libx264 re-encode)",
        params: &[int_param("crf", "20", 0.0, 51.0)],
        input_ports: VIDEO_PLUS_SUBTITLE_IN,
        output_port: PortType::Video,
        ffmpeg_build: FfmpegBuild {
            args: &[
                "-i",
                "{input}",
                "-i",
                "{input2}",
                "-vf",
                "subtitles={input2_filter}",
                "-c:v",
                "libx264",
                "-crf",
                "{param:crf}",
                "-c:a",
                "copy",
                "{output}",
            ],
            output_extension: "mp4",
            extension_param: None,
            output_is_dir: false,
        },
    },
    BuiltinTemplate {
        id: "watermark",
        kind: FFMPEG_TEMPLATE_KIND,
        display_name: "水印",
        display_name_en: "Watermark",
        category: CATEGORY_VIDEO,
        description: "图片水印叠加（位置 / 不透明度 / 边距）",
        description_en: "Overlay an image watermark (position, opacity, margin)",
        params: &[
            mapped_select_param(
                "position",
                "bottom-right",
                WATERMARK_POSITIONS,
                WATERMARK_POSITION_EXPRS,
            ),
            num_param("opacity", "0.8", 0.0, 1.0),
            int_param("margin", "10", 0.0, 200.0),
        ],
        input_ports: VIDEO_PLUS_IMAGE_IN,
        output_port: PortType::Video,
        ffmpeg_build: FfmpegBuild {
            args: &[
                "-i",
                "{input}",
                "-i",
                "{input2}",
                "-filter_complex",
                "[1:v]format=rgba,colorchannelmixer=aa={param:opacity}[wm];[0:v][wm]overlay={param:position}",
                "-c:a",
                "copy",
                "{output}",
            ],
            output_extension: "mp4",
            extension_param: None,
            output_is_dir: false,
        },
    },
];

/// 全部内置模板（静态表，`GET /api/builtins` / DAG / 执行器共用）。
pub fn builtin_templates() -> &'static [BuiltinTemplate] {
    TEMPLATES
}

/// 按 id 查模板；未知 id → `None`。
pub fn find_template(id: &str) -> Option<&'static BuiltinTemplate> {
    TEMPLATES.iter().find(|t| t.id == id)
}

/// 模板节点端口类型登记（供 `dag::builtin_port_type`）。
///
/// `params.template` 选定模板后按端口名查表；模板缺失/端口名不匹配 → `None`
/// （调用方跳过类型检查，与既有 builtin 语义一致）。
pub fn template_port_type(
    params: &Value,
    port: &str,
    is_output: bool,
) -> Option<PortType> {
    let template = find_template(params.get("template")?.as_str()?)?;
    if is_output {
        // 兼容画布 handle 名（out）与 spec 端口名（output）两种写法。
        return matches!(port, "output" | "out").then_some(template.output_port);
    }
    template
        .input_ports
        .iter()
        .find(|(name, _)| *name == port)
        // 画布单入口 handle（in）落到第一个输入端口。
        .or_else(|| (port == "in").then(|| template.input_ports.first()).flatten())
        .map(|(_, port_type)| *port_type)
}

// ─── 渲染 ──────────────────────────────────────────────────────────────────

/// 渲染上下文：上游输入文件与输出路径。
pub struct TemplateRenderInput<'a> {
    /// 第 1 个上游文件产物（`{input}`）
    pub input: Option<&'a Path>,
    /// 第 2 个上游文件产物（`{input2}`，多入口模板）
    pub input2: Option<&'a Path>,
    /// 本节点输出路径（`{output}`）
    pub output: &'a Path,
}

/// 占位符最大展开轮数（`select_exprs` 表达式可嵌套 `{param:*}`）。
const MAX_RENDER_PASSES: usize = 4;

/// 渲染模板 args：返回 `(渲染后的 args, {output} 是否被替换过)`。
///
/// 纯函数（不触盘）；参数缺省取模板默认值，非法/越界/未知占位符显式报错。
pub fn render_template_args(
    template: &BuiltinTemplate,
    params: &Value,
    io: &TemplateRenderInput<'_>,
) -> anyhow::Result<(Vec<String>, bool)> {
    let mut rendered: Vec<String> = Vec::with_capacity(template.ffmpeg_build.args.len());
    let mut output_substituted = false;

    for raw in template.ffmpeg_build.args {
        let mut text = (*raw).to_string();
        let mut saw_output = false;
        let mut omit = false;

        for _ in 0..MAX_RENDER_PASSES {
            let before = text.clone();
            text = expand_tokens(
                template,
                params,
                io,
                &text,
                &mut saw_output,
                &mut omit,
            )?;
            if text == before {
                break;
            }
        }

        if text.contains('{') || text.contains('}') {
            return Err(anyhow!(
                "ffmpeg_template '{}' arg '{}' has an unresolved placeholder",
                template.id,
                raw
            ));
        }

        output_substituted |= saw_output;

        if omit {
            // 空值参数：省略本 arg；紧邻的前一个 ffmpeg flag（如 `-vf`、`-ss`）
            // 失去取值后同样省略，避免把悬空 flag 交给 ffmpeg
            if rendered
                .last()
                .is_some_and(|prev| prev.starts_with('-') && prev.len() > 1)
            {
                rendered.pop();
            }
            continue;
        }
        rendered.push(text);
    }

    Ok((rendered, output_substituted))
}

/// 单轮占位符展开（`{}` 内 token 见模块文档）。
fn expand_tokens(
    template: &BuiltinTemplate,
    params: &Value,
    io: &TemplateRenderInput<'_>,
    text: &str,
    saw_output: &mut bool,
    omit: &mut bool,
) -> anyhow::Result<String> {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('{') {
        let Some(close_rel) = rest[start..].find('}') else {
            return Err(anyhow!(
                "ffmpeg_template '{}' has an unterminated placeholder in '{}'",
                template.id,
                text
            ));
        };
        let close = start + close_rel;
        result.push_str(&rest[..start]);
        let token = &rest[start + 1..close];
        result.push_str(&render_token(template, params, io, token, saw_output, omit)?);
        rest = &rest[close + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

/// 解析单个占位符 token（`input` / `input2` / `*_filter` / `output` / `param:<name>`）。
fn render_token(
    template: &BuiltinTemplate,
    params: &Value,
    io: &TemplateRenderInput<'_>,
    token: &str,
    saw_output: &mut bool,
    omit: &mut bool,
) -> anyhow::Result<String> {
    match token {
        "input" => Ok(render_input_path(template, io.input, "{input}")?
            .to_string_lossy()
            .into_owned()),
        "input2" => Ok(render_input_path(template, io.input2, "{input2}")?
            .to_string_lossy()
            .into_owned()),
        "input_filter" => Ok(escape_filter_path(render_input_path(
            template,
            io.input,
            "{input_filter}",
        )?)),
        "input2_filter" => Ok(escape_filter_path(render_input_path(
            template,
            io.input2,
            "{input2_filter}",
        )?)),
        "output" => {
            *saw_output = true;
            Ok(io.output.to_string_lossy().into_owned())
        }
        other if other.starts_with("param:") => {
            let name = &other["param:".len()..];
            let spec = template
                .params
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| {
                    anyhow!(
                        "ffmpeg_template '{}' declares no param '{}' (placeholder {{param:{}}})",
                        template.id,
                        name,
                        name
                    )
                })?;
            let value = resolve_param(template, spec, params)?;
            if value.is_empty() && spec.omit_when_empty {
                *omit = true;
            }
            // select 取值 → 表达式映射（表达式内可嵌套 {param:*}，由下一轮展开）
            let mapped = spec
                .select_exprs
                .iter()
                .find(|(key, _)| *key == value)
                .map(|(_, expr)| (*expr).to_string())
                .unwrap_or(value);
            Ok(mapped)
        }
        other => Err(anyhow!(
            "ffmpeg_template '{}' has an unknown placeholder {{{other}}}",
            template.id
        )),
    }
}

/// 取输入路径；缺失时给出可操作的错误（提示连接上游节点）。
fn render_input_path<'a>(
    template: &BuiltinTemplate,
    path: Option<&'a Path>,
    placeholder: &str,
) -> anyhow::Result<&'a Path> {
    path.ok_or_else(|| {
        anyhow!(
            "ffmpeg_template '{}' has the {placeholder} placeholder but no matching upstream file artifact; connect the required input port and ensure the upstream node succeeded",
            template.id
        )
    })
}

/// ffmpeg 滤镜参数中的路径转义：`\` → `/`，并转义 `:` `'` `[` `]` `,` `;`
/// （Windows 盘符 `C:\a\b.srt` → `C\:/a/b.srt`，供 `subtitles=` 等滤镜使用）。
fn escape_filter_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut escaped = String::with_capacity(raw.len() + 2);
    for ch in raw.chars() {
        if matches!(ch, ':' | '\'' | '[' | ']' | ',' | ';') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// 解析参数取值并校验（缺省 → 默认值；数值范围 / 枚举白名单）。
fn resolve_param(
    template: &BuiltinTemplate,
    spec: &TemplateParam,
    params: &Value,
) -> anyhow::Result<String> {
    let raw = params.get(spec.name).cloned().unwrap_or_else(|| spec.default_value());
    let text = match raw {
        Value::String(s) => s,
        Value::Null => String::new(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    };
    let text = text.trim().to_string();
    // 空值 → 回落默认值（select/数值必填语义；String 保持空以便省略逻辑生效）
    let text = if text.is_empty() && spec.kind != TemplateParamKind::String {
        spec.default.to_string()
    } else {
        text
    };

    match spec.kind {
        TemplateParamKind::Integer => {
            // 严格按整数解析：接受 "23" / `23`，拒绝 "23.9"（旧实现按 f64
            // 解析后静默截断为 23，用户输错得不到反馈）。
            let value: i64 = text.parse().map_err(|_| {
                anyhow!(
                    "ffmpeg_template '{}' param '{}' must be an integer, got '{}'",
                    template.id,
                    spec.name,
                    text
                )
            })?;
            check_range(template, spec, value as f64)?;
            Ok(value.to_string())
        }
        TemplateParamKind::Number => {
            let value: f64 = text.parse().map_err(|_| {
                anyhow!(
                    "ffmpeg_template '{}' param '{}' must be a number, got '{}'",
                    template.id,
                    spec.name,
                    text
                )
            })?;
            check_range(template, spec, value)?;
            Ok(format_number(value, false))
        }
        TemplateParamKind::Select => {
            if !spec.options.contains(&text.as_str()) {
                return Err(anyhow!(
                    "ffmpeg_template '{}' param '{}' must be one of {:?}, got '{}'",
                    template.id,
                    spec.name,
                    spec.options,
                    text
                ));
            }
            Ok(text)
        }
        TemplateParamKind::Bool => {
            let parsed = text.parse::<bool>().map_err(|_| {
                anyhow!(
                    "ffmpeg_template '{}' param '{}' must be a boolean, got '{}'",
                    template.id,
                    spec.name,
                    text
                )
            })?;
            Ok(parsed.to_string())
        }
        TemplateParamKind::String => Ok(text),
    }
}

/// 数值参数闭区间校验。
///
/// 非有限值（`NaN` / `inf`）先行拒绝：`NaN` 与任何值比较都为 false，
/// 若直接做 min/max 比较会静默通过并渲染出 `-crf NaN` 之类的非法命令。
fn check_range(
    template: &BuiltinTemplate,
    spec: &TemplateParam,
    value: f64,
) -> anyhow::Result<()> {
    if !value.is_finite() {
        return Err(anyhow!(
            "ffmpeg_template '{}' param '{}' must be a finite number, got '{value}'",
            template.id,
            spec.name
        ));
    }
    if let Some(min) = spec.min {
        if value < min {
            return Err(anyhow!(
                "ffmpeg_template '{}' param '{}' is below its minimum {min}, got {value}",
                template.id,
                spec.name
            ));
        }
    }
    if let Some(max) = spec.max {
        if value > max {
            return Err(anyhow!(
                "ffmpeg_template '{}' param '{}' exceeds its maximum {max}, got {value}",
                template.id,
                spec.name
            ));
        }
    }
    Ok(())
}

/// 构造目录型产物的 manifest（`kind = FRAME_MANIFEST_KIND`）。
///
/// 每个条目含文件名、绝对路径与字节数：daemon 侧据此把目录内文件逐个登记
/// 为可下载产物（`{node_id}` / `{node_id}__{文件名}`），从而让
/// `/api/tasks/{id}/artifacts` 的 `size` 有意义且下载可用。
pub fn frame_directory_manifest(dir: &Path, files: &[PathBuf]) -> Value {
    let entries: Vec<Value> = files
        .iter()
        .map(|path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
            json!({
                "name": name,
                "path": path.to_string_lossy(),
                "size": size,
            })
        })
        .collect();
    json!({
        "kind": FRAME_MANIFEST_KIND,
        "dir": dir.to_string_lossy(),
        "count": entries.len(),
        "files": entries,
    })
}

/// 数值的 ffmpeg 文本形态：整数值不带小数（`23` 而非 `23.0`）。
fn format_number(value: f64, integer: bool) -> String {
    if integer || value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

// ─── API 载荷（GET /api/builtins） ─────────────────────────────────────────

/// `GET /api/builtins` 响应载荷：与静态表同源，直接由 [`TEMPLATES`] 投影。
pub fn builtin_templates_json() -> Value {
    let templates: Vec<Value> = TEMPLATES.iter().map(template_json).collect();
    json!({
        "count": templates.len(),
        "templates": templates,
    })
}

fn template_json(template: &BuiltinTemplate) -> Value {
    let params: Vec<Value> = template
        .params
        .iter()
        .map(|param| {
            let mut exprs = Map::new();
            for (key, expr) in param.select_exprs {
                exprs.insert((*key).to_string(), Value::String((*expr).to_string()));
            }
            json!({
                "name": param.name,
                "kind": param.kind.as_str(),
                "default": param.default_value(),
                "min": param.min,
                "max": param.max,
                "options": param.options,
                "select_exprs": Value::Object(exprs),
                "omit_when_empty": param.omit_when_empty,
            })
        })
        .collect();
    let inputs: Vec<Value> = template
        .input_ports
        .iter()
        .map(|(name, port_type)| json!({ "name": name, "port_type": port_type.as_str() }))
        .collect();
    json!({
        "id": template.id,
        "kind": template.kind,
        "category": template.category,
        "display_name": template.display_name,
        "display_name_en": template.display_name_en,
        "description": template.description,
        "description_en": template.description_en,
        "inputs": inputs,
        "outputs": [{ "name": "output", "port_type": template.output_port.as_str() }],
        "params": params,
        "args": template.ffmpeg_build.args,
        "output_extension": template.ffmpeg_build.output_extension,
        "extension_param": template.ffmpeg_build.extension_param,
        "output_is_dir": template.ffmpeg_build.output_is_dir,
    })
}

// ─── 单测 ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 渲染辅助：默认参数 + 指定覆盖，单入口输入。
    fn render_with(
        id: &str,
        overrides: Value,
        input: &str,
        input2: Option<&str>,
        output: &str,
    ) -> Vec<String> {
        let template = find_template(id).expect("template must exist");
        let mut params = match overrides {
            Value::Object(_) => overrides,
            _ => Value::Object(Map::new()),
        };
        for spec in template.params {
            let object = params.as_object_mut().expect("object params");
            if !object.contains_key(spec.name) {
                object.insert(spec.name.to_string(), spec.default_value());
            }
        }
        let input2 = input2.map(Path::new);
        let (args, _) = render_template_args(
            template,
            &params,
            &TemplateRenderInput {
                input: Some(Path::new(input)),
                input2,
                output: Path::new(output),
            },
        )
        .expect("render must succeed");
        args
    }

    #[test]
    fn builtin_templates_are_nine_unique_ids() {
        let templates = builtin_templates();
        assert_eq!(templates.len(), 9, "首批 9 个模板");
        let mut ids: Vec<&str> = templates.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 9, "模板 id 必须唯一");
        let expected = [
            "transcode",
            "concat",
            "extract-frames",
            "clip",
            "speed",
            "volume-normalize",
            "subtitle-convert",
            "burn-in",
            "watermark",
        ];
        for id in expected {
            assert!(find_template(id).is_some(), "缺少模板 {id}");
        }
    }

    #[test]
    fn every_template_uses_shared_kind_and_has_ports() {
        for template in builtin_templates() {
            assert_eq!(template.kind, FFMPEG_TEMPLATE_KIND);
            assert!(
                !template.input_ports.is_empty(),
                "模板 {} 至少一个输入端口",
                template.id
            );
            assert!(
                !template.display_name.is_empty() && !template.display_name_en.is_empty(),
                "模板 {} 需双语名称",
                template.id
            );
            assert!(
                matches!(
                    template.category,
                    CATEGORY_VIDEO | CATEGORY_AUDIO | CATEGORY_SUBTITLE
                ),
                "模板 {} 分类未知：{}",
                template.id,
                template.category
            );
            assert!(
                template
                    .ffmpeg_build
                    .args
                    .iter()
                    .any(|arg| arg.contains("{output}")),
                "模板 {} 必须声明 {{output}}",
                template.id
            );
        }
    }

    #[test]
    fn render_transcode_args_uses_defaults_and_omits_empty_scale() {
        let args = render_with(
            "transcode",
            json!({}),
            "/tmp/in.mp4",
            None,
            "/tmp/out.mp4",
        );
        assert_eq!(
            args,
            vec![
                "-i",
                "/tmp/in.mp4",
                "-c:v",
                "libx264",
                "-crf",
                "23",
                "-preset",
                "medium",
                "-c:a",
                "copy",
                "/tmp/out.mp4",
            ]
        );
    }

    #[test]
    fn render_transcode_args_maps_codec_and_keeps_scale() {
        let args = render_with(
            "transcode",
            json!({ "codec": "hevc", "crf": 18, "scale": "1280:-2" }),
            "/tmp/in.mp4",
            None,
            "/tmp/out.mp4",
        );
        assert_eq!(
            args,
            vec![
                "-i",
                "/tmp/in.mp4",
                "-c:v",
                "libx265",
                "-crf",
                "18",
                "-preset",
                "medium",
                "-c:a",
                "copy",
                "-vf",
                "scale=1280:-2",
                "/tmp/out.mp4",
            ]
        );
    }

    #[test]
    fn render_concat_args_consumes_two_inputs() {
        let args = render_with(
            "concat",
            json!({}),
            "/tmp/a.mp4",
            Some("/tmp/b.mp4"),
            "/tmp/out.mp4",
        );
        assert_eq!(
            args,
            vec![
                "-i",
                "/tmp/a.mp4",
                "-i",
                "/tmp/b.mp4",
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                "-map",
                "[v]",
                "-c:v",
                "libx264",
                "-crf",
                "20",
                "/tmp/out.mp4",
            ]
        );
    }

    #[test]
    fn render_extract_frames_args_targets_directory_pattern() {
        let args = render_with(
            "extract-frames",
            json!({ "fps": 2, "format": "jpg", "start": 1.5 }),
            "/tmp/in.mp4",
            None,
            "/tmp/frames",
        );
        assert_eq!(
            args,
            vec![
                "-ss",
                "1.5",
                "-i",
                "/tmp/in.mp4",
                "-vf",
                "fps=2",
                "/tmp/frames/frame_%04d.jpg",
            ]
        );
    }

    #[test]
    fn render_clip_args_uses_start_end_seconds() {
        let args = render_with(
            "clip",
            json!({ "start": 5, "end": 12.5 }),
            "/tmp/in.mp4",
            None,
            "/tmp/out.mp4",
        );
        assert_eq!(
            args,
            vec![
                "-i",
                "/tmp/in.mp4",
                "-ss",
                "5",
                "-to",
                "12.5",
                "-c",
                "copy",
                "/tmp/out.mp4",
            ]
        );
    }

    #[test]
    fn render_speed_args_sets_pts() {
        let args = render_with(
            "speed",
            json!({ "factor": 1.5 }),
            "/tmp/in.mp4",
            None,
            "/tmp/out.mp4",
        );
        assert_eq!(
            args,
            vec![
                "-i",
                "/tmp/in.mp4",
                "-filter_complex",
                "[0:v]setpts=PTS/1.5[v]",
                "-map",
                "[v]",
                "-c:v",
                "libx264",
                "-crf",
                "20",
                "/tmp/out.mp4",
            ]
        );
    }

    #[test]
    fn render_volume_normalize_args_uses_loudnorm() {
        let args = render_with(
            "volume-normalize",
            json!({ "target": -14 }),
            "/tmp/in.wav",
            None,
            "/tmp/out.m4a",
        );
        assert_eq!(
            args,
            vec![
                "-i",
                "/tmp/in.wav",
                "-af",
                "loudnorm=I=-14:TP=-1.5:LRA=11",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-vn",
                "/tmp/out.m4a",
            ]
        );
    }

    #[test]
    fn render_subtitle_convert_args_converts_format() {
        let args = render_with(
            "subtitle-convert",
            json!({ "to_format": "ass" }),
            "/tmp/in.srt",
            None,
            "/tmp/out.ass",
        );
        assert_eq!(args, vec!["-i", "/tmp/in.srt", "/tmp/out.ass"]);
    }

    #[test]
    fn render_burn_in_args_escapes_subtitle_path_for_filter() {
        let args = render_with(
            "burn-in",
            json!({ "crf": 22 }),
            "/tmp/in.mp4",
            Some(r"C:\work\sub title.srt"),
            "/tmp/out.mp4",
        );
        assert_eq!(
            args,
            vec![
                "-i",
                "/tmp/in.mp4",
                "-i",
                r"C:\work\sub title.srt",
                "-vf",
                r"subtitles=C\:/work/sub title.srt",
                "-c:v",
                "libx264",
                "-crf",
                "22",
                "-c:a",
                "copy",
                "/tmp/out.mp4",
            ]
        );
    }

    #[test]
    fn render_watermark_args_expands_position_expression() {
        let args = render_with(
            "watermark",
            json!({ "position": "top-right", "opacity": 0.5, "margin": 24 }),
            "/tmp/in.mp4",
            Some("/tmp/logo.png"),
            "/tmp/out.mp4",
        );
        assert_eq!(
            args,
            vec![
                "-i",
                "/tmp/in.mp4",
                "-i",
                "/tmp/logo.png",
                "-filter_complex",
                "[1:v]format=rgba,colorchannelmixer=aa=0.5[wm];[0:v][wm]overlay=W-w-24:24",
                "-c:a",
                "copy",
                "/tmp/out.mp4",
            ]
        );
    }

    #[test]
    fn render_rejects_out_of_range_and_unknown_select() {
        let template = find_template("transcode").unwrap();
        let params = json!({ "crf": 99 });
        let err = render_template_args(
            template,
            &params,
            &TemplateRenderInput {
                input: Some(Path::new("/tmp/in.mp4")),
                input2: None,
                output: Path::new("/tmp/out.mp4"),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("maximum"), "crf 越界应报错：{err}");

        let params = json!({ "codec": "av1" });
        let err = render_template_args(
            template,
            &params,
            &TemplateRenderInput {
                input: Some(Path::new("/tmp/in.mp4")),
                input2: None,
                output: Path::new("/tmp/out.mp4"),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("must be one of"), "非法枚举应报错：{err}");
    }

    #[test]
    fn render_requires_missing_input2() {
        let template = find_template("concat").unwrap();
        let err = render_template_args(
            template,
            &Value::Object(Map::new()),
            &TemplateRenderInput {
                input: Some(Path::new("/tmp/a.mp4")),
                input2: None,
                output: Path::new("/tmp/out.mp4"),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("input2"), "缺第二个输入应报错：{err}");
    }

    #[test]
    fn template_port_type_maps_ports() {
        let params = json!({ "template": "burn-in" });
        assert_eq!(
            template_port_type(&params, "input", false),
            Some(PortType::Video)
        );
        assert_eq!(
            template_port_type(&params, "input2", false),
            Some(PortType::File)
        );
        assert_eq!(
            template_port_type(&params, "output", true),
            Some(PortType::Video)
        );
        assert_eq!(template_port_type(&params, "input3", false), None);
        // 画布 handle 别名：in → 首个输入端口，out → 输出端口（与前端 toSpec 的
        // in/out 端口名一致，使 DAG 类型校验对真实 spec 生效）
        assert_eq!(
            template_port_type(&params, "in", false),
            Some(PortType::Video)
        );
        assert_eq!(
            template_port_type(&params, "out", true),
            Some(PortType::Video)
        );
        assert_eq!(
            template_port_type(&json!({}), "input", false),
            None,
            "未声明 template 参数时跳过类型检查"
        );
        assert_eq!(
            template_port_type(&json!({ "template": "nope" }), "input", false),
            None
        );
    }

    #[test]
    fn json_payload_exposes_all_templates_and_typed_defaults() {
        let payload = builtin_templates_json();
        assert_eq!(payload["count"], json!(9));
        let templates = payload["templates"].as_array().expect("templates array");
        assert_eq!(templates.len(), 9);
        let transcode = templates
            .iter()
            .find(|t| t["id"] == json!("transcode"))
            .expect("transcode");
        assert_eq!(transcode["kind"], json!(FFMPEG_TEMPLATE_KIND));
        assert_eq!(transcode["display_name"], json!("转码"));
        assert_eq!(
            transcode["inputs"][0],
            json!({ "name": "input", "port_type": "video" })
        );
        assert_eq!(
            transcode["outputs"][0],
            json!({ "name": "output", "port_type": "video" })
        );
        let crf = transcode["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == json!("crf"))
            .unwrap();
        assert_eq!(crf["default"], json!(23), "数值默认值应为 JSON number");
        let codec = transcode["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == json!("codec"))
            .unwrap();
        assert_eq!(codec["select_exprs"]["h264"], json!("libx264"));
        assert_eq!(transcode["args"][0], json!("-i"));
        assert_eq!(transcode["output_extension"], json!("mp4"));
    }

    #[test]
    fn default_values_cover_all_param_kinds() {
        for template in builtin_templates() {
            for spec in template.params {
                let value = spec.default_value();
                match spec.kind {
                    TemplateParamKind::Integer => assert!(value.is_i64() || value.is_u64()),
                    TemplateParamKind::Number => assert!(value.is_number()),
                    TemplateParamKind::Bool => assert!(value.is_boolean()),
                    TemplateParamKind::Select | TemplateParamKind::String => {
                        assert!(value.is_string() || value.is_null())
                    }
                }
            }
        }
    }

    #[test]
    fn integer_params_parse_strictly_and_reject_fractions() {
        // JSON 数字 23 / 字符串 "18" 都能正常渲染
        let args = render_with("transcode", json!({ "crf": 23 }), "in.mp4", None, "out.mp4");
        assert!(
            args.windows(2).any(|w| w == ["-crf", "23"]),
            "args={args:?}"
        );
        let args = render_with(
            "transcode",
            json!({ "crf": "18" }),
            "in.mp4",
            None,
            "out.mp4",
        );
        assert!(
            args.windows(2).any(|w| w == ["-crf", "18"]),
            "args={args:?}"
        );

        // "23.9" 是常见误输入：必须显式报错，不能静默截断成 23
        let template = find_template("transcode").unwrap();
        let input = Path::new("in.mp4");
        let err = render_template_args(
            template,
            &json!({ "crf": "23.9" }),
            &TemplateRenderInput {
                input: Some(input),
                input2: None,
                output: Path::new("out.mp4"),
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("must be an integer"),
            "err={err}"
        );
    }

    #[test]
    fn number_params_reject_non_finite_values() {
        let template = find_template("speed").unwrap();
        let input = Path::new("in.mp4");
        for raw in ["NaN", "inf", "-inf"] {
            let err = render_template_args(
                template,
                &json!({ "factor": raw }),
                &TemplateRenderInput {
                    input: Some(input),
                    input2: None,
                    output: Path::new("out.mp4"),
                },
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("must be a finite number"),
                "raw={raw} err={err}"
            );
        }
        // check_range 是最后一道防线：NaN 直接调用也必须被拒绝
        let spec = template
            .params
            .iter()
            .find(|spec| spec.name == "factor")
            .unwrap();
        let err = check_range(template, spec, f64::NAN).unwrap_err();
        assert!(err.to_string().contains("must be a finite number"), "err={err}");
    }

    #[test]
    fn frame_directory_manifest_lists_files_with_sizes() {
        let dir = std::env::temp_dir().join(format!("ep_manifest_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("frame_0001.png");
        let second = dir.join("frame_0002.png");
        std::fs::write(&first, b"abcd").unwrap();
        std::fs::write(&second, b"abcdefgh").unwrap();

        let manifest = frame_directory_manifest(&dir, &[first.clone(), second.clone()]);
        assert_eq!(manifest["kind"], json!(FRAME_MANIFEST_KIND));
        assert_eq!(manifest["count"], json!(2));
        let files = manifest["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["name"], json!("frame_0001.png"));
        assert_eq!(files[0]["size"], json!(4));
        assert_eq!(files[1]["size"], json!(8));
        assert_eq!(
            files[0]["path"].as_str().unwrap(),
            first.to_string_lossy()
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
