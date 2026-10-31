use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fastembed::{
    InitOptionsUserDefined, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use crate::model;
use crate::model_tokenizer;

/// 嵌入模型封装：fastembed 加载本地 ONNX；
/// pooling 由模型目录名推导（model::pooling_for），配置中不指定
pub struct Embedder {
    model: TextEmbedding,
    /// 模型指纹（与 ModelTokenizer::fingerprint 一致）
    pub model_fp: String,
}

/// 在 model_dir 下找模型 .onnx 文件（递归，支持 onnx/ 子目录布局与平铺布局）：
/// 1) 优先标准名 model.onnx（树内唯一时）
/// 2) 否则树内仅一个 .onnx 时用之
/// 3) 否则报错（列出文件）
fn find_onnx(model_dir: &Path) -> Result<PathBuf> {
    let mut all: Vec<PathBuf> = Vec::new();
    let mut stack = vec![model_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("无法读取模型目录 {}", dir.display()))?;
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("onnx") {
                all.push(path);
            }
        }
    }
    let names: Vec<String> = all
        .iter()
        .map(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
        .collect();
    let canonical: Vec<PathBuf> = all
        .iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("model.onnx"))
        .cloned()
        .collect();
    match canonical.len() {
        1 => Ok(canonical.into_iter().next().expect("len == 1")),
        _ => match all.len() {
            1 => Ok(all.into_iter().next().expect("len == 1")),
            0 => bail!("模型目录 {} 中未找到 .onnx 模型文件", model_dir.display()),
            n => bail!(
                "模型目录 {} 中发现 {n} 个 .onnx 文件且无法唯一确定（需要唯一的 model.onnx 或仅一个 .onnx）: {}",
                model_dir.display(),
                names.join(", ")
            ),
        },
    }
}

impl Embedder {
    /// model_dir: 程序目录下的模型目录（如 models/<预设名>）
    pub fn load(model_dir: &Path) -> Result<Self> {
        let read_file = |p: &Path| {
            std::fs::read(p).with_context(|| format!("读取模型文件 {} 失败", p.display()))
        };

        let onnx_path = find_onnx(model_dir)?;
        let mut model = UserDefinedEmbeddingModel::new(
            read_file(&onnx_path)?,
            TokenizerFiles {
                tokenizer_file: read_file(&model_dir.join("tokenizer.json"))?,
                config_file: read_file(&model_dir.join("config.json"))?,
                special_tokens_map_file: read_file(&model_dir.join("special_tokens_map.json"))?,
                tokenizer_config_file: read_file(&model_dir.join("tokenizer_config.json"))?,
            },
        )
        .with_pooling(model::pooling_for(model_dir));

        // ONNX 外部数据：模型文件同目录的 <model file>.data（量化/大模型常见）
        let data_path = onnx_path.with_extension("onnx.data");
        if data_path.is_file() {
            let name = data_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            model = model.with_external_initializer(name, read_file(&data_path)?);
        }

        let model = TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())
            .context("加载嵌入模型失败")?;
        let model_fp = model_tokenizer::fingerprint(model_dir)?;
        Ok(Self { model, model_fp })
    }

    /// 批量编码（每段 ≤ 模型窗口，不截断）
    pub fn embed_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.model
            .embed(texts, None)
            .map_err(|e| anyhow::anyhow!("嵌入编码失败: {e}"))
    }
}
