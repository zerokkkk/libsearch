//! 嵌入模型管理：pooling 推导、模型下载（model pull）
//!
//! 模型目录名是模型身份的唯一来源：
//! - pooling 运行时由 fastembed 预设表推导（配置中无字段）
//! - model_fp = `目录名|pooling|sha256(tokenizer.json)`（见 model_tokenizer::fingerprint）

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use fastembed::{EmbeddingModel, Pooling, TextEmbedding};

/// 模型的 pooling 方式：按模型目录名查 fastembed 预设表；
/// 非预设名 / 查不到 → Mean（现有模型行为不变）
pub fn pooling_for(model_dir: &Path) -> Pooling {
    let Some(dir_name) = model_dir.file_name().and_then(|n| n.to_str()) else {
        return Pooling::Mean;
    };
    match EmbeddingModel::from_str(dir_name) {
        Ok(m) => TextEmbedding::get_default_pooling_method(&m).unwrap_or(Pooling::Mean),
        Err(_) => Pooling::Mean,
    }
}

/// 指纹用的 pooling 名："mean" / "cls"
pub fn pooling_name(pooling: Pooling) -> &'static str {
    match pooling {
        Pooling::Cls => "cls",
        Pooling::Mean => "mean",
    }
}

/// `model pull` 参数
pub struct PullSpec {
    /// 预设名（EmbeddingModel 变体名，大小写不敏感）；与 repo/file 互斥
    pub preset: Option<String>,
    /// 任意 HF repo `org/name`
    pub repo: Option<String>,
    /// repo 内 onnx 文件路径（相对路径，可含子目录）；与 repo 同现
    pub file: Option<String>,
    /// models/ 下目标目录名（默认：预设名 / repo 名）
    pub dir: Option<String>,
    /// 额外下载的文件（repo 相对路径，可重复）
    pub extra: Vec<String>,
}

/// HF repo 根目录必须提供的 4 个 tokenizer 文件
const TOKENIZER_FILES: [&str; 4] = [
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

/// 把模型下载到 `models/<dir>`，验证可加载后返回目标目录
pub fn pull(home: &Path, spec: &PullSpec) -> Result<PathBuf> {
    match (spec.preset.as_deref(), spec.repo.as_deref(), spec.file.as_deref()) {
        (Some(_), None, None) => {}
        (None, Some(_), Some(_)) => {}
        _ => bail!(
            "用法: `model pull <preset>` 或 `model pull --repo org/name --file <repo内路径>`（两种形式互斥）"
        ),
    }

    // 解析来源：(repo, 模型文件, 附加文件, 预设名)
    let (model_code, model_file, additional, preset_name) = match spec.preset.as_deref() {
        Some(preset) => {
            let m = EmbeddingModel::from_str(preset)
                .map_err(|e| anyhow::anyhow!("未知预设模型名 {preset}: {e}（可用 --repo/--file 拉取任意模型）"))?;
            let info = TextEmbedding::get_model_info(&m)?;
            (
                info.model_code.clone(),
                info.model_file.clone(),
                info.additional_files.clone(),
                Some(preset.to_string()),
            )
        }
        None => (
            spec.repo.clone().unwrap(),
            spec.file.clone().unwrap(),
            Vec::new(),
            None,
        ),
    };

    let dir_name = spec
        .dir
        .clone()
        .unwrap_or_else(|| preset_name.unwrap_or_else(|| model_code.rsplit('/').next().unwrap().to_string()));

    let target = home.join("models").join(&dir_name);
    if target.exists() {
        let empty = std::fs::read_dir(&target)
            .with_context(|| format!("无法读取目录 {}", target.display()))?
            .next()
            .is_none();
        if !empty {
            bail!("目录已存在且非空，拒绝覆盖: {}", target.display());
        }
    }
    std::fs::create_dir_all(&target)
        .with_context(|| format!("创建模型目录失败 {}", target.display()))?;

    // 文件清单（repo 相对路径，保序去重）
    let mut files: Vec<String> = Vec::new();
    let mut push_unique = |f: &str| {
        if !files.iter().any(|x| x == f) {
            files.push(f.to_string());
        }
    };
    push_unique(&model_file);
    for f in &additional {
        push_unique(f);
    }
    for f in &spec.extra {
        push_unique(f);
    }
    for f in TOKENIZER_FILES {
        push_unique(f);
    }

    // hf-hub 下载（尊重 HF_ENDPOINT / HF_TOKEN）
    use hf_hub::api::sync::{ApiBuilder, ApiError};

    let mut builder = ApiBuilder::new()
        .with_cache_dir(home.join(".hf-cache"))
        .with_progress(true);
    if let Ok(endpoint) = std::env::var("HF_ENDPOINT") {
        if !endpoint.is_empty() {
            builder = builder.with_endpoint(endpoint);
        }
    }
    if let Ok(token) = std::env::var("HF_TOKEN") {
        if !token.is_empty() {
            builder = builder.with_token(Some(token));
        }
    }
    let api = builder.build().context("初始化 hf-hub API 失败")?;
    let repo = api.model(model_code.clone());
    println!("从 {model_code} 下载 {} 个文件:", files.len());
    for f in &files {
        let dst = target.join(f);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败 {}", parent.display()))?;
        }
        let cached = repo
            .get(f)
            .with_context(|| format!("下载 {model_code}/{f} 失败"))?;
        std::fs::copy(&cached, &dst)
            .with_context(|| format!("复制 {} -> {} 失败", cached.display(), dst.display()))?;
        println!("  {model_code}/{f}");
    }

    // ONNX 外部数据（尽力）：模型文件同目录的 <model file>.data；repo 没有(404)则跳过
    let data_name = format!("{model_file}.data");
    match repo.get(&data_name) {
        Ok(cached) => {
            let dst = target.join(&data_name);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建目录失败 {}", parent.display()))?;
            }
            std::fs::copy(&cached, &dst)
                .with_context(|| format!("复制 {} -> {} 失败", cached.display(), dst.display()))?;
            println!("  {model_code}/{data_name} (外部数据)");
        }
        Err(ApiError::RequestError(e)) if is_not_found(&e) => {
            println!("  (repo 无 {data_name}，跳过)");
        }
        Err(e) => {
            bail!("尝试下载外部数据 {model_code}/{data_name} 失败: {e}")
        }
    }

    // 验证：加载模型（外部数据缺失时 ORT 会点名缺失文件）
    let mut emb = crate::embed::Embedder::load(&target)
        .with_context(|| format!("模型验证加载失败 {}", target.display()))?;
    let dim = emb.embed_batch(&["probe"])?[0].len();
    println!(
        "模型就绪: {} (dim={}, pooling={})",
        target.display(),
        dim,
        pooling_name(pooling_for(&target))
    );
    Ok(target)
}

/// 404（文件不存在）
fn is_not_found(e: &ureq::Error) -> bool {
    matches!(e, ureq::Error::StatusCode(404))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pooling_for 只依赖目录名，不碰文件系统
    fn dir_named(name: &str) -> PathBuf {
        Path::new("models").join(name)
    }

    #[test]
    fn pooling_for_preset_names() {
        assert_eq!(pooling_for(&dir_named("BGEBaseENV15")), Pooling::Cls);
        assert_eq!(pooling_for(&dir_named("AllMiniLML6V2")), Pooling::Mean);
        // 大小写不敏感
        assert_eq!(pooling_for(&dir_named("bgebaseenv15")), Pooling::Cls);
    }

    #[test]
    fn pooling_for_unknown_falls_back_to_mean() {
        // 现有模型目录名（非预设变体名）→ Mean，行为不变
        assert_eq!(pooling_for(&dir_named("paraphrase-multilingual-MiniLM-L12-v2")), Pooling::Mean);
        assert_eq!(pooling_for(&dir_named("some-custom-model")), Pooling::Mean);
    }

    #[test]
    fn pooling_name_roundtrip() {
        assert_eq!(pooling_name(Pooling::Mean), "mean");
        assert_eq!(pooling_name(Pooling::Cls), "cls");
    }
}
