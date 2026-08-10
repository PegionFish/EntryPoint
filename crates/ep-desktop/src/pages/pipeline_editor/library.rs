//! 管线库扫描 — 编辑器工具栏库下拉菜单的数据源。
//!
//! 用户裁决（对齐 WebUI 成熟模式）：裁撤 W3 的「库视图 → 编辑器视图」
//! 两态设计，管线页打开即编辑器；本模块不再绘制独立库视图，仅保留
//! `config/pipelines/*.toml` 扫描纯函数（[`scan_pipeline_library`]），
//! 供 `mod.rs::library_menu`（WebUI `PipelineLibraryBar` 下拉的 egui
//! 等价实现）列出「名称 + 节点数」条目。

use std::path::PathBuf;

use ep_core::pipeline::dag::Pipeline;

/// 管线库条目：`config/pipelines/*.toml` 扫描结果（路径 + 展示名/描述 + 节点数）
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LibraryEntry {
    pub path: PathBuf,
    pub name: String,
    pub description: String,
    pub node_count: usize,
    /// 随包内置管线（shipped）：不可从库菜单删除（对齐 WebUI builtin 语义）
    pub shipped: bool,
}

/// 随包内置管线文件名（去扩展名）：与仓库 `config/pipelines/` 随发行
/// 的管线一致；仅 custom（非此清单）管线可从库菜单删除。
pub(super) const SHIPPED_PIPELINE_STEMS: &[&str] = &["audio_extract", "video_to_srt"];

/// 管线库目录：`<EP_ROOT>/config/pipelines`（与整合包管线圈选同根解析）
pub(super) fn pipeline_library_dir() -> PathBuf {
    ep_core::config::resolve_root()
        .join("config")
        .join("pipelines")
}

/// 扫描目录下的 `*.toml` 管线定义 → 库条目列表（纯函数，可测）：
/// - 仅取扩展名为 `toml` 的文件（非 toml / 子目录过滤），按文件名排序；
/// - 解析成功优先用 `[pipeline] name/description`（name 为空回退文件名），
///   并统计节点数摘要；
/// - 解析失败回退文件名（去扩展名），不阻塞列表展示；
/// - 目录不存在或无 toml 时返回空列表。
pub(super) fn scan_pipeline_library(dir: &std::path::Path) -> Vec<LibraryEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let shipped = is_shipped_stem(&stem);
            let (name, description, node_count) = match Pipeline::from_toml(&path) {
                Ok(p) if !p.name.trim().is_empty() => (p.name, p.description, p.nodes.len()),
                Ok(p) => (stem, String::new(), p.nodes.len()),
                Err(_) => (stem, String::new(), 0),
            };
            LibraryEntry {
                path,
                name,
                description,
                node_count,
                shipped,
            }
        })
        .collect()
}

/// 目录下已有 `*.toml` 文件名（去扩展名）列表（纯函数可测）：
/// 供另存为/导入注册的重名检测（[`unique_library_file_name`]）。目录
/// 不存在返回空列表。
pub(super) fn existing_stems(dir: &std::path::Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect()
}

/// 文件名（去扩展名）是否属于随包内置管线（不可删除）
pub(super) fn is_shipped_stem(stem: &str) -> bool {
    SHIPPED_PIPELINE_STEMS.contains(&stem)
}

/// 另存为/导入注册的文件名清洗：去 `.toml` 扩展名、剔除文件系统非法
/// 字符与首尾点空白；清洗后为空回退 `pipeline`（纯函数可测）。
pub(super) fn sanitize_library_file_name(raw: &str) -> String {
    let stem = raw
        .trim()
        .strip_suffix(".toml")
        .or_else(|| raw.trim().strip_suffix(".TOML"))
        .unwrap_or(raw.trim());
    let cleaned: String = stem
        .chars()
        .filter(|c| !(c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')))
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        "pipeline".to_string()
    } else {
        cleaned
    }
}

/// 重名处理（纯函数可测）：`name` 在 `existing` 中不冲突原样返回；
/// 冲突则追加 `_2`/`_3`… 直到不冲突。
pub(super) fn unique_library_file_name(name: &str, existing: &[String]) -> String {
    let has = |c: &str| existing.iter().any(|e| e.eq_ignore_ascii_case(c));
    if !has(name) {
        return name.to_string();
    }
    for i in 2..1000 {
        let cand = format!("{name}_{i}");
        if !has(&cand) {
            return cand;
        }
    }
    format!("{name}_{}", existing.len() + 1)
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时目录夹具（唯一路径，用后清理）
    fn library_tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ep-library-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 最小合法管线 TOML（[pipeline] id/name/description）
    fn library_valid_toml(id: &str, name: &str, desc: &str) -> String {
        format!(
            "[pipeline]\nid = \"{id}\"\nname = \"{name}\"\ndescription = \"{desc}\"\n"
        )
    }

    #[test]
    fn library_scan_filters_non_toml_and_sorts() {
        let dir = library_tmp_dir("filter");
        // 乱序写入多个 toml + 非 toml + 子目录
        std::fs::write(dir.join("beta.toml"), library_valid_toml("b", "B 管线", "描述 B")).unwrap();
        std::fs::write(dir.join("alpha.toml"), library_valid_toml("a", "A 管线", "")).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a pipeline").unwrap();
        std::fs::write(dir.join("README.md"), "# readme").unwrap();
        std::fs::create_dir(dir.join("subdir.toml")).unwrap();

        let entries = scan_pipeline_library(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A 管线", "B 管线"], "仅保留 toml 且按文件名排序");
        assert_eq!(entries[0].description, "");
        assert_eq!(entries[1].description, "描述 B");
        assert!(entries.iter().all(|e| e.path.extension() == Some("toml".as_ref())));
        // 解析成功条目携带节点数摘要（空 [pipeline] = 0 节点）
        assert!(entries.iter().all(|e| e.node_count == 0));
    }

    #[test]
    fn library_scan_missing_or_empty_dir_returns_empty() {
        let dir = library_tmp_dir("empty");
        assert!(scan_pipeline_library(&dir).is_empty(), "空目录 → 空列表");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            scan_pipeline_library(&dir).is_empty(),
            "目录不存在 → 空列表"
        );
    }

    #[test]
    fn library_scan_parse_failure_falls_back_to_filename() {
        let dir = library_tmp_dir("fallback");
        std::fs::write(dir.join("broken.toml"), "this is [not valid toml").unwrap();
        std::fs::write(dir.join("noname.toml"), "[pipeline]\nid = \"x\"\nname = \"\"\n").unwrap();

        let entries = scan_pipeline_library(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["broken", "noname"],
            "解析失败与 name 为空均回退文件名（去扩展名）"
        );
        assert!(entries.iter().all(|e| e.description.is_empty()));
        // 解析失败条目节点数 0；解析成功（无节点）条目同样 0
        assert!(entries.iter().all(|e| e.node_count == 0));
    }

    /// 带节点的管线条目节点数摘要正确（库下拉菜单展示依赖）
    #[test]
    fn library_scan_reports_node_count() {
        let dir = library_tmp_dir("count");
        std::fs::write(
            dir.join("two_nodes.toml"),
            "[pipeline]\nid = \"t\"\nname = \"T\"\n\n[[nodes]]\nid = \"input\"\nkind = \"builtin\"\nbuiltin = \"file_input\"\n\n[[nodes]]\nid = \"output\"\nkind = \"builtin\"\nbuiltin = \"file_output\"\n",
        )
        .unwrap();

        let entries = scan_pipeline_library(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].node_count, 2);
    }

    /// shipped 判定：内置清单命中 / 未命中
    #[test]
    fn shipped_stem_detection() {
        assert!(is_shipped_stem("audio_extract"));
        assert!(is_shipped_stem("video_to_srt"));
        assert!(!is_shipped_stem("my_custom"));
    }

    /// existing_stems：仅收集 toml 文件 stem，忽略非 toml 与子目录
    #[test]
    fn existing_stems_collects_toml_stems_only() {
        let dir = library_tmp_dir("stems");
        std::fs::write(dir.join("a.toml"), "x").unwrap();
        std::fs::write(dir.join("b.toml"), "x").unwrap();
        std::fs::write(dir.join("c.txt"), "x").unwrap();
        std::fs::create_dir(dir.join("d.toml")).unwrap();

        let mut stems = existing_stems(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        stems.sort();
        assert_eq!(stems, vec!["a".to_string(), "b".to_string()]);

        // 目录不存在 → 空列表
        assert!(existing_stems(&dir).is_empty());
    }

    /// 文件名清洗：去扩展名、剔除非法字符、空回退
    #[test]
    fn sanitize_library_file_name_rules() {
        assert_eq!(sanitize_library_file_name("my_pipeline.toml"), "my_pipeline");
        assert_eq!(sanitize_library_file_name("a<b>c:d"), "abcd");
        assert_eq!(sanitize_library_file_name("..."), "pipeline");
        assert_eq!(sanitize_library_file_name("  spaced  "), "spaced");
    }

    /// 重名递增：不冲突原样返回，冲突追加 _2/_3…（大小写不敏感）
    #[test]
    fn unique_library_file_name_increments_on_conflict() {
        let existing = vec!["demo".to_string(), "demo_2".to_string()];
        assert_eq!(unique_library_file_name("other", &existing), "other");
        assert_eq!(unique_library_file_name("demo", &existing), "demo_3");
        assert_eq!(unique_library_file_name("DEMO", &existing), "DEMO_3");
    }
}
