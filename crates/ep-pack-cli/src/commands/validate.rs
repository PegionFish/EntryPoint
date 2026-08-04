//! `ep-pack validate [dir]` — 清单 schema + semver + qualified_id 语法 +
//! `pipelines.file` 存在性与 §6.2 结构校验 + CHECKSUMS（若存在）全量核对。
//!
//! 说明（任务冻结项）：ep-pack-cli 依赖 ep-core，其 `pipeline::Pipeline`
//! 提供完整 §6.2 节点/DAG 形状校验，故此处不止 TOML 语法层——管线文件按
//! `Pipeline::from_toml_str` + `Pipeline::validate()`（节点 id 唯一 / 边引用
//! 存在 / 无环 / 至少一个 file_input）逐级校验。
//!
//! 错误逐项列出；任一错误 → 退出码 1。

use std::path::Path;
use std::process::ExitCode;

use ep_core::pipeline::Pipeline;
use ep_pack::checksum::{ChecksumError, ChecksumTable, CHECKSUMS_FILE_NAME};
use ep_pack::extract::MANIFEST_FILE_NAME;
use ep_pack::manifest::{PackManifest, PackManifestError};

use crate::args;
use crate::commands::join_pack_rel;
use crate::output::{self, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};

const USAGE: &str = "usage: ep-pack validate [dir] [--json]";

/// 校验结果（build 命令先行校验复用）。
pub struct ValidationOutcome {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// 清单解析成功时的实例（build 复用其 id/version 生成默认输出名）
    pub manifest: Option<PackManifest>,
}

impl ValidationOutcome {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// 对包源目录执行全项校验（不打印、不退出，供 validate/build 复用）。
pub fn validate_pack_dir(dir: &Path) -> ValidationOutcome {
    let mut errors: Vec<String> = Vec::new();
    let warnings: Vec<String> = Vec::new();

    let manifest_path = dir.join(MANIFEST_FILE_NAME);
    let manifest = match PackManifest::from_file(&manifest_path) {
        Ok(m) => Some(m),
        Err(PackManifestError::Io(e)) => {
            errors.push(format!(
                "cannot read manifest {}: {e}",
                manifest_path.display()
            ));
            None
        }
        Err(PackManifestError::Parse(e)) => {
            errors.push(format!("manifest TOML parse failed: {e}"));
            None
        }
        Err(PackManifestError::Validation(list)) => {
            // from_file 不产生 Validation 变体；防御性收编
            errors.extend(list);
            None
        }
    };

    let Some(manifest) = manifest else {
        return ValidationOutcome {
            errors,
            warnings,
            manifest: None,
        };
    };

    // 1) schema + semver + qualified_id + pipelines.file 词法（库校验）
    if let Err(list) = manifest.validate() {
        errors.extend(list);
    }

    // 2) pipelines.file 存在性 + §6.2 结构校验
    for (i, pref) in manifest.pipelines.iter().enumerate() {
        let Some(path) = join_pack_rel(dir, &pref.file) else {
            errors.push(format!(
                "pipelines[{i}].file '{}' escapes the pack root",
                pref.file
            ));
            continue;
        };
        if !path.is_file() {
            errors.push(format!(
                "pipelines[{i}].file '{}' does not exist at {}",
                pref.file,
                path.display()
            ));
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!(
                    "pipelines[{i}].file '{}' cannot be read: {e}",
                    pref.file
                ));
                continue;
            }
        };
        match Pipeline::from_toml_str(&text) {
            Ok(pipeline) => {
                if let Err(verrs) = pipeline.validate() {
                    for ve in verrs {
                        errors.push(format!("pipelines[{i}].file '{}': {ve}", pref.file));
                    }
                }
            }
            Err(e) => errors.push(format!(
                "pipelines[{i}].file '{}' is not a valid §6.2 pipeline TOML: {e}",
                pref.file
            )),
        }
    }

    // 3) CHECKSUMS.toml 若存在 → 全量核对（缺失/多余/篡改逐项列出）
    if dir.join(CHECKSUMS_FILE_NAME).is_file() {
        match ChecksumTable::read(dir) {
            Ok(table) => match table.verify(dir) {
                Ok(()) => {}
                Err(ChecksumError::Integrity(report)) => {
                    for p in &report.missing {
                        errors.push(format!("CHECKSUMS: listed file missing on disk: `{p}`"));
                    }
                    for p in &report.unexpected {
                        errors.push(format!("CHECKSUMS: file on disk not listed: `{p}`"));
                    }
                    for m in &report.mismatched {
                        errors.push(format!(
                            "CHECKSUMS: sha256 mismatch for `{}` (expected {}, actual {})",
                            m.path, m.expected, m.actual
                        ));
                    }
                }
                Err(e) => errors.push(format!("CHECKSUMS verification failed: {e}")),
            },
            Err(e) => errors.push(format!("failed to read {CHECKSUMS_FILE_NAME}: {e}")),
        }
    }

    ValidationOutcome {
        errors,
        warnings,
        manifest: Some(manifest),
    }
}

pub fn run(argv: &[String]) -> ExitCode {
    let parsed = match args::parse(argv, &[]) {
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
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if !dir.is_dir() {
        return output::fail(
            EXIT_USAGE,
            json,
            vec![format!("pack dir does not exist: {}", dir.display())],
        );
    }

    let outcome = validate_pack_dir(&dir);

    if json {
        output::print_json(&serde_json::json!({
            "ok": outcome.ok(),
            "dir": dir.display().to_string(),
            "errors": outcome.errors,
            "warnings": outcome.warnings,
        }));
        return output::exit(if outcome.ok() { EXIT_OK } else { EXIT_FAILURE });
    }

    if outcome.ok() {
        for w in &outcome.warnings {
            println!("warning: {w}");
        }
        println!("validate OK: {}", dir.display());
        output::exit(EXIT_OK)
    } else {
        eprintln!(
            "validate FAILED: {} ({} error(s))",
            dir.display(),
            outcome.errors.len()
        );
        for e in &outcome.errors {
            eprintln!("  - {e}");
        }
        for w in &outcome.warnings {
            eprintln!("  warning: {w}");
        }
        output::exit(EXIT_FAILURE)
    }
}
