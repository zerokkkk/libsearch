use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokenizers::Tokenizer;

/// 模型目录名（相对程序目录；config 默认值）
pub const MODEL_DIR_NAME: &str = "models/paraphrase-multilingual-MiniLM-L12-v2";

/// 嵌入模型的 subword 分词器：切块时用它数 token（不做字符估算）
pub struct ModelTokenizer {
    tokenizer: Tokenizer,
    /// 模型指纹：目录名|pooling|sha256(tokenizer.json)
    pub fingerprint: String,
}

/// 模型指纹：模型目录名 | pooling | sha256(tokenizer.json)
/// （model_fp 的唯一身份来源；pooling 按目录名查预设表推导，见 crate::model）
pub fn fingerprint(model_dir: &Path) -> Result<String> {
    let path = model_dir.join("tokenizer.json");
    let bytes = std::fs::read(&path)
        .with_context(|| format!("无法读取 tokenizer 文件 {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = format!("{:x}", hasher.finalize());
    let dir_name = model_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>");
    let pooling = crate::model::pooling_name(crate::model::pooling_for(model_dir));
    Ok(format!("{dir_name}|{pooling}|sha256:{digest}"))
}

impl ModelTokenizer {
    pub fn load(model_dir: &Path) -> Result<Self> {
        let path = model_dir.join("tokenizer.json");
        let bytes = std::fs::read(&path)
            .with_context(|| format!("无法读取 tokenizer 文件 {}", path.display()))?;
        let mut tokenizer = Tokenizer::from_bytes(&bytes)
            .map_err(|e| anyhow::anyhow!("tokenizer 解析失败 {}: {e}", path.display()))?;
        tokenizer
            .with_truncation(None)
            .map_err(|e| anyhow::anyhow!("清除 truncation 失败: {e}"))?;
        let fingerprint = fingerprint(model_dir)?;
        Ok(Self { tokenizer, fingerprint })
    }

    /// 文本的 subword token 数（不含特殊 token）
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer.encode(text, false).map(|enc| enc.len()).unwrap_or(0)
    }

    /// 各 token 的字符偏移区间（tokenizers 返回字符偏移，非字节）
    pub fn token_char_spans(&self, text: &str) -> Vec<(usize, usize)> {
        match self.tokenizer.encode(text, false) {
            Ok(enc) => enc.get_offsets().to_vec(),
            Err(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format_includes_dir_and_pooling() {
        let dir = std::env::temp_dir()
            .join(format!("libsearch_fp_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        let fp = fingerprint(&dir).unwrap();
        assert!(fp.starts_with("libsearch_fp_test_"), "fp = {fp}");
        assert!(fp.contains("|mean|sha256:"), "fp = {fp}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
