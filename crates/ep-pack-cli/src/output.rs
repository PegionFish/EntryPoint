//! CLI 公共约定 — 退出码、输出助手。
//!
//! 实现所有者：Wave 3 **C6 (PackCLI)**。
//!
//! # 退出码约定（双平台一致）
//!
//! - `0` 成功
//! - `1` 校验/完整性/操作失败（validate 错误列表、checksum 不符、导入失败等）
//! - `2` 用法错误（未知子命令/参数缺失/指定的文件或注册表条目不存在）
//!
//! # 输出约定
//!
//! - 人类可读文本走 stdout（错误提示走 stderr）；
//! - `--json` 时 stdout 只输出一个机器可读 JSON 对象（成功与失败均如此，
//!   失败对象带 `"ok": false` 与 `errors` 列表），供脚本消费。

use std::process::ExitCode;

/// 成功
pub const EXIT_OK: u8 = 0;
/// 校验/完整性/操作失败
pub const EXIT_FAILURE: u8 = 1;
/// 用法错误
pub const EXIT_USAGE: u8 = 2;

pub fn exit(code: u8) -> ExitCode {
    ExitCode::from(code)
}

/// `--json` 模式下输出机器可读 JSON（pretty，尾随换行）。
pub fn print_json(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        // Value 序列化实际不会失败；防御性降级为手工拼接
        Err(_) => println!("{{\"ok\":false,\"errors\":[\"json serialization failed\"]}}"),
    }
}

/// `--json` 模式下输出失败对象并返回退出码。
pub fn fail_json(code: u8, errors: Vec<String>) -> ExitCode {
    print_json(&serde_json::json!({ "ok": false, "errors": errors }));
    exit(code)
}

/// 人类可读模式：输出错误列表到 stderr 并返回退出码。
pub fn fail_human(code: u8, errors: &[String]) -> ExitCode {
    for e in errors {
        eprintln!("error: {e}");
    }
    exit(code)
}

/// 按输出模式统一处理失败：`json` → JSON 对象；否则 stderr 错误列表。
pub fn fail(code: u8, json: bool, errors: Vec<String>) -> ExitCode {
    if json {
        fail_json(code, errors)
    } else {
        fail_human(code, &errors)
    }
}

/// 人类可读模式输出校验/警告条目列表（各一行，带前缀）。
pub fn print_items(prefix: &str, items: &[String]) {
    for item in items {
        println!("{prefix}{item}");
    }
}

/// 字节数人类可读形式（info/build 摘要用）。
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut idx = 0;
    while v >= 1024.0 && idx < UNITS.len() - 1 {
        v /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
