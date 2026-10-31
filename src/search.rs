use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::params;

use crate::config::LibConfig;
use crate::db;
use crate::embed::Embedder;
use crate::ftstok::FtsTokenizer;
use crate::model_tokenizer;

/// text 输出片段最大字符数
const SNIPPET_CHARS: usize = 120;

/// 单路检索的 chunk 排名
struct Ranked {
    chunk_id: String,
    score: f32,
    rank: usize,
}

/// lindera 词项 → FTS5 MATCH 表达式：OR 连接；纯 ASCII 词加 * 前缀；词内双引号转义
pub fn build_fts_match(tokens: &[String]) -> Option<String> {
    if tokens.is_empty() {
        return None;
    }
    Some(
        tokens
            .iter()
            .map(|t| {
                let esc = t.replace('"', "\"\"");
                let q: &str = "\"";
                if t.chars().all(|c| c.is_ascii_alphanumeric()) {
                    format!("{q}{esc}{q}*")
                } else {
                    format!("{q}{esc}{q}")
                }
            })
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn snippet(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    let mut out: String = flat.chars().take(max_chars).collect();
    if flat.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// 单条检索结果
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub rank: usize,
    pub chunk_id: String,
    pub file: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
    pub snippet: String,
    pub fusion: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_rank: Option<usize>,
}

/// 检索输出（CLI 本地路径与 daemon IPC 共用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchOutput {
    pub lib: String,
    pub query: String,
    pub methods: Vec<String>,
    pub count: usize,
    pub results: Vec<Entry>,
}

/// 检索核心：返回结构化输出（不打印）；fts/embedder 由调用方提供（daemon 进程内缓存）
pub fn search_core(
    cfg: &LibConfig,
    db_path: &Path,
    model_dir: &Path,
    fts: Option<&FtsTokenizer>,
    embedder: Option<&mut Embedder>,
    query: &str,
) -> Result<SearchOutput> {
    let r = &cfg.retrieval;
    if r.fusion.weights.len() != r.methods.len() {
        bail!(
            "fusion.weights 长度（{}）须与 methods 长度（{}）一致",
            r.fusion.weights.len(),
            r.methods.len()
        );
    }
    for m in &r.methods {
        if m != "bm25" && m != "vector" {
            bail!("不支持的检索方法: {m}");
        }
    }
    if r.fusion.method != "rrf" {
        bail!("v1 仅支持 fusion.method = rrf（实际: {}）", r.fusion.method);
    }
    if query.trim().is_empty() {
        bail!("查询文本为空");
    }

    let conn = db::open(db_path)?;
    let total = conn.query_row("SELECT count(*) FROM chunks", [], |r2| r2.get::<_, i64>(0))?;
    if total == 0 {
        bail!("库 {} 索引为空，请先 reindex", cfg.id);
    }

    // 查询词项：与索引同分词器（未提供分词器时回退原文按空白切分）
    let raw_tokens = match fts {
        Some(t) => t.token_stream(query),
        None => query.to_string(),
    };
    let mut seen = BTreeSet::new();
    let tokens: Vec<String> = raw_tokens
        .split_whitespace()
        .map(str::to_string)
        .filter(|t| seen.insert(t.clone()))
        .collect();

    // 路 1：BM25（FTS5）
    let mut bm25_list: Vec<Ranked> = Vec::new();
    if r.methods.iter().any(|m| m == "bm25") {
        let match_expr =
            build_fts_match(&tokens).with_context(|| "查询无可检索词项（均为标点/空白？）")?;
        let mut stmt = conn.prepare(
            "SELECT chunk_id, bm25(chunks_fts, 0.0, 1.0)
             FROM chunks_fts WHERE chunks_fts MATCH ?1
             ORDER BY 2 ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![match_expr, r.bm25.top_k as i64], |r2| {
            Ok((r2.get::<_, String>(0)?, r2.get::<_, f64>(1)?))
        })?;
        for (i, row) in rows.enumerate() {
            let (chunk_id, score) = row?;
            let s = score.abs();
            bm25_list.push(Ranked {
                chunk_id,
                score: (s / (1.0 + s)) as f32,
                rank: i + 1,
            });
        }
    }

    // 路 2：暴力向量余弦
    let mut vec_list: Vec<Ranked> = Vec::new();
    if r.methods.iter().any(|m| m == "vector") {
        let model_fp = crate::model_tokenizer::fingerprint(model_dir)?;
        let stale = conn.query_row(
            "SELECT count(*) FROM vectors WHERE model_fp != ?1",
            params![model_fp],
            |r2| r2.get::<_, i64>(0),
        )?;
        if stale > 0 {
            bail!("向量索引已过期（{} 个 chunk 的 model_fp 不一致），请先 reindex", stale);
        }
        let mut stmt = conn.prepare("SELECT chunk_id, dims, embedding FROM vectors")?;
        let rows = stmt.query_map([], |r2| {
            Ok((
                r2.get::<_, String>(0)?,
                r2.get::<_, i64>(1)?,
                r2.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut vectors: Vec<(String, Vec<f32>)> = Vec::new();
        for row in rows {
            let (chunk_id, dims, data) = row?;
            if dims as usize * 4 != data.len() {
                bail!("向量长度异常: {chunk_id}（dims={} bytes={}）", dims, data.len());
            }
            let v: Vec<f32> = data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            vectors.push((chunk_id, v));
        }
        if vectors.is_empty() {
            bail!("库 {} 无向量，请先 reindex", cfg.id);
        }
        let emb = embedder
            .ok_or_else(|| anyhow::anyhow!("vector 检索需要 Embedder（daemon 应已加载）"))?;
        if emb.model_fp != model_fp {
            bail!("嵌入模型已变更（daemon 缓存 model_fp 不一致），请先 reload daemon");
        }
        let qv = emb.embed_batch(&[query])?;
        let qv = &qv[0];
        let qn = norm(qv).max(f32::EPSILON);
        let mut scored: Vec<(String, f32)> = vectors
            .iter()
            .map(|(chunk_id, v)| {
                let dn = norm(v).max(f32::EPSILON);
                let dot: f32 = qv.iter().zip(v).map(|(a, b)| a * b).sum();
                (chunk_id.clone(), dot / (qn * dn))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (i, (chunk_id, score)) in scored
            .into_iter()
            .take(r.vector.top_k as usize)
            .enumerate()
        {
            vec_list.push(Ranked { chunk_id, score, rank: i + 1 });
        }
    }

    // RRF 融合: score(d) = Σ_l w_l / (k + rank_l(d))
    let mut fused: BTreeMap<String, f64> = BTreeMap::new();
    for (i, m) in r.methods.iter().enumerate() {
        let list: &[Ranked] = if m == "bm25" { &bm25_list } else { &vec_list };
        let w = r.fusion.weights[i] as f64;
        for hit in list {
            *fused.entry(hit.chunk_id.clone()).or_insert(0.0) +=
                w / (r.fusion.k as f64 + hit.rank as f64);
        }
    }
    let mut order: Vec<(String, f64)> = fused.into_iter().collect();
    order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    order.retain(|(_, s)| *s >= r.min_score as f64);
    order.truncate(r.top_k as usize);

    if order.is_empty() {
        return Ok(SearchOutput {
            lib: cfg.id.clone(),
            query: query.to_string(),
            methods: r.methods.clone(),
            count: 0,
            results: Vec::new(),
        });
    }

    // chunk 元数据（单次 IN 查询）
    let ids: Vec<String> = order.iter().map(|(id, _)| id.clone()).collect();
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT chunk_id, file, kind, start_line, end_line, content
         FROM chunks WHERE chunk_id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(refs.as_slice(), |r2| {
        Ok((
            r2.get::<_, String>(0)?,
            r2.get::<_, String>(1)?,
            r2.get::<_, String>(2)?,
            r2.get::<_, i64>(3)?,
            r2.get::<_, i64>(4)?,
            r2.get::<_, String>(5)?,
        ))
    })?;
    let mut meta: BTreeMap<String, (String, String, i64, i64, String)> = BTreeMap::new();
    for row in rows {
        let (chunk_id, file, kind, sl, el, content) = row?;
        meta.insert(chunk_id, (file, kind, sl, el, content));
    }
    order.retain(|(id, _)| meta.contains_key(id.as_str()));

    let bm25_map: BTreeMap<&str, (f32, usize)> = bm25_list
        .iter()
        .map(|h| (h.chunk_id.as_str(), (h.score, h.rank)))
        .collect();
    let vec_map: BTreeMap<&str, (f32, usize)> = vec_list
        .iter()
        .map(|h| (h.chunk_id.as_str(), (h.score, h.rank)))
        .collect();

    let mut seen_files = BTreeSet::new();
    let mut results: Vec<Entry> = Vec::new();
    for (_, (id, fusion)) in order.iter().enumerate() {
        let m = &meta[id];
        if r.dedup_per_file && !seen_files.insert(m.0.clone()) {
            continue;
        }
        results.push(Entry {
            rank: results.len() + 1,
            chunk_id: id.clone(),
            file: m.0.clone(),
            kind: m.1.clone(),
            start_line: m.2,
            end_line: m.3,
            snippet: snippet(&m.4, SNIPPET_CHARS),
            fusion: *fusion,
            bm25: bm25_map.get(id.as_str()).map(|(s, _)| *s),
            bm25_rank: bm25_map.get(id.as_str()).map(|(_, rk)| *rk),
            vector: vec_map.get(id.as_str()).map(|(s, _)| *s),
            vector_rank: vec_map.get(id.as_str()).map(|(_, rk)| *rk),
        });
    }

    Ok(SearchOutput {
        lib: cfg.id.clone(),
        query: query.to_string(),
        methods: r.methods.clone(),
        count: results.len(),
        results,
    })
}

/// CLI 本地检索：加载 FtsTokenizer / Embedder（vector 时），调 search_core 并打印
pub fn search(
    cfg: &LibConfig,
    db_path: &Path,
    model_dir: &Path,
    dict_dir: Option<&Path>,
    user_dict: Option<&Path>,
    query: &str,
    json_out: bool,
) -> Result<()> {
    let fts: Option<FtsTokenizer> = match dict_dir {
        Some(p) => Some(FtsTokenizer::load(p, user_dict)?),
        None => None,
    };
    let need_vector = cfg.retrieval.methods.iter().any(|m| m == "vector");
    let mut embedder: Option<Embedder> = None;
    if need_vector {
        // stale 预检：在付 5.5s 模型加载之前快速失败
        let model_fp = model_tokenizer::fingerprint(model_dir)?;
        let conn = db::open(db_path)?;
        let stale = conn.query_row(
            "SELECT count(*) FROM vectors WHERE model_fp != ?1",
            params![model_fp],
            |r2| r2.get::<_, i64>(0),
        )?;
        if stale > 0 {
            bail!("向量索引已过期（{stale} 个 chunk 的 model_fp 不一致），请先 reindex");
        }
        drop(conn);
        embedder = Some(Embedder::load(model_dir)?);
    }
    let out = search_core(
        cfg,
        db_path,
        model_dir,
        fts.as_ref(),
        embedder.as_mut(),
        query,
    )?;
    print_search_output(&out, json_out)
}

/// 打印检索输出（CLI 与 daemon 客户端共用）
pub fn print_search_output(out: &SearchOutput, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(out)?);
    } else if out.count == 0 {
        println!("无结果");
    } else {
        println!(
            "query: {}   lib: {}   （{} 条结果）",
            out.query, out.lib, out.count
        );
        for e in &out.results {
            let mut parts: Vec<String> = Vec::new();
            if let Some(s) = e.bm25 {
                parts.push(format!("bm25={s:.4} r{}", e.bm25_rank.unwrap_or(0)));
            }
            if let Some(s) = e.vector {
                parts.push(format!("vector={s:.4} r{}", e.vector_rank.unwrap_or(0)));
            }
            println!(
                "\n{}. {}:{}-{}   fusion={:.6}   [{}]",
                e.rank, e.file, e.start_line, e.end_line, e.fusion, parts.join(", ")
            );
            println!("   {}", e.snippet);
        }
    }
    Ok(())
}