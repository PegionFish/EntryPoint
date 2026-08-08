//! 管线库视图（两态重做默认态）— 卡片网格 + 新建/打开入口 + 空态。
//! 自 pipeline_editor.rs 拆分搬移（scan_pipeline_library / library_card
//! 逻辑保持），升格为正式库视图：glass 卡片新令牌、节点数摘要。

use std::path::PathBuf;

use ep_core::pipeline::dag::Pipeline;

use crate::pages::trfb;
use crate::ui::{
    card_frame_hover, grid_columns, keyboard_scroll, page_header, primary_button, subtle_button,
    Palette,
};

use super::{edit, VizState};

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

/// 库视图（两态默认态）：页头动作区（新建 / 打开文件）+ 卡片网格。
/// 点击卡片 = 既有加载流程并进入编辑器视图。
pub(super) fn draw_library_view(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    st: &mut VizState,
) {
    page_header(
        ui,
        &crate::i18n::tr(lang, "desktopApp.pipeline.libraryTitle", &[]),
        |ui| {
            if ui
                .add(subtle_button(
                    pal,
                    format!(
                        "📂 {}",
                        trfb(lang, "desktopApp.pipeline.libraryOpen", "打开文件…", &[])
                    ),
                ))
                .on_hover_text(trfb(
                    lang,
                    "desktopApp.pipeline.openTitle",
                    "打开管线 TOML",
                    &[],
                ))
                .clicked()
            {
                if let Some(file) = rfd::FileDialog::new()
                    .set_title(trfb(
                        lang,
                        "desktopApp.pipeline.openTitle",
                        "打开管线 TOML",
                        &[],
                    ))
                    .add_filter("TOML", &["toml"])
                    .pick_file()
                {
                    st.file_path = file.to_string_lossy().to_string();
                    edit::load_pipeline(st, lang);
                    if st.pipeline.is_some() {
                        st.mode = super::PageMode::Editor;
                    }
                }
            }
            if ui
                .add(primary_button(
                    pal,
                    format!(
                        "＋ {}",
                        trfb(lang, "desktopApp.pipeline.libraryNew", "新建管线", &[])
                    ),
                ))
                .on_hover_text(trfb(
                    lang,
                    "desktopApp.pipeline.newTip",
                    "新建含输入/输出的空白管线",
                    &[],
                ))
                .clicked()
            {
                edit::new_pipeline(st);
                st.mode = super::PageMode::Editor;
            }
        },
    );
    ui.label(
        egui::RichText::new(trfb(
            lang,
            "desktopApp.pipeline.librarySubtitle",
            "从管线库选择一个管线开始编排，或新建空白管线",
            &[],
        ))
        .small()
        .color(pal.text_dim),
    );
    ui.add_space(8.0);

    let entries = scan_pipeline_library(&pipeline_library_dir());
    if entries.is_empty() {
        crate::ui::empty_state(
            ui,
            pal,
            "🧩",
            &trfb(
                lang,
                "desktopApp.pipeline.libraryEmptyTitle",
                "管线库为空",
                &[],
            ),
            &trfb(
                lang,
                "desktopApp.pipeline.libraryEmptyHint",
                "config/pipelines 下暂无 .toml 管线；点右上「新建管线」开始",
                &[],
            ),
        );
        return;
    }

    keyboard_scroll(
        ui,
        "pipeline_library",
        egui::ScrollArea::vertical(),
        |ui| {
            let cols = grid_columns(ui.available_width(), 300.0, ui.spacing().item_spacing.x, entries.len());
            crate::ui::card_grid(ui, cols, &entries, |ui, entry| {
                let resp = library_card(ui, lang, pal, entry);
                if resp.clicked() {
                    st.file_path = entry.path.to_string_lossy().to_string();
                    edit::load_pipeline(st, lang);
                    if st.pipeline.is_some() {
                        st.mode = super::PageMode::Editor;
                    }
                }
            });
        },
    );
}

/// 单条管线卡片（glass 新令牌）：名称 + 节点数徽章 + 描述 + 路径；
/// hover 时描边升级 border_glow → border_glow_strong + primary 辉光。
fn library_card(ui: &mut egui::Ui, lang: &str, pal: &Palette, entry: &LibraryEntry) -> egui::Response {
    let rect_id = ui.id().with(("library_card", entry.path.as_os_str()));

    let inner = card_frame_hover(pal, false).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(entry.name.as_str()).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    crate::ui::badge(
                        ui,
                        pal,
                        pal.primary,
                        trfb(
                            lang,
                            "desktopApp.pipeline.nodeCount",
                            "{{count}} 节点",
                            &[("count", &entry.node_count.to_string())],
                        ),
                    );
                });
            });
            if !entry.description.is_empty() {
                ui.label(
                    egui::RichText::new(entry.description.as_str())
                        .small()
                        .color(pal.text_dim),
                );
            }
            ui.label(
                egui::RichText::new(entry.path.to_string_lossy().into_owned())
                    .monospace()
                    .small()
                    .color(pal.text_faint),
            );
        });
    });
    let rect = inner.response.rect;
    let resp = ui.interact(rect, rect_id, egui::Sense::click());
    if resp.hovered() {
        // hover 升级：强发光描边 + primary 辉光阴影近似
        ui.painter().rect(
            rect,
            egui::CornerRadius::same(crate::ui::CARD_ROUNDING),
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(1.5_f32, pal.border_glow_strong),
            egui::StrokeKind::Inside,
        );
        ui.ctx().request_repaint();
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(trfb(
            lang,
            "desktopApp.pipeline.libraryLoadTip",
            "点击加载进编辑器",
            &[],
        ))
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
        // 两态重做：解析成功条目携带节点数摘要（空 [pipeline] = 0 节点）
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

    /// 两态重做：带节点的管线条目节点数摘要正确
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
