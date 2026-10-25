//! 单实例锁的真实实现：Windows 命名互斥体（`CreateMutexW`）。
//!
//! 命名互斥体由 OS 持有、随进程退出（含被杀）自动销毁——无"陈旧锁文件"
//! 问题，是单实例检测的规范做法。`Local\` 前缀 = 每登录会话一份（多用户
//! 会话各自独立，托盘程序的正确作用域）。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

use crate::singleton::InstanceMutex;

/// 托盘单实例互斥体名。
pub const TRAY_MUTEX_NAME: &str = "Local\\EntryPoint.ep-tray.singleton";

/// 命名互斥体句柄持有者。构造即执行 `CreateMutexW`（创建/打开二合一），
/// `ERROR_ALREADY_EXISTS` 结果缓存在 `already_exists`（GetLastError 只在
/// 创建调用后立即可读）。
pub struct NamedMutex {
    /// 互斥体句柄：持有到 Drop / 进程退出，OS 释放
    handle: HANDLE,
    /// CreateMutexW 时互斥体已存在（= 已有实例）
    already_exists: bool,
}

impl NamedMutex {
    /// 创建（或打开既有）命名互斥体。返回 Err 仅限系统级失败（权限/内存）。
    pub fn acquire(name: &str) -> Result<Self, String> {
        let wide: Vec<u16> = OsStr::new(name).encode_wide().chain(Some(0)).collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        if handle.is_null() {
            return Err(format!(
                "CreateMutexW failed: GetLastError={}",
                unsafe { GetLastError() }
            ));
        }
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        Ok(Self {
            handle,
            already_exists,
        })
    }
}

impl InstanceMutex for NamedMutex {
    fn try_lock(&self) -> Result<bool, String> {
        // 创建成功且不存在同名互斥体 = 本进程是首个实例
        Ok(!self.already_exists)
    }
}

// SAFETY：HANDLE 是 OS 原生句柄，跨线程使用合法；本类型除 Drop 的
// CloseHandle 外不做任何句柄操作，且 Drop 只会在拥有它的线程上执行。
unsafe impl Send for NamedMutex {}
unsafe impl Sync for NamedMutex {}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { CloseHandle(self.handle) };
        }
    }
}
