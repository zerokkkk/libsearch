//! 程序目录与资源路径解析（从 main.rs 移入）

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::LibConfig;

/// 程序目录：--home 显式 > LIBSEARCH_HOME 环境变量 > exe 目录（符号链接跟链到真实路径）
pub fn program_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(h) = explicit {
        return Ok(h.to_path_buf());
    }
    if let Ok(h) = std::env::var("LIBSEARCH_HOME") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    let exe = std::env::current_exe()?;
    let exe = resolve_exe_symlinks(&exe)?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("无法确定程序目录"))
}

/// exe 为符号链接时跟链到真实路径（避免把链接所在目录当作程序目录）
pub fn resolve_exe_symlinks(exe: &Path) -> Result<PathBuf> {
    let mut current = exe.to_path_buf();
    for _ in 0..16 {
        let meta = std::fs::symlink_metadata(&current)?;
        if !meta.file_type().is_symlink() {
            return Ok(current);
        }
        let target = std::fs::read_link(&current)?;
        current = if target.is_absolute() {
            target
        } else {
            current
                .parent()
                .ok_or_else(|| {
                    anyhow::anyhow!("无法解析符号链接 {} 的父目录", current.display())
                })?
                .join(target)
        };
    }
    anyhow::bail!("符号链接链过长（>16 级），起始: {}", exe.display())
}

/// 解析 config 中的资源路径（相对路径以程序目录为基准）
pub fn resolve_resource(home: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        pb
    } else {
        home.join(pb)
    }
}

/// 模型/词典/用户词典路径（按 config 解析；词典缺失或禁用时为 None）
pub fn resource_paths(home: &Path, cfg: &LibConfig) -> (PathBuf, Option<PathBuf>, Option<PathBuf>) {
    let model_dir = resolve_resource(home, &cfg.embedding.dir);
    let user_dict_path = home.join("userdict.csv");
    let user_dict = if user_dict_path.is_file() {
        Some(user_dict_path)
    } else {
        None
    };
    let dict_bin = match &cfg.fts.dict_dir {
        Some(d) => {
            let dict_dir = resolve_resource(home, d);
            if dict_dir.is_dir() {
                Some(dict_dir)
            } else {
                None
            }
        }
        None => None,
    };
    (model_dir, dict_bin, user_dict)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_exe_symlinks_identity_for_regular_file() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(resolve_exe_symlinks(&exe).unwrap(), exe);
    }

    #[test]
    #[cfg(windows)]
    fn resolve_exe_symlinks_follows_chain() {
        // std 在 Windows 只提供 symlink_dir（目录），文件符号链接经 PowerShell 创建；不可用则跳过
        let dir = std::env::temp_dir()
            .join(format!("libsearch_symlink_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("real.exe");
        std::fs::write(&real, b"x").unwrap();

        fn mk_link(link: &Path, target: &Path) -> bool {
            std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "New-Item -ItemType SymbolicLink -Path '{}' -Target '{}'",
                        link.display(),
                        target.display()
                    ),
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        let link = dir.join("link.exe");
        if !mk_link(&link, &real) {
            eprintln!("skip: 无法创建文件符号链接（PowerShell 不可用？）");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(resolve_exe_symlinks(&link).unwrap(), real);
        let link2 = dir.join("link2.exe");
        assert!(mk_link(&link2, &link));
        assert_eq!(resolve_exe_symlinks(&link2).unwrap(), real);
        let _ = std::fs::remove_dir_all(&dir);
    }
}