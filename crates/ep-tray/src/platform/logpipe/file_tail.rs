//! daemon.log 文件 tail 源（GT-4 第二源）：增量追读 + ANSI 剥离 + 轮转重置。
//!
//! - **byte offset 追读**：每轮从上次消费位置读到 EOF，完整行产出、半行留
//!   `carry` 缓冲（[`MAX_CARRY`] 上限防病态无换行文件无界膨胀）；
//! - **每轮 sleep** [`TAIL_INTERVAL`]（300ms，调度在集成层 [`LogPipe::spawn`]，
//!   见 mod.rs——本文件只做单轮纯 IO 逻辑，可注入路径单测）；
//! - **文件变小/重建 → offset 归零**（daemon 重启覆盖写、外部轮转场景）；
//! - **文件不存在 → 静默等待**（daemon 未起属常态，不报错不刷屏）；
//! - 每行经 [`crate::platform::logpipe::strip_ansi`] 入 ring。
//!
//! [`LogPipe::spawn`]: crate::platform::logpipe

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// daemon.log tail 轮询间隔（契约 §2 原文 500ms，派发单定为 300ms——派发单优先）
pub const TAIL_INTERVAL: Duration = Duration::from_millis(300);

/// 半行缓冲上限：单行超过该值按断行强制产出（防御无换行的病态输出把
/// carry 撑爆——容量铁律同样适用于行缓冲）
pub const MAX_CARRY: usize = 256 * 1024;

/// daemon.log 在 root 下的固定位置（沿 daemon 语义：`runtime/logs/daemon.log`）
pub fn daemon_log_path(root: &Path) -> PathBuf {
    root.join("runtime").join("logs").join("daemon.log")
}

/// root 解析（沿 daemon `ep_core::config::resolve_root` 语义的两步简化版，
/// 写成可注入参数便于单测；与 GT-3 设置页 root 覆盖的接缝见报告）：
///
/// 1. `exe_dir/config/` 存在 → root = exe_dir（便携解压自包含布局）；
/// 2. 否则 `exe_dir.parent()/config/` 存在 → root = parent（`<root>/bin/` 部署布局）；
/// 3. 都不满足 → `None`（调用方决定兜底：UI 覆盖值 / cwd / 静默等待文件出现）。
///
/// 注：daemon 真实现判据是 `config/ + modules/` 双标记且含 EP_ROOT 环境变量
/// 与 cwd 兜底——tray 侧先按派发单两步口径，接 GT-3 时对齐（见报告 §4）。
pub fn resolve_root(exe_dir: &Path) -> Option<PathBuf> {
    if exe_dir.join("config").is_dir() {
        return Some(exe_dir.to_path_buf());
    }
    let parent = exe_dir.parent()?;
    if parent.join("config").is_dir() {
        return Some(parent.to_path_buf());
    }
    None
}

/// tail 起始策略：默认 [`TailStart::End`]（只跟随新行——与 WS 源「接入后开始」
/// 语义一致，不回灌历史）；[`TailStart::Beginning`] 供导出/冒烟测试读全量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailStart {
    /// 从文件末尾开始只跟随追加（默认）
    End,
    /// 从文件头开始读已有内容再跟随
    Beginning,
}

/// daemon.log tail 源。`poll` 单轮逻辑独立于线程调度，纯注入路径可测。
#[derive(Debug)]
pub struct FileTailSource {
    path: PathBuf,
    start: TailStart,
    /// 下一次读取的文件字节位置
    offset: u64,
    /// 半行缓冲（字节；UTF-8 字符可能跨轮拆分，故按字节留、整行再转 String）
    carry: Vec<u8>,
    /// 首轮定位是否已做过（首轮按 start 策略定 offset）
    started: bool,
    /// 构造时文件是否已存在（End 策略的定位依据：已存在 → 从尾跟随不回灌；
    /// 后出现/消失重现 → 视为重建从 0 读，daemon 启动 burst 不丢，
    /// 误回灌代价由 ring 背压有界吸收）
    ever_seen: bool,
}

impl FileTailSource {
    pub fn new(path: PathBuf, start: TailStart) -> Self {
        // 构造时文件是否已存在：End 策略据此区分「已有历史文件不回灌」
        // 与「文件后出现/重建 → 从头读全量」（daemon 启动 burst 不丢）
        let existed = path.exists();
        Self {
            path,
            start,
            offset: 0,
            carry: Vec::new(),
            started: false,
            ever_seen: existed,
        }
    }

    /// 单轮增量读取：产出完整行（剥 ANSI、去行尾 \r）。返回本轮行数。
    ///
    /// 任何「文件还不存在/暂时打不开」一律静默返回 0 并重置状态——
    /// daemon 未起属常态（红线），文件出现后自动从头跟随。
    pub fn poll(&mut self, out: &mut dyn FnMut(String)) -> usize {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => {
                self.reset();
                return 0;
            }
        };
        let len = match file.metadata() {
            Ok(m) => m.len(),
            Err(_) => {
                self.reset();
                return 0;
            }
        };
        if !self.started {
            // 定位规则：
            // - Beginning：永远从 0 读；
            // - End 且构造时文件已存在：定位到当前文件尾——只跟新增量，不回灌
            //   历史（与 WS 源「接入后开始」语义一致）；
            // - End 且构造时文件不存在 / 消失后重现（reset 已清 offset）：视为
            //   重建，从 0 读——daemon 启动 burst 不丢；瞬时打开失败误回灌整
            //   文件的代价由 ring 背压有界吸收（dropped 计数可见）。
            self.offset = match (self.start, self.ever_seen) {
                (TailStart::Beginning, _) => 0,
                (TailStart::End, true) => len,
                (TailStart::End, false) => 0,
            };
            self.started = true;
            self.ever_seen = true;
        }
        if len < self.offset {
            // 文件变小/重建（daemon 重启覆盖写或外部轮转）→ offset 归零重读
            self.offset = 0;
            self.carry.clear();
        }
        if len == self.offset {
            return 0; // 无新增量
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            self.reset();
            return 0;
        }
        let mut chunk = Vec::new();
        if file.read_to_end(&mut chunk).is_err() {
            self.reset();
            return 0;
        }
        self.offset += chunk.len() as u64;

        // carry + chunk 拼接后按 \n 切分：完整行产出，最后一段留 carry
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(&chunk);
        let last_nl = match data.iter().rposition(|&b| b == b'\n') {
            Some(p) => p,
            None => {
                // 整段无换行 → 全留 carry；超上限则丢弃（病态无换行输出防御，
                // 正常 tracing 行远短于此；容量铁律同样适用于行缓冲）
                if data.len() <= MAX_CARRY {
                    self.carry = data;
                } else {
                    self.carry.clear();
                }
                return 0;
            }
        };
        self.carry = data[last_nl + 1..].to_vec();

        let mut count = 0;
        for raw in data[..last_nl].split(|&b| b == b'\n') {
            let plain = strip_line(raw);
            if plain.is_empty() {
                continue; // 空行不入 ring（tracing 偶发的分隔空行）
            }
            out(plain);
            count += 1;
        }
        count
    }

    fn reset(&mut self) {
        self.offset = 0;
        self.carry.clear();
        self.started = false;
    }
}

/// 单行后处理：字节 → UTF-8（多字节跨轮安全已在整行转换下天然成立）→
/// 剥 ANSI → 去 CR。
fn strip_line(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let stripped = super::strip_ansi(&s);
    stripped.trim_end_matches('\r').to_string()
}

// ─── 单测（临时文件注入，零 GUI / 零网络） ─────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

    /// 进程内唯一临时目录（不引 tempfile 依赖；唯一名防并行测试互踩）
    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ep-tray-gt4-{}-{}-{}",
            tag,
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("临时目录应可创建");
        d
    }

    fn collect(tail: &mut FileTailSource) -> Vec<String> {
        let mut lines = Vec::new();
        tail.poll(&mut |l| lines.push(l));
        lines
    }

    #[test]
    fn resolve_root_prefers_exe_dir_then_parent() {
        let base = temp_dir("root");
        // 布局 1：exe_dir/config/ → root = exe_dir
        let exe_dir = base.join("app");
        fs::create_dir_all(exe_dir.join("config")).unwrap();
        assert_eq!(resolve_root(&exe_dir), Some(exe_dir.clone()));
        // 布局 2：parent/config/ → root = parent（<root>/bin 布局）
        let bin = base.join("deploy").join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(base.join("deploy").join("config")).unwrap();
        assert_eq!(resolve_root(&bin), Some(base.join("deploy")));
        // 布局 3：都没有 → None
        let bare = base.join("nowhere");
        fs::create_dir_all(&bare).unwrap();
        assert_eq!(resolve_root(&bare), None);
    }

    #[test]
    fn tail_follows_from_end_by_default() {
        let dir = temp_dir("tail-end");
        let log = dir.join("daemon.log");
        fs::write(&log, "old-1\nold-2\n").unwrap();
        let mut tail = FileTailSource::new(log.clone(), TailStart::End);
        assert_eq!(collect(&mut tail).len(), 0, "End 策略不回灌历史");
        // 追加 → 增量产出
        let mut f = fs::OpenOptions::new().append(true).open(&log).unwrap();
        use std::io::Write;
        writeln!(f, "new-1").unwrap();
        writeln!(f, "new-2").unwrap();
        let lines = collect(&mut tail);
        assert_eq!(lines, vec!["new-1", "new-2"]);
    }

    #[test]
    fn tail_beginning_reads_existing_then_incremental() {
        let dir = temp_dir("tail-begin");
        let log = dir.join("daemon.log");
        fs::write(&log, "a\nb\nc").unwrap(); // 末行无换行 → 半行留 carry
        let mut tail = FileTailSource::new(log.clone(), TailStart::Beginning);
        assert_eq!(collect(&mut tail), vec!["a", "b"], "半行 c 留缓冲");
        let mut f = fs::OpenOptions::new().append(true).open(&log).unwrap();
        use std::io::Write;
        writeln!(f, "d").unwrap();
        let lines = collect(&mut tail);
        assert_eq!(lines, vec!["cd"], "carry 与新块拼接成完整行");
    }

    #[test]
    fn tail_recreate_resets_offset() {
        let dir = temp_dir("tail-recreate");
        let log = dir.join("daemon.log");
        fs::write(&log, "l1\nl2\nl3\nl4\n").unwrap();
        let mut tail = FileTailSource::new(log.clone(), TailStart::Beginning);
        assert_eq!(collect(&mut tail).len(), 4);
        // 重建：文件变短（daemon 重启覆盖写）
        fs::write(&log, "fresh-1\n").unwrap();
        let lines = collect(&mut tail);
        assert_eq!(lines, vec!["fresh-1"], "变小 → offset 归零重读");
    }

    #[test]
    fn tail_missing_file_is_silent_then_picks_up() {
        let dir = temp_dir("tail-missing");
        let log = dir.join("daemon.log");
        let mut tail = FileTailSource::new(log.clone(), TailStart::End);
        assert_eq!(tail.poll(&mut |_| {}), 0, "文件不存在：静默 0 不报错");
        assert_eq!(tail.poll(&mut |_| {}), 0);
        // daemon 起来后文件出现 → 自动开始跟随
        fs::write(&log, "born-1\n").unwrap();
        assert_eq!(collect(&mut tail), vec!["born-1"]);
    }

    #[test]
    fn tail_strips_ansi_from_lines() {
        let dir = temp_dir("tail-ansi");
        let log = dir.join("daemon.log");
        // 真实 tracing ANSI 形状（与 mod.rs 样张同源）
        fs::write(
            &log,
            "\x1b[2m2026-08-24T14:06:10.002848Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mep_daemon\x1b[0m\x1b[2m:\x1b[0m Listening on 127.0.0.1:9800\r\n",
        )
        .unwrap();
        let mut tail = FileTailSource::new(log, TailStart::Beginning);
        let lines = collect(&mut tail);
        assert_eq!(
            lines,
            vec!["2026-08-24T14:06:10.002848Z  INFO ep_daemon: Listening on 127.0.0.1:9800"],
            "ANSI 剥离 + \\r 去除"
        );
    }
}
