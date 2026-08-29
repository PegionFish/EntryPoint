//! ep-pack — EntryPoint 整合包作者离线命令行工具（Wave 3 **C6**）。
//!
//! §4.7 SDK 表面：`ep-pack new / validate / build / import / info / export`。
//! 离线工具，不依赖 daemon；打包/导入编排复用 `ep-pack` 库 crate。
//!
//! 退出码约定（双平台一致，见 [`output`]）：
//! `0` 成功 / `1` 校验失败 / `2` 用法错误。

mod args;
mod commands;
mod output;
mod resolve;

use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
ep-pack — EntryPoint 整合包作者离线工具

用法:
  ep-pack <command> [options]

命令:
  new <dir>                脚手架整合包作者目录（清单模板 + pipelines/ + models/ + README）
  validate [dir]           校验清单 schema/semver/qualified_id/管线 §6.2/CHECKSUMS（缺省当前目录）
  build [dir] [-o <out>]   校验先行并打包为 .zip（缺省输出 <id>-<version>.zip 于当前目录）
  import <archive.zip>   导入 .zip 到目标根
      --root <dir>           目标 EP 应用根（缺省当前目录，按 models/ config/ runtime/ 布局）
      --modules-dir <dir>    模块清单目录（缺省 <root>/modules；resolve 匹配 qualified_id）
      --dry-run              只出适配/校验报告，不落位
  info <archive.zip|pack-id>
                           归档模式：清单摘要 + 文件清单 + CHECKSUMS 状态
                           注册表模式：--root 下已装包条目（runtime/packs/<id>.json）
      --root <dir>           注册表所在应用根（缺省当前目录）
  export <pack-id>         已装包重建 .zip
      --root <dir>           应用根（缺省当前目录）
      --modules-dir <dir>    模块清单目录（缺省 <root>/modules）
      -o, --output <out>     输出路径（缺省当前目录 <id>-<version>.zip）

全局选项:
  --json                   机器可读 JSON 输出（stdout 仅一个 JSON 对象）
  -h, --help               显示帮助

退出码: 0 成功 / 1 校验或操作失败 / 2 用法错误";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first() else {
        println!("ep-pack {VERSION}");
        println!("{USAGE}");
        return output::exit(output::EXIT_USAGE);
    };
    let rest = &argv[1..];
    match cmd.as_str() {
        "new" => commands::new::run(rest),
        "validate" => commands::validate::run(rest),
        "build" => commands::build::run(rest),
        "import" => commands::import::run(rest),
        "info" => commands::info::run(rest),
        "export" => commands::export::run(rest),
        "-h" | "--help" | "help" => {
            println!("ep-pack {VERSION}");
            println!("{USAGE}");
            output::exit(output::EXIT_OK)
        }
        "--version" => {
            println!("ep-pack {VERSION}");
            output::exit(output::EXIT_OK)
        }
        other => {
            eprintln!("error: unknown command '{other}'");
            eprintln!("{USAGE}");
            output::exit(output::EXIT_USAGE)
        }
    }
}
