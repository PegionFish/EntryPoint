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
}

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
            }
        })
        .collect()
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
}
