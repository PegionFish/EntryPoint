//! 开机自启开关（T-33）——Run 键抽象 + 纯逻辑，真实实现（winreg 读写
//! HKCU Run 键）在 `platform::registry`（`#[cfg(windows)]`），测试注入内存实现。
//!
//! 约定：Run 键值名 `EntryPointTray`，数据 `"<ep-tray.exe 绝对路径>" --minimized`
//! （登录启动时托盘静默，不自动打开浏览器）。

/// HKCU Run 键存储抽象（IO 注入点）。
pub trait RunKeyStore {
    fn get(&self, value_name: &str) -> Result<Option<String>, String>;
    fn set(&self, value_name: &str, data: &str) -> Result<(), String>;
    fn remove(&self, value_name: &str) -> Result<(), String>;
}

/// Run 键值名（HKCU\Software\Microsoft\Windows\CurrentVersion\Run 之下）。
pub const RUN_VALUE_NAME: &str = "EntryPointTray";

/// Run 键数据：带引号的 exe 绝对路径 + `--minimized`（登录静默启动）。
pub fn autostart_command(exe_path: &std::path::Path) -> String {
    format!("\"{}\" --minimized", exe_path.display())
}

/// 自启当前是否开启（Run 键存在即视为开启）。
pub fn is_autostart_enabled(store: &dyn RunKeyStore) -> Result<bool, String> {
    Ok(store.get(RUN_VALUE_NAME)?.is_some())
}

/// 设置/取消开机自启（幂等：重复设置/取消不报错）。
pub fn set_autostart(
    store: &dyn RunKeyStore,
    exe_path: &std::path::Path,
    enabled: bool,
) -> Result<(), String> {
    if enabled {
        store.set(RUN_VALUE_NAME, &autostart_command(exe_path))
    } else {
        // remove 不存在时不报错（幂等）——由实现方保证
        store.remove(RUN_VALUE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 内存 Run 键（测试注入）
    struct MemStore {
        values: Mutex<HashMap<String, String>>,
    }

    impl MemStore {
        fn new() -> Self {
            Self {
                values: Mutex::new(HashMap::new()),
            }
        }
    }

    impl RunKeyStore for MemStore {
        fn get(&self, value_name: &str) -> Result<Option<String>, String> {
            Ok(self.values.lock().unwrap().get(value_name).cloned())
        }
        fn set(&self, value_name: &str, data: &str) -> Result<(), String> {
            self.values
                .lock()
                .unwrap()
                .insert(value_name.to_string(), data.to_string());
            Ok(())
        }
        fn remove(&self, value_name: &str) -> Result<(), String> {
            self.values.lock().unwrap().remove(value_name);
            Ok(())
        }
    }

    #[test]
    fn autostart_command_quotes_path_and_appends_minimized() {
        let cmd = autostart_command(&std::path::PathBuf::from(r"C:\Program Files\EntryPoint\ep-tray.exe"));
        assert_eq!(
            cmd,
            r#""C:\Program Files\EntryPoint\ep-tray.exe" --minimized"#,
            "带空格路径必须加引号，且携带 --minimized"
        );
    }

    #[test]
    fn set_then_query_enabled_and_disable() {
        let store = MemStore::new();
        assert!(!is_autostart_enabled(&store).unwrap());

        set_autostart(&store, &std::path::PathBuf::from("C:\\x\\ep-tray.exe"), true).unwrap();
        assert!(is_autostart_enabled(&store).unwrap());
        assert_eq!(
            store.get(RUN_VALUE_NAME).unwrap().unwrap(),
            r#""C:\x\ep-tray.exe" --minimized"#
        );

        set_autostart(&store, &std::path::PathBuf::from("C:\\x\\ep-tray.exe"), false).unwrap();
        assert!(!is_autostart_enabled(&store).unwrap());
        assert_eq!(store.get(RUN_VALUE_NAME).unwrap(), None);
    }

    #[test]
    fn set_autostart_is_idempotent() {
        let store = MemStore::new();
        let exe = std::path::PathBuf::from("C:\\x\\ep-tray.exe");
        set_autostart(&store, &exe, true).unwrap();
        set_autostart(&store, &exe, true).unwrap();
        assert!(is_autostart_enabled(&store).unwrap());
        set_autostart(&store, &exe, false).unwrap();
        set_autostart(&store, &exe, false).unwrap(); // 取消不存在键不报错
        assert!(!is_autostart_enabled(&store).unwrap());
    }
}
