//! 整合包归档解包与路径安全 — 冻结契约见计划 §4.4（暂存 → 解包 → CHECKSUMS 校验）。
//!
//! 容器为标准归档格式：**zip 或 tar.gz**（按文件魔数嗅探，不依赖扩展名），
//! 归档布局契约（根级 `ep-pack.toml` + `CHECKSUMS.toml` + `models/` +
//! `pipelines/`）见 docs/PACK_AUTHORING.md §2。
//!
//! 实现所有者：Wave 1 **A4 (PackIO)**。
//!
//! 安全基线（与 ep-daemon `api/upload.rs` 的防护语义对齐，独立实现）：
//! - **路径清洗**：条目名逐组件校验，拒绝绝对路径（POSIX `/` 与 Windows
//!   `C:` 前缀）、任何 `..` 分段、反斜杠分隔符（归档契约恒正斜杠）与
//!   Windows 保留设备名（`CON`/`NUL`/…）；
//! - **纵深防御**：清洗后再经 zip crate `enclosed_name()`（zip 容器）与
//!   拼接后词法 `starts_with` 双重兜底；
//! - **symlink 逃逸防护**：symlink/hardlink 条目直接拒绝；同时逐组件探测
//!   目标路径祖先，任何已存在的符号链接（含暂存目录被预置链接的攻击面）
//!   都拒绝；
//! - **特殊文件**：unix 类型位非普通文件/目录（FIFO/设备/socket）拒绝；
//! - **大小上限**：解压字节数流式累计，超限立即中止（解压炸弹防御）；
//! - **重复条目**：大小写不敏感的同名条目拒绝——读取侧会把完全同名条目
//!   折叠为最后一个，而仅大小写不同的条目在 Windows（大小写不敏感文件
//!   系统）上会静默互相覆盖，双平台落盘必须拒绝。
//!
//! 出错时不做自动清理：暂存目录生命周期归调用方（§4.4 导入编排），
//! 出错后应整体丢弃 `dest`。

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

use zip::ZipArchive;

/// 归档内清单的固定文件名（§4.2 归档布局）。
pub const MANIFEST_FILE_NAME: &str = "ep-pack.toml";

/// 默认解包总字节上限：64 GiB（模型权重可达数 GB，留足余量；
/// 导入编排侧可按部署约束覆盖）。
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// 解压流式缓冲：256 KiB。
const EXTRACT_CHUNK_SIZE: usize = 256 * 1024;

/// unix 文件类型掩码与常量（zip 外部属性里的模式位）
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFIFO: u32 = 0o010000;

/// Windows 保留设备名（任意大小写、任意扩展名）：落盘会命中设备对象
/// 而非文件（如 `NUL`），双平台统一拒绝。
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 解包约束。
#[derive(Debug, Clone, Copy)]
pub struct ExtractLimits {
    /// 解压后总字节上限（流式累计，超限即中止；不信任条目元数据声明的大小）
    pub max_total_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        }
    }
}

/// 解包结果摘要。
#[derive(Debug, Clone)]
pub struct ExtractSummary {
    /// 解包目标（暂存）目录
    pub dest_dir: PathBuf,
    /// 解出的文件数（不含目录条目）
    pub file_count: usize,
    /// 解出的总字节数
    pub total_bytes: u64,
    /// 清单路径（解包成功时必然存在；前置校验已拒绝缺清单的归档）
    pub manifest_path: Option<PathBuf>,
}

/// 解包错误。
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("failed to open archive {}: {source}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid or unreadable archive: {0}")]
    Parse(#[source] zip::result::ZipError),
    #[error("io error on archive entry `{name}`: {source}")]
    EntryIo {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("extract io error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// zip-slip / 绝对路径 / 反斜杠分隔符 / 保留设备名等非法条目名
    #[error("unsafe archive entry path (absolute / `..` / backslash / reserved name): `{0}`")]
    UnsafePath(String),
    #[error("archive contains a symlink entry (forbidden in packs): `{0}`")]
    SymlinkEntry(String),
    #[error("extract target would traverse a pre-existing symlink out of the staging dir: `{0}`")]
    SymlinkEscape(String),
    #[error("archive contains a special-file entry `{name}` (mode {mode:o})")]
    SpecialFileEntry { name: String, mode: u32 },
    /// 大小写不敏感的同名冲突（含 Windows 大小写折叠后的互相覆盖）
    #[error("archive contains duplicate entry (case-insensitive collision): `{0}`")]
    DuplicateEntry(String),
    #[error("archive lacks manifest `ep-pack.toml`")]
    MissingManifest,
    #[error("unsupported archive format (pack archives must be standard zip or tar.gz): {0}")]
    UnsupportedFormat(String),
    #[error("extracted content exceeds size limit ({limit} bytes)")]
    SizeLimitExceeded { limit: u64 },
    #[error("extract destination {} is a symlink", path.display())]
    DestIsSymlink { path: PathBuf },
    #[error("extract destination {} exists and is not a directory", path.display())]
    DestNotDirectory { path: PathBuf },
}

/// 解包整合包归档到 `dest`（暂存目录，不存在则创建）。
///
/// 容器按魔数嗅探：`PK…` → zip，`\x1f\x8b` → tar.gz；不依赖扩展名。
/// 条目在落盘前逐条做安全校验（见模块级文档）；任何违规立即返回错误，
/// 已写入的部分内容留给调用方整体丢弃。
pub fn extract_pack(
    archive_path: &Path,
    dest: &Path,
    limits: &ExtractLimits,
) -> Result<ExtractSummary, ExtractError> {
    prepare_dest(dest)?;

    let file = File::open(archive_path).map_err(|source| ExtractError::Open {
        path: archive_path.to_path_buf(),
        source,
    })?;

    // 魔数嗅探容器格式（读 4 字节；不足 4 字节的文件必然不是合法归档）
    let mut magic = [0u8; 4];
    let mut reader = BufReader::new(file);
    let n = reader.read(&mut magic).map_err(|source| ExtractError::Open {
        path: archive_path.to_path_buf(),
        source,
    })?;
    if n < 4 {
        return Err(ExtractError::UnsupportedFormat(
            archive_path.display().to_string(),
        ));
    }

    match magic {
        [0x50, 0x4b, 0x03, 0x04] => extract_zip(reader, dest, limits),
        [0x1f, 0x8b, ..] => extract_tar_gz(reader, dest, limits),
        _ => Err(ExtractError::UnsupportedFormat(
            archive_path.display().to_string(),
        )),
    }
}

/// zip 容器解包（`extract_pack` 的 zip 分支）。
fn extract_zip(
    mut reader: BufReader<File>,
    dest: &Path,
    limits: &ExtractLimits,
) -> Result<ExtractSummary, ExtractError> {
    // 魔数嗅探已消费头部 4 字节，先回卷再交给 zip 解析器
    use std::io::Seek;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|source| ExtractError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
    let mut archive = ZipArchive::new(reader).map_err(ExtractError::Parse)?;

    // 前置校验：归档必须含清单（§4.2），缺失则在任何落盘前拒绝
    let mut has_manifest = false;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(ExtractError::Parse)?;
        if entry.name() == MANIFEST_FILE_NAME {
            has_manifest = true;
            break;
        }
    }
    if !has_manifest {
        return Err(ExtractError::MissingManifest);
    }

    let mut file_count: usize = 0;
    let mut total_bytes: u64 = 0;
    let mut seen: HashSet<String> = HashSet::new();
    let mut buf = vec![0u8; EXTRACT_CHUNK_SIZE];

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(ExtractError::Parse)?;
        let name = entry.name().to_string();

        // 1) symlink 条目：一律拒绝（§4.4 symlink 逃逸防护）
        if entry.is_symlink() {
            return Err(ExtractError::SymlinkEntry(name));
        }

        // 2) unix 类型位：仅允许普通文件 / 目录（类型位缺省 = 0 视为普通，
        //    兼容无 unix 属性的 Windows 产出归档）
        if let Some(mode) = entry.unix_mode() {
            let ft = mode & S_IFMT;
            if ft != 0 && ft != S_IFREG && ft != S_IFDIR {
                return Err(ExtractError::SpecialFileEntry { name, mode });
            }
        }

        // 3) 条目名清洗（主防线）+ enclosed_name 兜底（upload.rs 双层模式）
        let rel = sanitize_entry_name(&name).ok_or_else(|| ExtractError::UnsafePath(name.clone()))?;
        if entry.enclosed_name().is_none() {
            return Err(ExtractError::UnsafePath(name));
        }

        // 4) 拼接后词法边界兜底
        let out = resolve_within(dest, &rel)
            .ok_or_else(|| ExtractError::UnsafePath(name.clone()))?;

        // 5) 重复条目拒绝（大小写不敏感：Windows 文件系统大小写折叠）
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(ExtractError::DuplicateEntry(name));
        }

        // 6) 祖先 symlink 探测：暂存目录内任何已存在的符号链接都不得位于
        //    目标路径上（防「目录内预置链接指向外部」的逃逸）
        if contains_symlink_below(dest, &rel).map_err(|source| ExtractError::Io {
            path: out.clone(),
            source,
        })? {
            return Err(ExtractError::SymlinkEscape(name));
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|source| ExtractError::Io {
                path: out.clone(),
                source,
            })?;
            continue;
        }

        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut writer = File::create(&out).map_err(|source| ExtractError::Io {
            path: out.clone(),
            source,
        })?;

        loop {
            let n = entry.read(&mut buf).map_err(|source| ExtractError::EntryIo {
                name: name.clone(),
                source,
            })?;
            if n == 0 {
                break;
            }
            let n64 = n as u64;
            // 先判额度再落盘：磁盘上累计字节永不超限
            if total_bytes.saturating_add(n64) > limits.max_total_bytes {
                return Err(ExtractError::SizeLimitExceeded {
                    limit: limits.max_total_bytes,
                });
            }
            writer
                .write_all(&buf[..n])
                .map_err(|source| ExtractError::EntryIo {
                    name: name.clone(),
                    source,
                })?;
            total_bytes += n64;
        }
        file_count += 1;
    }

    let manifest_path = dest.join(MANIFEST_FILE_NAME);
    Ok(ExtractSummary {
        dest_dir: dest.to_path_buf(),
        file_count,
        total_bytes,
        manifest_path: manifest_path.is_file().then_some(manifest_path),
    })
}

/// tar.gz 容器解包（`extract_pack` 的 tar.gz 分支）。
///
/// 与 zip 分支同一套安全基线；清单前置校验改为解包后校验（tar.gz 需完整
/// 流式解码才能枚举条目，重复解码一遍做前置扫描得不偿失）——归档缺清单时
/// 内容已落暂存目录，但导入编排在本函数返回 MissingManifest 后整体丢弃
/// 暂存目录，不会发生任何安装动作。
fn extract_tar_gz(
    mut reader: BufReader<File>,
    dest: &Path,
    limits: &ExtractLimits,
) -> Result<ExtractSummary, ExtractError> {
    // 魔数嗅探已消费头部 4 字节，先回卷再交给 gzip 解码器
    use std::io::Seek;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|source| ExtractError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
    let gz = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(gz);

    let mut file_count: usize = 0;
    let mut total_bytes: u64 = 0;
    let mut seen: HashSet<String> = HashSet::new();
    let mut buf = vec![0u8; EXTRACT_CHUNK_SIZE];

    let entries = archive
        .entries()
        .map_err(|source| ExtractError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
    for entry in entries {
        let mut entry = entry.map_err(|source| ExtractError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
        let header = entry.header();
        let name = entry
            .path()
            .map_err(|source| ExtractError::Io {
                path: dest.to_path_buf(),
                source,
            })?
            .to_string_lossy()
            .to_string();

        // 1) symlink / hardlink 条目：一律拒绝（§4.4 symlink 逃逸防护）
        match header.entry_type() {
            tar::EntryType::Regular => {}
            tar::EntryType::Directory => {}
            tar::EntryType::Symlink | tar::EntryType::Link => {
                return Err(ExtractError::SymlinkEntry(name));
            }
            other => {
                return Err(ExtractError::SpecialFileEntry {
                    name,
                    mode: header.mode().unwrap_or(0) | format_type_tag(other),
                });
            }
        }

        // 2) unix 类型位：仅允许普通文件 / 目录（类型位缺省 = 0 视为普通）
        if let Ok(mode) = header.mode() {
            let ft = mode & S_IFMT;
            if ft != 0 && ft != S_IFREG && ft != S_IFDIR {
                return Err(ExtractError::SpecialFileEntry { name, mode });
            }
        }

        // 3) 条目名清洗（主防线）
        let rel =
            sanitize_entry_name(&name).ok_or_else(|| ExtractError::UnsafePath(name.clone()))?;

        // 4) 拼接后词法边界兜底
        let out = resolve_within(dest, &rel)
            .ok_or_else(|| ExtractError::UnsafePath(name.clone()))?;

        // 5) 重复条目拒绝（大小写不敏感：Windows 文件系统大小写折叠）
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(ExtractError::DuplicateEntry(name));
        }

        // 6) 祖先 symlink 探测（与 zip 分支同语义）
        if contains_symlink_below(dest, &rel).map_err(|source| ExtractError::Io {
            path: out.clone(),
            source,
        })? {
            return Err(ExtractError::SymlinkEscape(name));
        }

        if header.entry_type() == tar::EntryType::Directory {
            std::fs::create_dir_all(&out).map_err(|source| ExtractError::Io {
                path: out.clone(),
                source,
            })?;
            continue;
        }

        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut writer = File::create(&out).map_err(|source| ExtractError::Io {
            path: out.clone(),
            source,
        })?;

        loop {
            let n = entry.read(&mut buf).map_err(|source| ExtractError::EntryIo {
                name: name.clone(),
                source,
            })?;
            if n == 0 {
                break;
            }
            let n64 = n as u64;
            // 先判额度再落盘：磁盘上累计字节永不超限
            if total_bytes.saturating_add(n64) > limits.max_total_bytes {
                return Err(ExtractError::SizeLimitExceeded {
                    limit: limits.max_total_bytes,
                });
            }
            writer
                .write_all(&buf[..n])
                .map_err(|source| ExtractError::EntryIo {
                    name: name.clone(),
                    source,
                })?;
            total_bytes += n64;
        }
        file_count += 1;
    }

    // 清单校验（tar.gz 为解包后校验，见函数头注释）
    let manifest_path = dest.join(MANIFEST_FILE_NAME);
    if !manifest_path.is_file() {
        return Err(ExtractError::MissingManifest);
    }
    Ok(ExtractSummary {
        dest_dir: dest.to_path_buf(),
        file_count,
        total_bytes,
        manifest_path: Some(manifest_path),
    })
}

/// 非 Regular/Directory 的 tar 条目类型标记（用于错误信息可读）
fn format_type_tag(t: tar::EntryType) -> u32 {
    match t {
        tar::EntryType::Fifo => S_IFIFO,
        _ => 0,
    }
}

/// 校验/创建解包目标目录：本身不得是符号链接；已存在则必须是目录。
fn prepare_dest(dest: &Path) -> Result<(), ExtractError> {
    match std::fs::symlink_metadata(dest) {
        Ok(meta) if meta.file_type().is_symlink() => Err(ExtractError::DestIsSymlink {
            path: dest.to_path_buf(),
        }),
        Ok(meta) if !meta.is_dir() => Err(ExtractError::DestNotDirectory {
            path: dest.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dest).map_err(|source| ExtractError::Io {
                path: dest.to_path_buf(),
                source,
            })
        }
        Err(e) => Err(ExtractError::Io {
            path: dest.to_path_buf(),
            source: e,
        }),
    }
}

/// 清洗归档条目名（安全语义对齐 upload.rs `sanitize_relative_path`，
/// 并按 §4.2 归档契约额外拒绝反斜杠分隔符与 Windows 保留设备名）。
///
/// 拒绝：空名、NUL、任何 `\`、POSIX 绝对路径（`/…`）、Windows 盘符前缀
/// （`C:…`）、`..` 分段、Windows 保留设备名。`.` 与冗余分隔符归一化去掉。
fn sanitize_entry_name(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\0') || name.contains('\\') {
        return None;
    }
    let bytes = name.as_bytes();
    if bytes[0] == b'/' {
        return None; // POSIX 绝对路径
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None; // Windows 盘符前缀（C:/… 或 C:…）
    }

    let mut out = PathBuf::new();
    for seg in name.split('/') {
        match seg {
            "" | "." => {} // 冗余分隔符 / 当前目录分段：忽略
            ".." => return None,
            s => {
                if is_windows_reserved_name(s) {
                    return None;
                }
                out.push(s);
            }
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Windows 保留设备名判断（取首段 `.` 之前的 stem，大小写不敏感）。
fn is_windows_reserved_name(seg: &str) -> bool {
    let stem = seg.split('.').next().unwrap_or(seg);
    WINDOWS_RESERVED_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
}

/// 把相对路径拼到 base 下并保证不越出 base（词法兜底，upload.rs 同型）。
fn resolve_within(base: &Path, rel: &Path) -> Option<PathBuf> {
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return None;
    }
    let joined = base.join(rel);
    if joined.starts_with(base) {
        Some(joined)
    } else {
        None
    }
}

/// 探测 `base.join(rel)` 的每个祖先组件（base 之下）是否存在符号链接。
///
/// 路径尚不存在的组件视为无链接（NotFound 即止）。任一组件为 symlink →
/// 返回 true（调用方必须拒绝该条目）。
fn contains_symlink_below(base: &Path, rel: &Path) -> io::Result<bool> {
    let mut cur = base.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        let meta = match std::fs::symlink_metadata(&cur) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            return Ok(true);
        }
    }
    Ok(false)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use crate::build::{build_pack, BuildPlan};
    use crate::checksum::{ChecksumError, ChecksumTable, CHECKSUMS_FILE_NAME};

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(tag: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ep-pack-extract-{tag}-{}-{seq}",
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

    fn sample_source(root: &Path) -> PathBuf {
        let src = root.join("src");
        write_file(&src.join("ep-pack.toml"), b"[pack]\nid = \"t.p\"\n");
        write_file(
            &src.join("models").join("m1").join("weights.bin"),
            b"weights-data",
        );
        write_file(&src.join("pipelines").join("p.toml"), b"[pipeline]\n");
        src
    }

    /// fixture 条目：构造（可恶意的）zip 条目
    struct FixtureEntry {
        name: &'static str,
        content: Vec<u8>,
        /// Some(链接目标) → 用 add_symlink 写真实 symlink 条目（S_IFLNK 类型位）
        symlink_target: Option<&'static str>,
        is_dir: bool,
    }

    fn fixture(name: &'static str, content: impl Into<Vec<u8>>) -> FixtureEntry {
        FixtureEntry {
            name,
            content: content.into(),
            symlink_target: None,
            is_dir: false,
        }
    }

    /// 用 ZipWriter 直接按原始条目名写 zip（start_file 不清洗名字，
    /// 可构造 zip-slip / 绝对路径等恶意条目）。
    fn write_fixture_zip(path: &Path, entries: &[FixtureEntry]) {
        let file = File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        for e in entries {
            let opts = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .last_modified_time(zip::DateTime::default());
            if let Some(target) = e.symlink_target {
                zip.add_symlink(e.name, target, opts).unwrap();
            } else if e.is_dir {
                zip.add_directory(e.name, opts).unwrap();
            } else {
                zip.start_file(e.name, opts).unwrap();
                zip.write_all(&e.content).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    // ── 原始字节级 zip 构造器 ────────────────────────────────────────────
    // ZipWriter 拒绝重名条目、且 start_file 强制 S_IFREG，因此「重复条目」
    // 与「特殊文件类型位」两类恶意形状只能字节级手工构造。

    mod raw_zip {
        /// CRC-32（IEEE 802.3，zip 规范）
        fn crc32(data: &[u8]) -> u32 {
            let mut crc: u32 = 0xFFFF_FFFF;
            for &b in data {
                crc ^= b as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }

        pub struct RawEntry {
            pub name: &'static str,
            pub data: &'static [u8],
            /// 外部属性高 16 位 = unix 模式（含文件类型位）
            pub unix_mode: u32,
        }

        /// 最小合法 zip：stored 压缩、无 extra、zip32 不适用（测试数据极小）
        pub fn build(entries: &[RawEntry]) -> Vec<u8> {
            let mut out = Vec::new();
            let mut central = Vec::new();
            for e in entries {
                let offset = out.len() as u32;
                let crc = crc32(e.data);
                let len = e.data.len() as u32;
                // local file header
                out.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
                out.extend_from_slice(&20u16.to_le_bytes()); // version needed
                out.extend_from_slice(&0u16.to_le_bytes()); // flags
                out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
                out.extend_from_slice(&0u16.to_le_bytes()); // mod time
                out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date: 1980-01-01
                out.extend_from_slice(&crc.to_le_bytes());
                out.extend_from_slice(&len.to_le_bytes()); // compressed size
                out.extend_from_slice(&len.to_le_bytes()); // uncompressed size
                out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes()); // extra len
                out.extend_from_slice(e.name.as_bytes());
                out.extend_from_slice(e.data);
                // central directory header
                central.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
                central.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes()); // made by: Unix
                central.extend_from_slice(&20u16.to_le_bytes());
                central.extend_from_slice(&0u16.to_le_bytes()); // flags
                central.extend_from_slice(&0u16.to_le_bytes()); // method
                central.extend_from_slice(&0u16.to_le_bytes()); // time
                central.extend_from_slice(&0x21u16.to_le_bytes()); // date
                central.extend_from_slice(&crc.to_le_bytes());
                central.extend_from_slice(&len.to_le_bytes());
                central.extend_from_slice(&len.to_le_bytes());
                central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
                central.extend_from_slice(&0u16.to_le_bytes()); // extra len
                central.extend_from_slice(&0u16.to_le_bytes()); // comment len
                central.extend_from_slice(&0u16.to_le_bytes()); // disk number
                central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
                central.extend_from_slice(&(e.unix_mode << 16).to_le_bytes());
                central.extend_from_slice(&offset.to_le_bytes());
                central.extend_from_slice(e.name.as_bytes());
            }
            let cd_offset = out.len() as u32;
            out.extend_from_slice(&central);
            let cd_size = central.len() as u32;
            // EOCD
            out.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
            out.extend_from_slice(&0u16.to_le_bytes()); // disk
            out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
            out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            out.extend_from_slice(&cd_size.to_le_bytes());
            out.extend_from_slice(&cd_offset.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // comment len
            out
        }
    }

    fn write_raw_zip(path: &Path, entries: &[raw_zip::RawEntry]) {
        std::fs::write(path, raw_zip::build(entries)).unwrap();
    }

    fn dir_size(path: &Path) -> u64 {
        let mut total = 0;
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    total += dir_size(&p);
                } else if let Ok(m) = p.symlink_metadata() {
                    total += m.len();
                }
            }
        }
        total
    }

    // ── 正向：构建 → 解包 → 校验 全链 ────────────────────────────────────

    /// tar.gz 容器正向链：解包 + CHECKSUMS 全量校验通过
    #[test]
    fn extract_tar_gz_roundtrip() {
        use flate2::write::GzEncoder;
        let root = unique_root("targz");
        let src = sample_source(&root);
        let manifest_text = std::fs::read_to_string(src.join(MANIFEST_FILE_NAME)).unwrap();

        // 手工构造 tar.gz：<ep-pack.toml> + models/m1/weights.bin（目录条目按
        // tar 惯例可省略，父目录由解包侧 create_dir_all 补齐）
        let archive = root.join("pack.tar.gz");
        {
            let gz = GzEncoder::new(
                std::fs::File::create(&archive).unwrap(),
                flate2::Compression::default(),
            );
            let mut tar = tar::Builder::new(gz);
            let add = |tar: &mut tar::Builder<GzEncoder<std::fs::File>>, name: &str, data: &[u8]| {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                tar.append_data(&mut h, name, data).unwrap();
            };
            add(&mut tar, MANIFEST_FILE_NAME, manifest_text.as_bytes());
            add(&mut tar, "models/m1/weights.bin", b"weights-data");
            let mut entries = std::collections::BTreeMap::new();
            entries.insert(
                MANIFEST_FILE_NAME.to_string(),
                crate::checksum::sha256_file(&src.join(MANIFEST_FILE_NAME)).unwrap(),
            );
            entries.insert(
                "models/m1/weights.bin".to_string(),
                crate::checksum::sha256_file(&src.join("models/m1/weights.bin")).unwrap(),
            );
            let table = ChecksumTable::from_entries(entries);
            add(&mut tar, CHECKSUMS_FILE_NAME, table.to_toml_string().unwrap().as_bytes());
            tar.into_inner().unwrap().finish().unwrap();
        }

        let dest = root.join("staging");
        let xs = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap();
        assert_eq!(xs.file_count, 3);
        assert_eq!(xs.manifest_path, Some(dest.join(MANIFEST_FILE_NAME)));
        assert_eq!(
            std::fs::read(dest.join("models/m1/weights.bin")).unwrap(),
            b"weights-data"
        );

        // 解包产物通过 CHECKSUMS 全量校验
        let table = ChecksumTable::read(&dest).unwrap();
        table.verify(&dest).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_extract_verify_roundtrip() {
        let root = unique_root("roundtrip");
        let src = sample_source(&root);
        std::fs::create_dir_all(src.join("models").join("empty-dir")).unwrap();
        let archive = root.join("pack.zip");
        let summary = build_pack(&BuildPlan::new(&src, &archive)).unwrap();

        let dest = root.join("staging");
        let xs = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap();
        assert_eq!(xs.dest_dir, dest);
        assert_eq!(xs.file_count, summary.file_count); // 含 CHECKSUMS.toml
        // 字节数 = 源文件总和 + 归档内 CHECKSUMS.toml 文本
        assert!(xs.total_bytes >= summary.total_bytes);
        assert_eq!(xs.manifest_path, Some(dest.join("ep-pack.toml")));

        assert_eq!(
            std::fs::read(dest.join("models").join("m1").join("weights.bin")).unwrap(),
            b"weights-data"
        );
        assert!(dest.join("models").join("empty-dir").is_dir());
        assert!(dest.join(CHECKSUMS_FILE_NAME).is_file());

        // 解包产物通过 CHECKSUMS 全量校验
        let table = ChecksumTable::read(&dest).unwrap();
        table.verify(&dest).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 攻击矩阵 ─────────────────────────────────────────────────────────

    #[test]
    fn rejects_zip_slip_parent_dir_entries() {
        let root = unique_root("zipslip");
        let archive = root.join("evil.zip");
        write_fixture_zip(
            &archive,
            &[
                fixture("ep-pack.toml", b"[pack]"),
                fixture("../evil.txt", b"pwned"),
                fixture("nested/../../evil2.txt", b"pwned2"),
            ],
        );
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath(_)), "{err:?}");
        // 逃逸目标文件绝不得出现（../evil.txt 相对 staging 落在 root 下）
        assert!(!root.join("evil.txt").exists());
        assert!(!root.join("evil2.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_absolute_path_entries_posix() {
        let root = unique_root("abs-posix");
        let archive = root.join("evil.zip");
        write_fixture_zip(
            &archive,
            &[
                fixture("ep-pack.toml", b"[pack]"),
                fixture("/etc/passwd", b"root::0:0"),
            ],
        );
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath(_)), "{err:?}");
        assert!(!dest.join("etc").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_absolute_path_entries_windows_drive() {
        let root = unique_root("abs-win");
        let archive = root.join("evil.zip");
        write_fixture_zip(
            &archive,
            &[
                fixture("ep-pack.toml", b"[pack]"),
                fixture("C:/evil.txt", b"pwned"),
                fixture("D:evil2.txt", b"pwned"),
            ],
        );
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_backslash_separator_entries() {
        let root = unique_root("backslash");
        let archive = root.join("evil.zip");
        write_fixture_zip(
            &archive,
            &[
                fixture("ep-pack.toml", b"[pack]"),
                fixture("dir\\file.txt", b"non-canonical separator"),
            ],
        );
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_windows_reserved_device_names() {
        let root = unique_root("reserved");
        let archive = root.join("evil.zip");
        write_fixture_zip(
            &archive,
            &[
                fixture("ep-pack.toml", b"[pack]"),
                fixture("models/NUL", b"device"),
            ],
        );
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::UnsafePath(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_symlink_entry() {
        let root = unique_root("symlink-entry");
        let archive = root.join("evil.zip");
        write_fixture_zip(
            &archive,
            &[
                fixture("ep-pack.toml", b"[pack]"),
                FixtureEntry {
                    name: "models/link",
                    content: Vec::new(), // symlink 内容 = 链接目标，由 add_symlink 写入
                    symlink_target: Some("../../../evil-target"),
                    is_dir: false,
                },
            ],
        );
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::SymlinkEntry(_)), "{err:?}");
        // 既没有链接文件、也没有逃逸目标
        assert!(!dest.join("models").join("link").exists());
        assert!(!root.join("evil-target").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_special_file_entry() {
        let root = unique_root("fifo");
        let archive = root.join("evil.zip");
        // ZipWriter 强制 S_IFREG → FIFO 类型位须字节级构造
        write_raw_zip(
            &archive,
            &[
                raw_zip::RawEntry {
                    name: "ep-pack.toml",
                    data: b"[pack]",
                    unix_mode: 0o100644,
                },
                raw_zip::RawEntry {
                    name: "models/fifo",
                    data: b"",
                    unix_mode: 0o010644, // S_IFIFO
                },
            ],
        );
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::SpecialFileEntry { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_missing_manifest() {
        let root = unique_root("no-manifest");
        let archive = root.join("evil.zip");
        write_fixture_zip(&archive, &[fixture("models/w.bin", b"x")]);
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::MissingManifest), "{err:?}");
        // 拒绝发生在任何落盘之前
        assert!(!dest.join("models").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_duplicate_entries() {
        let root = unique_root("dup");
        let archive = root.join("evil.zip");
        // 完全同名条目会被 zip crate 读取侧折叠，故用「仅大小写不同」的条目
        // 构造——它们在 Windows 上落盘会互相覆盖，必须拒绝。
        // （ZipWriter 拒绝重名条目 → 字节级构造。）
        write_raw_zip(
            &archive,
            &[
                raw_zip::RawEntry {
                    name: "ep-pack.toml",
                    data: b"[pack]",
                    unix_mode: 0o100644,
                },
                raw_zip::RawEntry {
                    name: "models/a.txt",
                    data: b"first",
                    unix_mode: 0o100644,
                },
                raw_zip::RawEntry {
                    name: "models/A.TXT",
                    data: b"second",
                    unix_mode: 0o100644,
                },
            ],
        );
        let dest = root.join("staging");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::DuplicateEntry(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn size_limit_enforced_while_streaming() {
        let root = unique_root("sizelimit");
        let archive = root.join("big.zip");
        let block = vec![0xABu8; 64 * 1024];
        write_fixture_zip(
            &archive,
            &[
                fixture("ep-pack.toml", b"[pack]"),
                fixture("models/a.bin", block.clone()),
                fixture("models/b.bin", block.clone()),
                fixture("models/c.bin", block),
            ],
        );
        let dest = root.join("staging");
        // 3×64KiB + 清单 > 100KB 上限
        let limits = ExtractLimits {
            max_total_bytes: 100 * 1024,
        };
        let err = extract_pack(&archive, &dest, &limits).unwrap_err();
        assert!(matches!(err, ExtractError::SizeLimitExceeded { .. }), "{err:?}");
        // 先判额度再落盘 → 磁盘占用不超限
        assert!(dir_size(&dest) <= limits.max_total_bytes);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_dest_not_directory() {
        let root = unique_root("dest-file");
        let src = sample_source(&root);
        let archive = root.join("pack.zip");
        build_pack(&BuildPlan::new(&src, &archive)).unwrap();
        let dest = root.join("staging");
        write_file(&dest, b"i am a file");
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::DestNotDirectory { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_dest_symlink() {
        let root = unique_root("dest-symlink");
        let src = sample_source(&root);
        let archive = root.join("pack.zip");
        build_pack(&BuildPlan::new(&src, &archive)).unwrap();
        let real = root.join("elsewhere");
        std::fs::create_dir_all(&real).unwrap();
        let dest = root.join("staging");
        std::os::unix::fs::symlink(&real, &dest).unwrap();
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::DestIsSymlink { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_escape_via_preexisting_symlink_inside_dest() {
        let root = unique_root("symlink-inside");
        let archive = root.join("evil.zip");
        write_fixture_zip(
            &archive,
            &[
                fixture("ep-pack.toml", b"[pack]"),
                fixture("sub/evil.txt", b"escaped"),
            ],
        );
        let dest = root.join("staging");
        std::fs::create_dir_all(&dest).unwrap();
        // 暂存目录内预置 sub → 指向外部的符号链接
        std::os::unix::fs::symlink(root.join("outside"), dest.join("sub")).unwrap();
        let err = extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::SymlinkEscape(_)), "{err:?}");
        assert!(!root.join("outside").join("evil.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── CHECKSUMS 篡改 / 缺失 × 解包集成 ─────────────────────────────────

    #[test]
    fn detects_tampered_file_after_extract() {
        let root = unique_root("tamper-after-extract");
        let src = sample_source(&root);
        let archive = root.join("pack.zip");
        build_pack(&BuildPlan::new(&src, &archive)).unwrap();
        let dest = root.join("staging");
        extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap();

        // 篡改已解包的权重文件 → 全量校验必须报 mismatched
        write_file(&dest.join("models").join("m1").join("weights.bin"), b"EVIL");
        let table = ChecksumTable::read(&dest).unwrap();
        match table.verify(&dest).unwrap_err() {
            ChecksumError::Integrity(report) => {
                assert_eq!(report.mismatched.len(), 1);
                assert_eq!(report.mismatched[0].path, "models/m1/weights.bin");
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_missing_file_after_extract() {
        let root = unique_root("missing-after-extract");
        let src = sample_source(&root);
        let archive = root.join("pack.zip");
        build_pack(&BuildPlan::new(&src, &archive)).unwrap();
        let dest = root.join("staging");
        extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap();

        std::fs::remove_file(dest.join("pipelines").join("p.toml")).unwrap();
        let table = ChecksumTable::read(&dest).unwrap();
        match table.verify(&dest).unwrap_err() {
            ChecksumError::Integrity(report) => {
                assert_eq!(report.missing, vec!["pipelines/p.toml".to_string()]);
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_unexpected_file_after_extract() {
        let root = unique_root("unexpected-after-extract");
        let src = sample_source(&root);
        let archive = root.join("pack.zip");
        build_pack(&BuildPlan::new(&src, &archive)).unwrap();
        let dest = root.join("staging");
        extract_pack(&archive, &dest, &ExtractLimits::default()).unwrap();

        write_file(&dest.join("models").join("planted.bin"), b"malware");
        let table = ChecksumTable::read(&dest).unwrap();
        match table.verify(&dest).unwrap_err() {
            ChecksumError::Integrity(report) => {
                assert_eq!(report.unexpected, vec!["models/planted.bin".to_string()]);
            }
            other => panic!("expected Integrity, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
