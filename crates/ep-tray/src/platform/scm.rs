//! Windows 服务控制管理器（SCM）薄封装（GT-3 双模式管控的读/写面）。
//!
//! 职责：把 QueryServiceStatusEx / QueryServiceConfig(2) / StartService /
//! ControlService / ChangeServiceConfig(2) 收敛成一个可注入的
//! [`ServiceControl`] trait（真实实现 [`WindowsScm`]，测试可注入假实现），
//! 全部错误归一为结构化 [`ScmError`]——**UI 据此区分「需提权」与普通失败**。
//!
//! 权限模型（红线：主进程绝不提权）：
//! - 读路径（[`ServiceControl::query`]）：`SC_MANAGER_CONNECT` +
//!   `SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG`，**标准用户可用**——模式
//!   检测线程与提权动作完成后的即时复检都走这里，无 UAC；
//! - 写路径（start/stop/change_start_type/set_recovery_restart）：非提权
//!   调用会得到 `ERROR_ACCESS_DENIED` → [`ScmError::AccessDenied`] → UI 提示
//!   需提权；实际写操作经 [`super::elevate`] 自提权一次性子进程执行
//!   （`--sc-action`，见 elevate.rs）。
//!
//! 服务名单一事实源：[`SERVICE_NAME`] = `"EntryPoint"`，与
//! `scripts/install-service.ps1:19`（`$ServiceName = "EntryPoint"`）互引——
//! 改名必须两处同步（本仓服务端安装脚本与 GUI 管控的唯一约定锚点）。

use std::fmt;
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_SERVICE_ALREADY_RUNNING,
    ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE, ERROR_SERVICE_REQUEST_TIMEOUT,
    GetLastError,
};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfig2W,
    QueryServiceConfigW, QueryServiceStatusEx, SC_ACTION, SC_ACTION_RESTART, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_AUTO_START,
    SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONTROL_STOP,
    SERVICE_DEMAND_START, SERVICE_DISABLED, SERVICE_FAILURE_ACTIONSW, SERVICE_NO_CHANGE,
    SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP,
    SERVICE_STOP_PENDING, SERVICE_STOPPED, StartServiceW,
};

// ─── 常量（单一事实源互引） ─────────────────────────────────────────────────

/// Windows 服务名。**单一事实源**：与 `scripts/install-service.ps1:19`
/// （`$ServiceName = "EntryPoint"`，缺省值）互引；binPath =
/// `<BinDir>\ep-daemon.exe --service`、start= auto、账户 LocalSystem/
/// NetworkService、**不传 root**（root 由 ep-core `config::resolve_root`
/// 从 exe 位置解析，crates/ep-core/src/config.rs:23）。
pub const SERVICE_NAME: &str = "EntryPoint";

/// SCM 停止等待上限（restart = stop → 轮询 STOPPED → start；SCM stop 异步，
/// 超时视为失败而非挂死——提权子进程内执行，不阻塞 UI）
const STOP_WAIT: Duration = Duration::from_secs(20);
/// restart 停止轮询步进
const STOP_POLL: Duration = Duration::from_millis(250);

// ─── 错误与快照类型 ─────────────────────────────────────────────────────────

/// SCM 操作错误（结构化——UI 按 [`ScmError::AccessDenied`] 提示需提权）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScmError {
    /// 服务未安装（模式检测据此判 sidecar）
    NotFound,
    /// 拒绝访问（需要管理员权限；UI 提示「需提权」，经 elevate.rs 重试）
    AccessDenied,
    /// 其余 Win32 错误码（原样透传，UI 显示码值）
    Os(u32),
}

impl fmt::Display for ScmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "服务未安装"),
            Self::AccessDenied => write!(f, "拒绝访问（需要管理员权限）"),
            Self::Os(code) => write!(f, "Windows 错误 {code}"),
        }
    }
}

/// 服务运行状态（SERVICE_STATUS_PROCESS.dwCurrentState 归一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    /// Paused / ContinuePending / PausePending 归并（daemon 服务无暂停语义，
    /// 仅为枚举完整性；管控按钮按不可用处理）
    Paused,
}

impl ServiceState {
    /// 中文显示（管控页/状态页直接用）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "已停止",
            Self::StartPending => "启动中…",
            Self::StopPending => "正在停止…",
            Self::Running => "运行中",
            Self::Paused => "已暂停",
        }
    }
}

/// 服务启动类型（UI 可设置的三档；自启面统一：sidecar=HKCU Run ∥ Service=StartType）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartType {
    /// 自动（SERVICE_AUTO_START）
    Auto,
    /// 自动（延迟启动）——SERVICE_AUTO_START + DelayedAutoStart 标志
    DelayedAuto,
    /// 手动（SERVICE_DEMAND_START）
    Manual,
}

/// 启动类型原始判读（快照显示用；Disabled/其他不可经本 UI 修改）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartKind {
    Auto,
    DelayedAuto,
    Manual,
    Disabled,
    #[default]
    Other,
}

impl StartKind {
    /// 管控页单选组映射：可设置三档 → 单选下标；Disabled/Other → None（全不选）。
    pub fn radio_index(self) -> Option<usize> {
        match self {
            Self::Auto => Some(0),
            Self::DelayedAuto => Some(1),
            Self::Manual => Some(2),
            Self::Disabled | Self::Other => None,
        }
    }

    /// 中文显示。
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::DelayedAuto => "自动（延迟启动）",
            Self::Manual => "手动",
            Self::Disabled => "禁用",
            Self::Other => "其他",
        }
    }
}

/// 服务快照（模式检测 + 管控页显示的单一数据源；SCM 读路径，标准用户可取）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceSnap {
    /// 运行状态（None = 状态查询失败，降级显示）
    pub status: Option<ServiceState>,
    /// 服务进程 PID（None = 未运行/未知）
    pub pid: Option<u32>,
    /// 启动类型判读
    pub start: StartKind,
    /// 崩溃自动重启（SCM Recovery 存在 RESTART 动作 = Some(true)；无 = Some(false)；
    /// None = 读取失败/未知——对应 systemd `Restart=on-failure` 等价物）
    pub recovery_restart: Option<bool>,
}

// ─── 模式判定（纯函数，可测） ───────────────────────────────────────────────

/// ep-tray 运行模式：daemon 以 Windows Service 托管，还是 ep-tray sidecar 托管。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    /// sidecar：服务未安装，daemon 由 ep-tray 拉起/托管（GT-1/2 行为，回归红线）
    #[default]
    Sidecar,
    /// Windows 服务（EntryPoint）已安装——启停重启走 SCM（提权）
    Service,
}

impl RunMode {
    /// 模式判定（纯函数）：query 成功 → Service；NotFound → Sidecar；
    /// **其他 SCM 读错误也按 Sidecar**（SCM 不可用属异常态，保守回落 sidecar
    /// 展示，30s 复检线程会纠正；注释留档防止误改成 Err 传播）。
    pub fn detect(query: &Result<ServiceSnap, ScmError>) -> Self {
        match query {
            Ok(_) => Self::Service,
            Err(_) => Self::Sidecar,
        }
    }
}

// ─── 管控面 trait（真实实现 + 测试注入） ────────────────────────────────────

/// SCM 管控面抽象。真实实现 = [`WindowsScm`]；单测注入假实现验证调用序列与
/// 错误传播（提权动作执行器 elevate::execute_sc_action 按此 trait 编程）。
pub trait ServiceControl {
    /// 查询服务快照（读路径，标准用户可用）。
    fn query(&self, name: &str) -> Result<ServiceSnap, ScmError>;
    /// 启动服务（幂等：已在运行 → Ok）。
    fn start(&self, name: &str) -> Result<(), ScmError>;
    /// 停止服务（幂等：未运行 → Ok）。
    fn stop(&self, name: &str) -> Result<(), ScmError>;
    /// 重启服务（stop → 轮询至 STOPPED → start；超时 = [`ScmError::Os`]）。
    fn restart(&self, name: &str) -> Result<(), ScmError> {
        self.stop(name)?;
        let deadline = Instant::now() + STOP_WAIT;
        loop {
            match self.query(name) {
                Ok(s) if s.status == Some(ServiceState::Stopped) => break,
                Ok(_) if Instant::now() >= deadline => {
                    return Err(ScmError::Os(ERROR_SERVICE_REQUEST_TIMEOUT));
                }
                Ok(_) => sleep(STOP_POLL),
                Err(e) => return Err(e),
            }
        }
        self.start(name)
    }
    /// 修改启动类型（自启面：Service 模式的「自启配置」）。
    fn change_start_type(&self, name: &str, st: StartType) -> Result<(), ScmError>;
    /// 崩溃自动重启开关（SCM Recovery，等价 systemd Restart=on-failure）。
    fn set_recovery_restart(&self, name: &str, on: bool) -> Result<(), ScmError>;
}

/// 真实 SCM 实现（无状态；全部 API 调用即时开关句柄，不持锁不缓存）。
pub struct WindowsScm;

impl ServiceControl for WindowsScm {
    fn query(&self, name: &str) -> Result<ServiceSnap, ScmError> {
        unsafe {
            let scm = open_scm()?;
            let svc = match open_service(scm, name, SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG) {
                Ok(h) => h,
                Err(e) => {
                    CloseServiceHandle(scm);
                    return Err(e);
                }
            };
            let result = query_snapshot(svc);
            CloseServiceHandle(svc);
            CloseServiceHandle(scm);
            result
        }
    }

    fn start(&self, name: &str) -> Result<(), ScmError> {
        unsafe {
            with_service(name, SERVICE_START, |svc| {
                // 参数序：count 在前、向量在后（windows-sys 0.61 签名）；
                // 无参数启动 → (0, null)
                if StartServiceW(svc, 0, std::ptr::null()) == 0 {
                    let code = GetLastError();
                    // 幂等：已在运行视为成功（重复点「启动」不报错）
                    if code != ERROR_SERVICE_ALREADY_RUNNING {
                        return Err(map_last_error(code));
                    }
                }
                Ok(())
            })
        }
    }

    fn stop(&self, name: &str) -> Result<(), ScmError> {
        unsafe {
            with_service(name, SERVICE_STOP, |svc| {
                let mut status: SERVICE_STATUS = std::mem::zeroed();
                if ControlService(svc, SERVICE_CONTROL_STOP, &mut status) == 0 {
                    let code = GetLastError();
                    // 幂等：未在运行视为成功（重复点「停止」不报错）
                    if code != ERROR_SERVICE_NOT_ACTIVE {
                        return Err(map_last_error(code));
                    }
                }
                Ok(())
            })
        }
    }

    fn change_start_type(&self, name: &str, st: StartType) -> Result<(), ScmError> {
        // DelayedAuto = SERVICE_AUTO_START + DelayedAutoStart 标志（ChangeServiceConfig2），
        // 其余档位同时清掉延迟标志（delayed=false）——避免「自动→自动延迟→自动」残留
        let (dw_start, delayed) = match st {
            StartType::Auto => (SERVICE_AUTO_START, false),
            StartType::DelayedAuto => (SERVICE_AUTO_START, true),
            StartType::Manual => (SERVICE_DEMAND_START, false),
        };
        unsafe {
            with_service(name, SERVICE_CHANGE_CONFIG_ACCESS, |svc| {
                use windows_sys::Win32::System::Services::SERVICE_DELAYED_AUTO_START_INFO;
                if ChangeServiceConfigW(
                    svc,
                    SERVICE_NO_CHANGE, // dwServiceType 不动
                    dw_start,
                    SERVICE_NO_CHANGE, // dwErrorControl 不动
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                ) == 0
                {
                    return Err(map_last_error(GetLastError()));
                }
                let info = SERVICE_DELAYED_AUTO_START_INFO {
                    fDelayedAutostart: delayed as i32,
                };
                if ChangeServiceConfig2W(
                    svc,
                    SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                    &info as *const _ as *const core::ffi::c_void,
                ) == 0
                {
                    return Err(map_last_error(GetLastError()));
                }
                Ok(())
            })
        }
    }

    fn set_recovery_restart(&self, name: &str, on: bool) -> Result<(), ScmError> {
        unsafe {
            with_service(name, SERVICE_CHANGE_CONFIG_ACCESS, |svc| {
                // 与 install-service.ps1:75 同参：reset=86400、restart/10000/30000/60000
                // （GUI 开关与脚本安装默认值一致，互为等价口径）
                let mut fa = SERVICE_FAILURE_ACTIONSW {
                    dwResetPeriod: 0,
                    lpRebootMsg: std::ptr::null_mut(),
                    lpCommand: std::ptr::null_mut(),
                    cActions: 0,
                    lpsaActions: std::ptr::null_mut(),
                };
                let mut actions: [SC_ACTION; 3] = [
                    SC_ACTION { Type: SC_ACTION_RESTART, Delay: 10_000 },
                    SC_ACTION { Type: SC_ACTION_RESTART, Delay: 30_000 },
                    SC_ACTION { Type: SC_ACTION_RESTART, Delay: 60_000 },
                ];
                if on {
                    fa.dwResetPeriod = 86_400;
                    fa.cActions = 3;
                    fa.lpsaActions = actions.as_mut_ptr();
                } // off：cActions=0 + lpsaActions=null → 清空恢复动作（SCM 不再自动拉起）
                if ChangeServiceConfig2W(
                    svc,
                    SERVICE_CONFIG_FAILURE_ACTIONS,
                    &fa as *const _ as *const core::ffi::c_void,
                ) == 0
                {
                    return Err(map_last_error(GetLastError()));
                }
                Ok(())
            })
        }
    }
}

/// OpenService 所需写权限位（ChangeServiceConfig 家族）；常量在函数内 use
/// 会在两处重复——这里本地别名一次。
const SERVICE_CHANGE_CONFIG_ACCESS: u32 =
    windows_sys::Win32::System::Services::SERVICE_CHANGE_CONFIG;

// ─── Win32 句柄与查询内部件 ─────────────────────────────────────────────────

/// 打开 SCM（标准用户 SC_MANAGER_CONNECT 即可；写操作的权限卡在
/// OpenService 的服务对象 ACL 上，错误同样归一 AccessDenied）。
unsafe fn open_scm() -> Result<SC_HANDLE, ScmError> {
    let h = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
    if h.is_null() {
        Err(map_last_error(GetLastError()))
    } else {
        Ok(h)
    }
}

/// 打开服务对象（desired access 按需最小授权）。
unsafe fn open_service(scm: SC_HANDLE, name: &str, desired: u32) -> Result<SC_HANDLE, ScmError> {
    let wname = wide(name);
    let h = OpenServiceW(scm, wname.as_ptr(), desired);
    if h.is_null() {
        Err(map_last_error(GetLastError()))
    } else {
        Ok(h)
    }
}

/// with-句柄包装：打开 → 执行 → 必然关闭（句柄泄漏面收敛到一处）。
unsafe fn with_service<T>(
    name: &str,
    desired: u32,
    f: impl FnOnce(SC_HANDLE) -> Result<T, ScmError>,
) -> Result<T, ScmError> {
    let scm = open_scm()?;
    let svc = match open_service(scm, name, desired) {
        Ok(h) => h,
        Err(e) => {
            CloseServiceHandle(scm);
            return Err(e);
        }
    };
    let result = f(svc);
    CloseServiceHandle(svc);
    CloseServiceHandle(scm);
    result
}

/// 查询快照本体：状态+PID（StatusEx）→ 启动类型（Config）→ 延迟标志 →
/// 恢复动作（Config2）。单项失败降级为字段 None/Default，不让整次查询失败
/// （显示优先于完备——状态与启动类型任一可得即有值）。
unsafe fn query_snapshot(svc: SC_HANDLE) -> Result<ServiceSnap, ScmError> {
    let mut snap = ServiceSnap::default();

    // 1) 运行状态 + PID
    let mut ssp: SERVICE_STATUS_PROCESS = std::mem::zeroed();
    let mut needed = 0u32;
    let ok = QueryServiceStatusEx(
        svc,
        SC_STATUS_PROCESS_INFO,
        &mut ssp as *mut _ as *mut u8,
        std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
        &mut needed,
    );
    if ok != 0 {
        snap.status = Some(map_state(ssp.dwCurrentState));
        snap.pid = (ssp.dwProcessId != 0).then_some(ssp.dwProcessId);
    }

    // 2) 启动类型（QueryServiceConfigW 两段式：先探大小）
    let mut needed = 0u32;
    let probe = QueryServiceConfigW(svc, std::ptr::null_mut(), 0, &mut needed);
    if probe == 0 && GetLastError() == ERROR_INSUFFICIENT_BUFFER && needed > 0 {
        let mut buf = aligned_buf(needed as usize);
        let cfg = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
        if QueryServiceConfigW(svc, cfg, needed, &mut needed) != 0 {
            let raw_start = (*cfg).dwStartType;
            // 3) 延迟启动标志（仅 AUTO_START 时有意义）
            let delayed = if raw_start == SERVICE_AUTO_START {
                query_config2_flag(svc, SERVICE_CONFIG_DELAYED_AUTO_START_INFO)
            } else {
                None
            };
            snap.start = start_kind_from_raw(raw_start, delayed);
        }
    }

    // 4) 恢复动作（存在 RESTART = 崩溃自动重启已开启）
    if let Some(buf) = query_config2_buf(svc, SERVICE_CONFIG_FAILURE_ACTIONS) {
        let fa = &*(buf.as_ptr() as *const SERVICE_FAILURE_ACTIONSW);
        snap.recovery_restart = Some(
            fa.cActions > 0
                && !fa.lpsaActions.is_null()
                && std::slice::from_raw_parts(fa.lpsaActions, fa.cActions as usize)
                    .iter()
                    .any(|a| a.Type == SC_ACTION_RESTART),
        );
    }

    Ok(snap)
}

/// SERVICE_CONFIG_* 查询（两段式探大小 + u64 对齐缓冲——结构含指针，u8 缓冲
/// 强转会引入对齐 UB）。失败返回 None（调用方降级）。
unsafe fn query_config2_buf(svc: SC_HANDLE, info_level: u32) -> Option<Vec<u64>> {
    let mut needed = 0u32;
    let probe = QueryServiceConfig2W(svc, info_level, std::ptr::null_mut(), 0, &mut needed);
    if !(probe == 0 && GetLastError() == ERROR_INSUFFICIENT_BUFFER && needed > 0) {
        return None;
    }
    let mut buf = aligned_buf(needed as usize);
    if QueryServiceConfig2W(svc, info_level, buf.as_mut_ptr() as *mut u8, needed, &mut needed) == 0
    {
        return None;
    }
    Some(buf)
}

/// 布尔型 Config2 查询（DelayedAutoStart）：首个 BOOL（i32，低 32 位）。
unsafe fn query_config2_flag(svc: SC_HANDLE, info_level: u32) -> Option<bool> {
    query_config2_buf(svc, info_level).map(|buf| buf[0] as u32 != 0)
}

/// u64 对齐零缓冲（QueryServiceConfig(2) 家族的接收缓冲——结构含 PWSTR/指针
/// 成员，8 字节对齐；u8 Vec 强转有对齐 UB 风险）。
fn aligned_buf(bytes: usize) -> Vec<u64> {
    vec![0u64; bytes.div_ceil(8)]
}

fn map_state(raw: u32) -> ServiceState {
    match raw {
        SERVICE_STOPPED => ServiceState::Stopped,
        SERVICE_START_PENDING => ServiceState::StartPending,
        SERVICE_STOP_PENDING => ServiceState::StopPending,
        SERVICE_RUNNING => ServiceState::Running,
        // CONTINUE_PENDING(5) / PAUSE_PENDING(6) / PAUSED(7) 归并
        _ => ServiceState::Paused,
    }
}

/// dwStartType + 延迟标志 → 显示判读（boot/system start 不出现在常规服务 → Other）。
fn start_kind_from_raw(raw: u32, delayed: Option<bool>) -> StartKind {
    match raw {
        SERVICE_AUTO_START => {
            if delayed.unwrap_or(false) {
                StartKind::DelayedAuto
            } else {
                StartKind::Auto
            }
        }
        SERVICE_DEMAND_START => StartKind::Manual,
        SERVICE_DISABLED => StartKind::Disabled,
        _ => StartKind::Other,
    }
}

/// GetLastError 归一：NotFound / AccessDenied 单列（UI 分流），其余透传。
fn map_last_error(code: u32) -> ScmError {
    match code {
        ERROR_SERVICE_DOES_NOT_EXIST => ScmError::NotFound,
        ERROR_ACCESS_DENIED => ScmError::AccessDenied,
        other => ScmError::Os(other),
    }
}

/// UTF-16 NUL 结尾（Win32 宽字符串入参）。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 便捷自由函数（模式检测/提权执行器用真实实现直调）。
pub fn query_service(name: &str) -> Result<ServiceSnap, ScmError> {
    WindowsScm.query(name)
}

// ─── 单测（读路径真跑：标准用户 SC_MANAGER_CONNECT 可用；写路径不在此测，
//     提权执行序列由 elevate.rs 的假 trait 注入测试覆盖） ────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 SCM 读路径：不存在的服务 → NotFound（标准用户无需提权）。
    #[test]
    fn query_nonexistent_service_returns_not_found() {
        let err = query_service("EntryPoint__gt3_no_such_service__").unwrap_err();
        assert_eq!(err, ScmError::NotFound, "实际错误: {err}");
    }

    /// 真实 SCM 读路径：系统常驻服务 RpcSs 恒存在且运行中。
    #[test]
    fn query_rpcss_returns_running_snapshot() {
        let snap = query_service("RpcSs").expect("RpcSs 为 Windows 常驻服务，读路径应成功");
        assert_eq!(snap.status, Some(ServiceState::Running));
        assert!(snap.pid.is_some(), "运行中服务应报告 PID");
    }

    /// 本机 EntryPoint 服务（若已安装）的读路径冒烟：有快照则字段自洽。
    #[test]
    fn query_entrypoint_self_consistent_if_installed() {
        match query_service(SERVICE_NAME) {
            Ok(snap) => {
                // 已安装：状态必有值；PID 与运行态一致
                assert!(snap.status.is_some());
                if snap.status == Some(ServiceState::Running) {
                    assert!(snap.pid.is_some());
                } else {
                    assert!(snap.pid.is_none(), "非运行态不应有 PID，实际 {snap:?}");
                }
            }
            Err(ScmError::NotFound) => {} // 未安装：合法（sidecar 模式）
            Err(e) => panic!("EntryPoint 读路径不应有其他错误，实际 {e}"),
        }
    }

    // ── 模式判定（纯函数） ──

    #[test]
    fn run_mode_detect_maps_query_result() {
        let snap = ServiceSnap {
            status: Some(ServiceState::Running),
            pid: Some(42),
            ..Default::default()
        };
        assert_eq!(RunMode::detect(&Ok(snap)), RunMode::Service, "查到服务 → Service");
        assert_eq!(
            RunMode::detect(&Err(ScmError::NotFound)),
            RunMode::Sidecar,
            "NotFound → sidecar"
        );
        assert_eq!(
            RunMode::detect(&Err(ScmError::AccessDenied)),
            RunMode::Sidecar,
            "其他 SCM 读错误保守回落 sidecar（30s 复检纠正）"
        );
        assert_eq!(RunMode::default(), RunMode::Sidecar, "缺省 sidecar");
    }

    // ── 启动类型映射（纯函数） ──

    #[test]
    fn start_kind_mapping_covers_sc_raw_values() {
        use windows_sys::Win32::System::Services::{SERVICE_BOOT_START, SERVICE_SYSTEM_START};
        assert_eq!(start_kind_from_raw(SERVICE_AUTO_START, Some(false)), StartKind::Auto);
        assert_eq!(start_kind_from_raw(SERVICE_AUTO_START, Some(true)), StartKind::DelayedAuto);
        assert_eq!(start_kind_from_raw(SERVICE_AUTO_START, None), StartKind::Auto, "标志读取失败按非延迟");
        assert_eq!(start_kind_from_raw(SERVICE_DEMAND_START, None), StartKind::Manual);
        assert_eq!(start_kind_from_raw(SERVICE_DISABLED, None), StartKind::Disabled);
        assert_eq!(start_kind_from_raw(SERVICE_BOOT_START, None), StartKind::Other);
        assert_eq!(start_kind_from_raw(SERVICE_SYSTEM_START, None), StartKind::Other);
    }

    #[test]
    fn start_kind_radio_index_and_labels() {
        assert_eq!(StartKind::Auto.radio_index(), Some(0));
        assert_eq!(StartKind::DelayedAuto.radio_index(), Some(1));
        assert_eq!(StartKind::Manual.radio_index(), Some(2));
        assert_eq!(StartKind::Disabled.radio_index(), None, "禁用不可经本 UI 修改");
        assert_eq!(StartKind::Other.radio_index(), None);
        assert_eq!(StartKind::DelayedAuto.label(), "自动（延迟启动）");
    }

    // ── SCM 原始常量守护（防 windows-sys 升级漂移；值来自 Win32 头文件） ──

    #[test]
    fn sc_raw_constants_guard() {
        use windows_sys::Win32::System::Services::{
            SERVICE_DEMAND_START as DEMAND, SERVICE_AUTO_START as AUTO,
            SERVICE_DISABLED as DISABLED,
        };
        assert_eq!(AUTO, 2);
        assert_eq!(DEMAND, 3);
        assert_eq!(DISABLED, 4);
        // 注意：windows-sys 0.61 SC_ACTION_RESTART=1（NONE=0 REBOOT=2 RESTART=1
        // RUN_COMMAND=3 OWN_RESTART=4）——头文件实序，非网上常见记忆值
        assert_eq!(SC_ACTION_RESTART, 1);
        assert_eq!(SERVICE_STOPPED, 1);
        assert_eq!(SERVICE_RUNNING, 4);
        assert_eq!(SERVICE_NAME, "EntryPoint");
    }

    // ── wide 编码（纯函数） ──

    #[test]
    fn wide_appends_nul_terminator() {
        assert_eq!(wide("EntryPoint"), vec![69, 110, 116, 114, 121, 80, 111, 105, 110, 116, 0]);
        assert_eq!(wide(""), vec![0]);
    }
}
