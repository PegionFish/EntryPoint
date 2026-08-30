//! 产物归档命名与冲突处理 — 纯函数模块（PLAN_TRIGGER_UNIFIED_LOG §5.5）
//!
//! 供内置节点 `file_archive`（ep-core）与 daemon 直调物化（W1-D）共用。
//!
//! # 命名模板占位符（§5.1 冻结契约）
//!
//! | 占位符 | 含义 | 来源 |
//! |---|---|---|
//! | `{name}` | 无扩展名文件名 | 上游文件名 |
//! | `{ext}` | 小写扩展名（无点） | 上游文件名 |
//! | `{date}` | YYYYMMDD（本地时区） | 当前时间 |
//! | `{datetime}` | YYYYMMDD-HHMMSS（本地时区） | 当前时间 |
//! | `{rule}` | 规则名 | 任务 inputs 保留键 `_meta.rule`（缺省空串） |
//! | `{seq}` | 冲突序号 | 调用方给定（无冲突 0） |
//!
//! **未知占位符原样保留**（裁决）：模板作者笔误（如 `{nane}`）不静默吞字，
//! 渲染结果可见、可排查；`{` 无配对 `}` 时其后内容同样原样保留。
//! 占位符值（name/ext/rule）中的 `{...}` 样式字符串不做二次展开。
//!
//! # 冲突策略（§5.5）
//!
//! - `Suffix`（默认）：目标已存在 → 扩展名前追加 `-1`/`-2`…（上限 999）
//! - `Overwrite`：目标已存在 → 直接覆盖
//! - `Skip`：目标已存在 → [`ConflictResolution::SkipHit`]（调用方放弃归档，
//!   正常完成、不视为错误）

use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Local};

/// 归档冲突策略（§5.5：`suffix`/`overwrite`/`skip`，默认 `suffix`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// 目标已存在时在扩展名前追加序号（`-1`/`-2`…）
    Suffix,
    /// 目标已存在时直接覆盖
    Overwrite,
    /// 目标已存在时放弃归档（调用方按「正常完成」处理）
    Skip,
}

impl ConflictPolicy {
    /// 从节点参数字符串解析（非法值报错，不静默回退）
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "suffix" => Ok(Self::Suffix),
            "overwrite" => Ok(Self::Overwrite),
            "skip" => Ok(Self::Skip),
            other => Err(anyhow::anyhow!(
                "unknown on_conflict policy `{other}` (expected `suffix` / `overwrite` / `skip`)"
            )),
        }
    }
}

/// 冲突处理结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictResolution {
    /// 归档目标路径（原名无冲突 / 追加序号 / overwrite 原名）
    Target(PathBuf),
    /// `Skip` 策略命中：目标已存在，放弃归档
    SkipHit,
}

/// 拆分文件名为 `(stem, 小写扩展名)`。
///
/// 与 `Path::extension` 语义一致：隐藏文件（`.bashrc`）视为无扩展名；
/// 多点文件名（`a.b.c`）→ `("a.b", "c")`；无扩展名 → `("name", "")`。
pub fn split_name_ext(file_name: &str) -> (String, String) {
    let p = Path::new(file_name);
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name)
        .to_string();
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    (stem, ext)
}

/// 展开命名模板为最终文件名（§5.1 占位符契约）。
///
/// - `{seq}`：冲突序号（无冲突时调用方传 0）
/// - 未知占位符与未闭合 `{`：**原样保留**（见模块文档）
pub fn expand_name_template(
    template: &str,
    name: &str,
    ext: &str,
    rule: &str,
    seq: u32,
    now: DateTime<Local>,
) -> String {
    let date = now.format("%Y%m%d").to_string();
    let datetime = now.format("%Y%m%d-%H%M%S").to_string();
    let seq_str = seq.to_string();

    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close_rel) = after.find('}') else {
            // `{` 无配对 `}`：连同其后内容原样保留
            out.push_str(after);
            rest = "";
            break;
        };
        let key = &after[1..close_rel];
        match key {
            "name" => out.push_str(name),
            "ext" => out.push_str(ext),
            "date" => out.push_str(date.as_str()),
            "datetime" => out.push_str(datetime.as_str()),
            "rule" => out.push_str(rule),
            "seq" => out.push_str(seq_str.as_str()),
            // 未知占位符原样保留（含 `{` 与 `}`）
            _ => out.push_str(&after[..=close_rel]),
        }
        rest = &after[close_rel + 1..];
    }
    out.push_str(rest);
    out
}

/// 冲突解析（文件系统无关的纯逻辑版：`exists` 闭包注入占用判定）。
///
/// - [`ConflictPolicy::Suffix`]：`rendered` 占用 → `stem-1.ext`…`stem-999.ext`
///   顺序探测首个空闲名；全部占用 → `Err`
/// - [`ConflictPolicy::Overwrite`]：无论占用与否返回原名
/// - [`ConflictPolicy::Skip`]：占用 → [`ConflictResolution::SkipHit`]
pub fn resolve_conflict_with(
    dest_dir: &Path,
    rendered: &str,
    policy: ConflictPolicy,
    mut exists: impl FnMut(&Path) -> bool,
) -> Result<ConflictResolution> {
    let direct = dest_dir.join(rendered);
    if !exists(&direct) {
        return Ok(ConflictResolution::Target(direct));
    }
    match policy {
        ConflictPolicy::Overwrite => Ok(ConflictResolution::Target(direct)),
        ConflictPolicy::Skip => Ok(ConflictResolution::SkipHit),
        ConflictPolicy::Suffix => {
            let (stem, ext) = split_name_ext(rendered);
            for seq in 1..=999u32 {
                let candidate_name = if ext.is_empty() {
                    format!("{stem}-{seq}")
                } else {
                    format!("{stem}-{seq}.{ext}")
                };
                let candidate = dest_dir.join(candidate_name);
                if !exists(&candidate) {
                    return Ok(ConflictResolution::Target(candidate));
                }
            }
            Err(anyhow::anyhow!(
                "no free archive name after 999 suffix attempts for `{rendered}`"
            ))
        }
    }
}

/// 冲突解析便捷版：以真实文件系统存在性判定占用（执行层使用）。
pub fn resolve_conflict(
    dest_dir: &Path,
    rendered: &str,
    policy: ConflictPolicy,
) -> Result<ConflictResolution> {
    resolve_conflict_with(dest_dir, rendered, policy, |p| p.exists())
}

/// 归档产物落盘前安全校验（评审修复）：`dest` 必须位于 `dest_dir` 之内。
///
/// 防两类穿越（模板渲染结果不可信）：
/// - 绝对路径渲染：`Path::join` 遇绝对路径整体替换，`dest` 脱离 `dest_dir`
///   （`strip_prefix` 失败）→ 拒绝；
/// - `..` 渲染：对 `dest` 相对 `dest_dir` 的剩余路径逐组件做深度计数，
///   `..` 试图越过 `dest_dir` 根即拒绝（`sub/../a.txt` 未逃逸则放行）。
///
/// 纯函数（不触碰文件系统）；调用方须在**写盘前**调用——拒绝时不得落盘。
pub fn ensure_within_dest(dest_dir: &Path, dest: &Path) -> Result<()> {
    let reject = |why: &str| {
        anyhow::anyhow!(
            "archive target `{}` escapes dest_dir `{}` ({why})",
            dest.display(),
            dest_dir.display()
        )
    };
    let rel = dest.strip_prefix(dest_dir).map_err(|_| reject("absolute"))?;
    let mut depth: usize = 0;
    for comp in rel.components() {
        match comp {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| reject("parent-dir traversal"))?;
            }
            // strip_prefix 成功后不应再出现根/前缀组件，防御性拒绝
            Component::RootDir | Component::Prefix(_) => return Err(reject("absolute")),
        }
    }
    Ok(())
}

// ─── 单元测试（§5.5：≥8，覆盖占位符 / 冲突 / 非法模板） ─────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// 固定测试时刻：2026-08-30 14:30:15 本地时区
    fn test_now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 8, 30, 14, 30, 15).unwrap()
    }

    #[test]
    fn test_expand_default_template() {
        let out = expand_name_template("{name}.{ext}", "video", "mp4", "", 0, test_now());
        assert_eq!(out, "video.mp4");
    }

    #[test]
    fn test_expand_all_placeholders() {
        let out = expand_name_template(
            "{rule}/{date}/{datetime}/{name}-{seq}.{ext}",
            "clip",
            "MKV",
            "bt-watch",
            7,
            test_now(),
        );
        // expand 对 ext 入参原样使用；小写归一由 split_name_ext 负责
        assert_eq!(out, "bt-watch/20260830/20260830-143015/clip-7.MKV");
    }

    #[test]
    fn test_expand_missing_rule_is_empty() {
        // 普通手动任务缺省空串（§5.5：{rule} 缺省空）
        let out = expand_name_template("{name}-{rule}.{ext}", "a", "txt", "", 0, test_now());
        assert_eq!(out, "a-.txt");
    }

    #[test]
    fn test_expand_unknown_placeholder_kept_verbatim() {
        // 裁决：未知占位符原样保留，不静默吞字
        let out = expand_name_template("{name}-{bogus}.{ext}", "a", "txt", "", 0, test_now());
        assert_eq!(out, "a-{bogus}.txt");
    }

    #[test]
    fn test_expand_unclosed_brace_kept_verbatim() {
        // 嵌套花括号：一次扫描把 "{date.{ext}" 整体当未知占位符 → 原样保留
        let out = expand_name_template("{name}-{date.{ext}", "a", "txt", "", 0, test_now());
        assert_eq!(out, "a-{date.{ext}");
        // 真正未闭合的 `{`（无配对 `}`）→ 原样保留
        let out = expand_name_template("{name}-{date", "a", "txt", "", 0, test_now());
        assert_eq!(out, "a-{date");
    }

    #[test]
    fn test_expand_no_second_expansion_of_values() {
        // 占位符值中的 {...} 不二次展开
        let out = expand_name_template("{name}.{ext}", "x{date}y", "txt", "", 0, test_now());
        assert_eq!(out, "x{date}y.txt");
    }

    #[test]
    fn test_split_name_ext_rules() {
        assert_eq!(split_name_ext("video.MP4"), ("video".into(), "mp4".into()));
        assert_eq!(split_name_ext("a.b.c"), ("a.b".into(), "c".into()));
        assert_eq!(split_name_ext("noext"), ("noext".into(), "".into()));
        assert_eq!(split_name_ext(".bashrc"), (".bashrc".into(), "".into()));
    }

    #[test]
    fn test_conflict_suffix_appends_sequence() {
        let dir = Path::new("/dest");
        let occupied = |p: &Path| p == Path::new("/dest/a.txt") || p == Path::new("/dest/a-1.txt");
        let out =
            resolve_conflict_with(dir, "a.txt", ConflictPolicy::Suffix, occupied).unwrap();
        assert_eq!(out, ConflictResolution::Target(PathBuf::from("/dest/a-2.txt")));
    }

    #[test]
    fn test_conflict_suffix_no_ext() {
        let dir = Path::new("/dest");
        let occupied = |p: &Path| p == Path::new("/dest/a");
        let out =
            resolve_conflict_with(dir, "a", ConflictPolicy::Suffix, occupied).unwrap();
        assert_eq!(out, ConflictResolution::Target(PathBuf::from("/dest/a-1")));
    }

    #[test]
    fn test_conflict_overwrite_returns_original() {
        let dir = Path::new("/dest");
        let occupied = |p: &Path| p == Path::new("/dest/a.txt");
        let out =
            resolve_conflict_with(dir, "a.txt", ConflictPolicy::Overwrite, occupied).unwrap();
        assert_eq!(out, ConflictResolution::Target(PathBuf::from("/dest/a.txt")));
    }

    #[test]
    fn test_conflict_skip_hit_and_free() {
        let dir = Path::new("/dest");
        // 占用 → SkipHit
        let occupied = |p: &Path| p == Path::new("/dest/a.txt");
        let out = resolve_conflict_with(dir, "a.txt", ConflictPolicy::Skip, occupied).unwrap();
        assert_eq!(out, ConflictResolution::SkipHit);
        // 空闲 → 原名 Target
        let free = |_: &Path| false;
        let out = resolve_conflict_with(dir, "a.txt", ConflictPolicy::Skip, free).unwrap();
        assert_eq!(out, ConflictResolution::Target(PathBuf::from("/dest/a.txt")));
    }

    #[test]
    fn test_conflict_suffix_exhaustion_errors() {
        let dir = Path::new("/dest");
        // 全占用 → Err（而非静默覆盖）
        let all = |_: &Path| true;
        assert!(resolve_conflict_with(dir, "a.txt", ConflictPolicy::Suffix, all).is_err());
    }

    #[test]
    fn test_conflict_policy_parse_rejects_unknown() {
        assert_eq!(ConflictPolicy::parse("SUFFIX").unwrap(), ConflictPolicy::Suffix);
        assert_eq!(ConflictPolicy::parse("overwrite").unwrap(), ConflictPolicy::Overwrite);
        assert_eq!(ConflictPolicy::parse("skip").unwrap(), ConflictPolicy::Skip);
        assert!(ConflictPolicy::parse("replace").is_err());
    }

    // ── 评审修复：路径穿越防护（ensure_within_dest） ────────────────────────

    #[test]
    fn test_within_dest_allows_plain_and_subdir() {
        let dest_dir = Path::new("/dest");
        // 原名直落 dest_dir
        assert!(ensure_within_dest(dest_dir, Path::new("/dest/a.txt")).is_ok());
        // 子目录模板（如 `{date}/{name}.{ext}`）：产物位于 dest_dir 下层
        assert!(ensure_within_dest(dest_dir, Path::new("/dest/20260830/a.txt")).is_ok());
        // 渲染含 `..` 但未逃逸（sub/../a.txt 语义上仍在 dest_dir 内）
        assert!(ensure_within_dest(dest_dir, Path::new("/dest/sub/../a.txt")).is_ok());
    }

    #[test]
    fn test_within_dest_rejects_traversal() {
        let dest_dir = Path::new("/dest");
        // `..` 逃逸 dest_dir → 拒绝
        assert!(ensure_within_dest(dest_dir, Path::new("/dest/../a.txt")).is_err());
        assert!(ensure_within_dest(dest_dir, Path::new("/dest/sub/../../a.txt")).is_err());
        // 绝对路径渲染：Path::join 整体替换 → 脱离 dest_dir → 拒绝
        assert!(ensure_within_dest(dest_dir, Path::new("/etc/a.txt")).is_err());
        assert!(ensure_within_dest(dest_dir, Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn test_resolve_conflict_subdir_target() {
        // 子目录模板 + 无冲突：Target 保持 dest_dir 下子目录形状
        //（父目录由 executor 落盘前 create_dir_all）
        let free = |_: &Path| false;
        let out = resolve_conflict_with(
            Path::new("/dest"),
            "20260830/a.txt",
            ConflictPolicy::Suffix,
            free,
        )
        .unwrap();
        assert_eq!(
            out,
            ConflictResolution::Target(PathBuf::from("/dest/20260830/a.txt"))
        );
    }
}
