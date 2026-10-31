use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::LibConfig;
use crate::db;

/// database\libs 目录（每个子目录 = 一个库）
pub fn libs_dir(home: &Path) -> PathBuf {
    home.join("database").join("libs")
}

pub fn lib_dir(home: &Path, id: &str) -> PathBuf {
    libs_dir(home).join(id)
}

/// 枚举库：扫描 database\libs\ 子目录并加载各自 config.json
pub fn list(home: &Path) -> anyhow::Result<Vec<LibConfig>> {
    let dir = libs_dir(home);
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if !e.path().is_dir() {
            continue;
        }
        let cfg_path = e.path().join("config.json");
        if !cfg_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&cfg_path)?;
        match serde_json::from_str::<LibConfig>(&text) {
            Ok(cfg) => out.push(cfg),
            Err(err) => eprintln!("warning: 解析 {} 失败: {err}", cfg_path.display()),
        }
    }
    Ok(out)
}

#[allow(dead_code)]
pub fn load(home: &Path, id: &str) -> anyhow::Result<LibConfig> {
    let cfg_path = lib_dir(home, id).join("config.json");
    let text = std::fs::read_to_string(&cfg_path)?;
    serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("解析 {} 失败: {e}", cfg_path.display()))
}

pub fn add(
    home: &Path,
    id: &str,
    root: &Path,
    include_root: bool,
    recursive: bool,
    include: &[String],
    exclude: Option<&[String]>,
) -> anyhow::Result<()> {
    validate_id(id)?;
    if !root.is_dir() {
        anyhow::bail!("root 目录不存在或不是目录: {}", root.display());
    }
    for pat in include.iter().chain(exclude.unwrap_or(&[]).iter()) {
        regex::Regex::new(pat)
            .map_err(|e| anyhow::anyhow!("无效正则 {pat}: {e}"))?;
    }
    let dir = lib_dir(home, id);
    if dir.exists() {
        anyhow::bail!("库已存在: {}", dir.display());
    }
    std::fs::create_dir_all(&dir)?;

    let mut cfg = LibConfig::new(id, root.to_path_buf());
    let mut recursive = recursive;
    if !include_root && !recursive && include.is_empty() {
        recursive = true;
    }
    cfg.dir_filter.include_root = include_root;
    cfg.dir_filter.include_recursive = recursive;
    cfg.dir_filter.include = include.to_vec();
    if let Some(ex) = exclude {
        cfg.dir_filter.exclude = ex.to_vec();
    }

    let cfg_path = dir.join("config.json");
    let json = serde_json::to_string_pretty(&cfg)?;
    std::fs::write(&cfg_path, json + "\n")?;

    drop(db::open(&dir.join("index.db"))?);
    println!("已创建库 {id}");
    println!("  root:   {}", root.display());
    println!("  config: {}", cfg_path.display());
    Ok(())
}

pub fn remove(home: &Path, id: &str, yes: bool) -> anyhow::Result<()> {
    validate_id(id)?;
    let dir = lib_dir(home, id);
    if !dir.is_dir() {
        anyhow::bail!("库不存在: {}", dir.display());
    }
    if !yes {
        print!("确认删除库 {id}（{}）？[y/N] ", dir.display());
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let ans = line.trim().to_ascii_lowercase();
        if ans != "y" && ans != "yes" {
            println!("已取消");
            return Ok(());
        }
    }
    std::fs::remove_dir_all(&dir)?;
    println!("已删除库 {id}");
    Ok(())
}

fn validate_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() {
        anyhow::bail!("库 id 不能为空");
    }
    if id
        .chars()
        .any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
    {
        anyhow::bail!("库 id 只能含字母、数字、'-'、'_'（实际: {id}）");
    }
    Ok(())
}