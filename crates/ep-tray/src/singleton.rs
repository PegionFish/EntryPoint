//! 单实例锁（T-33）——抽象 + 纯门，真实实现（Windows 命名互斥体）在
//! `platform::mutex`（`#[cfg(windows)]`），测试用内存实现注入。
//!
//! 语义：命名互斥体由 OS 持有、随进程退出自动释放，无"陈旧锁文件"问题。

/// 实例互斥体抽象（IO 注入点）。
///
/// 实现方约定：`try_lock` 返回 `Ok(true)` 后，锁的生命周期 = 实现对象/进程
/// 存续期；重复调用返回 `Ok(false)`。
pub trait InstanceMutex: Send + Sync + 'static {
    /// 尝试持有单实例锁。
    ///
    /// - `Ok(true)`：首次持有（本进程是唯一实例）；
    /// - `Ok(false)`：已有实例在运行（本进程应静默退出）；
    /// - `Err`：系统级失败（无法判定，调用方应保守按"已运行"处理并记录）。
    fn try_lock(&self) -> Result<bool, String>;
}

/// 单实例判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SingletonOutcome {
    /// 首个实例，可以继续启动
    Acquired,
    /// 已有实例在运行，本进程应退出
    AlreadyRunning,
    /// 系统级失败（无法判定；保守处理为不启动）
    Error(String),
}

/// 单实例判定纯门：互斥体实现注入，逻辑可穷举单测。
pub fn acquire_single_instance(mutex: &dyn InstanceMutex) -> SingletonOutcome {
    match mutex.try_lock() {
        Ok(true) => SingletonOutcome::Acquired,
        Ok(false) => SingletonOutcome::AlreadyRunning,
        Err(e) => SingletonOutcome::Error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// 内存互斥体：CAS 语义模拟命名互斥体（真实实现随进程释放，测试显式释放）
    #[derive(Clone)]
    struct MemoryMutex {
        held: Arc<AtomicBool>,
    }

    impl MemoryMutex {
        fn new() -> Self {
            Self {
                held: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl InstanceMutex for MemoryMutex {
        fn try_lock(&self) -> Result<bool, String> {
            Ok(!self.held.swap(true, Ordering::SeqCst))
        }
    }

    #[test]
    fn first_instance_acquires_second_is_rejected() {
        let m = MemoryMutex::new();
        assert_eq!(acquire_single_instance(&m), SingletonOutcome::Acquired);
        // 第二个实例（模拟双开）
        assert_eq!(
            acquire_single_instance(&m),
            SingletonOutcome::AlreadyRunning
        );
    }

    #[test]
    fn error_surface_maps_to_error_outcome() {
        struct BrokenMutex;
        impl InstanceMutex for BrokenMutex {
            fn try_lock(&self) -> Result<bool, String> {
                Err("access denied".to_string())
            }
        }
        assert_eq!(
            acquire_single_instance(&BrokenMutex),
            SingletonOutcome::Error("access denied".to_string())
        );
    }
}
