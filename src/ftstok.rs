use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lindera::dictionary::{load_dictionary, load_user_dictionary};
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use sha2::{Digest, Sha256};

/// FTS 分词：lindera（CC-CEDICT）+ 可选用户词典
pub struct FtsTokenizer {
    segmenter: Segmenter,
}

impl FtsTokenizer {
    /// dict_dir：lindera 词典目录（含 metadata.json 与编译产物）；user_dict_csv：用户词典 CSV（可无）
    /// 用户词典按主词典 metadata.json 的 user_dictionary_schema 解析（lindera 内置 load_user_dictionary）
    pub fn load(dict_dir: &Path, user_dict_csv: Option<&Path>) -> Result<Self> {
        let dictionary = load_dictionary(dict_dir.to_str().context("词典路径须为 UTF-8")?)
            .map_err(|e| anyhow::anyhow!("加载词典 {} 失败: {e}", dict_dir.display()))?;
        let user = match user_dict_csv {
            Some(p) if p.is_file() => Some(
                load_user_dictionary(p.to_str().context("用户词典路径须为 UTF-8")?, &dictionary.metadata)
                    .map_err(|e| anyhow::anyhow!("加载用户词典 {} 失败: {e}", p.display()))?,
            ),
            _ => None,
        };
        let segmenter = Segmenter::new(Mode::Normal, dictionary, user);
        Ok(Self { segmenter })
    }

    /// 词元流（空格分隔；纯标点/空白词元剔除）
    pub fn token_stream(&self, text: &str) -> String {
        let mut out = String::new();
        if let Ok(tokens) = self.segmenter.segment(Cow::Borrowed(text)) {
            for token in tokens {
                let surface = token.surface.as_ref();
                if surface.chars().any(|c| c.is_alphanumeric()) {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(surface);
                }
            }
        }
        out
    }
}

/// 词典指纹：词典目录（按文件名排序）与用户词典的 sha256（名称+大小+内容）；均无 → "none"
pub fn dict_fingerprint(dict_dir: Option<&Path>, user_dict: Option<&Path>) -> Result<String> {
    let mut h = Sha256::new();
    let mut any = false;
    if let Some(dir) = dict_dir {
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        for e in std::fs::read_dir(dir).with_context(|| format!("读取词典目录 {} 失败", dir.display()))? {
            let e = e?;
            let p = e.path();
            if p.is_file() {
                entries.push((e.file_name().to_string_lossy().to_string(), p));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, path) in &entries {
            update_file_hash(&mut h, name, path)?;
            any = true;
        }
    }
    if let Some(p) = user_dict {
        update_file_hash(&mut h, "userdict.csv", p)?;
        any = true;
    }
    if !any {
        return Ok("none".to_string());
    }
    Ok(format!("{:x}", h.finalize()))
}

fn update_file_hash(h: &mut Sha256, name: &str, path: &Path) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("读取词典文件 {} 失败", path.display()))?;
    h.update(name.as_bytes());
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(&bytes);
    Ok(())
}