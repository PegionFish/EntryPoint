//! 日志页（GT-5）：LogPipe 引擎 → 显示缓冲 → 过滤 → 虚拟滚动渲染。
//!
//! 职责链（对应派发单 §3 GT-5）：
//! 1. **接线**（app.rs/ui.rs）：creator 里 `LogPipe::spawn`（三源三线程，IO 全
//!    后台）→ [`crate::platform::ui::TrayApp`] 持 `LogPipeHandle`；logic 帧
//!    `drain_new()` 追加进 [`LogBuffer`]（UI 侧显示缓冲，独立于引擎 ring）；
//! 2. **过滤**（[`LogFilter`]，纯逻辑）：来源多选 / 级别多选 / 大小写不敏感
//!    子串，全部在显示层做——红线：**过滤不动 ring**；
//! 3. **渲染**（ui.rs 的日志页分支调 [`LogPage::show`]）：egui
//!    `ScrollArea::show_rows` 虚拟滚动，只渲染可视行。
//!
//! ## 虚拟滚动路线裁决（2026-11-06，本票 commit① 评估记录）
//!
//! 派发单原案 = egui_extras TableView。实测评估结论：**走零依赖路线，不引
//! egui_extras**。理由：
//! - egui 0.37 自带 `ScrollArea::show_rows` 就是虚拟化（只渲染可视行，见本
//!   文件下方 `show_rows_only_renders_visible_rows` 探针测试：50_000 行数据
//!   单帧只渲染可视区几十行）；
//! - 行高固定（时间/来源/级别/消息四列单行截断），恰是 show_rows 的理想
//!   形状（非等高内容才需要 TableView/自定义）；
//! - 本地 registry 无 egui_extras 0.37 缓存，引入 = 新增编译单元 + 新特性面
//!   （TableView 需 feature 门），与收益不成比例。
//!
//! 列布局用 `ui.horizontal` + 定宽列（时间/来源/级别）+ 截断 Label（消息），
//! 不逐帧全量重建文本布局（红线：LayoutJob 高亮仅对可视行构建）。
//!
//! [`LogPage::show`]: LogPage::show

use eframe::egui;
use std::collections::{BTreeSet, VecDeque};
use std::sync::OnceLock;
use std::path::PathBuf;

use crate::platform::logpipe::{file_tail, LogRecord, LogSource, LogLevel};

// ─── 显示缓冲（UI 侧，独立于引擎 ring；过滤只动这里，红线：不动 ring） ──────

/// 显示缓冲上限（任务口径 50_000 行）：超限丢最旧 + 计数——与引擎 ring 的
/// 背压语义一致，但独立计数（引擎 ring 5000 行是「三源汇流面」，本缓冲是
/// 「UI 回看面」，GT-6 导出接缝：导出走文件不走这两层）。
pub const DISPLAY_BUFFER_CAP: usize = 50_000;

/// UI 侧显示缓冲。底层 `VecDeque`（任务口径写 Vec——此处取「超限丢最旧」的
/// O(1) 弹出形态，语义等价：按到达序保存、只从头部淘汰）。
///
/// seq 单调递增（引擎 ring 分配），入序即升序——`get(seq)` 二分查找，
/// 供过滤视图按 seq 取行而不克隆记录（视图只存 seq，见 LogPage::rebuild）。
#[derive(Debug)]
pub(crate) struct LogBuffer {
    buf: VecDeque<LogRecord>,
    cap: usize,
    /// 因超限被挤出的最旧行总数
    dropped: u64,
}

impl LogBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap.clamp(1, DISPLAY_BUFFER_CAP)),
            cap: cap.max(1),
            dropped: 0,
        }
    }

    pub fn push(&mut self, r: LogRecord) {
        if self.buf.len() >= self.cap {
            self.buf.pop_front();
            self.dropped += 1;
        }
        self.buf.push_back(r);
    }

    /// logic 帧批量追加（drain_new 的产物通常几十~几千行）
    pub fn extend<I: IntoIterator<Item = LogRecord>>(&mut self, rows: I) {
        for r in rows {
            self.push(r);
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    // 以下三个方法当前仅单测消费（过滤视图 / 渲染在本票后续 commit 接入）：
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 显示缓冲超限淘汰累计（引擎 ring 的丢弃见 LogPipeHandle::drop_count）
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 按 seq 取行（二分；buf 按 seq 升序）。行可能已被淘汰 → None。
    pub fn get(&self, seq: u64) -> Option<&LogRecord> {
        // 二分手写（VecDeque 无 binary_search_by_key 且借用不便）
        let buf = &self.buf;
        let mut lo = 0usize;
        let mut hi = buf.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            match buf[mid].seq.cmp(&seq) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(&buf[mid]),
            }
        }
        None
    }

    /// 按序遍历（过滤视图重建用）
    pub fn iter(&self) -> impl Iterator<Item = &LogRecord> {
        self.buf.iter()
    }
}


// ─── 过滤器（纯逻辑；红线：过滤只动显示层，不动引擎 ring） ─────────────────

/// 级别过滤：五级 + 「无级别」各自开关（默认全开）。GT-4 口径：WS 模块行与
/// 事件行 level=None（无级别语义）→ 由 `none` 开关独立控制显隐。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LevelFilter {
    pub trace: bool,
    pub debug: bool,
    pub info: bool,
    pub warn: bool,
    pub error: bool,
    /// level=None 的行（模块 stdout 行 / 事件行）
    pub none: bool,
}

impl LevelFilter {
    pub fn all_on() -> Self {
        Self {
            trace: true,
            debug: true,
            info: true,
            warn: true,
            error: true,
            none: true,
        }
    }

    fn matches(&self, level: Option<LogLevel>) -> bool {
        match level {
            Some(LogLevel::Trace) => self.trace,
            Some(LogLevel::Debug) => self.debug,
            Some(LogLevel::Info) => self.info,
            Some(LogLevel::Warn) => self.warn,
            Some(LogLevel::Error) => self.error,
            None => self.none,
        }
    }
}

impl Default for LevelFilter {
    fn default() -> Self {
        Self::all_on()
    }
}

/// 来源过滤：Daemon / Module(id) / Event 三类开关 + 模块 id 明细勾选。
/// 默认全开（derive 出的 Default 全 false 会吞掉一切——手写）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceFilter {
    pub daemon: bool,
    pub events: bool,
    pub modules: bool,
    /// 模块 id 明细：空 = 全部模块；非空 = 只显示勾选 id
    /// （候选清单 = LogPage.module_ids_seen，随流自动出现）
    pub module_ids: BTreeSet<String>,
}

impl Default for SourceFilter {
    fn default() -> Self {
        Self {
            daemon: true,
            events: true,
            modules: true,
            module_ids: BTreeSet::new(),
        }
    }
}

impl SourceFilter {
    fn matches(&self, src: &LogSource) -> bool {
        match src {
            LogSource::Daemon => self.daemon,
            LogSource::Event => self.events,
            LogSource::Module(id) => {
                self.modules && (self.module_ids.is_empty() || self.module_ids.contains(id))
            }
        }
    }
}

/// 日志过滤器（来源多选 + 级别多选 + 大小写不敏感子串）。全部在显示层求值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LogFilter {
    pub sources: SourceFilter,
    pub levels: LevelFilter,
    /// 大小写不敏感子串（空 = 不过滤；展示原样给 TextEdit）
    pub needle: String,
}

impl LogFilter {
    /// fold 后的子串（每次视图重建 fold 一次；谓词路径零 fold 零分配）
    pub fn folded_needle(&self) -> String {
        fold_ascii(&self.needle)
    }

    /// 谓词求值（`needle_folded` = [`Self::folded_needle`] 的缓存产物）。
    pub fn matches_with(&self, r: &LogRecord, needle_folded: &str) -> bool {
        if !self.sources.matches(&r.source) {
            return false;
        }
        if !self.levels.matches(r.level) {
            return false;
        }
        if !needle_folded.is_empty() && !contains_folded(&r.line, needle_folded) {
            return false;
        }
        true
    }
}

/// ASCII 大小写折叠（A-Z → a-z，其余原样）。逐字符等宽——fold 前后 char 与
/// byte 索引都不变，[`match_ranges`] 的区间可直接映射回原串。Unicode 变宽
/// 字符（如 İ）不折叠、不参与大小写匹配——日志主体为 ASCII，可接受的取舍。
pub(crate) fn fold_ascii(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c })
        .collect()
}

/// 大小写不敏感（ASCII 折叠）子串判定，无分配（needle 须已 fold）。
/// 热路径（5 万行 × 每次视图重建）——字节级扫描，不做逐行 String 分配。
fn contains_folded(hay: &str, needle_folded: &str) -> bool {
    let n = needle_folded.as_bytes();
    if n.is_empty() {
        return true;
    }
    let h = hay.as_bytes();
    if n.len() > h.len() {
        return false;
    }
    (0..=h.len() - n.len()).any(|i| {
        h[i..i + n.len()]
            .iter()
            .zip(n.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// 大小写不敏感子串命中区间（char 索引 [start, end)，非重叠、升序、贪心）。
/// 供高亮 LayoutJob 分段着色：行与 needle 同 [`fold_ascii`] 口径，索引直接
/// 映射回原串。只对**可视行**调用（渲染路径，不进 5 万行热路径）。
pub(crate) fn match_ranges(line: &str, needle_folded: &str) -> Vec<(usize, usize)> {
    if needle_folded.is_empty() {
        return Vec::new();
    }
    let hay: Vec<char> = fold_ascii(line).chars().collect();
    let n: Vec<char> = needle_folded.chars().collect();
    let mut out = Vec::new();
    if n.is_empty() || n.len() > hay.len() {
        return out;
    }
    let mut i = 0usize;
    while i + n.len() <= hay.len() {
        if hay[i..i + n.len()].iter().eq(n.iter()) {
            out.push((i, i + n.len()));
            i += n.len(); // 贪心：命中段不重叠
        } else {
            i += 1;
        }
    }
    out
}

// ─── 页面状态 ───────────────────────────────────────────────────────────────

/// 日志页状态（TrayApp 主线程独占；logic 帧进数据，ui 帧渲染）。
///
/// 过滤视图 = 匹配行的 **seq 列表**（零克隆：渲染时按 seq 从缓冲二分取行）。
/// 重算时机（惰性，`rebuild` 每帧调用但脏才真正扫缓冲）：新行 ingest /
/// 过滤器变更 / 缓冲超限淘汰——其余帧直接复用，过滤切换即时。
pub(crate) struct LogPage {
    pub buffer: LogBuffer,
    /// 当前过滤器（UI 控件直改；rebuild 时与 `applied` 比对判脏）
    pub filter: LogFilter,
    /// 过滤视图：匹配行 seq 升序
    view: Vec<u64>,
    /// 上次建视图时缓冲的淘汰计数（淘汰使旧视图失效）
    view_dropped_mark: u64,
    /// 上次建视图所用的过滤器快照
    applied: LogFilter,
    /// 上次建视图后是否 ingest 过新行
    dirty: bool,
    /// 引擎流里见过的模块 id（来源过滤 UI 的勾选候选清单；随流自动出现）
    pub module_ids_seen: BTreeSet<String>,
    /// 跟随模式：视图钉底（用户上滚自动暂停；手动滚到底自动回跟）
    follow: bool,
    /// 「回到底部」单次滚动请求（下一帧 ScrollArea 建构期注入 offset——
    /// egui 内部 stick 标志只在滚到端点时自恢复，跳底须显式 offset）
    jump_to_bottom: bool,
}

/// 行高与列宽（行高固定是 show_rows 虚拟化的前提；消息列占余宽截断显示，
/// 全文走 tooltip / 复制）
const ROW_TS_W: f32 = 100.0;
const ROW_SRC_W: f32 = 100.0;
const ROW_LVL_W: f32 = 56.0;
const ROW_COPY_W: f32 = 50.0;

impl LogPage {
    pub fn new() -> Self {
        Self::new_with_cap(DISPLAY_BUFFER_CAP)
    }

    /// 指定容量构造（生产恒 5 万行；单测用小容量验证淘汰联动）
    fn new_with_cap(cap: usize) -> Self {
        Self {
            buffer: LogBuffer::new(cap),
            filter: LogFilter::default(),
            view: Vec::new(),
            view_dropped_mark: 0,
            applied: LogFilter::default(),
            dirty: true,
            module_ids_seen: BTreeSet::new(),
            follow: true,
            jump_to_bottom: false,
        }
    }

    /// logic 帧消费引擎新行（drain_new 产物追加进显示缓冲 + 标记视图脏）。
    pub fn ingest(&mut self, rows: Vec<LogRecord>) {
        for r in &rows {
            if let LogSource::Module(id) = &r.source {
                self.module_ids_seen.insert(id.clone());
            }
        }
        self.buffer.extend(rows);
        self.dirty = true;
    }

    /// 视图惰性重算（ui 帧渲染前调用；不脏直接复用——不逐帧全量扫描）。
    pub fn rebuild(&mut self) {
        if !self.dirty && self.applied == self.filter && self.view_dropped_mark == self.buffer.dropped()
        {
            return;
        }
        let needle = self.filter.folded_needle();
        self.view.clear();
        for r in self.buffer.iter() {
            if self.filter.matches_with(r, &needle) {
                self.view.push(r.seq);
            }
        }
        self.applied = self.filter.clone();
        self.view_dropped_mark = self.buffer.dropped();
        self.dirty = false;
    }

    /// 视图行数（show_rows 的 total_rows）
    pub fn view_len(&self) -> usize {
        self.view.len()
    }

    /// 视图第 i 行（渲染层取行；缓冲淘汰后视图重算前可能 None——显示占位）
    pub fn view_row(&self, i: usize) -> Option<&LogRecord> {
        self.view.get(i).and_then(|seq| self.buffer.get(*seq))
    }

    // ─── 渲染（ui 帧；红线：show_rows 只渲染可视行，LayoutJob 仅对可视行构建）───

    /// 日志页主体（ui.rs Logs 分支调用）：过滤器行 → 跟随条 → 虚拟表格。
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.rebuild(); // 惰性：脏才扫缓冲（过滤切换即时）
        self.filter_bar(ui);
        self.follow_bar(ui);
        self.log_table(ui);
    }

    /// 过滤器行：来源多选（+模块 id 明细）/ 级别多选（含无级别）/ 子串 / 清空。
    fn filter_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.weak("来源:");
            ui.checkbox(&mut self.filter.sources.daemon, "Daemon");
            ui.checkbox(&mut self.filter.sources.modules, "模块");
            ui.checkbox(&mut self.filter.sources.events, "Event");
            // 模块 id 明细：空 = 全部模块；勾选后只显示所选（候选随流出现）
            if self.filter.sources.modules && !self.module_ids_seen.is_empty() {
                ui.weak("明细:");
                let seen: Vec<String> = self.module_ids_seen.iter().cloned().collect();
                for id in seen {
                    let mut on = self.filter.sources.module_ids.contains(&id);
                    if ui.checkbox(&mut on, &id).changed() {
                        if on {
                            self.filter.sources.module_ids.insert(id);
                        } else {
                            self.filter.sources.module_ids.remove(&id);
                        }
                    }
                }
                if !self.filter.sources.module_ids.is_empty() && ui.small_button("全选").clicked() {
                    self.filter.sources.module_ids.clear();
                }
            }
            ui.separator();
            ui.weak("级别:");
            ui.checkbox(&mut self.filter.levels.error, "ERROR");
            ui.checkbox(&mut self.filter.levels.warn, "WARN");
            ui.checkbox(&mut self.filter.levels.info, "INFO");
            ui.checkbox(&mut self.filter.levels.debug, "DEBUG");
            ui.checkbox(&mut self.filter.levels.trace, "TRACE");
            ui.checkbox(&mut self.filter.levels.none, "无级别");
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.filter.needle)
                    .desired_width(150.0)
                    .hint_text("过滤子串（大小写不敏感）…"),
            );
            if ui.small_button("清空").clicked() {
                self.filter = LogFilter::default();
            }
        });
    }

    /// 跟随条：跟随/暂停状态 + 回到底部按钮 + 视图/缓冲计数。
    fn follow_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.follow {
                ui.label(
                    egui::RichText::new("● 跟随中")
                        .color(egui::Color32::from_rgb(80, 200, 120)),
                );
                ui.weak("（上滚自动暂停）");
            } else {
                ui.label(
                    egui::RichText::new("‖ 跟随已暂停")
                        .color(egui::Color32::YELLOW),
                );
                if ui.button("回到底部").clicked() {
                    self.follow = true;
                    self.jump_to_bottom = true;
                }
            }
            ui.separator();
            ui.weak(format!(
                "显示 {} / {} 行",
                self.view_len(),
                self.buffer.len()
            ));
        });
    }

    /// 虚拟滚动表格（四列：时间/来源/级别/消息；`show_rows` 只渲染可视区）。
    fn log_table(&mut self, ui: &mut egui::Ui) {
        let row_h = ui
            .spacing()
            .interact_size
            .y
            .max(ui.text_style_height(&egui::TextStyle::Body));
        let total = self.view_len();
        let needle = self.filter.folded_needle();

        // 滚动建构：跳底单次注入 offset（egui 端点 clamp 收敛到底）；
        // 跟随模式用内置 stick（内容增长自动钉底，用户上滚即解锁）；
        // 暂停模式不干预。
        let scroll = egui::ScrollArea::vertical().auto_shrink(false);
        let scroll = if self.jump_to_bottom {
            self.jump_to_bottom = false;
            scroll.vertical_scroll_offset(1.0e9)
        } else if self.follow {
            scroll.stick_to_bottom(true)
        } else {
            scroll
        };
        let out = scroll.show_rows(ui, row_h, total, |ui, row_range| {
            for i in row_range {
                self.render_row(ui, i, row_h, &needle);
            }
        });

        // 跟随/暂停状态机：距底 > 1px = 离开底部（上滚/拖滚动条均覆盖）；
        // 暂停中手动滚到底 = 自动回跟（egui 内部 stick 也在端点自恢复）。
        let available = out.content_size.y - out.inner_rect.height();
        let deviation = available - out.state.clamped_offset().y;
        if self.follow {
            if deviation > 1.0 {
                self.follow = false;
            }
        } else if deviation <= 1.0 {
            self.follow = true;
        }
    }

    /// 单行渲染：时间（HH:MM:SS.mmm 本地时）/ 来源徽标 / 级别徽标 / 消息
    /// （子串命中段着色 + 截断 + 悬停全文 + 右键复制）/ 复制按钮。
    fn render_row(&self, ui: &mut egui::Ui, i: usize, row_h: f32, needle_folded: &str) {
        let Some(r) = self.view_row(i) else {
            ui.set_min_height(row_h);
            ui.weak("…"); // 视图引用了已被淘汰的 seq（下一帧 rebuild 收敛）
            return;
        };
        let ts = format_local_hms_mmm(r.ts);
        let (src_label, src_color) = source_badge(&r.source);
        let lvl_label = level_label(r.level);
        let lvl_color = level_color(r.level);
        let full_line = format!("{ts} [{src_label}] {lvl_label} {}", r.line);

        ui.horizontal(|ui| {
            ui.set_min_height(row_h);
            ui.add_sized(
                [ROW_TS_W, row_h],
                egui::Label::new(egui::RichText::new(&ts).monospace().weak()),
            );
            ui.add_sized(
                [ROW_SRC_W, row_h],
                egui::Label::new(egui::RichText::new(&src_label).color(src_color).strong()),
            );
            ui.add_sized(
                [ROW_LVL_W, row_h],
                egui::Label::new(egui::RichText::new(lvl_label).color(lvl_color).monospace()),
            );
            let msg_w = ui.available_width() - ROW_COPY_W;
            let job = self.highlight_job(ui, &r.line, needle_folded);
            let msg = ui.add_sized(
                [msg_w, row_h],
                egui::Label::new(job).truncate().sense(egui::Sense::click()),
            );
            if msg.secondary_clicked() {
                ui.ctx().copy_text(full_line.clone());
            }
            msg.on_hover_text(&r.line); // 截断行的全文 tooltip
            if ui.add_sized([ROW_COPY_W, row_h], egui::Button::new("复制")).clicked() {
                ui.ctx().copy_text(full_line);
            }
        });
    }

    /// 高亮 LayoutJob（只对可视行构建——红线：不逐帧全量重建文本布局）。
    fn highlight_job(
        &self,
        ui: &egui::Ui,
        line: &str,
        needle_folded: &str,
    ) -> egui::text::LayoutJob {
        let body_font = ui
            .style()
            .text_styles
            .get(&egui::TextStyle::Body)
            .cloned()
            .unwrap_or_default();
        let normal = egui::TextFormat::simple(body_font, ui.style().visuals.text_color());
        let mut hit = normal.clone();
        hit.background = egui::Color32::from_rgb(150, 105, 20); // 琥珀底 = 命中段

        let mut job = egui::text::LayoutJob::default();
        let ranges = match_ranges(line, needle_folded);
        if ranges.is_empty() {
            job.append(line, 0.0, normal);
            return job;
        }
        let total_chars = line.chars().count();
        let mut pos = 0usize;
        for &(s, e) in &ranges {
            if s > pos {
                job.append(slice_chars(line, pos, s), 0.0, normal.clone());
            }
            job.append(slice_chars(line, s, e), 0.0, hit.clone());
            pos = e;
        }
        if pos < total_chars {
            job.append(slice_chars(line, pos, total_chars), 0.0, normal);
        }
        job
    }
}

// ─── 渲染辅助（纯函数，可测） ────────────────────────────────────────────────

/// 来源徽标：Daemon=青 / Event=紫 / Module(id)=id 哈希稳定配色
fn source_badge(src: &LogSource) -> (String, egui::Color32) {
    match src {
        LogSource::Daemon => ("daemon".into(), egui::Color32::from_rgb(110, 195, 205)),
        LogSource::Event => ("event".into(), egui::Color32::from_rgb(190, 140, 230)),
        LogSource::Module(id) => (id.clone(), module_color(id)),
    }
}

/// 模块 id → 稳定中亮度配色（FNV-1a 哈希；每通道 90..217 保证深浅主题可读）
fn module_color(id: &str) -> egui::Color32 {
    let mut h: u32 = 2166136261;
    for b in id.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16777619);
    }
    let ch = |shift: u32| 90 + ((h >> shift) & 0x7F) as u8;
    egui::Color32::from_rgb(ch(0), ch(7), ch(14))
}

fn level_label(l: Option<LogLevel>) -> &'static str {
    match l {
        Some(LogLevel::Trace) => "TRACE",
        Some(LogLevel::Debug) => "DEBUG",
        Some(LogLevel::Info) => "INFO",
        Some(LogLevel::Warn) => "WARN",
        Some(LogLevel::Error) => "ERROR",
        None => "—", // WS 模块行 / 事件行（无级别语义，GT-4 口径）
    }
}

fn level_color(l: Option<LogLevel>) -> egui::Color32 {
    match l {
        Some(LogLevel::Error) => egui::Color32::from_rgb(235, 90, 90),
        Some(LogLevel::Warn) => egui::Color32::from_rgb(235, 185, 80),
        Some(LogLevel::Info) => egui::Color32::from_rgb(115, 195, 135),
        Some(LogLevel::Debug) => egui::Color32::from_rgb(115, 155, 225),
        Some(LogLevel::Trace) => egui::Color32::GRAY,
        None => egui::Color32::GRAY,
    }
}

/// 按 char 索引切片（match_ranges 产出 char 区间 → 原串子串）
fn slice_chars(s: &str, start: usize, end: usize) -> &str {
    let byte_at = |n: usize| {
        s.char_indices()
            .nth(n)
            .map(|(b, _)| b)
            .unwrap_or(s.len())
    };
    &s[byte_at(start)..byte_at(end)]
}

// ─── root 解析（LogPipe 注入口；GT-4 报告 §5 接缝口径） ─────────────────────

/// LogPipe root 解析，对齐 daemon `ep_core::config::resolve_root` 的三级口径：
/// 1. `EP_ROOT` 环境变量（值须为存在目录，否则视为无效往下跌）；
/// 2. `file_tail::resolve_root(exe_dir)`（GT-4 的 config/ 标记目录两步）；
/// 3. 兜底 cwd（daemon 同款）；连 cwd 都取不到 → exe_dir。
///
/// 设置页显式 root 覆盖归 GT-3（届时在调用方把第一级换成共享配置即可）。
pub(crate) fn resolve_log_root(exe_dir: &std::path::Path, ep_root: Option<PathBuf>, cwd: Option<PathBuf>) -> PathBuf {
    if let Some(p) = ep_root.filter(|p| p.is_dir()) {
        return p;
    }
    if let Some(p) = file_tail::resolve_root(exe_dir) {
        return p;
    }
    cwd.unwrap_or_else(|| exe_dir.to_path_buf())
}

// ─── 本地时间显示（ts 列 HH:MM:SS.mmm；不引 chrono——Windows API 直转） ─────

/// 1601-01-01（FILETIME 纪元）与 1970-01-01（Unix 纪元）的秒差 × 10⁷
const EPOCH_DIFF_100NS: u64 = 11_644_473_600 * 10_000_000;

/// SystemTime → 本地时区 `HH:MM:SS.mmm`。
///
/// 链路：SystemTime → FILETIME → `FileTimeToSystemTime`（UTC SYSTEMTIME）→
/// `SystemTimeToTzSpecificLocalTime`（按当前时区含 DST 规则转本地）→ 格式化。
/// DST 由 API 处理（不手算 Bias）；时区 API 失败 → 降级显示 UTC，不崩。
/// UI 线程安全性：仅内核态查询（每次调用 ~µs 级），只对**可视行**（几十行）
/// 调用——5 fps 空闲下开销可忽略。
pub(crate) fn format_local_hms_mmm(ts: std::time::SystemTime) -> String {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        FileTimeToSystemTime, GetTimeZoneInformation, SystemTimeToTzSpecificLocalTime,
        TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION,
    };

    let d = ts.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let ft_100ns = d.as_secs() * 10_000_000 + (d.subsec_nanos() / 100) as u64 + EPOCH_DIFF_100NS;
    let ft = FILETIME {
        dwLowDateTime: (ft_100ns & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ft_100ns >> 32) as u32,
    };
    let mut utc = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    if unsafe { FileTimeToSystemTime(&ft, &mut utc) } == 0 {
        return "--:--:--.---".into();
    }
    let mut tzi = TIME_ZONE_INFORMATION::default();
    let mut local = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    let tz_ok = unsafe { GetTimeZoneInformation(&mut tzi) } != TIME_ZONE_ID_INVALID
        && unsafe { SystemTimeToTzSpecificLocalTime(&tzi, &utc, &mut local) } != 0;
    let st = if tz_ok { &local } else { &utc };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    )
}

// ─── 帧率实测钩子（联调留证用；生产默认零开销——OnceLock 读一次环境变量） ───

/// EP_TRAY_LOG_BENCH=<n>：首帧注入 n 条合成日志进显示缓冲（绕过引擎 ring——
/// GT-4 已证引擎面；本钩子专测 UI 显示层 5 万行负载）。0/未设 = 关。
static BENCH_N: OnceLock<usize> = OnceLock::new();
/// EP_TRAY_FRAME_DEBUG=1：每 60 帧向 stderr 打印 logic/ui 平均帧耗时。
static FRAME_DEBUG: OnceLock<bool> = OnceLock::new();

fn bench_n() -> usize {
    *BENCH_N.get_or_init(|| {
        std::env::var("EP_TRAY_LOG_BENCH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    })
}

pub(crate) fn frame_debug_enabled() -> bool {
    *FRAME_DEBUG.get_or_init(|| std::env::var_os("EP_TRAY_FRAME_DEBUG").is_some())
}

/// logic 帧调用（缓冲为空时只注一次）。
pub(crate) fn maybe_bench_inject(page: &mut LogPage) {
    let n = bench_n();
    if n == 0 || !page.buffer.is_empty() {
        return;
    }
    eprintln!("ep-tray: GT-5 帧率实测钩子：注入 {n} 条合成日志（EP_TRAY_LOG_BENCH）");
    page.ingest(bench_records(n));
}

/// 三源混合合成记录（形状贴近真实流：daemon 行带级别词、模块行无级别、
/// 事件行 JSON 串；时间戳=当前时刻，seq 仅供缓冲定位——引擎 seq 语义见 GT-4）。
fn bench_records(n: usize) -> Vec<LogRecord> {
    use LogSource::{Daemon, Event, Module};
    let now = std::time::SystemTime::now();
    (0..n)
        .map(|i| {
            let (source, level, line) = match i % 4 {
                0 => (
                    Daemon,
                    Some(LogLevel::Info),
                    format!("bench daemon line {i} loading module config value=abc{i}"),
                ),
                1 => (
                    Module("kokoro-tts".into()),
                    None,
                    format!("bench module stream chunk {i} size=1024 latency=42ms"),
                ),
                2 => (
                    Event,
                    None,
                    format!("{{\"ts\":{i},\"type\":\"bench_task\",\"status\":\"running\"}}"),
                ),
                _ => (
                    Daemon,
                    Some(if i % 17 == 0 {
                        LogLevel::Error
                    } else {
                        LogLevel::Debug
                    }),
                    format!("bench mixed severity line {i} payload=xyz{i}"),
                ),
            };
            LogRecord {
                seq: i as u64,
                ts: now,
                source,
                level,
                line,
            }
        })
        .collect()
}

// ─── 虚拟化探针（commit① 路线评估证据 + 回归守卫） ─────────────────────────

/// 探针测试：50_000 行经 `show_rows` 单帧只渲染可视区——证明零依赖路线
/// 可行（egui 内建虚拟化），egui_extras 无必要。
///
/// 用 900×600 视口跑（egui 默认测试视口是 10_000px 高，可视行数随视口
/// 线性增长——虚拟化的正确行为）；600px 高 + 行高 ~19px → 可视区几十行。
#[test]
fn show_rows_only_renders_visible_rows() {
    use eframe::egui;
    use std::cell::Cell;
    use std::rc::Rc;

    let ctx = egui::Context::default();
    ctx.set_fonts(egui::FontDefinitions::empty()); // 测试不加载真字体（省时）
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 600.0),
        )),
        ..Default::default()
    };
    let rendered = Rc::new(Cell::new(0usize));
    let rendered_clone = Rc::clone(&rendered);
    let output = ctx.run_ui(input, move |ui| {
        let row_height = ui.text_style_height(&egui::TextStyle::Body);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, row_height, 50_000, |_ui, row_range| {
                rendered_clone.set(rendered_clone.get() + row_range.len());
            });
    });
    // 测试不接渲染后端：纹理增量显式弃用（否则 FullOutput drop 时 panic）
    output.drop_without_applying_deltas();
    let n = rendered.get();
    assert!(n < 200, "50_000 行应只渲染可视区，实际渲染 {n} 行");
}

// ─── 显示缓冲单测（纯逻辑，零 IO） ──────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::logpipe::{LogSource, LogLevel};
    use std::time::{Duration, UNIX_EPOCH};

    fn rec(seq: u64, line: &str) -> LogRecord {
        LogRecord {
            seq,
            ts: UNIX_EPOCH + Duration::from_secs(seq),
            source: LogSource::Daemon,
            level: Some(LogLevel::Info),
            line: line.to_owned(),
        }
    }

    #[test]
    fn buffer_overflows_dropping_oldest() {
        let mut b = LogBuffer::new(3);
        for i in 0..5u64 {
            b.push(rec(i, &format!("l{i}")));
        }
        assert_eq!(b.len(), 3, "容量铁律：缓冲有界");
        assert_eq!(b.dropped(), 2);
        assert_eq!(b.get(1), None, "最旧两行已淘汰");
        assert_eq!(b.get(2).map(|r| r.line.as_str()), Some("l2"));
        assert_eq!(b.get(4).map(|r| r.line.as_str()), Some("l4"));
    }

    #[test]
    fn buffer_cap_zero_clamps_to_one() {
        let mut b = LogBuffer::new(0);
        b.push(rec(0, "a"));
        b.push(rec(1, "b"));
        assert_eq!(b.len(), 1);
        assert_eq!(b.dropped(), 1);
    }

    #[test]
    fn buffer_get_binary_searches_by_seq() {
        let mut b = LogBuffer::new(DISPLAY_BUFFER_CAP);
        for i in [10u64, 11, 500, 9999] {
            b.push(rec(i, &format!("l{i}")));
        }
        assert_eq!(b.get(10).map(|r| r.line.as_str()), Some("l10"));
        assert_eq!(b.get(500).map(|r| r.line.as_str()), Some("l500"));
        assert_eq!(b.get(9999).map(|r| r.line.as_str()), Some("l9999"));
        assert_eq!(b.get(0), None);
        assert_eq!(b.get(600), None);
        assert_eq!(b.get(u64::MAX), None);
    }

    #[test]
    fn buffer_extend_batch() {
        let mut b = LogBuffer::new(DISPLAY_BUFFER_CAP);
        b.extend((0..100u64).map(|i| rec(i, "x")));
        assert_eq!(b.len(), 100);
        assert_eq!(b.dropped(), 0);
        
    }

    #[test]
    fn buffer_display_cap_matches_contract() {
        assert_eq!(DISPLAY_BUFFER_CAP, 50_000, "任务口径：显示缓冲 5 万行");
    }

    // ── root 解析 ──

    static TMP_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ep-tray-gt5-{}-{}-{}",
            tag,
            std::process::id(),
            TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("临时目录应可创建");
        d
    }

    #[test]
    fn resolve_log_root_prefers_valid_ep_root() {
        let base = temp_dir("root-a");
        let exe_dir = base.join("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let marker = base.join("marker-root");
        std::fs::create_dir_all(marker.join("config")).unwrap();
        // EP_ROOT 存在 → 最高优先
        assert_eq!(
            resolve_log_root(&exe_dir, Some(marker.clone()), Some(base.clone())),
            marker
        );
    }

    #[test]
    fn resolve_log_root_invalid_ep_root_falls_to_marker() {
        let base = temp_dir("root-b");
        let exe_dir = base.join("bin");
        std::fs::create_dir_all(exe_dir.join("config")).unwrap();
        let bogus = base.join("no-such-dir");
        assert_eq!(
            resolve_log_root(&exe_dir, Some(bogus), Some(base.clone())),
            exe_dir,
            "EP_ROOT 非目录 → 跌到 config/ 标记的 exe_dir"
        );
    }

    #[test]
    fn resolve_log_root_cwd_fallback_then_exe_dir() {
        let base = temp_dir("root-c");
        let exe_dir = base.join("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let cwd = base.join("somewhere");
        std::fs::create_dir_all(&cwd).unwrap();
        // 无标记目录 → cwd 兜底
        assert_eq!(resolve_log_root(&exe_dir, None, Some(cwd.clone())), cwd);
        // 连 cwd 都没有 → exe_dir
        assert_eq!(resolve_log_root(&exe_dir, None, None), exe_dir);
    }

    // ── 5 万行注入基准（逻辑面：入缓冲 + 二分取行；渲染面证据见报告） ──

    #[test]
    fn buffer_50k_ingest_and_lookup() {
        let mut b = LogBuffer::new(DISPLAY_BUFFER_CAP);
        let t0 = std::time::Instant::now();
        b.extend((0..50_000u64).map(|i| rec(i, &format!("synthetic line {i} payload=abc{i}"))));
        let ingest = t0.elapsed();
        assert_eq!(b.len(), 50_000);
        assert_eq!(b.dropped(), 0);
        let t1 = std::time::Instant::now();
        let mut hits = 0;
        for i in (0..50_000u64).step_by(97) {
            if b.get(i).is_some() {
                hits += 1;
            }
        }
        let lookup = t1.elapsed();
        assert_eq!(hits, 50_000 / 97 + 1);
        println!(
            "GT-5 基准：50_000 行 ingest={:?} lookup 516 次={:?}",
            ingest, lookup
        );
        assert!(ingest.as_millis() < 2_000, "5 万行入缓冲应在毫秒级完成");
    }

    // ── 大小写折叠与高亮区间（纯函数） ──

    #[test]
    fn fold_ascii_preserves_indices_and_cjk() {
        let folded = fold_ascii("ABC def 中文 X1");
        assert_eq!(folded, "abc def 中文 x1");
        // char 与 byte 索引都不变（ASCII 折叠等宽）——高亮区间直接映射回原串
        assert_eq!(folded.chars().count(), "ABC def 中文 X1".chars().count());
        assert_eq!(folded.len(), "ABC def 中文 X1".len());
    }

    #[test]
    fn contains_folded_case_insensitive_ascii() {
        assert!(contains_folded("Hello World", fold_ascii("WORLD").as_str()));
        assert!(contains_folded("hello world", fold_ascii("Hello").as_str()));
        assert!(!contains_folded("hello world", fold_ascii("xyz").as_str()));
        assert!(contains_folded("任意中文 abc", fold_ascii("中文").as_str()), "CJK 透传");
        assert!(contains_folded("", ""), "空 needle = 不过滤");
    }

    #[test]
    fn match_ranges_single_multiple_and_none() {
        // 单命中
        assert_eq!(match_ranges("Hello World", "world"), vec![(6, 11)]);
        // 多命中（贪心不重叠）
        assert_eq!(match_ranges("abab abab", "ab"), vec![(0, 2), (2, 4), (5, 7), (7, 9)]);
        // 大小写混合：needle 按契约先 fold（渲染层 fold 缓存一次复用）
        assert_eq!(match_ranges("INFO start", "info"), vec![(0, 4)]);
        assert!(match_ranges("nothing here", "zzz").is_empty());
        assert!(match_ranges("short", "longer-than-line").is_empty());
        assert!(match_ranges("anything", "").is_empty(), "空 needle 无高亮");
    }

    #[test]
    fn match_ranges_indices_map_to_original_chars() {
        // CJK 行 + ASCII needle：区间按 char 索引映射回原串（等宽折叠保证）
        let line = "staging 初始化 budget_mb=7967";
        let ranges = match_ranges(line, "budget");
        assert_eq!(ranges, vec![(12, 18)]);
        let chars: Vec<char> = line.chars().collect();
        let s = ranges[0].0;
        let e = ranges[0].1;
        assert_eq!(chars[s..e].iter().collect::<String>(), "budget");
    }

    // ── 过滤器谓词（来源 / 级别 / 子串组合） ──

    fn src_rec(seq: u64, source: LogSource, level: Option<LogLevel>, line: &str) -> LogRecord {
        LogRecord {
            seq,
            ts: UNIX_EPOCH + Duration::from_secs(seq),
            source,
            level,
            line: line.to_owned(),
        }
    }

    fn default_filter() -> LogFilter {
        LogFilter::default()
    }

    /// 谓词便捷入口（生产路径形状：fold 一次 → matches_with 逐行）
    fn m(f: &LogFilter, r: &LogRecord) -> bool {
        let needle = f.folded_needle();
        f.matches_with(r, &needle)
    }

    #[test]
    fn filter_defaults_show_everything() {
        let f = default_filter();
        assert!(m(&f, &src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "x")));
        assert!(m(&f, &src_rec(1, LogSource::Event, None, "{\"ts\":1}")));
        assert!(m(&f, &src_rec(2, LogSource::Module("asr".into()), None, "gpu ok")));
    }

    #[test]
    fn filter_source_toggles() {
        let mut f = default_filter();
        f.sources.daemon = false;
        f.sources.events = false;
        f.sources.modules = false;
        assert!(!m(&f, &src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "x")));
        assert!(!m(&f, &src_rec(1, LogSource::Event, None, "e")));
        assert!(!m(&f, &src_rec(2, LogSource::Module("asr".into()), None, "m")));
        f.sources.modules = true;
        assert!(m(&f, &src_rec(2, LogSource::Module("asr".into()), None, "m")));

        // 模块 id 明细：空 = 全部；非空 = 只显示勾选
        let mut f2 = default_filter();
        f2.sources.daemon = false;
        f2.sources.events = false;
        f2.sources.module_ids.insert("asr".into());
        assert!(m(&f2, &src_rec(0, LogSource::Module("asr".into()), None, "a")));
        assert!(!m(&f2, &src_rec(1, LogSource::Module("tts".into()), None, "t")));
    }

    #[test]
    fn filter_level_toggles_including_none() {
        let mut f = default_filter();
        f.levels.info = false;
        assert!(!m(&f, &src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "x")));
        assert!(m(&f, &src_rec(1, LogSource::Daemon, Some(LogLevel::Error), "y")));
        // None 级别（模块行/事件行）由 none 开关独立控制
        let mut f2 = default_filter();
        f2.levels.none = false;
        assert!(!m(&f, &src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "x")), "级别与来源互不相干时行仍按级别过滤");
        assert!(!m(&f2, &src_rec(1, LogSource::Event, None, "e")));
        assert!(m(&f2, &src_rec(2, LogSource::Event, Some(LogLevel::Warn), "w")));
    }

    #[test]
    fn filter_substring_case_insensitive() {
        let mut f = default_filter();
        f.needle = "ERROR".into();
        assert!(m(&f, &src_rec(0, LogSource::Daemon, Some(LogLevel::Error), "x error occurred")));
        assert!(!m(&f, &src_rec(1, LogSource::Daemon, Some(LogLevel::Info), "all good")));
        f.needle = "错误".into();
        assert!(m(&f, &src_rec(2, LogSource::Daemon, Some(LogLevel::Error), "发生错误: bad")));
        assert!(!m(&f, &src_rec(3, LogSource::Daemon, Some(LogLevel::Error), "no match")));
    }

    #[test]
    fn filter_matches_with_reuses_folded_needle() {
        // 热路径形状：fold 一次，逐行 matches_with
        let mut f = default_filter();
        f.needle = "GPU".into();
        let needle = f.folded_needle();
        assert!(f.matches_with(&src_rec(0, LogSource::Daemon, None, "gpu idle"), &needle));
        assert!(!f.matches_with(&src_rec(1, LogSource::Daemon, None, "cpu idle"), &needle));
    }

    // ── 过滤视图（seq 列表 + 惰性重算） ──

    #[test]
    fn page_view_filters_by_predicate() {
        let mut page = LogPage::new();
        page.ingest(vec![
            src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "daemon line"),
            src_rec(1, LogSource::Module("asr".into()), None, "module line"),
            src_rec(2, LogSource::Event, None, "event line"),
        ]);
        page.rebuild();
        assert_eq!(page.view_len(), 3);

        page.filter.sources.daemon = false;
        page.filter.sources.events = false;
        page.rebuild();
        assert_eq!(page.view_len(), 1, "只留模块行");
        assert_eq!(page.view_row(0).map(|r| r.line.as_str()), Some("module line"));
    }

    #[test]
    fn page_view_lazy_rebuild_skips_unchanged() {
        let mut page = LogPage::new();
        page.ingest(vec![src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "a")]);
        page.rebuild();
        // 不变 → 复用（视图 seq 不变；这里验证语义：rebuild 幂等）
        let before: Vec<u64> = page.view_row(0).map(|r| r.seq).into_iter().collect();
        page.rebuild();
        let after: Vec<u64> = page.view_row(0).map(|r| r.seq).into_iter().collect();
        assert_eq!(before, after);
        assert_eq!(page.view_len(), 1);
    }

    #[test]
    fn page_view_tracks_buffer_eviction_and_gap() {
        // 小容量页缓冲：5 行入 3 容量 → 前 2 行被淘汰（与 ring 背压同语义）
        let mut page = LogPage::new_with_cap(3);
        // 缓冲上限 3：5 行入 → 前 2 行被淘汰
        for i in 0..5u64 {
            page.ingest(vec![src_rec(
                i,
                LogSource::Daemon,
                Some(LogLevel::Info),
                &format!("l{i}"),
            )]);
        }
        page.rebuild();
        assert_eq!(page.view_len(), 3);
        assert_eq!(page.view_row(0).map(|r| r.seq), Some(2), "被淘汰的 seq 不在视图");
        assert_eq!(page.buffer.dropped(), 2, "淘汰计数在缓冲层（工具条展示口径）");
    }

    #[test]
    fn page_module_ids_seen_collects_from_stream() {
        let mut page = LogPage::new();
        page.ingest(vec![
            src_rec(0, LogSource::Module("asr".into()), None, "a"),
            src_rec(1, LogSource::Module("asr".into()), None, "b"),
            src_rec(2, LogSource::Module("tts".into()), None, "c"),
            src_rec(3, LogSource::Daemon, Some(LogLevel::Info), "d"),
        ]);
        let ids: Vec<&String> = page.module_ids_seen.iter().collect();
        assert_eq!(ids, vec!["asr", "tts"]);
    }

    // ── 本地时间格式化（Windows API 链路） ──

    #[test]
    fn local_time_formats_hms_mmm_shape() {
        use std::time::UNIX_EPOCH;
        let shape_ok = |s: &str| {
            s.len() == 12
                && s.as_bytes()[2] == b':'
                && s.as_bytes()[5] == b':'
                && s.as_bytes()[8] == b'.'
                && s.bytes().all(|b| b.is_ascii_digit() || b == b':' || b == b'.')
        };
        let now = format_local_hms_mmm(std::time::SystemTime::now());
        assert!(shape_ok(&now), "HH:MM:SS.mmm 形状，实际 {now}");
        // 纪元零值：本地时区偏移后的合法时刻（UTC+8 机器 = 08:00:00.000）——
        // 断言与时区无关的形状而非具体值
        let zero = format_local_hms_mmm(UNIX_EPOCH);
        assert!(shape_ok(&zero), "零值形状，实际 {zero}");
        // 纪元前钳到 UNIX_EPOCH 再转（不 panic，返回合法时刻）
        let pre = format_local_hms_mmm(UNIX_EPOCH - Duration::from_secs(100_000));
        assert!(shape_ok(&pre), "纪元前形状，实际 {pre}");
    }

    // ── 渲染辅助 ──

    #[test]
    fn module_color_stable_and_mid_brightness() {
        let c1 = module_color("kokoro-tts");
        let c2 = module_color("kokoro-tts");
        assert_eq!(c1, c2, "同 id 同色");
        assert_ne!(module_color("asr"), module_color("tts"), "不同 id 应大概率不同色");
        let [r, g, b, _] = c1.to_array();
        assert!(r >= 90 && g >= 90 && b >= 90, "中亮度下限");
        assert!(r <= 216 && g <= 216 && b <= 216, "中亮度上限");
    }

    #[test]
    fn slice_chars_handles_cjk_and_bounds() {
        let s = "中文abc";
        assert_eq!(slice_chars(s, 0, 2), "中文");
        assert_eq!(slice_chars(s, 2, 5), "abc");
        assert_eq!(slice_chars(s, 0, 5), s);
        assert_eq!(slice_chars(s, 5, 5), "");
        assert_eq!(slice_chars(s, 0, 99), s, "越界钳到串尾");
    }

    #[test]
    fn level_and_source_labels_cover_all_variants() {
        assert_eq!(level_label(Some(LogLevel::Error)), "ERROR");
        assert_eq!(level_label(None), "—");
        let (label, color) = source_badge(&LogSource::Module("m1".into()));
        assert_eq!(label, "m1");
        assert_ne!(color, egui::Color32::WHITE);
    }

    // ── 5 万行过滤视图基准（逻辑面证据；渲染面见 EP_TRAY_FRAME_DEBUG 留证） ──

    #[test]
    fn page_50k_view_rebuild_bench() {
        let mut page = LogPage::new();
        page.ingest(bench_records(50_000));
        let t0 = std::time::Instant::now();
        page.rebuild();
        let unfiltered = t0.elapsed();
        assert_eq!(page.view_len(), 50_000);

        page.filter.needle = "bench".into();
        page.filter.levels.none = false;
        let t1 = std::time::Instant::now();
        page.rebuild();
        let filtered = t1.elapsed();
        assert!(page.view_len() > 0);
        assert!(page.view_len() < 50_000, "needle+级别过滤后应少于全量");
        println!(
            "GT-5 基准：5 万行视图重建 无过滤={unfiltered:?} 子串+级别过滤={filtered:?}（命中 {} 行）",
            page.view_len()
        );
        assert!(filtered.as_millis() < 2_000, "5 万行过滤重建应在毫秒级");
    }
}
