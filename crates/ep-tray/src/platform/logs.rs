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

// commit③ 过滤引擎纯逻辑先行：过滤器/视图消费面（渲染层）在本票后续 commit
// 接入——接入时移除本 allow（届时残留的未用项应直接删除）。
#![allow(dead_code)]

use std::collections::{BTreeSet, VecDeque};
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

    /// 缓冲内最小 seq（首个可显示行；过滤视图判定「起始缺口」用）
    pub fn first_seq(&self) -> Option<u64> {
        self.buf.front().map(|r| r.seq)
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
    /// fold 后的子串（每次视图重建 fold 一次；matches 路径零 fold 零分配）
    pub fn folded_needle(&self) -> String {
        fold_ascii(&self.needle)
    }

    /// 便捷版（单测用）：即时 fold。
    pub fn matches(&self, r: &LogRecord) -> bool {
        let needle = self.folded_needle();
        self.matches_with(r, &needle)
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
}

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
        assert_eq!(b.first_seq(), Some(2));
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
        assert_eq!(b.first_seq(), Some(0));
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

    #[test]
    fn filter_defaults_show_everything() {
        let f = default_filter();
        assert!(f.matches(&src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "x")));
        assert!(f.matches(&src_rec(1, LogSource::Event, None, "{\"ts\":1}")));
        assert!(f.matches(&src_rec(2, LogSource::Module("asr".into()), None, "gpu ok")));
    }

    #[test]
    fn filter_source_toggles() {
        let mut f = default_filter();
        f.sources.daemon = false;
        f.sources.events = false;
        f.sources.modules = false;
        assert!(!f.matches(&src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "x")));
        assert!(!f.matches(&src_rec(1, LogSource::Event, None, "e")));
        assert!(!f.matches(&src_rec(2, LogSource::Module("asr".into()), None, "m")));
        f.sources.modules = true;
        assert!(f.matches(&src_rec(2, LogSource::Module("asr".into()), None, "m")));

        // 模块 id 明细：空 = 全部；非空 = 只显示勾选
        let mut f2 = default_filter();
        f2.sources.daemon = false;
        f2.sources.events = false;
        f2.sources.module_ids.insert("asr".into());
        assert!(f2.matches(&src_rec(0, LogSource::Module("asr".into()), None, "a")));
        assert!(!f2.matches(&src_rec(1, LogSource::Module("tts".into()), None, "t")));
    }

    #[test]
    fn filter_level_toggles_including_none() {
        let mut f = default_filter();
        f.levels.info = false;
        assert!(!f.matches(&src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "x")));
        assert!(f.matches(&src_rec(1, LogSource::Daemon, Some(LogLevel::Error), "y")));
        // None 级别（模块行/事件行）由 none 开关独立控制
        let mut f2 = default_filter();
        f2.levels.none = false;
        assert!(!f.matches(&src_rec(0, LogSource::Daemon, Some(LogLevel::Info), "x")), "级别与来源互不相干时行仍按级别过滤");
        assert!(!f2.matches(&src_rec(1, LogSource::Event, None, "e")));
        assert!(f2.matches(&src_rec(2, LogSource::Event, Some(LogLevel::Warn), "w")));
    }

    #[test]
    fn filter_substring_case_insensitive() {
        let mut f = default_filter();
        f.needle = "ERROR".into();
        assert!(f.matches(&src_rec(0, LogSource::Daemon, Some(LogLevel::Error), "x error occurred")));
        assert!(!f.matches(&src_rec(1, LogSource::Daemon, Some(LogLevel::Info), "all good")));
        f.needle = "错误".into();
        assert!(f.matches(&src_rec(2, LogSource::Daemon, Some(LogLevel::Error), "发生错误: bad")));
        assert!(!f.matches(&src_rec(3, LogSource::Daemon, Some(LogLevel::Error), "no match")));
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
}
