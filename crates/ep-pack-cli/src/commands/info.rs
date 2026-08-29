//! `ep-pack info <archive.zip | pack-id> [--root <dir>]`
//!
//! - 位置参数是**已存在的文件** → 归档模式：流式读 `.zip`（不全量解包），
//!   输出清单摘要 + 文件清单 + CHECKSUMS 状态（逐文件重算 sha256 核对）。
//! - 否则按 **pack id** 在 `--root`（缺省当前目录）的已装包注册表
//!   （`runtime/packs/<id>.json`）查找 → 注册表模式。
//!
//! 归档文件不存在 / pack id 未安装 → 用法错误（退出码 2）；
//! 归档损坏 / 清单非法 / CHECKSUMS 核对失败 → 退出码 1。

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sha2::{Digest, Sha256};
use zip::ZipArchive;

use ep_pack::checksum::ChecksumTable;
use ep_pack::extract::MANIFEST_FILE_NAME;
use ep_pack::import::{list_installed_packs, InstalledPack};
use ep_pack::manifest::PackManifest;

use crate::args::{self, OptDef};
use crate::output::{self, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};

const USAGE: &str =
    "usage: ep-pack info <archive.zip | pack-id> [--root <dir>] [--json]";

/// 流式哈希缓冲：1 MiB。
const HASH_CHUNK_SIZE: usize = 1024 * 1024;

const CHECKSUMS_FILE_NAME: &str = "CHECKSUMS.toml";

pub fn run(argv: &[String]) -> ExitCode {
    let opts = [OptDef {
        name: "root",
        long: "--root",
        short: None,
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
    let positional = match parsed.positional_exact(1, USAGE) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return output::exit(EXIT_USAGE);
        }
    };
    let target = PathBuf::from(&positional[0]);
    let root = parsed
        .value("root")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    if target.is_file() {
        info_archive(&target, json)
    } else if target.exists() {
        output::fail(
            EXIT_USAGE,
            json,
            vec![format!("not an archive file: {}", target.display())],
        )
    } else {
        // 按 pack id 查注册表
        let target_str = positional[0].as_str();
        if target_str.contains('/') || target_str.contains('\\') {
            return output::fail(
                EXIT_USAGE,
                json,
                vec![format!("archive does not exist: {}", target.display())],
            );
        }
        info_installed(target_str, &root, json)
    }
}

// ─── 归档模式 ────────────────────────────────────────────────────────────────

/// 归档内文件条目（目录条目不入列）。
struct ArchiveFile {
    name: String,
    size: u64,
    sha256: String,
}

fn info_archive(path: &Path, json: bool) -> ExitCode {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return output::fail(
                EXIT_USAGE,
                json,
                vec![format!("cannot open {}: {e}", path.display())],
            )
        }
    };
    let mut archive = match ZipArchive::new(BufReader::new(file)) {
        Ok(a) => a,
        Err(e) => {
            return output::fail(
                EXIT_FAILURE,
                json,
                vec![format!("invalid or unreadable archive: {e}")],
            )
        }
    };

    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut checksums_text: Option<String> = None;
    let mut files: Vec<ArchiveFile> = Vec::new();
    let mut buf = vec![0u8; HASH_CHUNK_SIZE];

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                return output::fail(
                    EXIT_FAILURE,
                    json,
                    vec![format!("failed to read archive entry {i}: {e}")],
                )
            }
        };
        let name = entry.name().to_string();
        if entry.is_dir() {
            continue;
        }
        if name == CHECKSUMS_FILE_NAME {
            let mut text = String::new();
            if let Err(e) = entry.read_to_string(&mut text) {
                return output::fail(
                    EXIT_FAILURE,
                    json,
                    vec![format!("{CHECKSUMS_FILE_NAME} entry is not valid UTF-8: {e}")],
                );
            }
            checksums_text = Some(text);
            continue;
        }
        // 流式 sha256（数 GB 权重不整块进内存；清单同样入表核对，
        // CHECKSUMS 表含 ep-pack.toml 条目，仅排除 CHECKSUMS.toml 自身）
        let is_manifest = name == MANIFEST_FILE_NAME;
        let mut hasher = Sha256::new();
        let mut content: Vec<u8> = Vec::new();
        let mut size: u64 = 0;
        loop {
            let n = match entry.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    return output::fail(
                        EXIT_FAILURE,
                        json,
                        vec![format!("failed to read archive entry `{name}`: {e}")],
                    )
                }
            };
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            if is_manifest {
                content.extend_from_slice(&buf[..n]);
            }
            size += n as u64;
        }
        if is_manifest {
            manifest_bytes = Some(content);
        }
        files.push(ArchiveFile {
            name,
            size,
            sha256: hex::encode(hasher.finalize()),
        });
    }

    // 清单摘要
    let manifest: PackManifest = match manifest_bytes {
        Some(bytes) => {
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(e) => {
                    return output::fail(
                        EXIT_FAILURE,
                        json,
                        vec![format!("manifest entry is not valid UTF-8: {e}")],
                    )
                }
            };
            match toml::from_str(&text) {
                Ok(m) => m,
                Err(e) => {
                    return output::fail(
                        EXIT_FAILURE,
                        json,
                        vec![format!("archive manifest failed to parse: {e}")],
                    )
                }
            }
        }
        None => {
            return output::fail(
                EXIT_FAILURE,
                json,
                vec![format!("archive lacks manifest `{MANIFEST_FILE_NAME}`")],
            )
        }
    };

    // CHECKSUMS 状态：缺失 / 条目不符（missing / unexpected / mismatched）
    let mut missing: Vec<String> = Vec::new();
    let mut unexpected: Vec<String> = Vec::new();
    let mut mismatched: Vec<String> = Vec::new();
    let checksums_present = checksums_text.is_some();
    let checksum_entries: usize;
    match checksums_text {
        Some(text) => match ChecksumTable::from_toml_str(&text) {
            Ok(table) => {
                checksum_entries = table.len();
                for f in &files {
                    match table.get(&f.name) {
                        None => unexpected.push(f.name.clone()),
                        Some(expected) if expected != f.sha256 => mismatched.push(f.name.clone()),
                        Some(_) => {}
                    }
                }
                for (rel, _) in table.entries() {
                    if !files.iter().any(|f| f.name == rel) {
                        missing.push(rel.to_string());
                    }
                }
            }
            Err(e) => {
                return output::fail(
                    EXIT_FAILURE,
                    json,
                    vec![format!("{CHECKSUMS_FILE_NAME} failed to parse: {e}")],
                )
            }
        },
        None => {
            checksum_entries = 0;
        }
    }
    let checksums_ok = checksums_present && missing.is_empty() && unexpected.is_empty() && mismatched.is_empty();

    if json {
        output::print_json(&serde_json::json!({
            "ok": checksums_ok,
            "kind": "archive",
            "path": path.display().to_string(),
            "manifest": manifest_summary_json(&manifest),
            "files": files.iter().map(|f| serde_json::json!({
                "path": f.name,
                "size": f.size,
                "sha256": f.sha256,
            })).collect::<Vec<_>>(),
            "checksums": {
                "present": checksums_present,
                "ok": checksums_ok,
                "entries": checksum_entries,
                "missing": missing,
                "unexpected": unexpected,
                "mismatched": mismatched,
            },
        }));
        return output::exit(if checksums_ok { EXIT_OK } else { EXIT_FAILURE });
    }

    print_manifest_summary(&manifest);
    println!("archive files ({}):", files.len());
    for f in &files {
        println!("  {:>10}  {}", output::human_bytes(f.size), f.name);
    }
    if !checksums_present {
        println!("CHECKSUMS: MISSING (archive has no {CHECKSUMS_FILE_NAME})");
        return output::exit(EXIT_FAILURE);
    }
    if checksums_ok {
        println!("CHECKSUMS: OK ({checksum_entries} entries verified)");
        output::exit(EXIT_OK)
    } else {
        println!(
            "CHECKSUMS: FAILED ({} missing, {} unexpected, {} mismatched)",
            missing.len(),
            unexpected.len(),
            mismatched.len()
        );
        output::print_items("  missing: ", &missing);
        output::print_items("  unexpected: ", &unexpected);
        output::print_items("  mismatched: ", &mismatched);
        output::exit(EXIT_FAILURE)
    }
}

// ─── 注册表模式 ──────────────────────────────────────────────────────────────

fn info_installed(pack_id: &str, root: &Path, json: bool) -> ExitCode {
    let registry_dir = root.join("runtime").join("packs");
    let packs = match list_installed_packs(&registry_dir) {
        Ok(p) => p,
        Err(e) => {
            return output::fail(
                EXIT_FAILURE,
                json,
                vec![format!("failed to read registry: {e}")],
            )
        }
    };
    let pack: InstalledPack = match packs.iter().find(|p| p.id == pack_id) {
        Some(p) => p.clone(),
        None => {
            return output::fail(
                EXIT_USAGE,
                json,
                vec![format!(
                    "pack '{pack_id}' is not installed under {} (registry: {})",
                    root.display(),
                    registry_dir.display()
                )],
            )
        }
    };

    if json {
        output::print_json(&serde_json::json!({
            "ok": true,
            "kind": "installed",
            "pack": pack,
        }));
        return output::exit(EXIT_OK);
    }

    println!(
        "installed pack '{}' v{}",
        pack.id, pack.version
    );
    if let Some(n) = &pack.name {
        println!("  name: {n}");
    }
    if let Some(d) = &pack.description {
        println!("  description: {d}");
    }
    println!("  installed_at: {}", pack.installed_at);
    if pack.models.is_empty() {
        println!("models: (none)");
    } else {
        println!("models:");
        for m in &pack.models {
            println!(
                "  - {}@{} [{}]{}",
                m.qualified_id,
                m.variant,
                m.mode.as_str(),
                if m.tags.is_empty() {
                    String::new()
                } else {
                    format!(" tags: {}", m.tags.join(", "))
                }
            );
        }
    }
    if pack.pipelines.is_empty() {
        println!("pipelines: (none)");
    } else {
        println!("pipelines: {}", pack.pipelines.join(", "));
    }
    output::exit(EXIT_OK)
}

// ─── 清单摘要（两种模式的公共展示）──────────────────────────────────────────

fn manifest_summary_json(m: &PackManifest) -> serde_json::Value {
    serde_json::json!({
        "id": m.pack.id,
        "version": m.pack.version,
        "name": m.pack.name,
        "description": m.pack.description,
        "authors": m.pack.authors,
        "license": m.pack.license,
        "homepage": m.pack.homepage,
        "min_ep_version": m.pack.min_ep_version,
        "tags": m.pack.tags,
        "backends": m.compute.backends.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
        "models": m.models.iter().map(|e| serde_json::json!({
            "qualified_id": e.qualified_id,
            "variant": e.variant,
            "mode": e.mode.as_str(),
            "tags": e.tags,
        })).collect::<Vec<_>>(),
        "pipelines": m.pipelines.iter().map(|p| p.file.clone()).collect::<Vec<_>>(),
    })
}

fn print_manifest_summary(m: &PackManifest) {
    println!("pack '{}' v{}", m.pack.id, m.pack.version);
    println!("  name: {}", m.pack.name);
    println!("  description: {}", m.pack.description);
    if !m.pack.authors.is_empty() {
        println!("  authors: {}", m.pack.authors.join(", "));
    }
    if let Some(l) = &m.pack.license {
        println!("  license: {l}");
    }
    if let Some(h) = &m.pack.homepage {
        println!("  homepage: {h}");
    }
    if let Some(min) = &m.pack.min_ep_version {
        println!("  min_ep_version: {min}");
    }
    if !m.pack.tags.is_empty() {
        println!("  tags: {}", m.pack.tags.join(", "));
    }
    println!(
        "  backends: {}",
        m.compute
            .backends
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if m.models.is_empty() {
        println!("models: (none)");
    } else {
        println!("models:");
        for e in &m.models {
            println!(
                "  - {}@{} [{}]",
                e.qualified_id,
                e.variant,
                e.mode.as_str()
            );
        }
    }
    if m.pipelines.is_empty() {
        println!("pipelines: (none)");
    } else {
        println!("pipelines:");
        for p in &m.pipelines {
            println!("  - {}", p.file);
        }
    }
}
