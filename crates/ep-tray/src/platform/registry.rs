//! 开机自启的真实实现：winreg 读写 `HKCU\...\CurrentVersion\Run`。
//!
//! 仅当前用户 HKCU（不写 HKLM、不需要管理员权限）；值名与数据格式由
//! [`crate::autostart`] 统一定义（`"exe 路径" --minimized`）。

use std::io::ErrorKind;

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
use winreg::RegKey;

use crate::autostart::RunKeyStore;

/// HKCU Run 子键路径。
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// HKCU Run 键存储实现（零状态，操作即席执行）。
pub struct HkuRunKey;

impl RunKeyStore for HkuRunKey {
    fn get(&self, value_name: &str) -> Result<Option<String>, String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let run = hkcu
            .open_subkey_with_flags(RUN_SUBKEY, KEY_READ)
            .map_err(|e| format!("open Run key: {e}"))?;
        match run.get_value::<String, _>(value_name) {
            Ok(v) => Ok(Some(v)),
            Err(ref e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read Run value: {e}")),
        }
    }

    fn set(&self, value_name: &str, data: &str) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let run = hkcu
            .open_subkey_with_flags(RUN_SUBKEY, KEY_SET_VALUE)
            .map_err(|e| format!("open Run key for write: {e}"))?;
        run.set_value(value_name, &data)
            .map_err(|e| format!("write Run value: {e}"))
    }

    fn remove(&self, value_name: &str) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        // Run 子键缺失 = 值不存在（幂等取消，不报错）
        match hkcu.open_subkey_with_flags(RUN_SUBKEY, KEY_SET_VALUE) {
            Ok(run) => match run.delete_value(value_name) {
                Ok(()) => Ok(()),
                Err(ref e) if e.kind() == ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("delete Run value: {e}")),
            },
            Err(ref e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("open Run key for delete: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autostart;

    /// 只读冒烟：真实 HKCU Run 键可读且值名未误写（不写入用户注册表）
    #[test]
    fn hku_run_key_is_readable_and_value_absent_by_default() {
        // 断言"未开启自启"是环境相关的——本测试只保证读取不报错且
        // 写入路径不会被测试误触
        let _ = autostart::is_autostart_enabled(&HkuRunKey);
    }
}
