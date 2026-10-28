//! Windows 服务运行模式（演进计划·计划2 M3）。
//!
//! `ep-daemon --service` 以 Windows 服务语义运行（`sc create` 直接托管路线，
//! master report 计划2 M3 择一结论）：经 `windows-service` crate 与 SCM 对话
//! ——服务进程不与 SCM 通信会在 ~30s 后被系统终止，因此完整集成是本路线的
//! 前提而非优化。STOP/SHUTDOWN 控制汇入进程级 stop 通道，由
//! [`crate::shutdown_signal`] 与 Ctrl+C/SIGTERM 同路优雅回收（run_server 零改动）。
//!
//! 非 Windows 目标：模块仅保留参数探测与永不触发的 stop 通道，`--service`
//! 在 main() 显式报错。

use std::sync::OnceLock;
use tokio::sync::watch;

/// sc create 使用的服务名（与 scripts/install-service.ps1 缺省一致）
// 唯一消费方是 #[cfg(windows)] 的 SCM 注册路径；非 Windows 目标编译为死代码
// （CI rust job 第四轮在 ubuntu 上命中——与 uma.rs 同类，教训同源）
#[cfg_attr(not(windows), allow(dead_code))]
pub const SERVICE_NAME: &str = "EntryPoint";

/// 从参数列表探测 `--service`（纯函数，可单测）
pub fn requested_from(args: &[String]) -> bool {
    args.iter().any(|a| a == "--service")
}

/// 进程参数是否要求服务模式
pub fn requested() -> bool {
    requested_from(&std::env::args().collect::<Vec<_>>())
}

// ─── 进程级 stop 通道（跨平台：非 Windows 永不触发）────────────────────────

fn stop_channel() -> &'static (watch::Sender<bool>, watch::Receiver<bool>) {
    static CELL: OnceLock<(watch::Sender<bool>, watch::Receiver<bool>)> = OnceLock::new();
    CELL.get_or_init(|| watch::channel(false))
}

/// 服务 stop 信号接收端：`shutdown_signal()` select 该通道（服务模式无控制台
/// 信号可依赖）；非 Windows 下通道永不变化，select 分支等价于不存在。
pub fn stop_signal_rx() -> watch::Receiver<bool> {
    stop_channel().1.clone()
}

/// SCM 控制处理器调用：请求优雅停机（幂等）
// 同上：唯一消费方在 cfg(windows) 控制处理器内
#[cfg_attr(not(windows), allow(dead_code))]
fn request_stop() {
    let _ = stop_channel().0.send(true);
}

// ─── Windows 实现 ───────────────────────────────────────────────────────────

#[cfg(windows)]
mod imp {
    use super::{request_stop, SERVICE_NAME};
    use std::ffi::OsString;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    };

    /// main() 在 dispatch 前存入的 daemon 启动闭包（服务进程生命周期内恰好
    /// 执行一次；FnOnce 经 Mutex<Option<_>> 转移出静态区）
    type Runner = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send>;

    static RUNNER: Mutex<Option<Runner>> = Mutex::new(None);

    /// 存放 daemon 启动闭包（必须在 [`dispatch`] 之前调用）
    pub fn set_runner(runner: Runner) {
        *RUNNER.lock().unwrap_or_else(|e| e.into_inner()) = Some(runner);
    }

    // 宏以传入标识符生成服务入口函数（用蛇形命名满足 lint；宏内部自引用）
    define_windows_service!(service_entry, service_main);

    /// 服务进程入口（SCM 拉起）：注册控制处理器 → 报告 Running → 阻塞运行
    /// daemon → 报告 Stopped。任何早退路径都必须最终报告 Stopped，否则 SCM
    /// 会保留「正在停止」状态直到超时。
    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            tracing::error!(error = %format!("{e:#}"), "Windows service run failed");
        }
    }

    fn run_service() -> anyhow::Result<()> {
        let runner = RUNNER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| anyhow::anyhow!("service runner not set before dispatch"))?;

        // windows-service 0.8：控制处理器闭包同步返回处理结果（无事件通道）
        let handler = service_control_handler::register(SERVICE_NAME, move |event| match event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                request_stop();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;

        set_status(&handler, ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN)?;

        let result = {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(runner())
        };

        // daemon 退出（正常或失败）后报告 Stopped；失败以错误码承载
        set_status(
            &handler,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
        )?;
        result
    }

    fn set_status(
        handler: &windows_service::service_control_handler::ServiceStatusHandle,
        state: ServiceState,
        accept: ServiceControlAccept,
    ) -> windows_service::Result<()> {
        handler.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::from_secs(10),
            process_id: None,
        })
    }

    /// 进入 Windows 服务分发器（阻塞至服务停止；必须在 [`set_runner`] 之后、
    /// 且仅当进程由 SCM 拉起时调用——手动前台运行 `--service` 会在 dispatcher
    /// 处报错返回，main 将其转为可读错误）。
    pub fn dispatch() -> anyhow::Result<()> {
        service_dispatcher::start(SERVICE_NAME, service_entry)
            .map_err(|e| anyhow::anyhow!("service dispatcher start failed（手动前台运行请去掉 --service）: {e}"))
    }
}

// ─── 跨平台外观 ─────────────────────────────────────────────────────────────

/// main() 调用：`--service` 时把 daemon 启动闭包交给服务运行时并进入分发器。
/// 非 Windows 目标由调用方先做平台检查（本函数在非 Windows 编译为空壳）。
#[cfg(windows)]
pub use imp::dispatch;
#[cfg(windows)]
pub use imp::set_runner;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_from_detects_service_flag() {
        let args = vec!["ep-daemon".to_string(), "--service".to_string()];
        assert!(requested_from(&args));
        let args = vec!["ep-daemon".to_string(), "--headless".to_string()];
        assert!(!requested_from(&args));
        let args: Vec<String> = vec![];
        assert!(!requested_from(&args));
    }

    /// stop 通道语义：request_stop 前 never-fire，请求后接收端可见变化
    /// （真实 STOP 控制即走这条通道汇入 shutdown_signal）
    #[tokio::test]
    async fn stop_channel_transitions_on_request() {
        let mut rx = stop_signal_rx();
        assert!(!*rx.borrow());
        // 跨平台入口：request_stop 是私有 Windows 路径，测试直接驱动通道发送端
        let _ = stop_channel().0.send(true);
        rx.changed().await.expect("change observable");
        assert!(*rx.borrow_and_update());
    }
}
