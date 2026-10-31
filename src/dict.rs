//! 词典管理：预编译词典下载（`dict download`，移植自 lindera-cli download）
//!
//! 预编译词典来自 GitHub releases：
//! `https://github.com/lindera/lindera/releases/download/v{version}/lindera-{name}-{version}.zip`
//! zip 内含单一顶层目录 `lindera-{name}/`，解压内容落 home 下 `dict/<name>`。

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use lindera::dictionary::load_dictionary;
use zip::ZipArchive;

/// `dict download` 接受的词典名（与 lindera-cli 一致）
pub const DOWNLOADABLE_DICTIONARIES: [&str; 7] = [
    "ipadic",
    "ipadic-neologd",
    "unidic",
    "sudachidict",
    "ko-dic",
    "cc-cedict",
    "jieba",
];

/// 完整预编译词典目录的文件清单
pub const REQUIRED_DICTIONARY_FILES: [&str; 9] = [
    "metadata.json",
    "char_def.bin",
    "matrix.mtx",
    "dict.trie",
    "dict.valsidx",
    "dict.vals",
    "dict.wordsidx",
    "dict.words",
    "unk.bin",
];

/// 默认词典版本（须与 lindera 加载器的 format_version 匹配）
pub const DEFAULT_DICTIONARY_VERSION: &str = "6.2.0";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_SIZE: usize = 64 * 1024;
/// 进度打印粒度（字节）
const PROGRESS_STEP: u64 = 1024 * 1024;

/// 词典安装目标目录（home/dict/<name>）
pub fn dict_dir(home: &Path, name: &str) -> PathBuf {
    home.join("dict").join(name)
}

/// 预编译词典 GitHub release URL
pub fn download_url(name: &str, version: &str) -> String {
    format!(
        "https://github.com/lindera/lindera/releases/download/v{version}/lindera-{name}-{version}.zip"
    )
}

/// 列出目录中缺失的必需词典文件
pub fn missing_dictionary_files(dir: &Path) -> Vec<&'static str> {
    REQUIRED_DICTIONARY_FILES
        .iter()
        .copied()
        .filter(|file| !dir.join(file).is_file())
        .collect()
}

/// 下载并安装预编译词典，成功时返回目标目录
///
/// 目标目录已完整：直接返回（`force` 除外）；存在但不完整：报错并提示 `--force`。
pub fn download(home: &Path, name: &str, version: &str, force: bool) -> Result<PathBuf> {
    if !DOWNLOADABLE_DICTIONARIES.contains(&name) {
        bail!(
            "未知词典名 {name}。可用: {}",
            DOWNLOADABLE_DICTIONARIES.join(" / ")
        );
    }

    let target = dict_dir(home, name);
    let missing = missing_dictionary_files(&target);
    if missing.is_empty() {
        if force {
            fs::remove_dir_all(&target)
                .with_context(|| format!("删除旧词典 {} 失败", target.display()))?;
        } else {
            println!("词典已存在且完整，跳过: {}", target.display());
            return Ok(target);
        }
    } else if target.is_dir() {
        bail!(
            "词典 {} 不完整（缺失: {}）。运行 `dict download {name} --force` 重新下载。",
            target.display(),
            missing.join(", ")
        );
    }

    let url = download_url(name, version);
    let staging = target.with_extension("staging");
    let zip_path = target.with_extension("zip");
    if staging.is_file() {
        fs::remove_file(&staging)
            .with_context(|| format!("删除残留 staging {} 失败", staging.display()))?;
    } else if staging.exists() {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("删除残留 staging 目录 {} 失败", staging.display()))?;
    }
    fs::create_dir_all(&staging)
        .with_context(|| format!("创建 staging 目录 {} 失败", staging.display()))?;

    let result = (|| -> Result<()> {
        download_zip(&url, &zip_path)?;
        extract_zip(&zip_path, &staging, &format!("lindera-{name}"))?;
        let missing = missing_dictionary_files(&staging);
        if !missing.is_empty() {
            bail!("词典包不完整（缺失: {}）", missing.join(", "));
        }
        // 校验加载器能实际加载（同时拒绝 format 版本不匹配）
        load_dictionary(staging.to_str().context("词典路径须为 UTF-8")?)
            .map_err(|e| anyhow::anyhow!("词典加载校验失败（版本格式不匹配？）: {e}"))?;
        fs::rename(&staging, &target)
            .with_context(|| format!("移动 {} 到 {} 失败", staging.display(), target.display()))?;
        Ok(())
    })();

    let _ = fs::remove_file(&zip_path);
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        Err(e)
    } else {
        Ok(target)
    }
}

/// 流式下载 zip 到磁盘（进度打印到 stderr；不设总超时，词典包可达数百 MB）
fn download_zip(url: &str, dest: &Path) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(format!("libsearch/{}", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    let response = agent
        .get(url)
        .call()
        .with_context(|| format!("请求失败 {url}"))?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        bail!(
            "词典下载失败 (HTTP {status}): {url}\n该版本预编译词典可能尚未发布，可尝试其他 --version（当前 {}）",
            DEFAULT_DICTIONARY_VERSION
        );
    }
    let body = response.into_body();
    let total = body.content_length();
    let mut reader = body.into_with_config().limit(u64::MAX).reader();
    let mut out = File::create(dest).with_context(|| format!("创建 {} 失败", dest.display()))?;
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut written: u64 = 0;
    let mut last_report: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("读取中断 {url}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).with_context(|| format!("写入 {} 失败", dest.display()))?;
        written += n as u64;
        if written.saturating_sub(last_report) >= PROGRESS_STEP {
            last_report = written;
            match total {
                Some(total) => eprintln!(
                    "\r下载中: {} / {} ({:.1}%)",
                    written,
                    total,
                    written as f64 * 100.0 / total as f64
                ),
                None => eprintln!("\r下载中: {} 字节", written),
            }
        }
    }
    eprintln!();
    Ok(())
}

/// 解压 zip 中顶层目录 `lindera-{name}/` 的内容到 `dest`
fn extract_zip(zip_path: &Path, dest: &Path, top_dir: &str) -> Result<()> {
    let zip_file = File::open(zip_path)
        .with_context(|| format!("打开 {} 失败", zip_path.display()))?;
    let mut archive = ZipArchive::new(BufReader::new(zip_file))
        .with_context(|| format!("解析 zip {} 失败", zip_path.display()))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .with_context(|| format!("读取 zip 条目 {i} 失败"))?;
        let entry_name = entry
            .name()
            .with_context(|| format!("读取 zip 条目 {i} 名称失败"))?
            .into_owned();
        // 仅接受 `lindera-{name}/<文件>` 形式的条目（防意外路径）
        let rest = Path::new(&entry_name)
            .strip_prefix(top_dir)
            .with_context(|| {
                format!("zip 内条目意外: {entry_name}（期望顶层目录 {top_dir}）")
            })?;
        if rest == Path::new("") {
            continue; // 目录条目本身
        }
        if rest.components().count() != 1 {
            bail!("zip 内条目嵌套意外: {entry_name}");
        }
        let out_path = dest.join(rest);
        let mut out = File::create(&out_path)
            .with_context(|| format!("创建 {} 失败", out_path.display()))?;
        io::copy(&mut entry, &mut out).with_context(|| format!("写入条目 {entry_name} 失败"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_url_builds_expected_release_url() {
        assert_eq!(
            download_url("cc-cedict", "6.2.0"),
            "https://github.com/lindera/lindera/releases/download/v6.2.0/lindera-cc-cedict-6.2.0.zip"
        );
    }

    #[test]
    fn dict_dir_layout() {
        assert_eq!(
            dict_dir(Path::new("/home"), "cc-cedict"),
            PathBuf::from("/home/dict/cc-cedict")
        );
    }

    #[test]
    fn missing_dictionary_files_reports_all_when_dir_absent() {
        let dir = std::env::temp_dir().join("libsearch_dict_missing_test");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(missing_dictionary_files(&dir), REQUIRED_DICTIONARY_FILES.to_vec());
    }

    #[test]
    fn missing_dictionary_files_reports_only_absent() {
        let dir = std::env::temp_dir().join("libsearch_dict_partial_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for name in ["metadata.json", "char_def.bin"] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        let missing = missing_dictionary_files(&dir);
        assert!(!missing.contains(&"metadata.json"));
        assert!(!missing.contains(&"char_def.bin"));
        assert!(missing.contains(&"unk.bin"));
        let _ = fs::remove_dir_all(&dir);
    }
}
