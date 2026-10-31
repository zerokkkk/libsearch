use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// serde 默认值（各 Default impl 同源；与 `lib add` 写出的配置一致）
fn d_max_tokens() -> u32 { 512 }
fn d_overlap() -> f32 { 0.15 }
fn d_top_k() -> u32 { 20 }
fn d_output_top_k() -> u32 { 10 }
fn d_rrf_k() -> u32 { 60 }
fn d_true() -> bool { true }
fn d_fusion_method() -> String { "rrf".to_string() }
fn d_methods() -> Vec<String> { vec!["bm25".to_string(), "vector".to_string()] }
fn d_weights() -> Vec<f32> { vec![1.0, 1.0] }
fn d_embedding_dir() -> String { crate::model_tokenizer::MODEL_DIR_NAME.to_string() }
fn d_dict_dir() -> Option<String> { Some("models/dict/cc-cedict".to_string()) }
fn d_exclude() -> Vec<String> {
    vec!["(^|/)(\\.git|node_modules|target|\\.venv|dist|build)(/|$)".to_string()]
}
fn d_by_type() -> BTreeMap<String, TypeChunking> {
    let mut m = BTreeMap::new();
    m.insert(
        "markdown".to_string(),
        TypeChunking { strategy: "breakpoints".to_string(), template: None },
    );
    m.insert(
        "yaml".to_string(),
        TypeChunking { strategy: "template".to_string(), template: Some("generic-kv".to_string()) },
    );
    m.insert(
        "json".to_string(),
        TypeChunking { strategy: "template".to_string(), template: Some("generic-kv".to_string()) },
    );
    m
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeChunking {
    /// breakpoints | template
    pub strategy: String,
    /// 模板名（strategy = template 时）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkingConfig {
    /// 每块目标 token 数
    #[serde(default = "d_max_tokens")]
    pub max_tokens: u32,
    /// 相邻块 overlap 比例
    #[serde(default = "d_overlap")]
    pub overlap_ratio: f32,
    /// 按文件类型分派切块策略；未列出的长文本类型默认 breakpoints
    #[serde(default = "d_by_type")]
    pub by_type: BTreeMap<String, TypeChunking>,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            max_tokens: d_max_tokens(),
            overlap_ratio: d_overlap(),
            by_type: d_by_type(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodConfig {
    #[serde(default = "d_top_k")]
    pub top_k: u32,
}

impl Default for MethodConfig {
    fn default() -> Self {
        Self { top_k: d_top_k() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionConfig {
    /// 融合方法（v1 仅 rrf）
    #[serde(default = "d_fusion_method")]
    pub method: String,
    /// RRF 常数 k
    #[serde(default = "d_rrf_k")]
    pub k: u32,
    /// 各路权重 w_l，顺序与 retrieval.methods 一致
    #[serde(default = "d_weights")]
    pub weights: Vec<f32>,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            method: d_fusion_method(),
            k: d_rrf_k(),
            weights: d_weights(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    /// 检索路列表：bm25 / vector
    #[serde(default = "d_methods")]
    pub methods: Vec<String>,
    #[serde(default)]
    pub fusion: FusionConfig,
    #[serde(default)]
    pub bm25: MethodConfig,
    #[serde(default)]
    pub vector: MethodConfig,
    /// 输出 top_k
    #[serde(default = "d_output_top_k")]
    pub top_k: u32,
    /// 融合分下限
    #[serde(default)]
    pub min_score: f32,
    /// true 时每个文件只保留最高分 chunk
    #[serde(default)]
    pub dedup_per_file: bool,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            methods: d_methods(),
            fusion: FusionConfig::default(),
            bm25: MethodConfig::default(),
            vector: MethodConfig::default(),
            top_k: d_output_top_k(),
            min_score: 0.0,
            dedup_per_file: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirFilterConfig {
    /// 包含仅根目录下的条目
    #[serde(default = "d_true")]
    pub include_root: bool,
    /// 包含文件夹及子文件夹（递归）
    #[serde(default = "d_true")]
    pub include_recursive: bool,
    /// include 正则列表（匹配文件相对路径；空 = 收遍历范围内全部）
    #[serde(default)]
    pub include: Vec<String>,
    /// exclude 正则列表
    #[serde(default = "d_exclude")]
    pub exclude: Vec<String>,
}

impl Default for DirFilterConfig {
    fn default() -> Self {
        Self {
            include_root: true,
            include_recursive: true,
            include: Vec::new(),
            exclude: d_exclude(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// 嵌入模型目录（相对程序目录或绝对路径）
    #[serde(default = "d_embedding_dir")]
    pub dir: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self { dir: d_embedding_dir() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtsConfig {
    /// 词典目录（lindera 编译产物目录，相对程序目录或绝对路径）；null = 无词典（FTS 回退原文）
    #[serde(default = "d_dict_dir")]
    pub dict_dir: Option<String>,
}

impl Default for FtsConfig {
    fn default() -> Self {
        Self { dict_dir: d_dict_dir() }
    }
}

/// 单个库的配置（库的唯一配置；目录名 == id）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibConfig {
    /// 库 id，与目录名一致
    pub id: String,
    /// 库根目录（root 的唯一来源，可变；索引以 relpath 为键不受移动影响）
    pub root: PathBuf,
    #[serde(default)]
    pub chunking: ChunkingConfig,
    #[serde(default)]
    pub retrieval: RetrievalConfig,
    #[serde(default)]
    pub dir_filter: DirFilterConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub fts: FtsConfig,
}

impl LibConfig {
    /// 构造带默认检索/切块参数的配置（与 README 示例一致）
    pub fn new(id: &str, root: PathBuf) -> Self {
        Self {
            id: id.to_string(),
            root,
            chunking: ChunkingConfig::default(),
            retrieval: RetrievalConfig::default(),
            dir_filter: DirFilterConfig::default(),
            embedding: EmbeddingConfig::default(),
            fts: FtsConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_gets_full_defaults() {
        let cfg: LibConfig = serde_json::from_str(r#"{"id": "a", "root": "D:\\docs\\a"}"#).unwrap();
        assert_eq!(cfg.chunking.max_tokens, 512);
        assert_eq!(cfg.chunking.overlap_ratio, 0.15);
        assert_eq!(cfg.chunking.by_type.len(), 3);
        assert_eq!(cfg.retrieval.methods, vec!["bm25", "vector"]);
        assert_eq!(cfg.retrieval.fusion.method, "rrf");
        assert_eq!(cfg.retrieval.fusion.k, 60);
        assert_eq!(cfg.retrieval.fusion.weights, vec![1.0, 1.0]);
        assert_eq!(cfg.retrieval.bm25.top_k, 20);
        assert_eq!(cfg.retrieval.vector.top_k, 20);
        assert_eq!(cfg.retrieval.top_k, 10);
        assert_eq!(cfg.retrieval.min_score, 0.0);
        assert!(!cfg.retrieval.dedup_per_file);
        assert!(cfg.dir_filter.include_root);
        assert!(cfg.dir_filter.include_recursive);
        assert!(cfg.dir_filter.include.is_empty());
        assert_eq!(cfg.dir_filter.exclude.len(), 1);
        assert_eq!(cfg.embedding.dir, "models/paraphrase-multilingual-MiniLM-L12-v2");
        assert_eq!(cfg.fts.dict_dir, Some("models/dict/cc-cedict".to_string()));
    }

    #[test]
    fn partial_retrieval_overrides_only_specified() {
        let cfg: LibConfig = serde_json::from_str(
            r#"{"id": "a", "root": "D:\\docs\\a", "retrieval": {"top_k": 5}}"#,
        )
        .unwrap();
        assert_eq!(cfg.retrieval.top_k, 5);
        assert_eq!(cfg.retrieval.methods, vec!["bm25", "vector"]);
        assert_eq!(cfg.retrieval.fusion.k, 60);
        assert_eq!(cfg.retrieval.bm25.top_k, 20);
    }

    #[test]
    fn embedding_fts_defaults_and_overrides() {
        let cfg: LibConfig = serde_json::from_str(
            r#"{"id": "a", "root": "D:\\docs\\a", "embedding": {"dir": "models/other-model"}, "fts": {"dict_dir": null}}"#,
        )
        .unwrap();
        assert_eq!(cfg.embedding.dir, "models/other-model");
        assert_eq!(cfg.fts.dict_dir, None);
    }
}