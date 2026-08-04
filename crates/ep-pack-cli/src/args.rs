//! 手写子命令参数解析 — 现有依赖树无 clap（Cargo.lock 门禁），
//! 任务冻结约束「禁止新增重型 CLI crate」，故用 std::env::args 手写分发。
//!
//! 支持形状：
//! - 位置参数（裸参数）
//! - 长选项 `--root <值>` 与 `--root=<值>`
//! - 短选项 `-o <值>`
//! - 开关 `--dry-run`（无值）
//! - `--` 之后一律视为位置参数
//! - `-h` / `--help` 一律识别为开关 `help`（各命令自行打印用法）
//!
//! 未知选项 / 缺值 → `Err`（调用方按用法错误退出码 2 处理）。

use std::collections::{HashMap, HashSet};

/// 单个选项定义。
pub struct OptDef {
    /// 归一化名称（values/switches 的键）
    pub name: &'static str,
    /// 长选项（含 `--` 前缀）
    pub long: &'static str,
    /// 短选项（含 `-` 前缀）；None = 无短形
    pub short: Option<&'static str>,
    /// true = 需要跟随一个值；false = 布尔开关
    pub takes_value: bool,
}

/// 解析结果。
#[derive(Debug, Default)]
pub struct ParsedArgs {
    pub positional: Vec<String>,
    /// 选项名 → 值（仅 takes_value 选项）
    pub values: HashMap<String, String>,
    /// 开关名集合（含 `help`、`json`、`dry-run` 等）
    pub switches: HashSet<String>,
}

impl ParsedArgs {
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn switch(&self, name: &str) -> bool {
        self.switches.contains(name)
    }

    /// 恰好 n 个位置参数，否则 Err（用法错误文案素材）
    pub fn positional_exact(&self, n: usize, usage: &str) -> Result<&[String], String> {
        if self.positional.len() != n {
            return Err(format!(
                "expected exactly {n} positional argument(s), found {}: {usage}",
                self.positional.len()
            ));
        }
        Ok(&self.positional)
    }

    /// 至多 n 个位置参数
    pub fn positional_at_most(&self, n: usize, usage: &str) -> Result<&[String], String> {
        if self.positional.len() > n {
            return Err(format!(
                "expected at most {n} positional argument(s), found {}: {usage}",
                self.positional.len()
            ));
        }
        Ok(&self.positional)
    }
}

/// 各命令共用的全局开关：`--json`（机器可读输出）与 `-h/--help`。
const HELP_LONG: &str = "--help";
const HELP_SHORT: &str = "-h";
const JSON_LONG: &str = "--json";

fn find_opt<'a>(opts: &'a [OptDef], token: &str) -> Option<&'a OptDef> {
    opts.iter()
        .find(|o| o.long == token || o.short == Some(token))
}

/// 解析子命令参数（`args` 不含子命令名本身）。
pub fn parse(args: &[String], opts: &[OptDef]) -> Result<ParsedArgs, String> {
    let mut out = ParsedArgs::default();
    let mut after_separator = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if after_separator {
            out.positional.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--" {
            after_separator = true;
            i += 1;
            continue;
        }
        if arg == HELP_LONG || arg == HELP_SHORT {
            out.switches.insert("help".to_string());
            i += 1;
            continue;
        }
        if arg == JSON_LONG {
            out.switches.insert("json".to_string());
            i += 1;
            continue;
        }

        // `--name=value` 形状拆分
        let (token, inline_value): (&str, Option<String>) =
            if let Some((t, v)) = arg.split_once('=') {
                if !t.starts_with('-') || v.is_empty() {
                    return Err(format!("malformed option '{arg}'"));
                }
                (t, Some(v.to_string()))
            } else {
                (arg.as_str(), None)
            };

        if token.starts_with("--") || (token.starts_with('-') && token.len() > 1) {
            let opt = find_opt(opts, token)
                .ok_or_else(|| format!("unknown option '{token}'"))?;
            if opt.takes_value {
                let value = match inline_value {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| format!("option '{}' requires a value", opt.long))?
                    }
                };
                out.values.insert(opt.name.to_string(), value);
            } else {
                if inline_value.is_some() {
                    return Err(format!("option '{}' does not take a value", opt.long));
                }
                out.switches.insert(opt.name.to_string());
            }
            i += 1;
            continue;
        }

        out.positional.push(arg.clone());
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn test_opts() -> Vec<OptDef> {
        vec![
            OptDef {
                name: "root",
                long: "--root",
                short: None,
                takes_value: true,
            },
            OptDef {
                name: "output",
                long: "--output",
                short: Some("-o"),
                takes_value: true,
            },
            OptDef {
                name: "dry-run",
                long: "--dry-run",
                short: None,
                takes_value: false,
            },
        ]
    }

    #[test]
    fn positional_and_options_mixed() {
        let p = parse(
            &args(&[
                "pack.epzip", "--root", "C:\\ep", "-o", "out.epzip", "--dry-run", "--json",
            ]),
            &test_opts(),
        )
        .unwrap();
        assert_eq!(p.positional, vec!["pack.epzip"]);
        assert_eq!(p.value("root"), Some("C:\\ep"));
        assert_eq!(p.value("output"), Some("out.epzip"));
        assert!(p.switch("dry-run"));
        assert!(p.switch("json"));
    }

    #[test]
    fn long_option_inline_value() {
        let p = parse(&args(&["--root=/opt/ep"]), &test_opts()).unwrap();
        assert_eq!(p.value("root"), Some("/opt/ep"));
    }

    #[test]
    fn unknown_option_rejected() {
        let err = parse(&args(&["--frobnicate"]), &test_opts()).unwrap_err();
        assert!(err.contains("unknown option"), "{err}");
    }

    #[test]
    fn missing_value_rejected() {
        let err = parse(&args(&["--root"]), &test_opts()).unwrap_err();
        assert!(err.contains("requires a value"), "{err}");
    }

    #[test]
    fn separator_forces_positional() {
        let p = parse(&args(&["--", "--root", "x"]), &test_opts()).unwrap();
        assert_eq!(p.positional, vec!["--root", "x"]);
        assert!(p.value("root").is_none());
    }

    #[test]
    fn switch_with_inline_value_rejected() {
        let err = parse(&args(&["--dry-run=yes"]), &test_opts()).unwrap_err();
        assert!(err.contains("does not take a value"), "{err}");
    }

    #[test]
    fn positional_count_checks() {
        let p = parse(&args(&["a", "b"]), &test_opts()).unwrap();
        assert!(p.positional_exact(2, "u").is_ok());
        assert!(p.positional_exact(1, "u").is_err());
        assert!(p.positional_at_most(2, "u").is_ok());
        assert!(p.positional_at_most(1, "u").is_err());
    }
}
