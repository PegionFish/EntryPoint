//! 整合包构建（`POST /api/packs/build` 与 CLI `ep-pack build` 共用）—
//! 把备妥的包内容目录打包为 `.epzip`（§4.5）。
//!
//! 实现所有者：Wave 1 **A4 (PackIO)**。
//!
//! 输入约定：`source_dir` 已含 `ep-pack.toml`（清单生成属 A3/编排层职责）
//! 与 `models/`、`pipelines/` 等布局（§4.2）。本函数负责：
//! 1. 源侧安全检查（拒绝 symlink / 非普通文件，见 [`crate::checksum`]）；
//! 2. 逐文件 sha256 → 生成 `CHECKSUMS.toml` 条目（仅写入归档，不改动源目录）；
//! 3. 按确定性排序写 zip：目录条目在前，文件条目（含 CHECKSUMS.toml）按
//!    条目名字典序，固定时间戳（zip 纪元 1980-01-01）与固定权限
//!    → 同样内容两次打包字节一致（可复现构建）。
//!
//! 归档条目名一律 `/` 分隔的相对路径（zip 规范 + §4.2）。

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::checksum::{
    sha256_file, walk_pack_tree, ChecksumError, ChecksumTable, CHECKSUMS_FILE_NAME,
};
use crate::extract::MANIFEST_FILE_NAME;

/// 归档写盘缓冲：1 MiB。
const BUILD_CHUNK_SIZE: usize = 1024 * 1024;

/// 文件条目固定权限：普通文件 0644（含 S_IFREG 类型位）。
const FILE_MODE: u32 = 0o100644;
/// 目录条目固定权限基础位（zip crate `add_directory` 会再并入 S_IFDIR）。
const DIR_MODE: u32 = 0o755;

/// 打包请求描述：备妥的包内容目录 → 目标 `.epzip` 路径。
///
/// （模型圈选 / 管线选择 / bundle-reference 决策由编排层完成并布局到
/// `source_dir`；见 §4.5 与 B2/C6 的消费面。）
#[derive(Debug, Clone)]
pub struct BuildPlan {
    /// 包内容目录（必须已含 `ep-pack.toml`）
    pub source_dir: PathBuf,
    /// 输出 `.epzip` 路径（不得位于 source_dir 内）
    pub output_path: PathBuf,
}

impl BuildPlan {
    pub fn new(source_dir: impl Into<PathBuf>, output_path: impl Into<PathBuf>) -> Self {
        Self {
            source_dir: source_dir.into(),
            output_path: output_path.into(),
        }
    }
}

/// 打包结果摘要。
#[derive(Debug, Clone)]
pub struct BuildSummary {
    /// 生成的 `.epzip` 路径
    pub archive_path: PathBuf,
    /// 归档文件条目数（含 CHECKSUMS.toml）
    pub file_count: usize,
    /// 源文件未压缩总字节（不含生成的 CHECKSUMS.toml）
    pub total_bytes: u64,
    /// 随包写入的校验和表
    pub checksums: ChecksumTable,
}

/// 打包错误。
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("pack source dir does not exist or is not a directory: {}", path.display())]
    SourceDirMissing { path: PathBuf },
    #[error("pack source dir lacks manifest `ep-pack.toml`: {}", path.display())]
    ManifestMissing { path: PathBuf },
    #[error("output path {} must not live inside pack source dir {}", output.display(), src_dir.display())]
    OutputInsideSource {
        output: PathBuf,
        src_dir: PathBuf,
    },
    #[error("pack build io error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("zip write failed: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Checksum(#[from] ChecksumError),
}

/// 固定条目时间戳：zip 纪元 1980-01-01 00:00:00（可复现构建）。
fn fixed_mtime() -> DateTime {
    DateTime::default()
}

fn file_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(fixed_mtime())
        .unix_permissions(FILE_MODE)
}

fn dir_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .last_modified_time(fixed_mtime())
        .unix_permissions(DIR_MODE)
}

/// 把 `source_dir` 打包为 `.epzip`（见模块级文档）。
pub fn build_pack(plan: &BuildPlan) -> Result<BuildSummary, BuildError> {
    let source = &plan.source_dir;
    if !source.is_dir() {
        return Err(BuildError::SourceDirMissing {
            path: source.clone(),
        });
    }
    if !source.join(MANIFEST_FILE_NAME).is_file() {
        return Err(BuildError::ManifestMissing {
            path: source.clone(),
        });
    }
    ensure_output_outside_source(source, &plan.output_path)?;

    // 一次遍历（含安全检查）+ 逐文件哈希 → 校验和表
    let tree = walk_pack_tree(source)?;
    let mut entries = std::collections::BTreeMap::new();
    for (rel, path) in &tree.files {
        let digest = sha256_file(path).map_err(|source| BuildError::Io {
            path: path.clone(),
            source,
        })?;
        entries.insert(rel.clone(), digest);
    }
    let checksums = ChecksumTable::from_entries(entries);
    let checksums_toml = checksums.to_toml_string()?;

    if let Some(parent) = plan.output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| BuildError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let out_file = File::create(&plan.output_path).map_err(|source| BuildError::Io {
        path: plan.output_path.clone(),
        source,
    })?;
    let mut zip = ZipWriter::new(BufWriter::new(out_file));

    // 1) 目录条目（含空目录），排序确定
    for dir in &tree.dirs {
        zip.add_directory(dir, dir_options())?;
    }

    // 2) 文件条目 + CHECKSUMS.toml，按条目名全局字典序写入
    let mut names: Vec<String> = tree.files.keys().cloned().collect();
    names.push(CHECKSUMS_FILE_NAME.to_string());
    names.sort();

    let mut total_bytes: u64 = 0;
    let mut buf = vec![0u8; BUILD_CHUNK_SIZE];
    for name in &names {
        if name == CHECKSUMS_FILE_NAME {
            zip.start_file(name.as_str(), file_options())?;
            zip.write_all(checksums_toml.as_bytes()).map_err(|source| {
                BuildError::Io {
                    path: plan.output_path.clone(),
                    source,
                }
            })?;
            continue;
        }
        let src_path = &tree.files[name];
        zip.start_file(name.as_str(), file_options())?;
        let mut input = File::open(src_path).map_err(|source| BuildError::Io {
            path: src_path.clone(),
            source,
        })?;
        loop {
            let n = input.read(&mut buf).map_err(|source| BuildError::Io {
                path: src_path.clone(),
                source,
            })?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n]).map_err(|source| BuildError::Io {
                path: plan.output_path.clone(),
                source,
            })?;
            total_bytes += n as u64;
        }
    }

    zip.finish()?;

    Ok(BuildSummary {
        archive_path: plan.output_path.clone(),
        file_count: tree.files.len() + 1, // + CHECKSUMS.toml
        total_bytes,
        checksums,
    })
}

/// 输出路径不得位于源目录内（否则首次遍历会把归档自身/旧产物打包进去）。
///
/// 尽力规范化比较（输出文件可能尚不存在：向上找最近存在的祖先做
/// canonicalize）。注意 Windows 文件系统大小写不敏感而 `starts_with` 为
/// 逐组件精确比较——此处是防呆护栏而非安全边界，足够覆盖常规误用。
fn ensure_output_outside_source(source: &Path, output: &Path) -> Result<(), BuildError> {
    let src_canon = std::fs::canonicalize(source).map_err(|e| BuildError::Io {
        path: source.to_path_buf(),
        source: e,
    })?;
    let out_abs = if output.is_absolute() {
        output.to_path_buf()
    } else {
        let cwd = std::env::current_dir().map_err(|e| BuildError::Io {
            path: output.to_path_buf(),
            source: e,
        })?;
        cwd.join(output)
    };
    let out_canon = canonicalize_best_effort(&out_abs);
    if out_canon.starts_with(&src_canon) {
        return Err(BuildError::OutputInsideSource {
            output: output.to_path_buf(),
            src_dir: source.to_path_buf(),
        });
    }
    Ok(())
}

/// 规范化路径；目标不存在时向上找最近可规范化的祖先再拼回剩余段。
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&existing) {
            let mut out = c;
            for seg in tail.into_iter().rev() {
                out.push(seg);
            }
            return out;
        }
        match existing.file_name() {
            Some(seg) => {
                tail.push(seg.to_os_string());
                existing.pop();
            }
            None => return path.to_path_buf(),
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use zip::ZipArchive;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-pack-build-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_file(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// 标准测试源目录：清单 + bundle 权重 + 管线 + 空目录
    fn sample_source(root: &Path) -> PathBuf {
        let src = root.join("src");
        write_file(&src.join("ep-pack.toml"), b"[pack]\nid = \"t.p\"\nversion = \"1.0.0\"\n");
        write_file(&src.join("models").join("m1").join("weights.bin"), b"weights-data");
        write_file(&src.join("pipelines").join("video_to_srt.toml"), b"[pipeline]\n");
        std::fs::create_dir_all(src.join("models").join("empty-dir")).unwrap();
        src
    }

    #[test]
    fn build_roundtrip_and_entry_names() {
        let root = unique_root("roundtrip");
        let src = sample_source(&root);
        let out = root.join("out").join("t.p-1.0.0.epzip");

        let summary = build_pack(&BuildPlan::new(&src, &out)).unwrap();
        assert_eq!(summary.archive_path, out);
        assert_eq!(summary.file_count, 4); // 3 源文件 + CHECKSUMS.toml
        assert_eq!(summary.total_bytes, b"weights-data".len() as u64
            + b"[pack]\nid = \"t.p\"\nversion = \"1.0.0\"\n".len() as u64
            + b"[pipeline]\n".len() as u64);
        assert_eq!(summary.checksums.len(), 3);

        // 归档内容：条目名恒正斜杠、含清单/校验和/空目录条目
        let file = File::open(&out).unwrap();
        let mut archive = ZipArchive::new(std::io::BufReader::new(file)).unwrap();
        let mut names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        for n in &names {
            assert!(!n.contains('\\'), "entry `{n}` has backslash");
            assert!(!n.starts_with('/'), "entry `{n}` is absolute");
        }
        assert!(names.contains(&"ep-pack.toml".to_string()));
        assert!(names.contains(&"CHECKSUMS.toml".to_string()));
        assert!(names.contains(&"models/m1/weights.bin".to_string()));
        assert!(names.contains(&"models/empty-dir/".to_string()));

        // 文件条目名（去目录）字典序有序 → 确定性布局
        names.retain(|n| !n.ends_with('/'));
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);

        // 归档内 CHECKSUMS.toml 可解析且与 summary 一致
        let mut entry = archive.by_name("CHECKSUMS.toml").unwrap();
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        let parsed = ChecksumTable::from_toml_str(&text).unwrap();
        assert_eq!(parsed, summary.checksums);
        drop(entry);
        drop(archive);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_is_deterministic_byte_for_byte() {
        let root = unique_root("deterministic");
        let src = sample_source(&root);
        let out1 = root.join("a.epzip");
        let out2 = root.join("b.epzip");

        build_pack(&BuildPlan::new(&src, &out1)).unwrap();
        build_pack(&BuildPlan::new(&src, &out2)).unwrap();

        let b1 = std::fs::read(&out1).unwrap();
        let b2 = std::fs::read(&out2).unwrap();
        assert_eq!(b1, b2, "same source must produce identical archive bytes");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_requires_manifest() {
        let root = unique_root("no-manifest");
        let src = root.join("src");
        write_file(&src.join("models").join("m.bin"), b"x"); // 无 ep-pack.toml
        let err = build_pack(&BuildPlan::new(&src, root.join("o.epzip"))).unwrap_err();
        assert!(matches!(err, BuildError::ManifestMissing { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_requires_existing_source_dir() {
        let root = unique_root("no-src");
        let err = build_pack(&BuildPlan::new(root.join("missing"), root.join("o.epzip")))
            .unwrap_err();
        assert!(matches!(err, BuildError::SourceDirMissing { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_rejects_output_inside_source() {
        let root = unique_root("out-inside");
        let src = sample_source(&root);
        let err = build_pack(&BuildPlan::new(&src, src.join("self.epzip"))).unwrap_err();
        assert!(matches!(err, BuildError::OutputInsideSource { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn build_rejects_symlink_in_source() {
        let root = unique_root("symlink");
        let src = sample_source(&root);
        std::os::unix::fs::symlink(
            src.join("models").join("m1").join("weights.bin"),
            src.join("models").join("link.bin"),
        )
        .unwrap();
        let err = build_pack(&BuildPlan::new(&src, root.join("o.epzip"))).unwrap_err();
        assert!(
            matches!(err, BuildError::Checksum(ChecksumError::SymlinkInSource { .. })),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
