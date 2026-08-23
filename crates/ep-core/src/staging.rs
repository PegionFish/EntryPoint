//! 管线中间产物暂存（Staging）——任务级 RAM 盘生命周期管理
//!
//! ## 问题
//!
//! 单模块 ad-hoc 任务在内存/VRAM 中完成推理后输出文件即可；但管线级业务
//! （如 抽帧 → 逐帧超分 → 回封）的节点间产物是海量中间数据——25min 1080p
//! 视频抽帧约 36k 张 PNG ≈ 数十 GB，全量落在物理盘/NAS 上既慢又磨损介质，
//! 且各模块「自扫门前雪」式清理缺乏系统级兜底：进程崩溃即泄漏孤儿目录。
//!
//! ## 方案：双区布局 + 系统托管生命周期
//!
//! ```text
//! runtime/staging/<task_id>/        ← 易失区：优先 tmpfs(RAM)，节点中间产物与
//!                                     adapter 帧序列驻留于此，任务终态即清算
//! workspace/tasks/<task_id>/files/  ← 持久区：终态归集产物，供下载服务
//! ```
//!
//! 平台契约不变——节点产物仍以**文件路径**传递（MODULE_SPEC §5），只是路径
//! 落位由系统按预算决策。执行器向模块请求注入 `output_path`/`staging_dir`
//! 时指向暂存区；归集步骤把终产物拷入持久 `files/`；任务终态由 daemon 统一
//! 清算易失区，启动时再全量清扫孤儿。
//!
//! ## 落位策略（v1：准入时决策，不做运行中迁移）
//!
//! - `mode = "auto"`（缺省）：Linux 探测到可用 tmpfs（`/dev/shm`，可经配置
//!   覆盖）→ 用之；否则整层退化为盘上目录（Windows 无原生 tmpfs，恒走此路）
//! - 准入检查：tmpfs 可用空间 ≥ `staging_floor_mb` 才接纳，否则该任务落盘
//!   （宁可慢不可 OOM——tmpfs 耗尽会触发交换或分配失败，危害全机）
//! - v1 不做字节级精确配额与热迁移：空间压力以准入水位线兜底，诚实记录

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

/// 单次分配结果：落位路径 + 是否 RAM 驻留
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedDir {
    pub path: PathBuf,
    pub ram_backed: bool,
}

/// 暂存管理器：任务级分配 / 清算 / 孤儿清扫。
///
/// 线程安全（内部 Mutex 仅护 active 表）；`free_bytes_probe` 可注入以便测试。
pub struct StagingManager {
    mode: StagingMode,
    /// 盘上回退根（恒可用）：`<workspace>/staging`
    disk_root: PathBuf,
    /// tmpfs 候选根（探测成功才有值）
    tmpfs_root: Option<PathBuf>,
    /// tmpfs 准入水位（字节）
    floor_bytes: u64,
    /// 活跃分配表（path → 任务 id），用于孤儿清扫的对账白名单
    active: Mutex<HashMap<PathBuf, String>>,
    /// 可用空间探针（生产 = statvfs(tmpfs_root)；测试可注入假值）
    free_bytes_probe: FreeBytesProbe,
}

/// 可用空间探针类型（生产 statvfs / 测试假值注入共用）
pub type FreeBytesProbe = Box<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

#[cfg(target_os = "linux")]
fn statvfs_to_u64(v: u64) -> Option<u64> {
    Some(v)
}
#[cfg(not(target_os = "linux"))]
fn statvfs_to_u64(v: libc::fsblkcnt_t) -> Option<u64> {
    u64::try_from(v).ok()
}

impl StagingManager {
    /// 生产构造：按模式解析 tmpfs 候选（Linux `/proc/self/mounts` 判定 +
    /// 试建校验），盘上回退根即时创建。
    pub fn new(mode: StagingMode, root_override: Option<&str>, workspace: &Path, floor_mb: u64) -> Self {
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
        Self {
            mode,
            disk_root,
            tmpfs_root,
            floor_bytes: floor_mb.saturating_mul(1024 * 1024),
            active: Mutex::new(HashMap::new()),
            free_bytes_probe: Box::new(free_bytes_statvfs),
        }
    }

    /// tmpfs 是否已就绪（观测/日志用）
    pub fn ram_backed(&self) -> bool {
        self.tmpfs_root.is_some()
    }

    /// tmpfs 根路径（观测/API 用）
    pub fn ram_root(&self) -> Option<&Path> {
        self.tmpfs_root.as_deref()
    }

    /// 为任务分配暂存目录：tmpfs 准入通过则驻留 RAM，否则落盘回退。
    /// 幂等：同 task_id 重复分配返回既有条目（重试语义安全）。
    pub fn alloc_task(&self, task_id: &str) -> StagedDir {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((path, _)) = active.iter().find(|(_, v)| v.as_str() == task_id) {
            let path = path.clone();
            let ram_backed = self.tmpfs_root.as_ref().is_some_and(|r| path.starts_with(r));
            return StagedDir { path, ram_backed };
        }

        // 落位决策：tmpfs 可用 && 水位达标 → RAM；否则盘上回退
        let use_ram = self
            .tmpfs_root
            .as_ref()
            .map(|root| (self.free_bytes_probe)(root).is_some_and(|free| free >= self.floor_bytes))
            .unwrap_or(false);

        let (base, ram_backed) = match (self.mode, use_ram) {
            (StagingMode::Disk, _) => (&self.disk_root, false),
            (_, true) => (self.tmpfs_root.as_ref().unwrap(), true),
            (_, false) => (&self.disk_root, false),
        };

        let path = base.join(sanitize_task_id(task_id));
        if let Err(e) = std::fs::create_dir_all(&path) {
            // tmpfs 建目录失败（满/权限）→ 就地降级盘上
            tracing::warn!(task_id, error = %e, "staging tmpfs 分配失败，回退盘上");
            let p = self.disk_root.join(sanitize_task_id(task_id));
            let _ = std::fs::create_dir_all(&p);
            active.insert(p.clone(), task_id.to_string());
            return StagedDir { path: p, ram_backed: false };
        }
        active.insert(path.clone(), task_id.to_string());
        tracing::debug!(task_id, dir = %path.display(), ram_backed, "staging allocated");
        StagedDir { path, ram_backed }
    }

    /// 清算任务暂存目录（终态调用）。幂等；目录不存在视为成功。
    pub fn free_task(&self, task_id: &str) -> std::io::Result<()> {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        let owned: Vec<PathBuf> = active
            .iter()
            .filter(|(_, v)| v.as_str() == task_id)
            .map(|(k, _)| k.clone())
            .collect();
        for path in &owned {
            let r = std::fs::remove_dir_all(path);
            if let Err(e) = &r {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(task_id, error = %e, "staging 清算失败");
                    continue;
                }
            }
            active.remove(path);
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
    fn set_free_bytes_probe(&mut self, probe: FreeBytesProbe) {
        self.free_bytes_probe = probe;
    }
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

/// statvfs 可用空间（字节；f_bavail = 非特权进程可用），探测失败返回 None。
/// Windows 无此 API → 恒 None（tmpfs 候选本就不在 Windows 探测，双保险）。
#[cfg(unix)]
fn free_bytes_statvfs(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut vfs) };
    if rc != 0 {
        return None;
    }
    // f_bavail/f_frsize 跨平台符号/宽度不一，统一饱和换算
    let avail = statvfs_to_u64(vfs.f_bavail)?;
    let frsize = statvfs_to_u64(vfs.f_frsize)?;
    Some(avail.saturating_mul(frsize))
}

#[cfg(not(unix))]
fn free_bytes_statvfs(_path: &Path) -> Option<u64> {
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

    fn mgr(mode: StagingMode) -> StagingManager {
        let ws = unique_ws(mode_debug(&mode));
        StagingManager::new(mode, None, &ws, 1)
    }

    fn mode_debug(m: &StagingMode) -> &'static str {
        match m {
            StagingMode::Auto => "auto",
            StagingMode::Tmpfs => "tmpfs",
            StagingMode::Disk => "disk",
        }
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

    #[test]
    fn admission_floor_spills_to_disk_when_below_watermark() {
        let ws = unique_ws("admission");
        let mut m = StagingManager::new(StagingMode::Auto, None, &ws, 100);
        // 注入假探针：tmpfs 只剩 10MB < floor(100MB) → 必须落盘
        m.set_free_bytes_probe(Box::new(|_| Some(10 * 1024 * 1024)));
        // 强制给出 tmpfs 候选（绕过沙箱探测）
        let shm_dir = unique_ws("shm-fake");
        m.tmpfs_root = Some(shm_dir.clone());

        let d = m.alloc_task("t-low-mem");
        assert!(!d.ram_backed, "水位不足必须回退盘上");

        // 水位充足 → RAM 驻留
        m.set_free_bytes_probe(Box::new(|_| Some(u64::MAX)));
        let d2 = m.alloc_task("t-ok");
        assert!(d2.ram_backed);
        assert!(d2.path.starts_with(&shm_dir));
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
