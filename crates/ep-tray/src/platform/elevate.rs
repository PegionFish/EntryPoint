//! SCM 写操作的自提权执行面（GT-3）。
//!
//! UAC 模型（红线：**主进程绝不提权**）：
//! 1. 主进程（托盘/GUI，标准权限）点管控按钮 → [`run_elevated_self`] 以
//!    `ShellExecuteExW(verb="runas")` 拉起**自身** + `--sc-action <op>` 参数族
//!    （UAC 弹窗由用户现场批准——拒绝 → `ElevateError::Cancelled`）；
//! 2. 提权子进程在 main.rs 最先解析 `--sc-action`：命中即执行
//!    [`execute_sc_action`]（SCM 写操作），按退出码 0=ok / 2=拒绝 / 3=失败
//!    **立即退出**——绝不进 GUI 循环、绝不碰托盘/单实例锁/sidecar；
//! 3. 主进程后台线程等子进程退出码（[`run_elevated_self`] 阻塞等待，跑在
//!    独立线程不阻塞 UI），映射回 UI 反馈（commit⑤ 接线）。
//!
//! arg 解析与退出码映射全部为纯函数（单测覆盖）；[`WindowsScm`] 注入点使
//! 执行序列可离线测试（假 [`ServiceControl`]）。

use std::fmt;

use crate::platform::scm::{ScmError, ServiceControl, StartType, WindowsScm, SERVICE_NAME};

/// 提权动作参数族前缀（main.rs 与本模块的唯一约定锚点）
pub const SC_ACTION_ARG: &str = "--sc-action";

/// 提权子进程退出码约定（主进程 UI 据此反馈）
pub const EXIT_OK: i32 = 0;
/// 拒绝：SCM 返回 ERROR_ACCESS_DENIED（或主进程侧 UAC 被拒）
pub const EXIT_DENIED: i32 = 2;
/// 失败：其余错误（服务未安装/超时/参数错等）
pub const EXIT_FAILED: i32 = 3;

// ─── 动作模型与编码（纯函数） ───────────────────────────────────────────────

/// 一次性提权动作（UI 按钮 → 子进程参数的唯一载体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScAction {
    /// 启动服务（StartService）
    Start,
    /// 停止服务（ControlService STOP）
    Stop,
    /// 重启服务（stop → 轮询 STOPPED → start）
    Restart,
    /// 修改启动类型（自启面：Service 模式）
    SetStartType(StartType),
    /// 崩溃自动重启开关（SCM Recovery，等价 systemd Restart=on-failure）
    SetRecovery(bool),
}

impl ScAction {
    /// 编码为 `--sc-action` 的动作参数（无空格——子进程参数拼接免引号）。
    pub fn spec(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::SetStartType(StartType::Auto) => "set-start-type=auto",
            Self::SetStartType(StartType::DelayedAuto) => "set-start-type=delayed-auto",
            Self::SetStartType(StartType::Manual) => "set-start-type=manual",
            Self::SetRecovery(true) => "set-recovery=on",
            Self::SetRecovery(false) => "set-recovery=off",
        }
    }

    /// 动作的人类可读名（UI 操作反馈「上次操作：…」用）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "启动服务",
            Self::Stop => "停止服务",
            Self::Restart => "重启服务",
            Self::SetStartType(_) => "修改启动类型",
            Self::SetRecovery(true) => "开启崩溃自动重启",
            Self::SetRecovery(false) => "关闭崩溃自动重启",
        }
    }

    /// 解析动作参数（[`Self::spec`] 的逆运算；未知值 → Err）。
    pub fn from_spec(spec: &str) -> Result<Self, String> {
        match spec {
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            "restart" => Ok(Self::Restart),
            "set-start-type=auto" => Ok(Self::SetStartType(StartType::Auto)),
            "set-start-type=delayed-auto" => Ok(Self::SetStartType(StartType::DelayedAuto)),
            "set-start-type=manual" => Ok(Self::SetStartType(StartType::Manual)),
            "set-recovery=on" => Ok(Self::SetRecovery(true)),
            "set-recovery=off" => Ok(Self::SetRecovery(false)),
            other => Err(format!("未知 --sc-action 动作: {other}")),
        }
    }
}

/// 从进程参数扫描 `--sc-action <spec>`。
///
/// - 未出现 → `Ok(None)`（常规 GUI 启动路径）；
/// - 出现但缺值 / 未知动作 → `Err`（提权子进程按 EXIT_FAILED 退出）；
/// - 其余参数（`--minimized` 等）忽略——提权子进程不进 GUI，语义无冲突。
pub fn parse_sc_action(args: &[String]) -> Result<Option<ScAction>, String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == SC_ACTION_ARG {
            let spec = iter.next().ok_or("--sc-action 缺少动作参数")?;
            return ScAction::from_spec(spec).map(Some);
        }
    }
    Ok(None)
}

/// 组装提权子进程参数（主进程侧：`["--sc-action", "<spec>"]`）。
pub fn sc_action_args(op: ScAction) -> Vec<String> {
    vec![SC_ACTION_ARG.to_owned(), op.spec().to_owned()]
}

// ─── 提权子进程执行器（SCM 写操作 → 退出码） ────────────────────────────────

/// 执行一次性 SCM 写动作并返回进程退出码（提权子进程主路径）。
/// `scm` 注入点：生产传 [`WindowsScm`]；单测注入假实现离线验证调用序列。
pub fn execute_sc_action(op: &ScAction, scm: &dyn ServiceControl) -> i32 {
    let result = match op {
        ScAction::Start => scm.start(SERVICE_NAME),
        ScAction::Stop => scm.stop(SERVICE_NAME),
        ScAction::Restart => scm.restart(SERVICE_NAME),
        ScAction::SetStartType(st) => scm.change_start_type(SERVICE_NAME, *st),
        ScAction::SetRecovery(on) => scm.set_recovery_restart(SERVICE_NAME, *on),
    };
    match &result {
        Ok(()) => {
            eprintln!("ep-tray(sc-action): {} 成功", op.label());
            exit_code(&result)
        }
        Err(e) => {
            eprintln!("ep-tray(sc-action): {} 失败: {e}", op.label());
            exit_code(&result)
        }
    }
}

/// 退出码映射（纯函数）：Ok→0 / AccessDenied→2 / 其余→3。
/// NotFound 归 3（服务消失属失败态；UI 文案区分显示由 ScmError 文本承担——
/// 子进程 stderr 有明细，退出码只分「成功/拒绝/失败」三档）。
pub fn exit_code(result: &Result<(), ScmError>) -> i32 {
    match result {
        Ok(()) => EXIT_OK,
        Err(ScmError::AccessDenied) => EXIT_DENIED,
        Err(_) => EXIT_FAILED,
    }
}

/// 生产便捷入口：真实 SCM 执行（提权子进程 main 调用）。
pub fn execute_sc_action_real(op: &ScAction) -> i32 {
    execute_sc_action(op, &WindowsScm)
}

// ─── 主进程侧：ShellExecuteExW 自提权拉起 ───────────────────────────────────

/// 自提权失败类型（UI 反馈分流：Cancelled → 「未获授权」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevateError {
    /// 用户拒绝了 UAC 授权（子进程未启动）
    Cancelled,
    /// ShellExecuteEx/等待链路的其余失败（含 Win32 错误码）
    Os(u32),
}

impl fmt::Display for ElevateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "未获授权（UAC 被拒绝）"),
            Self::Os(code) => write!(f, "提权启动失败（Windows 错误 {code}）"),
        }
    }
}

/// 以管理员权限拉起**自身**执行 `args`（如 `["--sc-action", "stop"]`），
/// 阻塞等待退出并返回子进程退出码。**必须在后台线程调用**（UAC 等待可能
/// 任意久——用户不点弹窗就挂着）；调用方（主进程）永不提权。
///
/// 链路：ShellExecuteExW(SEE_MASK_NOCLOSEPROCESS | FLAG_NO_UI, verb="runas",
/// nShow=0 隐藏子进程控制台窗) → WaitForSingleObject(INFINITE) →
/// GetExitCodeProcess → CloseHandle。
pub fn run_elevated_self(args: &[String]) -> Result<i32, ElevateError> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    let exe = std::env::current_exe()
        .map_err(|_| ElevateError::Os(windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND))?;
    let exe_w = crate::platform::scm::wide(&exe.display().to_string());
    // 参数拼接：--sc-action 参数族无空格，空格分隔即可（防御性处理含空格参数）
    let params = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let params_w = crate::platform::scm::wide(&params);
    let verb_w = crate::platform::scm::wide("runas");

    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
    sei.lpVerb = verb_w.as_ptr();
    sei.lpFile = exe_w.as_ptr();
    sei.lpParameters = params_w.as_ptr();
    sei.nShow = 0; // SW_HIDE：提权子进程是无窗口的一次性执行体

    unsafe {
        if ShellExecuteExW(&mut sei) == 0 {
            let code = GetLastError();
            return Err(if code == ERROR_CANCELLED {
                ElevateError::Cancelled
            } else {
                ElevateError::Os(code)
            });
        }
        if sei.hProcess.is_null() {
            // SEE_MASK_NOCLOSEPROCESS 下正常必有句柄；缺失即异常（保守报失败）
            return Err(ElevateError::Os(0));
        }
        let wait = WaitForSingleObject(sei.hProcess, INFINITE);
        if wait == 0xFFFF_FFFF {
            // WAIT_FAILED：句柄失效等异常（子进程已无法回收退出码）
            CloseHandle(sei.hProcess);
            return Err(ElevateError::Os(GetLastError()));
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(sei.hProcess, &mut code);
        CloseHandle(sei.hProcess);
        if ok == 0 {
            return Err(ElevateError::Os(GetLastError()));
        }
        Ok(code as i32)
    }
}

// ─── 单测（纯函数 + 假 trait 注入；真实提权 UAC 交互留 GT-7 用户在场验收） ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::scm::{ServiceState, ServiceSnap};
    use std::sync::Mutex;

    // ── parse / 编码往返 ──

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_absent_returns_none() {
        assert_eq!(parse_sc_action(&[]), Ok(None));
        assert_eq!(parse_sc_action(&s(&["--minimized"])), Ok(None));
        assert_eq!(parse_sc_action(&s(&["--port", "9800"])), Ok(None));
    }

    #[test]
    fn parse_all_specs_roundtrip() {
        let ops = [
            ScAction::Start,
            ScAction::Stop,
            ScAction::Restart,
            ScAction::SetStartType(StartType::Auto),
            ScAction::SetStartType(StartType::DelayedAuto),
            ScAction::SetStartType(StartType::Manual),
            ScAction::SetRecovery(true),
            ScAction::SetRecovery(false),
        ];
        for op in ops {
            let args = sc_action_args(op);
            assert_eq!(args.len(), 2, "参数族形状: --sc-action <spec>");
            assert_eq!(args[0], "--sc-action");
            assert!(!args[1].contains(' '), "spec 无空格（免引号拼接）");
            assert_eq!(parse_sc_action(&args), Ok(Some(op)), "往返: {}", op.spec());
        }
    }

    #[test]
    fn parse_errors_are_structured() {
        assert_eq!(
            parse_sc_action(&s(&["--sc-action"])).unwrap_err(),
            "--sc-action 缺少动作参数"
        );
        assert!(parse_sc_action(&s(&["--sc-action", "uninstall"])).is_err(), "未知动作拒绝");
        assert!(parse_sc_action(&s(&["--sc-action", "set-start-type=disabled"])).is_err(),
            "Disabled 不可经本 UI 设置（红线：不自动装/卸）");
        // 出现在其余参数之后仍可解析（位置无关）
        assert_eq!(
            parse_sc_action(&s(&["--minimized", "--sc-action", "stop"])),
            Ok(Some(ScAction::Stop))
        );
        // 只认第一个 --sc-action（重复参数取先者）
        assert_eq!(
            parse_sc_action(&s(&["--sc-action", "stop", "--sc-action", "start"])),
            Ok(Some(ScAction::Stop))
        );
    }

    #[test]
    fn labels_cover_all_actions() {
        assert_eq!(ScAction::Start.label(), "启动服务");
        assert_eq!(ScAction::SetRecovery(true).label(), "开启崩溃自动重启");
        assert_eq!(ScAction::SetStartType(StartType::Manual).label(), "修改启动类型");
    }

    // ── 退出码映射（纯函数） ──

    #[test]
    fn exit_code_maps_three_tiers() {
        assert_eq!(exit_code(&Ok(())), EXIT_OK);
        assert_eq!(exit_code(&Err(ScmError::AccessDenied)), EXIT_DENIED);
        assert_eq!(exit_code(&Err(ScmError::NotFound)), EXIT_FAILED);
        assert_eq!(exit_code(&Err(ScmError::Os(1053))), EXIT_FAILED);
    }

    // ── 假 trait 注入：执行序列与退出码（离线，无 UAC） ──

    /// 内存假 SCM：记录调用序列，按动作返回预设结果。
    struct FakeScm {
        calls: Mutex<Vec<&'static str>>,
        result: Mutex<Result<(), ScmError>>,
        state: Mutex<ServiceState>,
    }

    impl FakeScm {
        fn new(result: Result<(), ScmError>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                result: Mutex::new(result),
                state: Mutex::new(ServiceState::Stopped),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().iter().map(|x| x.to_string()).collect()
        }
    }

    impl ServiceControl for FakeScm {
        fn query(&self, _name: &str) -> Result<ServiceSnap, ScmError> {
            self.calls.lock().unwrap().push("query");
            Ok(ServiceSnap {
                status: Some(*self.state.lock().unwrap()),
                ..Default::default()
            })
        }
        fn start(&self, _name: &str) -> Result<(), ScmError> {
            self.calls.lock().unwrap().push("start");
            self.result.lock().unwrap().clone()
        }
        fn stop(&self, _name: &str) -> Result<(), ScmError> {
            self.calls.lock().unwrap().push("stop");
            self.result.lock().unwrap().clone()
        }
        fn change_start_type(&self, _name: &str, _st: StartType) -> Result<(), ScmError> {
            self.calls.lock().unwrap().push("change_start_type");
            self.result.lock().unwrap().clone()
        }
        fn set_recovery_restart(&self, _name: &str, _on: bool) -> Result<(), ScmError> {
            self.calls.lock().unwrap().push("set_recovery_restart");
            self.result.lock().unwrap().clone()
        }
    }

    #[test]
    fn execute_ok_yields_zero() {
        for op in [
            ScAction::Start,
            ScAction::Stop,
            ScAction::SetStartType(StartType::Auto),
            ScAction::SetRecovery(true),
        ] {
            let fake = FakeScm::new(Ok(()));
            assert_eq!(execute_sc_action(&op, &fake), EXIT_OK, "{}", op.spec());
        }
    }

    #[test]
    fn execute_denied_yields_two() {
        let fake = FakeScm::new(Err(ScmError::AccessDenied));
        assert_eq!(execute_sc_action(&ScAction::Start, &fake), EXIT_DENIED);
        assert_eq!(execute_sc_action(&ScAction::Stop, &fake), EXIT_DENIED);
    }

    #[test]
    fn execute_other_failure_yields_three() {
        let fake = FakeScm::new(Err(ScmError::NotFound));
        assert_eq!(execute_sc_action(&ScAction::Start, &fake), EXIT_FAILED);
        let fake = FakeScm::new(Err(ScmError::Os(1053)));
        assert_eq!(execute_sc_action(&ScAction::SetRecovery(true), &fake), EXIT_FAILED);
    }

    #[test]
    fn execute_dispatches_to_right_scm_call() {
        let fake = FakeScm::new(Ok(()));
        execute_sc_action(&ScAction::SetStartType(StartType::DelayedAuto), &fake);
        assert_eq!(fake.calls(), vec!["change_start_type"]);
        let fake = FakeScm::new(Ok(()));
        execute_sc_action(&ScAction::SetRecovery(false), &fake);
        assert_eq!(fake.calls(), vec!["set_recovery_restart"]);
    }

    /// 默认 restart = stop → 轮询 STOPPED → start（trait 默认实现经假实现
    /// 验证调用序列——SCM 无原子 restart，语义在此固化）。
    #[test]
    fn restart_sequence_is_stop_query_until_stopped_then_start() {
        let fake = FakeScm::new(Ok(()));
        assert_eq!(execute_sc_action(&ScAction::Restart, &fake), EXIT_OK);
        let calls = fake.calls();
        assert_eq!(calls.first().map(String::as_str), Some("stop"), "先停");
        assert_eq!(calls.last().map(String::as_str), Some("start"), "后启");
        assert!(calls.contains(&"query".to_string()), "中间轮询状态");
        // 顺序：stop 全在 start 之前
        let stop_pos = calls.iter().position(|c| c == "stop").unwrap();
        let start_pos = calls.iter().position(|c| c == "start").unwrap();
        assert!(stop_pos < start_pos);
    }

    /// stop 失败时 restart 不再尝试 start（快速失败，退出码 3）。
    #[test]
    fn restart_stop_failure_skips_start() {
        let fake = FakeScm::new(Err(ScmError::Os(1062))); // ERROR_SERVICE_NOT_ACTIVE 之外的失败
        assert_eq!(execute_sc_action(&ScAction::Restart, &fake), EXIT_FAILED);
        assert!(!fake.calls().contains(&"start".to_string()));
    }
}
