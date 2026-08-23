//! 管线中间产物暂存（Staging）——任务级 RAM 盘生命周期管理
//!
//! ## 问题
//!
//! 单模块 ad-hoc 任务在内存/VRAM 中完成推理后输出文件即可；但管线级业务
//! （如 抽帧 → 逐帧超分 → 回封）的节点间产物是海量中间数据——25min 1080p
//! 视频抽帧约 36k 张 PNG ≈ 数十 GB，全量落在物理盘/NAS 上既慢又磨损介质，
//! 且各模块「自扫门前雪」式清理缺乏系统级兜底：进程崩溃即泄漏孤儿目录。
//!
//! ## 方案：双区布局 + 系统托管生命周期 + 受限内存三保护
//!
//! ```text
//! runtime/staging|/dev/shm/ep-staging/<task_id>/  ← 易失区：优先 tmpfs(RAM)
//! workspace/tasks/<task_id>/files/                ← 持久区：终态归集产物
//! ```
//!
//! 平台契约不变——节点产物仍以**文件路径**传递（MODULE_SPEC §5），只是路径
//! 落位由系统按预算决策。执行器向模块请求注入 `output_path`/`staging_dir`
//! 时指向暂存区；归集步骤把终产物拷入持久 `files/`；任务终态由 daemon 统一
//! 清算易失区，启动时再全量清扫孤儿。
//!
//! ## 内存受限设备的三层保护（v2）
//!
//! 大内存机器上「水位线准入」即可安心运行，但受限设备必须回答三个问题：
//!
//! 1. **预算从哪来**：`budget = min(staging_max_ram_mb, tmpfs总容量)`；
//!    配置缺省 0 = 自动取 tmpfs 容量的 25%（下限 256MB）。8GB 小机的
//!    /dev/shm 通常 4GB → 预算自动只有 ~1GB，不会误判自己很富裕。
//! 2. **怎么防超卖**：预留制。提交时按输入体积 × 扩张系数估算任务足迹
//!    （视频→PNG 帧序列典型 20~30×），预算内才接纳 RAM 落位，否则一开始
//!    就落盘——绝不启动一个注定中途撑爆内存的任务。
//! 3. **失控怎么办**：不做运行中熔断。部署者应对所部署模型/负载的资源
//!    需求有基本预期；设备真不足时 OOM Kill 是诚实反馈——为此增加常驻
//!    看门狗与任务熔断的复杂度不值得（设计取舍，2026-08 产品裁决）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 暂存模式（`[pipeline].staging_mode`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagingMode {
    /// Linux 优先 tmpfs、失败退化盘上目录（缺省）
    Auto,
    /// 强制尝试 tmpfs（探测失败时回退盘上目录并告警）
    Tmpfs,
    /// 禁用内存驻留，全部走盘上目录
    Disk,
}

impl StagingMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "tmpfs" => Self::Tmpfs,
            "disk" => Self::Disk,
            _ => Self::Auto,
        }
    }
}

/// 空间探针类型（生产 statvfs / 测试假值注入共用）
pub type SpaceProbe = Box<dyn Fn(&Path) -> Option<(u64, u64)> + Send + Sync>;

/// 单次分配结果：落位路径 + 是否 RAM 驻留
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedDir {
    pub path: PathBuf,
    pub ram_backed: bool,
}

/// 活跃分配条目
#[derive(Debug, Clone)]
struct ActiveEntry {
    task_id: String,
    ram_backed: bool,
}

/// 观测快照（API/日志用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingStats {
    /// 活跃任务数
    pub active_tasks: usize,
    /// 其中 RAM 驻留数
    pub ram_tasks: usize,
    /// 暂存预算（字节）
    pub budget_bytes: u64,
}

/// 暂存管理器：任务级分配 / 清算 / 孤儿清扫 / 压力熔断。
///
/// 线程安全（内部 Mutex 仅护 active 表）；空间探针可注入以便测试。
pub struct StagingManager {
    /// 盘上回退根（恒可用）：`<workspace>/staging`
    disk_root: PathBuf,
    /// tmpfs 候选根（探测成功才有值）
    tmpfs_root: Option<PathBuf>,
    /// tmpfs 准入水位（字节）：低于此新任务直接落盘
    floor_bytes: u64,
    /// 显式配置的暂存预算上限（MB；0 = 自动）。预算惰性解析：每次分配时
    /// 按当前探针现算 = min(配置, tmpfs×75%)，自动口径取容量 25%（下限
    /// 256MB）——内存受限设备的核心保护。
    max_ram_mb: u64,
    /// 活跃分配表（path → 条目），孤儿清扫白名单 + 预算对账
    active: Mutex<HashMap<PathBuf, ActiveEntry>>,
    /// 空间探针：(total_bytes, free_bytes)；生产 = statvfs
    space_probe: SpaceProbe,
}

impl StagingManager {
    /// 生产构造：按模式解析 tmpfs 候选（Linux `/proc/self/mounts` 判定 +
    /// 试建校验），解析预算并即时创建盘上回退根。
    pub fn new(
        mode: StagingMode,
        root_override: Option<&str>,
        workspace: &Path,
        floor_mb: u64,
        max_ram_mb: u64,
    ) -> Self {
        let disk_root = workspace.join("staging");
        let _ = std::fs::create_dir_all(&disk_root);
        let tmpfs_root = match mode {
            StagingMode::Disk => None,
            StagingMode::Auto | StagingMode::Tmpfs => {
                detect_tmpfs_candidate(root_override).or_else(|| {
                    if mode == StagingMode::Tmpfs {
                        tracing::warn!(
                            "staging_mode=tmpfs 但未探测到可用 tmpfs，回退盘上目录 {}",
                            disk_root.display()
                        );
                    }
                    None
                })
            }
        };
        let space_probe: SpaceProbe = Box::new(statvfs_space);
        let budget_mb =
            resolve_budget(&space_probe, tmpfs_root.as_deref(), max_ram_mb) / (1024 * 1024);
        tracing::info!(
            budget_mb,
            ram_backed = tmpfs_root.is_some(),
            "staging 初始化"
        );
        Self {
            disk_root,
            tmpfs_root,
            floor_bytes: floor_mb.saturating_mul(1024 * 1024),
            max_ram_mb,
            active: Mutex::new(HashMap::new()),
            space_probe,
        }
    }

    /// 当前有效预算（字节）：惰性解析，跟随探针现值
    fn current_budget(&self) -> u64 {
        resolve_budget(&self.space_probe, self.tmpfs_root.as_deref(), self.max_ram_mb)
    }

    /// tmpfs 是否已就绪（观测/日志用）
    pub fn ram_backed(&self) -> bool {
        self.tmpfs_root.is_some()
    }

    /// tmpfs 根路径（观测/API 用）
    pub fn ram_root(&self) -> Option<&Path> {
        self.tmpfs_root.as_deref()
    }

    /// 当前观测快照
    pub fn stats(&self) -> StagingStats {
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        StagingStats {
            active_tasks: active.len(),
            ram_tasks: active.values().filter(|e| e.ram_backed).count(),
            budget_bytes: self.current_budget(),
        }
    }

    /// 为任务分配暂存目录。
    ///
    /// RAM 落位条件（准入时一次决策，不做运行中迁移）：tmpfs 可用 &&
    /// 空闲 ≥ 准入水位 && 暂存根实测占用 ≤ 预算。任一不满足即整任务落盘
    /// 回退。设计取舍：受限设备不做运行中熔断——真不足时 OOM Kill 即诚实
    /// 反馈（部署者应对模型需求有预期），不为极端场景增加常驻复杂度。
    /// 幂等：同 task_id 重复分配返回既有条目（重试语义安全）。
    pub fn alloc_task(&self, task_id: &str) -> StagedDir {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((path, entry)) = active.iter().find(|(_, e)| e.task_id == task_id) {
            return StagedDir {
                path: path.clone(),
                ram_backed: entry.ram_backed,
            };
        }

        let (use_ram, reason) = match self.tmpfs_root.as_ref() {
            None => (false, "no-tmpfs"),
            Some(root) => match (self.space_probe)(root) {
                Some((_total, free)) if free < self.floor_bytes => (false, "below-floor"),
                Some(_) => {
                    let staged = measure_dir(root);
                    if staged > self.current_budget() {
                        (false, "over-budget")
                    } else {
                        (true, "admitted")
                    }
                }
                None => (false, "probe-failed"),
            },
        };
        if !use_ram {
            tracing::debug!(task_id, reason, "staging 落盘回退");
        }

        let base: &Path = if use_ram {
            self.tmpfs_root.as_deref().expect("checked above")
        } else {
            &self.disk_root
        };

        let path = base.join(sanitize_task_id(task_id));
        if let Err(e) = std::fs::create_dir_all(&path) {
            // 建目录失败（满/权限）→ 就地降级盘上
            tracing::warn!(task_id, error = %e, "staging 目录创建失败，回退盘上");
            let p = self.disk_root.join(sanitize_task_id(task_id));
            let _ = std::fs::create_dir_all(&p);
            active.insert(
                p.clone(),
                ActiveEntry { task_id: task_id.to_string(), ram_backed: false },
            );
            return StagedDir { path: p, ram_backed: false };
        }
        active.insert(
            path.clone(),
            ActiveEntry { task_id: task_id.to_string(), ram_backed: use_ram },
        );
        tracing::debug!(task_id, dir = %path.display(), ram_backed = use_ram, "staging allocated");
        StagedDir { path, ram_backed: use_ram }
    }

    /// 清算任务暂存目录（终态调用）。幂等；目录不存在视为成功。
    pub fn free_task(&self, task_id: &str) -> std::io::Result<()> {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        let owned: Vec<PathBuf> = active
            .iter()
            .filter(|(_, e)| e.task_id == task_id)
            .map(|(k, _)| k.clone())
            .collect();
        for path in owned {
            let r = std::fs::remove_dir_all(&path);
            if let Err(e) = &r {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(task_id, error = %e, "staging 清算失败");
                    continue;
                }
            }
            active.remove(&path);
        }
        Ok(())
    }

    /// 启动清扫：daemon 启动期不存在任何活跃任务，暂存根下的一切皆为
    /// 上次进程的遗留（崩溃/被杀产生的孤儿）→ 全量删除。
    pub fn sweep_orphans(&self) {
        for root in [self.tmpfs_root.as_deref(), Some(self.disk_root.as_path())] {
            let Some(root) = root else { continue };
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            let mut n = 0u32;
            for entry in entries.flatten() {
                let p = entry.path();
                // 白名单对账（防御性：正常情况下启动期 active 必为空）
                if self
                    .active
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains_key(&p)
                {
                    continue;
                }
                if std::fs::remove_dir_all(&p).is_ok() {
                    n += 1;
                }
            }
            if n > 0 {
                tracing::info!(count = n, root = %root.display(), "已清扫 staging 孤儿目录");
            }
        }
    }


    #[cfg(test)]
    fn set_space_probe(&mut self, probe: SpaceProbe) {
        self.space_probe = probe;
    }


    #[cfg(test)]
    fn set_tmpfs_root(&mut self, root: PathBuf) {
        self.tmpfs_root = Some(root);
    }
}

/// 预算解析：显式配置优先（与 tmpfs 总容量取小），0 = 自动 25% 容量（下限 256MB）
/// 预算解析：
/// - 显式配置 `max_ram_mb > 0` → min(配置, tmpfs×75% 硬顶)
/// - 缺省 0 = 自动取 tmpfs 总容量的 25%（下限 256MB）——内存受限设备
///   的核心保护：8GB 小机 /dev/shm=4GB 时预算自动只有 ~1GB
fn resolve_budget(probe: &SpaceProbe, root: Option<&Path>, max_ram_mb: u64) -> u64 {
    let total = root.and_then(probe.as_ref()).map(|(t, _)| t).unwrap_or(0);
    let hard_cap = total.saturating_mul(3) / 4; // 绝不计划吃满 tmpfs
    if max_ram_mb > 0 {
        let want = max_ram_mb.saturating_mul(1024 * 1024);
        return if hard_cap > 0 { want.min(hard_cap) } else { want };
    }
    (total / 4).max(256 * 1024 * 1024).min(hard_cap.max(256 * 1024 * 1024))
}

/// 目录实测占用（du 语义；不可读项跳过）。看门狗计量与清算遥测共用。
pub fn measure_dir(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    let mut sum = 0u64;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            sum += measure_dir(&p);
        } else if let Ok(md) = entry.metadata() {
            sum += md.len();
        }
    }
    sum
}

/// task_id → 目录名净化（防路径穿越；正常 task-id 本就安全，第三方输入兜底）
fn sanitize_task_id(task_id: &str) -> String {
    let safe: String = task_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() { "unnamed".into() } else { safe }
}

/// Linux tmpfs 候选探测：显式覆盖 > `/dev/shm`。判定依据 `/proc/self/mounts`
/// 中该路径挂载的文件系统类型为 `tmpfs` 且试建可写。
fn detect_tmpfs_candidate(override_root: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = override_root.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(p);
        return ensure_writable_dir(path.clone()).then_some(path);
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let is_tmpfs = mounts.lines().any(|line| {
        // 格式：<dev> <mountpoint> <fstype> <opts…>
        let mut it = line.split_whitespace();
        let _dev = it.next();
        let mnt = it.next();
        let fstype = it.next();
        mnt == Some("/dev/shm") && fstype == Some("tmpfs")
    });
    if !is_tmpfs {
        return None;
    }
    let path = PathBuf::from("/dev/shm/ep-staging");
    ensure_writable_dir(path.clone()).then_some(path)
}

fn ensure_writable_dir(path: PathBuf) -> bool {
    if std::fs::create_dir_all(&path).is_err() {
        return false;
    }
    std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false)
}

/// statvfs 空间探测：(总容量, 可用) 字节对；f_bavail = 非特权进程可用。
/// Windows 无此 API → 恒 None（tmpfs 候选本就不在 Windows 探测，双保险）。
/// statvfs 计数字段 → u64（Linux glibc 本就是 u64，直接恒等；其余平台 try_from）
#[cfg(target_os = "linux")]
fn statvfs_u64(v: u64) -> Option<u64> {
    Some(v)
}
#[cfg(all(unix, not(target_os = "linux")))]
fn statvfs_u64(v: libc::fsblkcnt_t) -> Option<u64> {
    u64::try_from(v).ok()
}

#[cfg(unix)]
fn statvfs_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut vfs) };
    if rc != 0 {
        return None;
    }
    // f_blocks/f_bavail/f_frsize 跨平台符号/宽度不一，统一饱和换算
    let frsize = statvfs_u64(vfs.f_frsize)?;
    let blocks = statvfs_u64(vfs.f_blocks)?;
    let bavail = statvfs_u64(vfs.f_bavail)?;
    Some((blocks.saturating_mul(frsize), bavail.saturating_mul(frsize)))
}

#[cfg(not(unix))]
fn statvfs_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_ws(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ep-staging-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mode_debug(m: &StagingMode) -> &'static str {
        match m {
            StagingMode::Auto => "auto",
            StagingMode::Tmpfs => "tmpfs",
            StagingMode::Disk => "disk",
        }
    }

    fn mgr(mode: StagingMode) -> StagingManager {
        let ws = unique_ws(mode_debug(&mode));
        StagingManager::new(mode, None, &ws, 1, 0)
    }

    #[test]
    fn parse_mode_variants() {
        assert_eq!(StagingMode::parse("auto"), StagingMode::Auto);
        assert_eq!(StagingMode::parse(" TMPFS "), StagingMode::Tmpfs);
        assert_eq!(StagingMode::parse("disk"), StagingMode::Disk);
        assert_eq!(StagingMode::parse("nonsense"), StagingMode::Auto);
    }

    #[test]
    fn disk_mode_never_rams() {
        let m = mgr(StagingMode::Disk);
        let d = m.alloc_task("task-x-0001");
        let root = m.disk_root.clone();
        assert!(!d.ram_backed);
        assert!(d.path.starts_with(&root));
        assert!(d.path.is_dir());
        m.free_task("task-x-0001").unwrap();
        assert!(!d.path.exists(), "清算应删除目录");
    }

    #[test]
    fn auto_mode_falls_back_to_disk_without_shm_in_sandbox() {
        // CI/沙箱环境通常无 /dev/shm 挂载记录 → Auto 应优雅落盘不 panic
        let m = mgr(StagingMode::Auto);
        let root = m.disk_root.clone();
        let d = m.alloc_task("t1");
        if m.ram_backed() {
            assert!(d.path.starts_with("/dev/shm"));
            m.free_task("t1").unwrap();
        } else {
            assert!(d.path.starts_with(&root));
            m.free_task("t1").unwrap();
        }
    }

    fn constrained_mgr(total_mb: u64, free_mb: u64, floor_mb: u64, max_ram_mb: u64) -> (StagingManager, PathBuf) {
        let ws = unique_ws("constrained");
        let shm = unique_ws("shm-fake");
        let shm_path = shm.clone();
        let mut m = StagingManager::new(
            StagingMode::Auto,
            None,
            &ws,
            floor_mb,
            max_ram_mb,
        );
        m.set_tmpfs_root(shm.clone());
        let p = shm_path.clone();
        m.set_space_probe(Box::new(move |_| Some((total_mb * 1024 * 1024, free_mb * 1024 * 1024))));
        let _ = p;
        (m, shm)
    }

    #[test]
    fn admission_floor_spills_to_disk_when_below_watermark() {
        let (m, _shm) = constrained_mgr(4096, 10, 100, 0); // 空闲 10MB < 水位 100MB
        let d = m.alloc_task("t-low-mem");
        assert!(!d.ram_backed, "水位不足必须回退盘上");

        let (m2, shm2) = constrained_mgr(4096, 4096, 100, 0);
        let d2 = m2.alloc_task("t-ok");
        assert!(d2.ram_backed);
        assert!(d2.path.starts_with(&shm2));
    }

    #[test]
    fn budget_derivation_auto_quarter_of_capacity() {
        // 8GB 小机 /dev/shm=4GB：auto 预算应为容量的 25% = 1GB
        let (m, _) = constrained_mgr(4096, 4096, 1, 0);
        assert_eq!(m.stats().budget_bytes, 1024 * 1024 * 1024);
        // 显式配置 512MB → min(配置, 容量) = 512MB
        let (m2, _) = constrained_mgr(4096, 4096, 1, 512);
        assert_eq!(m2.stats().budget_bytes, 512 * 1024 * 1024);
        // 显式配置超硬顶（tmpfs×75%）→ 截到硬顶
        let (m3, _) = constrained_mgr(1024, 1024, 1, 8192);
        assert_eq!(m3.stats().budget_bytes, 768 * 1024 * 1024);
    }

    #[test]
    fn alloc_signature_without_reserve_still_admits() {
        let (m, shm) = constrained_mgr(1024, 1024, 64, 256);
        let a = m.alloc_task("t-a");
        assert!(a.ram_backed);
        assert!(a.path.starts_with(&shm));
        m.free_task("t-a").unwrap();
        assert!(!a.path.exists());
    }

    #[test]
    fn alloc_is_idempotent_per_task_id() {
        let m = mgr(StagingMode::Disk);
        let a = m.alloc_task("t-dup");
        let b = m.alloc_task("t-dup");
        assert_eq!(a.path, b.path);
        m.free_task("t-dup").unwrap();
        assert!(!a.path.exists());
    }

    #[test]
    fn sweep_removes_unowned_entries_but_keeps_active() {
        let m = mgr(StagingMode::Disk);
        let keep = m.alloc_task("t-alive");
        let orphan = m.disk_root.join("task-dead-9999");
        std::fs::create_dir_all(&orphan).unwrap();
        m.sweep_orphans();
        assert!(keep.path.exists(), "活跃条目不得误删");
        assert!(!orphan.exists(), "孤儿必须被清算");
    }

    #[test]
    fn sanitize_blocks_traversal() {
        // 路径分隔符与点号全部滤除，无法构造穿越
        assert_eq!(sanitize_task_id("../../etc"), "etc");
        assert_eq!(sanitize_task_id("a/b\\c d"), "abcd");
        assert_eq!(sanitize_task_id("///"), "unnamed");
    }
}
