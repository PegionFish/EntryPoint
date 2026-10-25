//! 触发规则注册表 + 扫描引擎（PLAN_TRIGGER_UNIFIED_LOG §5.1 / §5.2）
//!
//! ## 设计
//!
//! 触发规则与管线定义**分离**（同 schedule.rs 模式）：规则独立存放于
//! `runtime/watchers.json`（`rule_id → WatchRule`），tmp+rename 原子落盘，
//! `load` 失败按空表启动并告警。
//!
//! 扫描语义核心是**水位线 + 在途稳定表**：
//! - `checkpoint`（水位线，epoch 秒）：只考察 mtime 大于它的文件，
//!   十万存量文件天然被排除，不进入任何索引结构（内存与目录规模解耦）；
//! - `in_flight`（在途稳定表）：仅"前沿批次"（每轮 ≤ `max_batch` 个）
//!   候选进入，签名跨轮一致即视为稳定触发——内存 = O(新文件到达速率)。
//!
//! [`collect_watch_events`] 是**纯函数**（只读目录，不落盘、不提交）：
//! 返回本窗口应触发的 [`WatchHit`] 列表与推进后的新注册表，持久化与
//! 提交由调用方（main.rs 巡检循环）决定。提交失败（如 `QueueFull`）时
//! 调用方可整轮放弃新表、沿用旧注册表——文件保持待触发、下轮重试
//! （D6 语义，不丢文件）。
//!
//! ## 水位线推进（§5.2-6）
//!
//! "已解决" = 已提交触发（本批次内）、被过滤规则排除（黑名单/白名单）、
//! 或 `include_modified=false` 下签名变化。`in_flight` 中未决文件阻塞
//! 其后的水位推进；目录中已消失的 `in_flight` 条目剪除。
//!
//! ## 产物登记表（T-25，自我触发防护）
//!
//! daemon 直调任务（`pipeline_id = "watcher/<rule_id>"`）的归档产物落盘时，
//! execution.rs 在 `record_artifact` 挂钩登记「产物绝对路径 + 来源规则 id +
//! task_id + 时间」到 `runtime/processed_registry.json`（与水位线
//! `watchers.json` 同目录惯例）。扫描侧（[`collect_watch_events`]）对登记
//! 命中的路径整条剔除——**系统产物永不自我触发**（watch 目录 = 产物目录
//! 场景不再死循环；管线模式规则的管线若把 `file_archive` 指向 watch 目录，
//! 本登记表是创建期校验覆盖不到时的运行期安全网）。
//!
//! 语义边界（T-25 冻结）：登记表只记 daemon 自己落盘的产物**路径**——
//! 用户手改/新增的文件仍照常触发；同一路径的原位手改不再触发（路径级登记
//! 的已知边界）；**不做内容级去重**（sha256 内容指纹与 exclude glob 留
//! follow-up）。自防膨胀：条目上限 10000（LRU 淘汰最旧）+ 加载时丢弃已
//! 不存在路径的条目；登记文件损坏按空表启动并告警。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

/// BT/下载器半截文件后缀黑名单（§5.2-2，逐字冻结）
pub const IGNORED_SUFFIXES: &[&str] = &[
    ".part",
    ".tmp",
    ".download",
    ".!qB",
    ".crdownload",
    ".bc",
    ".td",
    ".xltd",
];

/// 递归扫描的目录深度上限（防御异常深树；符号链接目录不跟随，天然无环）
const MAX_SCAN_DEPTH: usize = 64;

/// 命名模板默认值（§5.1 冻结）
fn default_template() -> String {
    "{name}.{ext}".to_string()
}

fn default_true() -> bool {
    true
}

fn default_stability() -> u64 {
    30
}

fn default_max_batch() -> usize {
    16
}

/// 文件签名（跨轮一致 → 稳定）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSig {
    pub mtime: i64,
    pub size: u64,
}

/// 直接模式动作（§5.1 冻结形状；`kind` 以 `type` 为 tag 判别）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectAction {
    pub kind: DirectKind,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DirectKind {
    /// 仅归档（纯搬运重命名）
    Archive,
    /// 模块直调（全能力清单）
    Module { module_id: String, capability: String },
}

/// 管线模式动作
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineAction {
    pub pipeline_id: String,
    pub input_node: String,
}

/// 产物输出配置（直接模式必填；管线模式可选）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputConfig {
    /// 绝对路径
    pub dest_dir: String,
    #[serde(default = "default_template")]
    pub name_template: String,
    #[serde(default)]
    pub on_conflict: ConflictPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    #[default]
    Suffix,
    Overwrite,
    Skip,
}

/// 触发记录速览（环形缓冲条目，最近 5 条；全量在统一事件日志）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub ts: i64,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 单条触发规则（§5.1 契约逐字冻结）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchRule {
    /// 服务端生成，8 位小写十六进制
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 绝对路径
    pub watch_dir: String,
    /// 递归子目录
    #[serde(default)]
    pub recursive: bool,
    /// 空=全部；小写无点
    #[serde(default)]
    pub extensions: Vec<String>,
    /// 默认仅新文件
    #[serde(default)]
    pub include_modified: bool,
    /// 静默期（秒）：mtime 距 now 超过该值才可能成为候选
    #[serde(default = "default_stability")]
    pub stability_secs: u64,
    /// 含存量文件（§2 D5）
    #[serde(default)]
    pub backfill: bool,
    /// 水位线（epoch 秒）
    #[serde(default)]
    pub checkpoint: i64,
    /// 在途稳定表（有界：每轮仅前沿批次进入）
    #[serde(default)]
    pub in_flight: HashMap<String, FileSig>,
    #[serde(default = "default_max_batch")]
    pub max_batch: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct: Option<DirectAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<PipelineAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task_id: Option<String>,
    /// 最近 5 条速览
    #[serde(default)]
    pub recent: VecDeque<EventRecord>,
}

/// 全量注册表：rule_id → 规则（与 schedule.rs 同款 flatten map 形状）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchRegistry {
    #[serde(flatten)]
    pub rules: HashMap<String, WatchRule>,
}

impl WatchRegistry {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(file = %path.display(), error = %e, "watchers.json 解析失败，按空表启动");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// tmp + rename 原子落盘（复刻 schedule.rs 持久化模式）
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 生成不与现有规则冲突的 8 位小写十六进制 id
    pub fn generate_rule_id(&mut self) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        loop {
            let nanos = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
                .unwrap_or(0);
            let mixed = nanos
                .wrapping_mul(0x9E37_79B9)
                ^ SEQ.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x85EB_CA6B);
            let id = format!("{:08x}", mixed);
            if !self.rules.contains_key(&id) {
                return id;
            }
        }
    }
}

/// 注册表文件默认路径：`<root>/runtime/watchers.json`
pub fn default_registry_path(root: &Path) -> PathBuf {
    root.join("runtime").join("watchers.json")
}

// ─── 路径归一化与重叠判定（T-25 创建期校验 + 产物登记比较共用）──────────────

/// 路径归一键（T-25；**仅供比较，不得当真实路径使用**）：
///
/// - 剥离 Windows 扩展长度前缀 `\\?\`（`\\?\UNC\server\share` 先还原为
///   `\\server\share` 形状再统一处理）；
/// - 分隔符统一为 `/`；丢弃空段与 `.` 段；`..` 做词法上浮；
/// - 保留 POSIX 根标记（`/` 开头的归一键以 `/` 开头，使 `/` 与 `/data`
///   的包含关系可判定）；
/// - Windows（`cfg!(windows)`）整体小写折叠——Win32/NTFS 路径大小写不敏感；
///   POSIX 保持原样（大小写敏感）。
pub(crate) fn normalize_path_key(raw: &str) -> String {
    let trimmed = raw.trim();
    let s = match trimmed.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix("UNC\\") {
            Some(unc) => format!(r"\\{unc}"),
            None => rest.to_string(),
        },
        None => trimmed.to_string(),
    };
    let unified = s.replace('\\', "/");
    let leading_root = unified.starts_with('/');
    let mut comps: Vec<&str> = Vec::new();
    for seg in unified.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            other => comps.push(other),
        }
    }
    let joined = comps.join("/");
    let key = if leading_root {
        format!("/{joined}")
    } else {
        joined
    };
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    }
}

/// 目录重叠判定（T-25 创建期校验核心）：两路径经 [`normalize_path_key`]
/// 归一后，相等或一方为另一方的组件级前缀即视为重叠（一方递归包含另一方）。
///
/// 局限（有意为之，注释留档）：本判定是**词法**比较，不解析 junction /
/// 符号链接别名——如 watch 目录经 junction 指向 dest_dir 内部时检测不到。
/// 不采用 `canonicalize` 的原因：创建期目录可能尚不存在，且解析会跨
/// junction 改写形状、行为随挂载状态漂移；该场景留待后续票（配合
/// 「风险自理」确认通道）一并考虑。
pub(crate) fn paths_overlap(a: &str, b: &str) -> bool {
    let na = normalize_path_key(a);
    let nb = normalize_path_key(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    // POSIX 根 `/` 包含一切绝对路径
    if na == "/" || nb == "/" {
        return true;
    }
    na == nb || na.starts_with(&format!("{nb}/")) || nb.starts_with(&format!("{na}/"))
}

// ─── 产物登记表（processed registry，T-25）───────────────────────────────────

/// 登记表落盘文件名（与水位线 watchers.json 同目录惯例：`<root>/runtime/`）
const PROCESSED_FILE_NAME: &str = "processed_registry.json";

/// 条目上限：超出按 LRU 淘汰最旧（自防膨胀）
pub const PROCESSED_MAX_ENTRIES: usize = 10_000;

/// 单条产物登记
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessedEntry {
    /// 产物绝对路径（按登记原样保存；比较时经 [`normalize_path_key`] 归一）
    pub path: String,
    /// 来源规则 id（自任务 `pipeline_id = "watcher/<rule_id>"` 解出）
    pub rule_id: String,
    /// 产生该产物的任务 id
    pub task_id: String,
    /// 登记时间（epoch 秒）
    pub ts: i64,
}

/// 产物登记表：daemon 自己落盘的产物路径集合（扫描侧据此排除，系统产物
/// 永不自我触发）。**只记路径**——不做内容级去重（sha256 与 exclude glob
/// 留 follow-up）；用户手改/新增的文件不在表内，照常触发。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessedRegistry {
    /// 新条目在前（LRU 语义：超限淘汰尾部最旧）
    #[serde(default)]
    pub entries: VecDeque<ProcessedEntry>,
}

impl ProcessedRegistry {
    /// 登记一条产物（默认上限 [`PROCESSED_MAX_ENTRIES`]）
    pub fn record(&mut self, path: &Path, rule_id: &str, task_id: &str, ts: i64) {
        self.record_with_cap(path, rule_id, task_id, ts, PROCESSED_MAX_ENTRIES);
    }

    /// 登记核心（`max` 参数化供测试小容量验证 LRU）：同一路径（归一后）
    /// 重复登记 → 刷新位置与元数据（LRU 触碰，覆盖写场景不累积）。
    pub(crate) fn record_with_cap(
        &mut self,
        path: &Path,
        rule_id: &str,
        task_id: &str,
        ts: i64,
        max: usize,
    ) {
        let entry = ProcessedEntry {
            path: path.to_string_lossy().into_owned(),
            rule_id: rule_id.to_string(),
            task_id: task_id.to_string(),
            ts,
        };
        let key = normalize_path_key(&entry.path);
        self.entries.retain(|e| normalize_path_key(&e.path) != key);
        self.entries.push_front(entry);
        self.entries.truncate(max.max(1));
    }

    /// 路径是否已登记（归一比较；仅测试断言用——扫描侧走 [`Self::path_keys`]
    /// 集合比较，避免逐文件全表扫描）
    #[cfg(test)]
    pub(crate) fn contains_path(&self, path: &Path) -> bool {
        let key = normalize_path_key(&path.to_string_lossy());
        self.entries
            .iter()
            .any(|e| normalize_path_key(&e.path) == key)
    }

    /// 全部条目的归一键集合（扫描侧每轮构建一次）
    pub(crate) fn path_keys(&self) -> HashSet<String> {
        self.entries
            .iter()
            .map(|e| normalize_path_key(&e.path))
            .collect()
    }

    /// 丢弃已不存在路径的条目（加载时调用，自防膨胀）
    pub fn prune_missing(&mut self) {
        self.entries.retain(|e| Path::new(&e.path).exists());
    }

    /// 加载；文件缺失按空表，损坏降级为空表并告警（复刻 WatchRegistry::load）
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(file = %path.display(), error = %e, "产物登记表解析失败，按空表启动");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// tmp + rename 原子落盘（复刻 WatchRegistry::save 模式）
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// 进程内登记表状态：内存表 + 落盘路径（None = 无法定位 runtime 目录，
/// 退化为仅内存生效——本次运行内仍防自我触发，重启即失，告警留痕）。
struct ProcessedState {
    disk: Option<PathBuf>,
    registry: ProcessedRegistry,
}

static PROCESSED: OnceLock<Mutex<Option<ProcessedState>>> = OnceLock::new();

fn processed_cell() -> &'static Mutex<Option<ProcessedState>> {
    PROCESSED.get_or_init(|| Mutex::new(None))
}

/// 落盘路径定位（自包含，不经 AppState）：经任务注册表持久化目录反推
/// `<root>/runtime/`——`bind_persistence` 在 `AppState::new` 启动即绑定，
/// watcher 巡检 / 产物登记发生时必然已就绪。
fn resolve_processed_disk_path() -> Option<PathBuf> {
    let tasks_dir = crate::api::execute::execution::persistence_dir()?;
    tasks_dir
        .parent()
        .map(|runtime| runtime.join(PROCESSED_FILE_NAME))
}

/// 从落盘路径构建登记表状态（加载 + 清理死路径；独立函数供「模拟重启」
/// 测试与 [`init_processed_state`] 共用）
fn load_processed_state(disk: Option<&Path>) -> ProcessedState {
    match disk {
        Some(p) => {
            let mut reg = ProcessedRegistry::load(p);
            let before = reg.entries.len();
            reg.prune_missing();
            let pruned = before - reg.entries.len();
            if pruned > 0 {
                tracing::info!(file = %p.display(), pruned, "产物登记表加载时清理已不存在路径的条目");
                // 修剪结果即时回写，避免每次启动重复清扫同一批死路径
                if let Err(e) = reg.save(p) {
                    tracing::warn!(file = %p.display(), error = %e, "产物登记表修剪后回写失败");
                }
            }
            ProcessedState {
                disk: Some(p.to_path_buf()),
                registry: reg,
            }
        }
        None => {
            tracing::warn!("无法定位 runtime 目录，产物登记表仅内存生效（重启即失）");
            ProcessedState {
                disk: None,
                registry: ProcessedRegistry::default(),
            }
        }
    }
}

fn init_processed_state() -> ProcessedState {
    load_processed_state(resolve_processed_disk_path().as_deref())
}

fn with_processed<T>(f: impl FnOnce(&mut ProcessedState) -> T) -> T {
    let mut cell = processed_cell().lock().unwrap_or_else(|e| e.into_inner());
    let st = cell.get_or_insert_with(init_processed_state);
    f(st)
}

/// 产物登记入口（execution.rs `record_artifact` 唯一挂钩调用）：watcher
/// 直调任务的 archive 产物落盘即入表并落盘。落盘失败仅告警——登记缺失的
/// 后果只是该产物路径可能自我触发一次，不阻断任务链路。
pub(crate) fn record_processed_product(path: &Path, rule_id: &str, task_id: &str) {
    if path.as_os_str().is_empty() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    with_processed(|st| {
        st.registry.record(path, rule_id, task_id, ts);
        if let Some(disk) = st.disk.clone() {
            if let Err(e) = st.registry.save(&disk) {
                tracing::warn!(file = %disk.display(), error = %e, "产物登记表落盘失败");
            }
        }
    });
}

/// 扫描侧排除键集合（[`collect_watch_events`] 每轮构建一次；登记表尚未
/// 初始化时此调用即完成惰性加载）。
fn processed_path_keys() -> HashSet<String> {
    with_processed(|st| st.registry.path_keys())
}

#[cfg(test)]
pub(crate) fn processed_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 仅测试：整体重置登记表全局态（`disk` 显式指定落盘路径；`None` = 下次
/// 使用时按生产逻辑重新惰性初始化）。
#[cfg(test)]
pub(crate) fn processed_reset_for_tests(disk: Option<PathBuf>) {
    *processed_cell().lock().unwrap_or_else(|e| e.into_inner()) =
        Some(ProcessedState {
            disk,
            registry: ProcessedRegistry::default(),
        });
}

/// 单次巡检产生的待触发项（`rule` 为收集完成后的规则快照，供调用方
/// 提交与回写使用）
#[derive(Debug, Clone)]
pub struct WatchHit {
    pub rule_id: String,
    pub path: String,
    pub rule: WatchRule,
}

// ─── 目录扫描 ────────────────────────────────────────────────────────────────

/// 目录内常规文件快照（扫描与判定解耦，供合成测试注入 mock 文件列表）
#[derive(Debug, Clone)]
pub(crate) struct DirFile {
    pub path: PathBuf,
    pub mtime: i64,
    pub size: u64,
}

/// 遍历目录收集常规文件（按 `recursive` 决定是否递归）。
/// 目录不可读 → Err（调用方 warn 跳过）；单个条目 stat 失败 → 跳过。
fn scan_dir_files(dir: &Path, recursive: bool) -> std::io::Result<Vec<DirFile>> {
    let mut out = Vec::new();
    visit_dir(dir, recursive, 0, &mut out)?;
    Ok(out)
}

fn visit_dir(
    dir: &Path,
    recursive: bool,
    depth: usize,
    out: &mut Vec<DirFile>,
) -> std::io::Result<()> {
    if depth > MAX_SCAN_DEPTH {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir)?;
    for entry in entries.flatten() {
        // file_type 不跟随符号链接：符号链接目录不递归（防环）、
        // 符号链接文件按 metadata 跟随后按常规文件处理
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            if recursive {
                let _ = visit_dir(&path, recursive, depth + 1, out);
            }
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(DirFile {
            path,
            mtime,
            size: md.len(),
        });
    }
    Ok(())
}

/// 黑名单：文件名以任一半截后缀结尾（大小写不敏感，如 ".PART" / ".!qb"）
fn is_ignored_suffix(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    IGNORED_SUFFIXES
        .iter()
        .any(|s| name.ends_with(&s.to_lowercase()))
}

/// 归一化单个扩展名条目：去点、转小写、去空白
fn normalize_extension(ext: &str) -> String {
    ext.trim()
        .trim_start_matches('.')
        .to_lowercase()
        .to_string()
}

/// 白名单过滤（`extensions` 为空 = 全部放行）
fn extensions_allow(extensions: &[String], path: &Path) -> bool {
    if extensions.is_empty() {
        return true;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext.is_empty() {
        return false;
    }
    extensions
        .iter()
        .any(|e| normalize_extension(e) == ext)
}

// ─── 扫描引擎（纯函数）──────────────────────────────────────────────────────

/// 巡检纯函数（§5.2-1..8 逐条实现）：对注册表求本窗口应触发的文件集合，
/// 并返回推进后的新注册表。只读目录、无落盘副作用，持久化由调用方决定。
///
/// - 停用规则完全跳过（不扫描、不推进水位线）
/// - 目录缺失/不可读 → `tracing::warn!` 跳过，不拖垮整个循环
/// - T-25：产物登记表命中的路径在进入判定**之前**整条剔除——不触发、不入
///   在途表、不阻塞水位线（系统产物永不自我触发；水位线/在途表语义不动，
///   被剔除文件对 §5.2 机制完全不可见）。误入在途表的残留条目（登记前
///   极窄竞态窗口）按"目录中已消失"语义自然剪除。
pub fn collect_watch_events(
    registry: &WatchRegistry,
    now_epoch_secs: i64,
) -> (Vec<WatchHit>, WatchRegistry) {
    let mut next = registry.clone();
    let mut hits = Vec::new();
    // T-25：产物登记表排除键（每轮构建一次，全规则共享；登记表不可用 →
    // 空集，语义退化为无排除）
    let processed_keys = processed_path_keys();
    for (rule_id, rule) in next.rules.iter_mut() {
        // §5.2-1：停用规则完全跳过；重新启用后首轮不触发存量
        if !rule.enabled {
            continue;
        }
        // §5.2-8：目录缺失/不可读 → warn 跳过（复刻 collect_due 容错）
        let files = match scan_dir_files(Path::new(&rule.watch_dir), rule.recursive) {
            Ok(files) => files,
            Err(e) => {
                tracing::warn!(
                    rule_id = %rule_id,
                    dir = %rule.watch_dir,
                    error = %e,
                    "watch 目录缺失或不可读，本轮跳过"
                );
                continue;
            }
        };
        let files: Vec<DirFile> = if processed_keys.is_empty() {
            files
        } else {
            files
                .into_iter()
                .filter(|f| {
                    !processed_keys
                        .contains(&normalize_path_key(&f.path.to_string_lossy()))
                })
                .collect()
        };
        let (triggered, updated) = collect_rule_events(rule, &files, now_epoch_secs);
        *rule = updated;
        for path in triggered {
            hits.push(WatchHit {
                rule_id: rule_id.clone(),
                path,
                rule: rule.clone(),
            });
        }
    }
    (hits, next)
}

/// 单规则扫描判定核心（[`collect_watch_events`] 的分解，参数化文件列表
/// 供合成断言注入 mock 数据，不依赖真实文件系统）。
///
/// 返回 `(触发路径列表（mtime 升序，已截断 max_batch）, 推进后的规则)`。
pub(crate) fn collect_rule_events(
    rule: &WatchRule,
    files: &[DirFile],
    now_epoch_secs: i64,
) -> (Vec<String>, WatchRule) {
    let mut next = rule.clone();
    let max_batch = next.max_batch.max(1);

    // ── §5.2-7 backfill 哨兵 / 非回灌基线 ────────────────────────────────
    // backfill=true 且 checkpoint==0：首轮置为现存候选文件的最旧 mtime，
    // 之后按正常流程有序追赶（每轮仅前沿批次进 in_flight，内存有界）。
    // backfill=false：创建即 checkpoint=now（存量不回灌）。
    if next.checkpoint == 0 {
        if next.backfill {
            let oldest = files
                .iter()
                .filter(|f| !is_ignored_suffix(&f.path) && extensions_allow(&next.extensions, &f.path))
                .map(|f| f.mtime)
                .min();
            next.checkpoint = oldest.unwrap_or(now_epoch_secs);
        } else {
            next.checkpoint = now_epoch_secs;
        }
    }

    let stability = next.stability_secs as i64;
    let current: HashMap<String, &DirFile> = files
        .iter()
        .map(|f| (f.path.to_string_lossy().to_string(), f))
        .collect();

    // 已消失的 in_flight 条目剪除（§5.2-6）
    let vanished: HashSet<String> = next
        .in_flight
        .keys()
        .filter(|p| !current.contains_key(p.as_str()))
        .cloned()
        .collect();

    // 前缀水位推进的逐文件解决标记：(mtime, path, resolved?)
    let mut marks: Vec<(i64, String, bool)> = Vec::new();
    let mut stable_ready: Vec<(String, i64)> = Vec::new(); // 稳定待触发
    let mut pending_insert: Vec<(String, i64, FileSig)> = Vec::new(); // 新候选，前沿批次入表
    let mut sig_updates: Vec<(String, FileSig)> = Vec::new(); // 签名变化（include_modified=true）
    let mut removes: HashSet<String> = vanished; // 从 in_flight 移除的条目

    for f in files {
        let path_str = f.path.to_string_lossy().to_string();
        // 水位线以下不考察（十万存量文件在此被排除，不进入索引结构）
        if f.mtime <= next.checkpoint {
            // 残留的 in_flight 条目（水位线之下）一并清理
            if next.in_flight.contains_key(&path_str) {
                removes.insert(path_str);
            }
            continue;
        }
        // 过滤规则（§5.2-2）：黑名单 → 白名单；被排除即"已解决"
        let filtered = is_ignored_suffix(&f.path) || !extensions_allow(&next.extensions, &f.path);
        if filtered {
            removes.insert(path_str.clone());
            marks.push((f.mtime, path_str, true));
            continue;
        }
        // 静默期（§5.2-3）：mtime 距 now 不足 stability_secs → 尚非候选，未决
        if f.mtime > now_epoch_secs - stability {
            marks.push((f.mtime, path_str, false));
            continue;
        }
        // ── 候选判定（§5.2-4）────────────────────────────────────────────
        let sig = FileSig {
            mtime: f.mtime,
            size: f.size,
        };
        match next.in_flight.get(&path_str) {
            // 签名一致 → 稳定（先按未决标记，批次确定后回填已解决）
            Some(prev) if *prev == sig => {
                stable_ready.push((path_str.clone(), f.mtime));
                marks.push((f.mtime, path_str, false));
            }
            // 签名变化
            Some(_) => {
                if next.include_modified {
                    sig_updates.push((path_str.clone(), sig));
                    marks.push((f.mtime, path_str, false));
                } else {
                    // include_modified=false：视为已解决，永不触发
                    removes.insert(path_str.clone());
                    marks.push((f.mtime, path_str, true));
                }
            }
            // 新候选：仅前沿批次进表（内存有界）
            None => {
                pending_insert.push((path_str.clone(), f.mtime, sig));
                marks.push((f.mtime, path_str, false));
            }
        }
    }

    // ── §5.2-5 触发按 mtime 升序，截断 max_batch ────────────────────────
    stable_ready.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut triggered: Vec<String> = Vec::new();
    let mut triggered_set: HashSet<String> = HashSet::new();
    for (path, _) in stable_ready.iter().take(max_batch) {
        triggered.push(path.clone());
        triggered_set.insert(path.clone());
        removes.insert(path.clone()); // 触发即解决：移出在途表
    }
    // 批次内触发 = 已解决（回填前缀标记）；批次外保持未决，阻塞其后水位推进
    for mark in marks.iter_mut() {
        if !mark.2 && triggered_set.contains(&mark.1) {
            mark.2 = true;
        }
    }

    // 新候选同样仅前沿批次入表（backfill 追赶风暴限流）
    pending_insert.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let inserts: Vec<(String, FileSig)> = pending_insert
        .iter()
        .take(max_batch)
        .map(|(p, _, sig)| (p.clone(), *sig))
        .collect();

    // ── §5.2-6 checkpoint 推进到已解决前缀的最大 mtime ──────────────────
    marks.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut new_checkpoint = next.checkpoint;
    for (mtime, _, resolved) in &marks {
        if !resolved {
            break; // 未决文件阻塞其后水位推进
        }
        new_checkpoint = *mtime;
    }
    next.checkpoint = new_checkpoint;

    // ── 在途表写回 ──────────────────────────────────────────────────────
    for path in removes {
        next.in_flight.remove(&path);
    }
    for (path, sig) in inserts {
        next.in_flight.insert(path, sig);
    }
    for (path, sig) in sig_updates {
        next.in_flight.insert(path, sig);
    }

    (triggered, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造单规则注册表的辅助
    fn rule_with(mutate: impl FnOnce(&mut WatchRule)) -> WatchRegistry {
        let mut r = WatchRegistry::default();
        let mut rule = WatchRule {
            id: "r1".into(),
            name: "test".into(),
            enabled: true,
            watch_dir: "/nowhere".into(),
            recursive: false,
            extensions: vec![],
            include_modified: false,
            stability_secs: 30,
            backfill: false,
            checkpoint: 0,
            in_flight: HashMap::new(),
            max_batch: 16,
            direct: None,
            pipeline: None,
            output: None,
            last_task_id: None,
            recent: VecDeque::new(),
        };
        mutate(&mut rule);
        r.rules.insert(rule.id.clone(), rule);
        r
    }

    /// 构造 mock 文件列表（不落盘）
    fn df(path: &str, mtime: i64, size: u64) -> DirFile {
        DirFile {
            path: PathBuf::from(path),
            mtime,
            size,
        }
    }

    /// 经由完整入口跑单规则（目录指向不存在路径会走容错分支，
    /// 故这里直接断言 collect_rule_events，目录容错另有专项测试）
    fn run(rule: &WatchRule, files: Vec<DirFile>, now: i64) -> (Vec<String>, WatchRule) {
        collect_rule_events(rule, &files, now)
    }

    // 1. 稳定触发：首轮入表不触发，次轮签名一致即触发并移出在途表
    #[test]
    fn stable_file_triggers_on_second_round() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
        });
        let rule = &r.rules["r1"];
        let files = vec![df("/d/a.mkv", 1500, 100)];
        // 首轮：新候选入表，不触发
        let (hits, next) = run(rule, files.clone(), 2000);
        assert!(hits.is_empty(), "首轮仅入表不得触发");
        assert_eq!(next.in_flight.get("/d/a.mkv"), Some(&FileSig { mtime: 1500, size: 100 }));
        // 次轮：签名一致 → 触发，移出在途表
        let (hits, next) = run(&next, files, 2000);
        assert_eq!(hits, vec!["/d/a.mkv".to_string()]);
        assert!(next.in_flight.is_empty());
        // 触发后水位线推进到该文件 mtime
        assert_eq!(next.checkpoint, 1500);
        // 再次巡检不重放
        let (hits, _) = run(&next, vec![df("/d/a.mkv", 1500, 100)], 2000);
        assert!(hits.is_empty(), "已解决文件不得重触发");
    }

    // 2. 半文件后缀黑名单过滤：.part 文件永不触发、不入表、视为已解决
    #[test]
    fn half_file_suffixes_are_filtered() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
        });
        let rule = &r.rules["r1"];
        let files = vec![
            df("/d/movie.mkv.part", 1500, 100),
            df("/d/movie.mkv.!qB", 1500, 100),
            df("/d/file.crdownload", 1500, 100),
        ];
        let (hits, next) = run(rule, files, 2000);
        assert!(hits.is_empty());
        assert!(next.in_flight.is_empty(), "黑名单文件不得进入在途表");
        assert_eq!(next.checkpoint, 1500, "被过滤文件视为已解决，水位线推进");
        // 跨轮一致：永不触发
        let files2 = vec![df("/d/movie.mkv.part", 1500, 200)];
        let (hits, _) = run(&next, files2, 3000);
        assert!(hits.is_empty());
    }

    // 3. 静默期未满不触发：mtime 距 now 不足 stability_secs 时不候选；
    //    且未决文件阻塞水位线推进
    #[test]
    fn silence_window_blocks_trigger_and_watermark() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
        });
        let rule = &r.rules["r1"];
        let files = vec![df("/d/new.mkv", 1980, 100)]; // now-20 < stability 30
        let (hits, next) = run(rule, files.clone(), 2000);
        assert!(hits.is_empty(), "静默期未满不得触发");
        assert!(next.in_flight.is_empty(), "静默期文件不入在途表");
        assert_eq!(next.checkpoint, 1000, "未决文件阻塞水位推进");
        // 时间前进越过静默期后：先入表，再触发
        let (hits, next) = run(&next, files.clone(), 2011);
        assert!(hits.is_empty(), "越过静默期首轮仍需入表");
        let (hits, _) = run(&next, files, 2011);
        assert_eq!(hits, vec!["/d/new.mkv".to_string()]);
    }

    // 4. 扩展名白名单过滤：不匹配被排除并推进水位线；大小写归一
    #[test]
    fn extension_whitelist_filters_and_normalizes_case() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
            r.extensions = vec!["jpg".into()];
        });
        let rule = &r.rules["r1"];
        let files = vec![
            df("/d/notes.txt", 1500, 10),
            df("/d/photo.JPG", 1600, 20),
        ];
        let (hits, next) = run(rule, files.clone(), 2000);
        assert!(hits.is_empty(), "首轮只入表");
        assert!(next.in_flight.contains_key("/d/photo.JPG"), "白名单内文件入表");
        assert!(!next.in_flight.contains_key("/d/notes.txt"), "白名单外文件被排除");
        let (hits, _) = run(&next, files, 2000);
        assert_eq!(hits, vec!["/d/photo.JPG".to_string()]);
    }

    // 5. 停用规则完全跳过：不扫描、不推进、不触发
    #[test]
    fn disabled_rule_skips_entirely() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
            r.enabled = false;
        });
        let (hits, next) = collect_watch_events(&r, 5000);
        assert!(hits.is_empty());
        assert_eq!(
            next.rules["r1"].checkpoint, 1000,
            "停用规则不得推进水位线"
        );
        assert!(next.rules["r1"].in_flight.is_empty());
        // 重新启用后首轮不触发存量（先入表）
        let mut re = next;
        re.rules.get_mut("r1").unwrap().enabled = true;
        let (hits, next) = collect_watch_events(&re, 5000);
        assert!(hits.is_empty(), "重新启用首轮不触发");
        let rule = &next.rules["r1"];
        let files = vec![df("/nowhere/x.mp4", 4000, 1)];
        let (hits, next) = run(rule, files.clone(), 5000);
        assert!(hits.is_empty(), "启用后首轮先入表");
        let (hits, _) = run(&next, files, 5000);
        assert_eq!(hits, vec!["/nowhere/x.mp4".to_string()]);
    }

    // 6. 目录缺失容错：warn 跳过单规则，不拖垮注册表其他规则
    #[test]
    fn missing_dir_is_tolerated_and_other_rules_still_run() {
        let mut reg = rule_with(|r| {
            r.checkpoint = 1000;
            r.watch_dir = "/definitely/missing/dir".into();
        });
        // 第二条规则走纯函数路径（直接置入在途表模拟已稳定）
        let mut r2 = reg.rules["r1"].clone();
        r2.id = "r2".into();
        r2.watch_dir = "/also/missing".into();
        reg.rules.insert("r2".into(), r2);
        let (hits, next) = collect_watch_events(&reg, 5000);
        assert!(hits.is_empty());
        assert_eq!(next.rules["r1"].checkpoint, 1000, "缺失目录规则原样保留");
    }

    // 7. 水位线推进与剪枝：已解决前缀推进；消失条目剪除；
    //    未决文件阻塞其后推进
    #[test]
    fn watermark_advances_over_resolved_prefix_and_prunes_vanished() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
        });
        let mut rule = r.rules["r1"].clone();
        // 预置一个已消失的在途条目 + 一个存留的在途条目
        rule.in_flight.insert("/d/ghost.mkv".into(), FileSig { mtime: 1100, size: 5 });
        rule.in_flight.insert("/d/kept.mkv".into(), FileSig { mtime: 1300, size: 7 });
        let files = vec![
            df("/d/ghost.mkv", 1100, 5),  // 目录中已不存在 → 下面用不包含它的列表
            df("/d/junk.part", 1150, 1),  // 黑名单 → 已解决
            df("/d/kept.mkv", 1300, 7),   // 签名一致 → 稳定触发（批次内）
            df("/d/late.mkv", 1990, 9),   // 静默期未满 → 未决，阻塞
        ];
        let mut files_no_ghost = files.clone();
        files_no_ghost.remove(0);
        let (hits, next) = run(&rule, files_no_ghost, 2000);
        assert_eq!(hits, vec!["/d/kept.mkv".to_string()]);
        assert!(!next.in_flight.contains_key("/d/ghost.mkv"), "消失条目必须剪除");
        assert!(!next.in_flight.contains_key("/d/kept.mkv"), "触发后移出在途表");
        // 已解决前缀：junk.part(1150) resolved、kept.mkv(1300) resolved →
        // checkpoint 推进到 1300；late.mkv(1990) 未决阻塞
        assert_eq!(next.checkpoint, 1300);
    }

    // 8. 存量不回灌：backfill=false 时创建即 checkpoint=now，存量文件跨轮不触发；
    //    之后到达的新文件正常触发
    #[test]
    fn no_backfill_excludes_existing_files() {
        let now = 5000;
        let r = rule_with(|r| {
            r.checkpoint = 0; // 新建规则
            r.backfill = false;
        });
        let rule = &r.rules["r1"];
        let existing = vec![df("/d/old1.mkv", now - 100, 1), df("/d/old2.mkv", now - 1, 1)];
        let (hits, next) = run(rule, existing.clone(), now);
        assert!(hits.is_empty());
        assert_eq!(next.checkpoint, now, "非回灌规则基线即 now");
        assert!(next.in_flight.is_empty(), "存量文件不进入在途表");
        // 跨轮多次巡检：存量永不触发，水位线保持基线
        let (hits, next) = run(&next, existing, now + 3600);
        assert!(hits.is_empty());
        assert_eq!(next.checkpoint, now, "无未决新文件时水位线保持基线");
        // 新文件（mtime 晚于基线）正常两轮触发
        let new_files = vec![df("/d/fresh.mkv", now + 3600, 1)];
        let (hits, next) = run(&next, new_files.clone(), now + 3640);
        assert!(hits.is_empty(), "新文件先入表");
        let (hits, _) = run(&next, new_files, now + 3640);
        assert_eq!(hits, vec!["/d/fresh.mkv".to_string()]);
    }

    // 9. backfill 有序追赶且内存有界（合成断言）：前沿批次进表，
    //    按 mtime 升序逐批触发，在途表规模始终 ≤ max_batch
    #[test]
    fn backfill_catches_up_in_order_with_bounded_memory() {
        let r = rule_with(|r| {
            r.checkpoint = 0;
            r.backfill = true;
            r.max_batch = 4;
            r.stability_secs = 30;
        });
        let rule = r.rules["r1"].clone();
        // 合成 50 个存量文件（不造真文件），mtime 1000..1049
        let files: Vec<DirFile> = (0..50)
            .map(|i| df(&format!("/d/f{i:03}.mkv"), 1000 + i, 1))
            .collect();
        let now = 5000;

        // 首轮：哨兵推进到最旧 mtime，前沿批次入表（≤ max_batch）
        let (hits, next) = run(&rule, files.clone(), now);
        assert!(hits.is_empty(), "backfill 首轮仅入表");
        assert_eq!(next.checkpoint, 1000, "哨兵置为最旧 mtime");
        assert!(
            next.in_flight.len() <= 4,
            "在途表必须有界（≤ max_batch），实际 {}",
            next.in_flight.len()
        );

        // 追赶若干轮：触发严格按 mtime 升序，且每轮在途表有界
        let mut fired: Vec<i64> = Vec::new();
        let mut cur = next;
        for _ in 0..30 {
            let (hits, next) = run(&cur, files.clone(), now);
            assert!(hits.len() <= 4, "单轮触发不得超过 max_batch");
            for h in &hits {
                let idx: usize = h
                    .trim_start_matches("/d/f")
                    .trim_end_matches(".mkv")
                    .parse()
                    .unwrap();
                fired.push(1000 + idx as i64);
            }
            assert!(next.in_flight.len() <= 4, "追赶期在途表必须有界");
            cur = next;
            if fired.len() == 49 {
                break;
            }
        }
        // 49 个文件按 mtime 升序全部追赶完成（最旧的 f000 因哨兵
        // checkpoint=最旧 mtime 且候选要求 mtime > checkpoint 而排除——
        // 契约 §5.2-3/§5.2-7 逐字组合的既定行为）
        let mut sorted = fired.clone();
        sorted.sort();
        assert_eq!(fired, sorted, "backfill 必须按 mtime 升序追赶");
        assert_eq!(fired.len(), 49, "除哨兵排除的最旧文件外全部追赶完成");
    }

    // 10. 重启不重放：注册表经 JSON 往返（模拟重启）后不重复触发
    #[test]
    fn restart_does_not_replay() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
        });
        let rule = &r.rules["r1"];
        let files = vec![df("/d/a.mkv", 1500, 100)];
        // 两轮走完：触发 + 水位线推进
        let (_, next) = run(rule, files.clone(), 2000);
        let (hits, persisted) = run(&next, files.clone(), 2000);
        assert_eq!(hits, vec!["/d/a.mkv".to_string()]);

        // 模拟 daemon 重启：注册表落盘 → 重新加载
        let mut registry = WatchRegistry::default();
        registry.rules.insert(persisted.id.clone(), persisted);
        let json = serde_json::to_string_pretty(&registry).unwrap();
        let reloaded: WatchRegistry = serde_json::from_str(&json).unwrap();
        let reloaded_rule = &reloaded.rules["r1"];
        assert_eq!(reloaded_rule.checkpoint, 1500, "水位线随注册表持久化");

        // 同一 now 与更晚 now：均不重放（文件 mtime 已在水位线之下）
        let (hits, _) = run(reloaded_rule, files, 2000);
        assert!(hits.is_empty(), "重启后同一窗口不得重放");
        let (hits, _) = run(reloaded_rule, vec![df("/d/a.mkv", 1500, 100)], 9000);
        assert!(hits.is_empty(), "重启后更晚窗口也不得重放已解决文件");

        // 注册表层入口：目录缺失路径走容错分支也不产生命中
        let (hits, _) = collect_watch_events(&reloaded, 9000);
        assert!(hits.is_empty());
    }

    // 11. 十万文件量级合成断言：水位线以下的存量不进入任何索引结构
    #[test]
    fn synthetic_100k_files_below_watermark_are_excluded() {
        let r = rule_with(|r| {
            r.checkpoint = 1_000_000;
        });
        let rule = &r.rules["r1"];
        // 合成 10 万条 mock 文件（内存构造，不造真文件）：全部低于水位线
        let files: Vec<DirFile> = (0..100_000)
            .map(|i| df(&format!("/d/huge/f{i:06}.jpg"), 1_000_000 - 1 - (i as i64 % 500_000), 1))
            .collect();
        let (hits, next) = run(rule, files.clone(), 2_000_000);
        assert!(hits.is_empty(), "水位线以下存量不得触发");
        assert!(
            next.in_flight.is_empty(),
            "十万存量不得进入在途表（内存与目录规模解耦）"
        );
        assert_eq!(next.checkpoint, 1_000_000, "无新增时水位线不动");

        // 对照组：同样 10 万条中混入 1 条新文件 → 只有新文件入表
        let mut with_new = files.clone();
        with_new.push(df("/d/huge/NEW.jpg", 1_900_000, 1));
        let (hits, next) = run(rule, with_new, 2_000_000);
        assert!(hits.is_empty(), "新文件首轮入表");
        assert_eq!(next.in_flight.len(), 1, "仅前沿新文件进入在途表");
        assert!(next.in_flight.contains_key("/d/huge/NEW.jpg"));
    }

    // 12. include_modified=false：签名变化视为已解决，永不触发
    #[test]
    fn signature_change_without_include_modified_resolves_forever() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
            r.include_modified = false;
        });
        let mut rule = r.rules["r1"].clone();
        rule.in_flight.insert("/d/a.mkv".into(), FileSig { mtime: 1500, size: 100 });
        // 文件被续写：mtime/size 变化
        let files = vec![df("/d/a.mkv", 1800, 500)];
        let (hits, next) = run(&rule, files, 2000);
        assert!(hits.is_empty(), "签名变化不触发（include_modified=false）");
        assert!(!next.in_flight.contains_key("/d/a.mkv"), "已解决文件移出在途表");
        assert_eq!(next.checkpoint, 1800, "已解决文件推进水位线");
        // 永不触发：跨轮仍无
        let files = vec![df("/d/a.mkv", 1800, 500)];
        let (hits, _) = run(&next, files, 9000);
        assert!(hits.is_empty());
    }

    // 13. include_modified=true：签名变化更新签名，稳定后可触发
    #[test]
    fn signature_change_with_include_modified_retriggers_after_stable() {
        let r = rule_with(|r| {
            r.checkpoint = 1000;
            r.include_modified = true;
        });
        let mut rule = r.rules["r1"].clone();
        rule.in_flight.insert("/d/a.mkv".into(), FileSig { mtime: 1500, size: 100 });
        let files = vec![df("/d/a.mkv", 1800, 500)];
        let (hits, next) = run(&rule, files.clone(), 2000);
        assert!(hits.is_empty(), "签名变化轮不触发");
        assert_eq!(
            next.in_flight.get("/d/a.mkv"),
            Some(&FileSig { mtime: 1800, size: 500 }),
            "签名必须更新"
        );
        // 新签名稳定后触发
        let (hits, _) = run(&next, files, 2000);
        assert_eq!(hits, vec!["/d/a.mkv".to_string()]);
    }

    // 14. 注册表原子落盘 + 加载失败按空表启动（复刻 schedule.rs 模式）
    #[test]
    fn registry_save_load_roundtrip_and_corrupt_falls_back_to_empty() {
        let mut reg = rule_with(|r| {
            r.checkpoint = 1234;
            r.extensions = vec![".MKV".into()];
        });
        reg.rules.get_mut("r1").unwrap().in_flight.insert(
            "/d/x.mkv".into(),
            FileSig { mtime: 5, size: 6 },
        );
        let dir = std::env::temp_dir().join(format!("ep-watcher-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watchers.json");
        reg.save(&path).expect("保存应成功");
        let loaded = WatchRegistry::load(&path);
        assert_eq!(loaded.rules.len(), 1);
        assert_eq!(loaded.rules["r1"].checkpoint, 1234);
        assert_eq!(loaded.rules["r1"].in_flight.get("/d/x.mkv"), Some(&FileSig { mtime: 5, size: 6 }));
        // 损坏文件 → 空表 + warn（不 panic）
        std::fs::write(&path, "{not json").unwrap();
        let broken = WatchRegistry::load(&path);
        assert!(broken.rules.is_empty(), "解析失败按空表启动");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // 15. 扩展名归一化：去点、转小写（API 校验链第 7 步共用逻辑）
    #[test]
    fn extension_normalization_strips_dots_and_lowercases() {
        assert_eq!(normalize_extension(".MKV"), "mkv");
        assert_eq!(normalize_extension("Jpg"), "jpg");
        assert_eq!(normalize_extension(" .mp4 "), "mp4");
        // 黑名单大小写不敏感
        assert!(is_ignored_suffix(Path::new("/d/x.MKV.PART")));
        assert!(!is_ignored_suffix(Path::new("/d/x.mkv")));
    }

    // ── T-25：路径归一化 / 重叠判定 / 产物登记表 ──────────────────────────

    static TEST_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn temp_root(tag: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "ep-processed-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 把文件 mtime 设为 now - age_secs（扫描按真实 FS mtime 判定）
    fn set_file_mtime_age(path: &Path, age_secs: i64) {
        let mtime = std::time::SystemTime::now()
            - std::time::Duration::from_secs(age_secs.max(0) as u64);
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
    }

    fn now_epoch() -> i64 {
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// 跨平台期望值助手：Windows 折叠小写，POSIX 保持原样
    fn expect_key(s: &str) -> String {
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_string()
        }
    }

    // 16. 路径归一化：分隔符统一、\\?\ 前缀剥离、Windows 大小写折叠、
    //     组件归一（空段/./..）、POSIX 根标记保留
    #[test]
    fn normalize_path_key_unifies_separators_prefix_case_and_components() {
        // 分隔符统一 + 尾分隔符 + 空段
        assert_eq!(
            normalize_path_key("F:\\watch\\sub\\"),
            expect_key("F:/watch/sub")
        );
        assert_eq!(
            normalize_path_key("F://watch///sub"),
            expect_key("F:/watch/sub")
        );
        // Windows 大小写折叠（POSIX 上保持原样）
        if cfg!(windows) {
            assert_eq!(normalize_path_key("F:\\Watch\\SUB"), "f:/watch/sub");
            assert_eq!(normalize_path_key("f:/WATCH"), "f:/watch");
        } else {
            assert_eq!(normalize_path_key("/Data/Watch"), "/Data/Watch");
        }
        // \\?\ 前缀剥离（盘符）
        assert_eq!(
            normalize_path_key(r"\\?\F:\watch\sub"),
            expect_key("F:/watch/sub")
        );
        // \\?\UNC\ 还原 UNC 形状后统一分隔符（归一键以根标记开头）
        assert_eq!(
            normalize_path_key(r"\\?\UNC\nas\share\x"),
            expect_key("/nas/share/x")
        );
        // 组件归一：. 与 .. 与空段；.. 词法上浮
        assert_eq!(
            normalize_path_key("/data/./watch/../sub"),
            expect_key("/data/sub")
        );
        // POSIX 根标记保留
        assert_eq!(normalize_path_key("/"), "/");
        // 空输入 → 空键
        assert_eq!(normalize_path_key("   "), "");
    }

    // 17. 重叠判定：相等、dest 包含 watch、watch 包含 dest、无关放行、
    //     Windows 大小写/分隔符变体
    #[test]
    fn paths_overlap_detection_matrix() {
        // 相等
        assert!(paths_overlap("/data/watch", "/data/watch"));
        // dest 包含 watch（watch 是 dest 的子目录）
        assert!(paths_overlap("/data/watch/src", "/data/watch"));
        // watch 包含 dest
        assert!(paths_overlap("/data/watch", "/data/watch/out"));
        // 兄弟目录 / 无关路径放行
        assert!(!paths_overlap("/data/watch", "/data/other"));
        assert!(!paths_overlap("/data/watch", "/data/watch2"));
        assert!(!paths_overlap("/a/b", "/c/d"));
        // 空输入不判重叠
        assert!(!paths_overlap("", "/data"));
        // POSIX 根包含一切
        assert!(paths_overlap("/", "/data/watch"));
        // Windows：大小写不敏感 + 分隔符变体 + \\?\ 前缀等价
        if cfg!(windows) {
            assert!(paths_overlap("F:\\watch", "f:/WATCH"));
            assert!(paths_overlap("F:\\watch\\src", "F:/watch"));
            assert!(paths_overlap(r"\\?\F:\watch", "F:/watch/sub"));
        }
        // POSIX（非 Windows）大小写敏感：不同路径不重叠
        if !cfg!(windows) {
            assert!(!paths_overlap("/data/Watch", "/data/watch"));
        }
        // junction / 符号链接别名是词法判定的已知局限（见 paths_overlap 文档）
    }

    // 18. 登记表 record：归一去重（重复登记刷新位置与元数据）+ 超限 LRU
    //     淘汰最旧（小容量参数化验证淘汰逻辑）
    #[test]
    fn processed_record_dedupes_and_evicts_oldest_beyond_cap() {
        let mut reg = ProcessedRegistry::default();
        for i in 0..6 {
            reg.record_with_cap(
                Path::new(&format!("/d/f{i}.mp4")),
                "r1",
                &format!("t{i}"),
                1000 + i,
                4,
            );
        }
        assert_eq!(reg.entries.len(), 4, "超限截断到 max");
        assert!(!reg.contains_path(Path::new("/d/f0.mp4")), "最旧被淘汰");
        assert!(!reg.contains_path(Path::new("/d/f1.mp4")), "次旧被淘汰");
        assert!(reg.contains_path(Path::new("/d/f5.mp4")));
        assert_eq!(reg.entries[0].path, "/d/f5.mp4", "新条目在前");

        // 重复登记同路径（Windows 下大小写/分隔符变体视为同一路径）→
        // 去重并刷新到最前 + 元数据更新，不累积
        if cfg!(windows) {
            reg.record_with_cap(Path::new(r"\\?\D:\F3.MP4"), "r1", "t3-new", 2000, 4);
            assert_eq!(reg.entries.len(), 4, "归一去重不累积");
            assert_eq!(reg.entries[0].path, r"\\?\D:\F3.MP4");
            assert_eq!(reg.entries[0].task_id, "t3-new");
            assert_eq!(reg.entries[0].ts, 2000);
        }
        // 默认 record 上限接线：小量登记不触发淘汰
        let mut small = ProcessedRegistry::default();
        small.record(Path::new("/d/x.mp4"), "r", "t", 1);
        assert_eq!(small.entries.len(), 1);
        assert_eq!(PROCESSED_MAX_ENTRIES, 10_000, "上限契约冻结");
    }

    // 19. 登记表持久化：save/load 往返；加载时清理已不存在路径；损坏降级
    //     为空表 + 告警
    #[test]
    fn processed_registry_roundtrip_prunes_missing_and_corrupt_degrades() {
        let dir = temp_root("roundtrip");
        let path = dir.join("processed_registry.json");
        let existing = dir.join("kept.mp4");
        std::fs::write(&existing, b"x").unwrap();

        let mut reg = ProcessedRegistry::default();
        reg.record(&existing, "r1", "t1", 100);
        reg.record(Path::new("/definitely/ghost/gone.mp4"), "r1", "t2", 101);
        reg.save(&path).unwrap();

        // 直接 load：两条都在
        let loaded = ProcessedRegistry::load(&path);
        assert_eq!(loaded.entries.len(), 2);
        // load + prune（load_processed_state 同款）：死路径被清理
        let mut pruned = ProcessedRegistry::load(&path);
        pruned.prune_missing();
        assert_eq!(pruned.entries.len(), 1);
        assert!(pruned.contains_path(&existing));
        assert!(!pruned.contains_path(Path::new("/definitely/ghost/gone.mp4")));

        // 损坏 → 空表 + 告警（不 panic）
        std::fs::write(&path, "{not json").unwrap();
        let broken = ProcessedRegistry::load(&path);
        assert!(broken.entries.is_empty(), "损坏按空表启动");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 20. 零死循环 E2E（真实 FS）：watch == 产物目录场景，登记的产物永不
    //     触发（不入在途表、不阻塞水位线）；用户手写新文件照常触发
    #[test]
    fn watch_dir_equals_dest_products_never_trigger_user_files_still_do() {
        let _guard = processed_test_lock();
        let watch = temp_root("e2e-watch");
        // 登记表落盘路径放在 watch 目录之外（避免登记文件本身成为扫描候选）
        let disk_dir = temp_root("e2e-disk");
        let disk = disk_dir.join("processed_registry.json");
        processed_reset_for_tests(Some(disk));

        let src = watch.join("a.mkv");
        std::fs::write(&src, b"video").unwrap();
        set_file_mtime_age(&src, 60);

        let mut reg = rule_with(|r| {
            r.watch_dir = watch.to_string_lossy().into_owned();
            r.stability_secs = 5;
            r.checkpoint = 0;
        });
        reg.rules.get_mut("r1").unwrap().checkpoint = now_epoch() - 100;

        // r1：a.mkv 入表（不触发）
        let (hits, next) = collect_watch_events(&reg, now_epoch());
        assert!(hits.is_empty(), "首轮仅入表");
        assert!(
            next.rules["r1"]
                .in_flight
                .contains_key(src.to_string_lossy().as_ref())
        );
        // r2：a.mkv 稳定触发，水位线推进到其 mtime
        let (hits, next) = collect_watch_events(&next, now_epoch());
        assert_eq!(hits.len(), 1, "触发文件：{:?}", hits);
        assert_eq!(hits[0].path, src.to_string_lossy().as_ref());

        // 模拟 daemon 产物落盘（watch == dest：产物落回监控目录）并登记
        let product = watch.join("a.done.mp4");
        std::fs::write(&product, b"result").unwrap();
        set_file_mtime_age(&product, 2);
        record_processed_product(&product, "r1", "task-e2e");
        // 登记表已落盘且包含产物
        let saved = ProcessedRegistry::load(&disk_dir.join("processed_registry.json"));
        assert!(saved.contains_path(&product), "登记必须即时落盘");

        // 用户手写新文件
        let manual = watch.join("b.txt");
        std::fs::write(&manual, b"note").unwrap();
        set_file_mtime_age(&manual, 50);

        // r3：产物被排除（不得入表）；b.txt 正常入表
        let (hits, next) = collect_watch_events(&next, now_epoch());
        assert!(hits.is_empty());
        let in_flight = &next.rules["r1"].in_flight;
        assert!(
            !in_flight.contains_key(product.to_string_lossy().as_ref()),
            "登记产物不得进入在途表"
        );
        assert!(
            in_flight.contains_key(manual.to_string_lossy().as_ref()),
            "用户手写文件照常入表"
        );
        // r4：b.txt 触发，产物仍未触发
        let (hits, next) = collect_watch_events(&next, now_epoch());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, manual.to_string_lossy().as_ref());
        // r5：多轮之后产物永不触发（不死循环）
        let (hits, _) = collect_watch_events(&next, now_epoch());
        assert!(
            hits.is_empty(),
            "产物路径永不触发（不死循环）：{:?}",
            hits
        );

        // 收尾：重置全局态，避免泄漏到其它测试
        processed_reset_for_tests(None);
        let _ = std::fs::remove_dir_all(&watch);
        let _ = std::fs::remove_dir_all(&disk_dir);
    }

    // 21. 全局态：经 load_processed_state 模拟重启 → 登记从落盘文件恢复；
    //     恢复时同步清理已消失路径
    #[test]
    fn processed_state_survives_restart_and_prunes_on_reload() {
        let _guard = processed_test_lock();
        let dir = temp_root("restart");
        let disk = dir.join("processed_registry.json");
        processed_reset_for_tests(Some(disk.clone()));

        let kept = dir.join("kept.mp4");
        std::fs::write(&kept, b"x").unwrap();
        record_processed_product(&kept, "r1", "t1");
        record_processed_product(Path::new("/ghost/gone.mp4"), "r1", "t2");

        // 模拟重启：从落盘文件重建状态（不经全局缓存）
        let rebooted = load_processed_state(Some(&disk));
        assert!(rebooted.registry.contains_path(&kept), "重启后登记仍在");
        assert!(
            !rebooted
                .registry
                .contains_path(Path::new("/ghost/gone.mp4")),
            "重启加载时清理已消失路径"
        );
        // 修剪结果已回写落盘
        let on_disk = ProcessedRegistry::load(&disk);
        assert_eq!(on_disk.entries.len(), 1, "修剪后落盘不积累死路径");

        processed_reset_for_tests(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
