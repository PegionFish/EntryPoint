//! `ep-pack build [dir] [-o out.epzip]` — 校验先行，随后 `build_pack`
//! 确定性打包（§4.5）。
//!
//! 缺省输出路径：当前工作目录下 `<pack.id>-<pack.version>.epzip`；
//! 输出不得位于源目录内（build_pack 护栏）。

use std::path::PathBuf;
use std::process::ExitCode;

use ep_pack::build::{build_pack, BuildPlan};

use crate::args::{self, OptDef};
use crate::commands::validate::validate_pack_dir;
use crate::output::{self, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};

const USAGE: &str = "usage: ep-pack build [dir] [-o|--output <file.epzip>] [--json]";

pub fn run(argv: &[String]) -> ExitCode {
    let opts = [OptDef {
        name: "output",
        long: "--output",
        short: Some("-o"),
        takes_value: true,
    }];
    let parsed = match args::parse(argv, &opts) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("{USAGE}");
            return output::exit(EXIT_USAGE);
        }
    };
    let json = parsed.switch("json");
    if parsed.switch("help") {
        println!("{USAGE}");
        return output::exit(EXIT_OK);
    }
    let positional = match parsed.positional_at_most(1, USAGE) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return output::exit(EXIT_USAGE);
        }
    };
    let dir = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if !dir.is_dir() {
        return output::fail(
            EXIT_USAGE,
            json,
            vec![format!("pack dir does not exist: {}", dir.display())],
        );
    }

    // 校验先行：任何 validate 错误都不得产出归档
    let outcome = validate_pack_dir(&dir);
    if !outcome.ok() {
        if json {
            output::print_json(&serde_json::json!({
                "ok": false,
                "stage": "validate",
                "errors": outcome.errors,
                "warnings": outcome.warnings,
            }));
        } else {
            eprintln!(
                "build aborted: validation failed ({} error(s))",
                outcome.errors.len()
            );
            for e in &outcome.errors {
                eprintln!("  - {e}");
            }
        }
        return output::exit(EXIT_FAILURE);
    }
    // validate 通过 → manifest 必然存在
    let manifest = outcome.manifest.expect("manifest present after validation");

    let output_path = match parsed.value("output") {
        Some(o) => PathBuf::from(o),
        None => std::env::current_dir()
            .unwrap_or_default()
            .join(format!("{}-{}.epzip", manifest.pack.id, manifest.pack.version)),
    };

    match build_pack(&BuildPlan::new(&dir, &output_path)) {
        Ok(summary) => {
            if json {
                output::print_json(&serde_json::json!({
                    "ok": true,
                    "archive": summary.archive_path.display().to_string(),
                    "file_count": summary.file_count,
                    "total_bytes": summary.total_bytes,
                    "checksum_entries": summary.checksums.len(),
                    "warnings": outcome.warnings,
                }));
            } else {
                output::print_items("warning: ", &outcome.warnings);
                println!(
                    "built {} ({} files, {})",
                    summary.archive_path.display(),
                    summary.file_count,
                    output::human_bytes(summary.total_bytes)
                );
            }
            output::exit(EXIT_OK)
        }
        Err(e) => output::fail(EXIT_FAILURE, json, vec![format!("build failed: {e}")]),
    }
}
