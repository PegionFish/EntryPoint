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

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::platform::logpipe::{file_tail, LogRecord};

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
    // 过滤视图按 seq 取行用（本票渲染 commit 接入；当前仅单测消费）
    #[allow(dead_code)]
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
    // 过滤视图接入前仅单测消费
    #[allow(dead_code)]
    pub fn first_seq(&self) -> Option<u64> {
        self.buf.front().map(|r| r.seq)
    }
}

// ─── 页面状态 ───────────────────────────────────────────────────────────────

/// 日志页状态（TrayApp 主线程独占；logic 帧进数据，ui 帧渲染）。
pub(crate) struct LogPage {
    pub buffer: LogBuffer,
}

impl LogPage {
    pub fn new() -> Self {
        Self {
            buffer: LogBuffer::new(DISPLAY_BUFFER_CAP),
        }
    }

    /// logic 帧消费引擎新行（drain_new 产物追加进显示缓冲）。
    pub fn ingest(&mut self, rows: Vec<LogRecord>) {
        self.buffer.extend(rows);
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
}
