//! 任务中心页 — 管线任务进度（含 queued 队列位置）+ 产物列表/打开 +
//! 运行中的服务。
//!
//! 用户裁决：裁撤「全部模块状态」表区块，任务页只保留管线任务 +
//! 运行中服务（与 WebUI 任务页同步裁撤，保持两端一致）。
//!
//! 产物目录约定（C4）：任务产物落盘 `{workspace}/tasks/{task_id}/`
//! （与 ep-core TaskRecord.work_dir 同口径），本页直接扫描该目录展示产物，
//! 平台分支经 [`crate::pages::open_path`] 打开（Windows `start` / Linux `xdg-open`）。
//!
//! queued 语义（§6.8）：S2 骨架的 [`TaskSummary`] 仅携带引擎状态
//! [`TaskStatus`]，其中 `Pending` = 排队等待闸门；队列位置按列表中
//! Pending 任务的次序展示（注册表 `queue_position` 的桌面侧接线见 C5 报告）。
//!
//! 用户可见文案经 [`crate::i18n::tr`] 查找；状态/类别文案复用
//! [`crate::pages::modules`] 的本地化 helper，颜色一律取自当前主题色板。

use std::path::{Path, PathBuf};

use eframe::egui;
use ep_core::config::AppConfig;
use ep_core::pipeline::runner::TaskSummary;
use ep_core::types::{ServiceStatus, TaskStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{AppCmd, ModuleEntry};
use crate::i18n::tr;
use crate::pages::modules::{category_label, service_label};
use crate::pages::{format_size, open_path, publish_tasks_snapshot, trfb};
use crate::ui::{
    badge, card, card_running, empty_state, glow_breath_alpha, keyboard_scroll, page_header,
    progress_gradient, section_title, segmented_tabs, stat_cards, status_badge, subtle_button,
    Palette, StatItem,
};

/// 产物目录递归扫描的最大深度（`files/{node_id}/…` 布局足够，防御深目录）
const ARTIFACT_SCAN_DEPTH: usize = 4;

/// P2 修复：产物扫描结果缓存 TTL（展开产物区时避免每帧同步 IO 重扫）
const ARTIFACT_SCAN_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// P2 修复：单任务产物扫描缓存（egui temp data 存储，需 Clone）
#[derive(Clone)]
struct ArtifactScanCache {
    scanned_at: std::time::Instant,
    files: Vec<ArtifactEntry>,
}

/// 任务状态筛选（§7.4 SegmentedTabs；口径与 WebUI 协调：运行中含排队 Pending）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TaskFilter {
    #[default]
    All,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskFilter {
    /// 声明顺序 = SegmentedTabs 段顺序（`as usize` 下标依赖此序）
    const ALL: [TaskFilter; 5] = [
        TaskFilter::All,
        TaskFilter::Running,
        TaskFilter::Completed,
        TaskFilter::Failed,
        TaskFilter::Cancelled,
    ];

    fn matches(&self, status: &TaskStatus) -> bool {
        match self {
            TaskFilter::All => true,
            TaskFilter::Running => matches!(status, TaskStatus::Running | TaskStatus::Pending),
            TaskFilter::Completed => matches!(status, TaskStatus::Completed),
            TaskFilter::Failed => matches!(status, TaskStatus::Failed(_)),
            TaskFilter::Cancelled => matches!(status, TaskStatus::Cancelled),
        }
    }
}

/// 筛选状态持久化键（egui temp data，会话内保持）
fn filter_state_id() -> egui::Id {
    egui::Id::new("tasks_page_filter")
}

/// 任务统计条带（W4-B7）：四态大数字，非零时按语义色强调
fn stats_band(ui: &mut egui::Ui, lang: &str, pal: &Palette, tasks: &[TaskSummary]) {
    let running = tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Running))
        .count();
    let queued = tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Pending))
        .count();
    let completed = tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed))
        .count();
    let failed = tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Failed(_)))
        .count();
    let stats = [
        StatItem {
            label: tr(lang, "common.status.running", &[]),
            value: running.to_string(),
            color: if running > 0 { pal.status_running } else { pal.text },
        },
        StatItem {
            label: tr(lang, "common.status.queued", &[]),
            value: queued.to_string(),
            color: if queued > 0 { pal.warning } else { pal.text },
        },
        StatItem {
            label: tr(lang, "common.status.completed", &[]),
            value: completed.to_string(),
            color: if completed > 0 { pal.status_ready } else { pal.text },
        },
        StatItem {
            label: tr(lang, "common.status.failed", &[]),
            value: failed.to_string(),
            color: if failed > 0 { pal.status_error } else { pal.text },
        },
    ];
    stat_cards(ui, pal, "tasks_stats", &stats);
}

/// 任务中心页入口：`cmd_tx` 为后台命令通道（queued/running 任务卡内取消）。
pub fn show_full(
    ui: &mut egui::Ui,
    config: &AppConfig,
    modules: &[ModuleEntry],
    tasks: &[TaskSummary],
    cmd_tx: Option<&UnboundedSender<AppCmd>>,
) {
    let lang = ep_core::i18n::normalize_language(&config.general.language);
    let pal = Palette::new(ui.style().visuals.dark_mode);

    // 发布任务快照：管线编辑器的节点状态回显消费
    publish_tasks_snapshot(ui.ctx(), tasks);

    page_header(ui, &tr(lang, "tasks.page.title", &[]), |_| {});
    ui.add_space(8.0);

    // 统计条带（W4-B7）：运行中/排队/已完成/失败大数字 + 渐变下划线，
    // 与仪表盘统计共用 stat_cards 组件
    stats_band(ui, lang, &pal, tasks);
    ui.add_space(12.0);

    // queued（S2 形状下为 Pending）任务的队列位置映射：task_id → 位置（1 起）
    let queue_positions = compute_queue_positions(tasks);

    // 主滚动区启用键盘滚动（P2-1）
    keyboard_scroll(ui, "tasks_main", egui::ScrollArea::vertical(), |ui| {
        // ── 管线任务 ──
        section_title(ui, &tr(lang, "tasks.stats.pipelineTasks", &[]));
        ui.add_space(6.0);

        if tasks.is_empty() {
            empty_state(
                ui,
                &pal,
                "📋",
                &tr(lang, "tasks.tasks.emptyTitle", &[]),
                &tr(lang, "desktopApp.tasks.emptyHint", &[]),
            );
        } else {
            // ── 状态筛选 SegmentedTabs（§7.4；全部/运行中/已完成/失败/取消） ──
            let mut filter = ui
                .ctx()
                .data(|d| d.get_temp::<TaskFilter>(filter_state_id()))
                .unwrap_or_default();
            let tab_labels = [
                tr(lang, "tasks.filter.all", &[]),
                tr(lang, "tasks.filter.running", &[]),
                tr(lang, "tasks.filter.completed", &[]),
                tr(lang, "tasks.filter.failed", &[]),
                tr(lang, "tasks.filter.cancelled", &[]),
            ];
            let tabs: Vec<(String, usize)> = TaskFilter::ALL
                .iter()
                .zip(tab_labels)
                .map(|(f, label)| {
                    (label, tasks.iter().filter(|t| f.matches(&t.status)).count())
                })
                .collect();
            if let Some(idx) = segmented_tabs(ui, &pal, &tabs, filter as usize) {
                filter = TaskFilter::ALL[idx];
                ui.ctx().data_mut(|d| d.insert_temp(filter_state_id(), filter));
            }
            ui.add_space(8.0);

            let visible: Vec<&TaskSummary> =
                tasks.iter().filter(|t| filter.matches(&t.status)).collect();
            if visible.is_empty() {
                empty_state(
                    ui,
                    &pal,
                    "🔍",
                    &tr(lang, "tasks.filteredEmpty", &[]),
                    &tr(lang, "tasks.filteredEmptyHint", &[]),
                );
            } else {
                // 运行态呼吸辉光（§7.4）：存在运行中任务卡时按 ~20fps 追加重绘
                let now_ms = ui.ctx().input(|i| i.time * 1000.0);
                let breath = glow_breath_alpha(now_ms);
                let any_running = visible
                    .iter()
                    .any(|t| matches!(t.status, TaskStatus::Running));
                for task in &visible {
                    task_card(ui, lang, &pal, config, task, &queue_positions, cmd_tx, breath);
                    ui.add_space(8.0);
                }
                if any_running {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(48));
                }
            }
        }

        ui.add_space(12.0);

        // ── 运行中的服务 ──
        section_title(ui, &tr(lang, "tasks.stats.runningServices", &[]));
        ui.add_space(6.0);

        let running: Vec<&ModuleEntry> = modules
            .iter()
            .filter(|m| m.status.is_running() || m.status == ServiceStatus::Starting)
            .collect();

        if running.is_empty() {
            ui.label(
                egui::RichText::new(tr(lang, "tasks.services.emptyTitle", &[]))
                    .color(pal.text_dim),
            );
        } else {
            card(ui, &pal, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    module_grid(ui, lang, &pal, "tasks_running_grid", &running);
                });
            });
        }

        ui.add_space(8.0);
    });
}

/// queued（Pending）任务的队列位置：按列表顺序 1 起编号。
///
/// S2 的 [`TaskSummary`] 无 `queue_position` 字段（注册表形状才有，§6.8）；
/// 此处按展示顺序给出位置，注册表接线后由生产侧直接携带（见 C5 报告）。
fn compute_queue_positions(tasks: &[TaskSummary]) -> std::collections::HashMap<String, usize> {
    let mut positions = std::collections::HashMap::new();
    let mut pos = 0usize;
    for task in tasks {
        if matches!(task.status, TaskStatus::Pending) {
            pos += 1;
            positions.insert(task.id.clone(), pos);
        }
    }
    positions
}

// ─── 管线任务卡片 ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn task_card(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    task: &TaskSummary,
    queue_positions: &std::collections::HashMap<String, usize>,
    cmd_tx: Option<&UnboundedSender<AppCmd>>,
    breath: f32,
) {
    let (color, label) = task_status_meta(lang, &task.status, pal);

    // glass 风格卡（§7.4）：运行态呼吸辉光，静止态 hover 描边提亮
    card_running(
        ui,
        pal,
        matches!(task.status, TaskStatus::Running),
        breath,
        |ui| {
        // 行1：管线名 + 状态徽章 + 队列位置（queued）+ 任务 ID（右对齐 mono）
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&task.pipeline_name).strong());
            badge(ui, pal, color, label);
            if let Some(pos) = queue_positions.get(&task.id) {
                let pos_s = pos.to_string();
                badge(
                    ui,
                    pal,
                    pal.warning,
                    trfb(
                        lang,
                        "desktopApp.tasks.queuePosition",
                        "队列位置 {{pos}}",
                        &[("pos", &pos_s)],
                    ),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("#{}", task.id))
                        .monospace()
                        .small()
                        .color(pal.text_faint),
                );
            });
        });
        ui.add_space(8.0);

        // 行2：整体进度（§7.4 渐变进度条：运行中=主色渐变，终态/排队=状态色单档）
        let progress = if task.node_count > 0 {
            task.completed_nodes as f32 / task.node_count as f32
        } else {
            0.0
        };
        let alert = match &task.status {
            TaskStatus::Running => None,
            TaskStatus::Pending => Some(pal.warning),
            TaskStatus::Completed => Some(pal.status_ready),
            TaskStatus::Failed(_) => Some(pal.status_error),
            TaskStatus::Cancelled => Some(pal.status_stopped),
        };
        progress_gradient(ui, pal, progress, alert);
        ui.add_space(6.0);

        // 行3：时间（ISO 截短到秒）+ 节点进度
        let mut info = String::new();
        if let Some(started) = &task.started_at {
            if let Some(finished) = &task.finished_at {
                info.push_str(&tr(
                    lang,
                    "desktopApp.tasks.startedFinished",
                    &[
                        ("start", iso_to_secs(started)),
                        ("end", iso_to_secs(finished)),
                    ],
                ));
            } else {
                info.push_str(&tr(
                    lang,
                    "desktopApp.tasks.startedRunning",
                    &[("start", iso_to_secs(started))],
                ));
            }
            info.push_str("    ");
        }
        let completed = task.completed_nodes.to_string();
        let total = task.node_count.to_string();
        info.push_str(&tr(
            lang,
            "tasks.task.nodeProgress",
            &[("completed", &completed), ("total", &total)],
        ));
        ui.label(egui::RichText::new(info).small().color(pal.text_dim));

        // 失败原因（ep-core 原始消息以本地化前缀附加原文）
        if let TaskStatus::Failed(err) = &task.status {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr(lang, "desktopApp.tasks.error", &[("detail", err)]))
                    .small()
                    .color(pal.danger),
            );
        }

        // 取消按钮（queued/running）：门禁接线完成（C4 冻结入口）
        if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            if let Some(tx) = cmd_tx {
                ui.add_space(4.0);
                if ui
                    .add(subtle_button(
                        pal,
                        format!(
                            "✕ {}",
                            trfb(lang, "desktopApp.tasks.cancel", "取消", &[])
                        ),
                    ))
                    .clicked()
                {
                    let _ = tx.send(AppCmd::CancelTask {
                        task_id: task.id.clone(),
                    });
                }
            }
        }

        // ── 产物区（展开式；queued/运行中同样可查看已落盘的部分产物） ──
        ui.add_space(6.0);
        artifacts_section(ui, lang, pal, config, task);
        },
    );
}

// ─── 产物列表 ────────────────────────────────────────────────────────────────

/// 任务产物区：扫描 `{workspace}/tasks/{task_id}/` 并逐文件提供打开入口。
fn artifacts_section(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    config: &AppConfig,
    task: &TaskSummary,
) {
    let header = egui::CollapsingHeader::new(
        egui::RichText::new(trfb(
            lang,
            "desktopApp.tasks.artifacts.title",
            "输出产物",
            &[],
        ))
        .color(pal.text_dim),
    )
    .id_salt(egui::Id::new(("task_artifacts", task.id.clone())))
    .default_open(false);

    header.show(ui, |ui| {
        let root = ep_core::config::resolve_root();
        let task_dir = config
            .resolve_workspace_dir(&root)
            .join("tasks")
            .join(&task.id);

        if !task_dir.is_dir() {
            ui.label(
                egui::RichText::new(trfb(
                    lang,
                    "desktopApp.tasks.artifacts.dirMissing",
                    "任务目录不存在（任务可能未在本机执行或产物已清理）",
                    &[],
                ))
                .small()
                .color(pal.text_faint),
            );
            return;
        }

        // P2 修复：扫描结果按任务缓存（2s TTL），产物区展开时不再每帧同步 IO
        let scan_key = egui::Id::new(("task_artifacts_scan", task.id.clone()));
        let cached = ui.ctx().data(|d| d.get_temp::<ArtifactScanCache>(scan_key));
        let files = match cached {
            Some(c) if c.scanned_at.elapsed() < ARTIFACT_SCAN_TTL => c.files,
            _ => {
                let mut files = Vec::new();
                collect_artifacts(&task_dir, &task_dir, 0, &mut files);
                ui.ctx().data_mut(|d| {
                    d.insert_temp(
                        scan_key,
                        ArtifactScanCache {
                            scanned_at: std::time::Instant::now(),
                            files: files.clone(),
                        },
                    )
                });
                files
            }
        };

        if files.is_empty() {
            ui.label(
                egui::RichText::new(tr(lang, "tasks.artifacts.empty", &[]))
                    .small()
                    .color(pal.text_faint),
            );
        } else {
            for artifact in &files {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(egui::RichText::new("📄").color(pal.text_faint));
                    ui.label(
                        egui::RichText::new(&artifact.rel_display)
                            .monospace()
                            .color(pal.text),
                    )
                    .on_hover_text(artifact.path.to_string_lossy());
                    ui.label(
                        egui::RichText::new(format_size(artifact.size_bytes))
                            .small()
                            .color(pal.text_faint),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(subtle_button(
                                pal,
                                format!("↗ {}", tr(lang, "common.action.open", &[])),
                            ))
                            .clicked()
                        {
                            open_path(&artifact.path);
                        }
                    });
                });
            }
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add(subtle_button(
                    pal,
                    format!(
                        "📂 {}",
                        trfb(lang, "desktopApp.tasks.artifacts.openDir", "打开任务目录", &[])
                    ),
                ))
                .clicked()
            {
                open_path(&task_dir);
            }
        });
    });
}

/// 单个产物文件条目
#[derive(Clone)]
struct ArtifactEntry {
    /// 相对任务目录的展示路径（正斜杠统一显示）
    rel_display: String,
    /// 绝对路径（打开用）
    path: PathBuf,
    size_bytes: u64,
}

/// 递归收集任务目录下的文件（深度受限，跳过隐藏条目与临时文件）。
///
/// 路径拼接一律 `Path::join`（双平台硬约束）；展示名统一正斜杠。
fn collect_artifacts(base: &Path, dir: &Path, depth: usize, out: &mut Vec<ArtifactEntry>) {
    if depth >= ARTIFACT_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut batch: Vec<ArtifactEntry> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // 跳过隐藏条目与常见临时/元数据文件（仅展示用户关心的产物）
        if name_str.starts_with('.') || name_str.ends_with(".tmp") {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().ok();
        if file_type.map(|t| t.is_dir()).unwrap_or(false) {
            subdirs.push(path);
        } else if file_type.map(|t| t.is_file()).unwrap_or(false) {
            let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let rel = path.strip_prefix(base).unwrap_or(&path);
            batch.push(ArtifactEntry {
                rel_display: rel.to_string_lossy().replace('\\', "/"),
                path,
                size_bytes,
            });
        }
    }
    batch.sort_by(|a, b| a.rel_display.cmp(&b.rel_display));
    out.extend(batch);
    subdirs.sort();
    for sub in subdirs {
        collect_artifacts(base, &sub, depth + 1, out);
    }
}

// ─── 运行中服务网格（卡片内横向滚动） ──────────────────────────────────────────

fn module_grid(
    ui: &mut egui::Ui,
    lang: &str,
    pal: &Palette,
    id: &str,
    rows: &[&ModuleEntry],
) {
    egui::Grid::new(id)
        .striped(true)
        .spacing([28.0, 10.0])
        .show(ui, |ui| {
            // 表头
            let headers = [
                tr(lang, "common.label.module", &[]),
                tr(lang, "tasks.moduleTable.category", &[]),
                tr(lang, "common.label.status", &[]),
                tr(lang, "desktopPages.dashboard.col.port", &[]),
                tr(lang, "desktopPages.modules.info.uptime", &[]),
            ];
            for col in headers {
                ui.label(egui::RichText::new(col).small().color(pal.text_faint));
            }
            ui.end_row();

            for m in rows {
                ui.label(&m.name);
                ui.label(egui::RichText::new(category_label(lang, &m.category)).color(pal.text_dim));
                // 四态色 StatusBadge（§9）；文案走 i18n
                status_badge(ui, pal, &m.status, service_label(lang, &m.status));
                ui.label(
                    egui::RichText::new(
                        m.port
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "-".into()),
                    )
                    .monospace()
                    .color(pal.text_dim),
                );
                ui.label(
                    egui::RichText::new(
                        m.started_at
                            .map(|t| format_uptime(lang, t.elapsed()))
                            .unwrap_or_else(|| "-".into()),
                    )
                    .color(pal.text_dim),
                );
                ui.end_row();
            }
        });
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// 任务状态 → (颜色, 本地化文案)。颜色一律取自当前主题色板，禁止硬编码 RGB。
///
/// Pending 即 §6.8 的 queued（等待全局/管线闸门），用 warning 色区分于运行中；
/// 其余四态按 UNIFIED_UI_REDESIGN_PROPOSAL §1.2 统一（就绪绿/运行青/错误珊瑚红/停止灰）。
fn task_status_meta(lang: &str, status: &TaskStatus, pal: &Palette) -> (egui::Color32, String) {
    match status {
        TaskStatus::Completed => (pal.status_ready, tr(lang, "common.status.completed", &[])),
        TaskStatus::Running => (pal.status_running, tr(lang, "common.status.running", &[])),
        TaskStatus::Pending => (
            pal.warning,
            trfb(lang, "common.status.queued", "排队中", &[]),
        ),
        TaskStatus::Failed(_) => (pal.status_error, tr(lang, "common.status.failed", &[])),
        TaskStatus::Cancelled => (pal.status_stopped, tr(lang, "common.status.cancelled", &[])),
    }
}

/// ISO 8601 时间字符串截短到秒（前 19 字符）；长度不足或边界不安全时原样返回
fn iso_to_secs(iso: &str) -> &str {
    iso.get(..19).unwrap_or(iso)
}

/// 运行时长（与模块页同一套键，保证两页文案一致）
fn format_uptime(lang: &str, d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        let s = secs.to_string();
        tr(lang, "desktopPages.modules.uptime.seconds", &[("s", &s)])
    } else if secs < 3600 {
        let m = (secs / 60).to_string();
        let s = (secs % 60).to_string();
        tr(
            lang,
            "desktopPages.modules.uptime.minutes",
            &[("m", &m), ("s", &s)],
        )
    } else {
        let h = (secs / 3600).to_string();
        let m = ((secs % 3600) / 60).to_string();
        let s = (secs % 60).to_string();
        tr(
            lang,
            "desktopPages.modules.uptime.hours",
            &[("h", &h), ("m", &m), ("s", &s)],
        )
    }
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: TaskStatus) -> TaskSummary {
        TaskSummary {
            id: id.to_string(),
            pipeline_name: "p".to_string(),
            status,
            started_at: None,
            finished_at: None,
            node_count: 2,
            completed_nodes: 0,
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn queue_positions_number_pending_tasks_in_order() {
        let tasks = vec![
            task("run-1", TaskStatus::Running),
            task("q-1", TaskStatus::Pending),
            task("done", TaskStatus::Completed),
            task("q-2", TaskStatus::Pending),
        ];
        let positions = compute_queue_positions(&tasks);
        assert_eq!(positions.len(), 2);
        assert_eq!(positions["q-1"], 1);
        assert_eq!(positions["q-2"], 2);
        assert!(!positions.contains_key("run-1"));
        assert!(!positions.contains_key("done"));
    }

    #[test]
    fn queue_positions_empty_when_no_pending() {
        let tasks = vec![task("a", TaskStatus::Running)];
        assert!(compute_queue_positions(&tasks).is_empty());
    }

    /// SegmentedTabs 筛选语义（§7.4，口径与 WebUI 协调：运行中含排队 Pending）
    #[test]
    fn task_filter_matches_status_buckets() {
        assert!(TaskFilter::All.matches(&TaskStatus::Cancelled));
        assert!(TaskFilter::Running.matches(&TaskStatus::Running));
        assert!(TaskFilter::Running.matches(&TaskStatus::Pending));
        assert!(!TaskFilter::Running.matches(&TaskStatus::Completed));
        assert!(TaskFilter::Completed.matches(&TaskStatus::Completed));
        assert!(TaskFilter::Failed.matches(&TaskStatus::Failed("e".into())));
        assert!(!TaskFilter::Failed.matches(&TaskStatus::Cancelled));
        assert!(TaskFilter::Cancelled.matches(&TaskStatus::Cancelled));
        assert!(!TaskFilter::Cancelled.matches(&TaskStatus::Running));
    }

    /// 段顺序 = Tab 展示顺序；`as usize` 下标映射依赖此不变量
    #[test]
    fn task_filter_all_order_stable() {
        assert_eq!(TaskFilter::ALL.len(), 5);
        assert_eq!(TaskFilter::ALL[0], TaskFilter::All);
        assert_eq!(TaskFilter::ALL[4], TaskFilter::Cancelled);
        assert_eq!(TaskFilter::Completed as usize, 2);
    }

    #[test]
    fn iso_to_secs_truncates_safely() {
        assert_eq!(iso_to_secs("2026-08-05T12:34:56.789Z"), "2026-08-05T12:34:56");
        assert_eq!(iso_to_secs("short"), "short");
        assert_eq!(iso_to_secs(""), "");
    }

    #[test]
    fn collect_artifacts_walks_and_sorts() {
        let base = std::env::temp_dir().join(format!(
            "ep-c5-artifacts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let nested = base.join("files").join("asr");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("b.txt"), "hello").unwrap();
        std::fs::write(base.join("a.srt"), "1").unwrap();
        std::fs::write(nested.join("out.json"), "{}").unwrap();
        std::fs::write(base.join(".hidden"), "x").unwrap();
        std::fs::write(base.join("leftover.tmp"), "x").unwrap();

        let mut files = Vec::new();
        collect_artifacts(&base, &base, 0, &mut files);

        let names: Vec<&str> = files.iter().map(|f| f.rel_display.as_str()).collect();
        assert_eq!(names, vec!["a.srt", "b.txt", "files/asr/out.json"]);
        assert_eq!(files[0].size_bytes, 1);
        assert_eq!(files[1].size_bytes, 5);

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn collect_artifacts_respects_depth_limit() {
        let base = std::env::temp_dir().join(format!(
            "ep-c5-artifacts-depth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // 深度 5：超过 ARTIFACT_SCAN_DEPTH=4，深层文件不应被收集
        let deep = base.join("d1").join("d2").join("d3").join("d4").join("d5");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("deep.txt"), "x").unwrap();
        std::fs::write(base.join("top.txt"), "x").unwrap();

        let mut files = Vec::new();
        collect_artifacts(&base, &base, 0, &mut files);
        let names: Vec<&str> = files.iter().map(|f| f.rel_display.as_str()).collect();
        assert_eq!(names, vec!["top.txt"]);

        std::fs::remove_dir_all(&base).ok();
    }
}
