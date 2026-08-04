//! 六个子命令实现（new / validate / build / import / info / export）。
//!
//! 实现所有者：Wave 3 **C6 (PackCLI)**。
//! 各命令返回进程退出码（约定见 [`crate::output`]）。

pub mod build;
pub mod export;
pub mod import;
pub mod info;
pub mod new;
pub mod validate;

use std::path::{Path, PathBuf};

/// 把包内相对路径（恒 `/` 分隔）逐组件拼到 `base` 下。
///
/// 双平台纪律：不信任字符串分隔符，逐 `split('/')` 组件 join；
/// 防御性拒绝 `..`（清单 validate() 已拒绝，此处兜底）。
pub fn join_pack_rel(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    let mut any = false;
    for seg in rel.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return None,
            s => {
                out.push(s);
                any = true;
            }
        }
    }
    any.then_some(out)
}
