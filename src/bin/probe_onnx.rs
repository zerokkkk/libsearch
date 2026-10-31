use std::time::Instant;

use anyhow::{Context, Result};
use fastembed::{InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel};

fn main() -> Result<()> {
    let model_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("models")
        .join("paraphrase-multilingual-MiniLM-L12-v2");
    let read = |name: &str| {
        std::fs::read(model_dir.join(name)).with_context(|| format!("读取 {name} 失败"))
    };
    let model = UserDefinedEmbeddingModel::new(
        read("onnx/model.onnx")?,
        TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        },
    )
    .with_pooling(Pooling::Mean);
    let mut emb = TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())
        .context("加载模型失败")?;

    // 1) 短查询：验证加载 + 维度 + 归一化
    let t = Instant::now();
    let v = emb.embed(["rust 本地文档检索引擎"], None)?;
    let norm = v[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    println!("short: dim={} norm={:.4} first3={:?} ({:.1} ms)", v[0].len(), norm, v[0].iter().take(3).map(|x| format!("{x:.3}")).collect::<Vec<_>>(), t.elapsed().as_secs_f64() * 1000.0);

    // 2) 长文本：约 1000 CJK 字符，验证 512 截断
    let long: String = std::iter::repeat("这是一个用于探测截断行为的中文填充句子，包含标点，用来验证模型窗口。")
        .take(40)
        .collect();
    let enc = emb.tokenizer.encode(&*long, true).unwrap();
    println!("long: chars={} encoded_len(截断后)={}", long.chars().count(), enc.len());
    let t = Instant::now();
    let v = emb.embed([long.as_str()], None)?;
    println!("long: dim={} ({:.1} ms)", v[0].len(), t.elapsed().as_secs_f64() * 1000.0);

    // 3) 批量吞吐：8 条中等长度文本
    let texts: Vec<String> = (0..8)
        .map(|i| format!("第 {i} 段：嵌入吞吐测试文本，用于估算 reindex 时的编码速度。"))
        .collect();
    let t = Instant::now();
    let v = emb.embed(&texts, None)?;
    println!("batch8: {} 向量 ({:.1} ms, {:.1} ms/条)", v.len(), t.elapsed().as_secs_f64() * 1000.0, t.elapsed().as_secs_f64() * 125.0);
    Ok(())
}
