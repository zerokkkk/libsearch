use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::chunking::{self, ChunkPiece};
use crate::config::{ChunkingConfig, DirFilterConfig, LibConfig};
use crate::ftstok::FtsTokenizer;
use crate::embed::Embedder;
use crate::model_tokenizer::ModelTokenizer;

pub struct ReindexStats {
    pub scanned: usize,
    pub added: usize,
    pub changed: usize,
    pub fts_only: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub chunks: usize,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 受支持文件类型（v1）：扩展名 → kind
fn kind_of(relpath: &str) -> Option<&'static str> {
    let ext = relpath.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "md" => Some("markdown"),
        "txt" => Some("txt"),
        "yaml" | "yml" => Some("yaml"),
        "json" => Some("json"),
        _ => None,
    }
}

/// 按 dir_filter 遍历 root，返回 (relpath[正斜杠], 完整路径)
pub fn walk(root: &Path, filter: &DirFilterConfig) -> Result<Vec<(String, PathBuf)>> {
    let includes: Vec<Regex> = filter
        .include
        .iter()
        .map(|p| Regex::new(p).with_context(|| format!("无效 include 正则 {p}")))
        .collect::<Result<_>>()?;
    let excludes: Vec<Regex> = filter
        .exclude
        .iter()
        .map(|p| Regex::new(p).with_context(|| format!("无效 exclude 正则 {p}")))
        .collect::<Result<_>>()?;
    let mut out = Vec::new();
    walk_dir(root, root, "", filter, &includes, &excludes, &mut out)?;
    Ok(out)
}

fn walk_dir(
    dir: &Path,
    root: &Path,
    rel: &str,
    filter: &DirFilterConfig,
    includes: &[Regex],
    excludes: &[Regex],
    out: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("无法读取目录 {}", dir.display()))?
    {
        let entry = entry?;
        let full = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if full.is_dir() {
            if filter.include_recursive {
                walk_dir(&full, root, &child_rel, filter, includes, excludes, out)?;
            }
        } else if full.is_file() {
            let in_scope = if rel.is_empty() {
                filter.include_root
            } else {
                filter.include_recursive
            };
            if !in_scope || kind_of(&child_rel).is_none() {
                continue;
            }
            if excludes.iter().any(|r| r.is_match(&child_rel)) {
                continue;
            }
            if !includes.is_empty() && !includes.iter().any(|r| r.is_match(&child_rel)) {
                continue;
            }
            out.push((child_rel, full));
        }
    }
    Ok(())
}

fn strategy_for(kind: &str, chunking: &ChunkingConfig) -> String {
    if let Some(t) = chunking.by_type.get(kind) {
        return t.strategy.clone();
    }
    match kind {
        "yaml" | "json" => "template".to_string(),
        _ => "breakpoints".to_string(),
    }
}

fn replace_file_chunks(
    conn: &mut Connection,
    lib_id: &str,
    relpath: &str,
    pieces: &[ChunkPiece],
    embeddings: &[Vec<f32>],
    model_fp: &str,
    dict_fp: &str,
    fts: &Option<FtsTokenizer>,
) -> Result<()> {
    let tx = conn.transaction()
        .context("开启文件级事务失败")?;
    {
        let mut stmt = tx.prepare("SELECT chunk_id FROM chunks WHERE file = ?1")?;
        let mut ids: Vec<String> = stmt
            .query_map([relpath], |row| row.get(0))
            .context("读取旧 chunk id 失败")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("读取旧 chunk id 失败")?;
        let mut del_fts = tx.prepare("DELETE FROM chunks_fts WHERE chunk_id = ?1")?;
        let mut del_vec = tx.prepare("DELETE FROM vectors WHERE chunk_id = ?1")?;
        for id in ids.drain(..) {
            del_fts.execute(params![id.as_str()])?;
            del_vec.execute(params![id.as_str()])?;
        }
    }
    tx.execute("DELETE FROM chunks WHERE file = ?1", [relpath])?;
        {
            let mut ins_chunk = tx.prepare(
                "INSERT INTO chunks (chunk_id, file, kind, strategy, seq, start_off, end_off, start_line, end_line, content_hash, model_fp, dict_fp, content)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            let mut ins_fts = tx.prepare("INSERT INTO chunks_fts (chunk_id, content) VALUES (?1, ?2)")?;
            let mut ins_vec = tx
                .prepare("INSERT INTO vectors (chunk_id, model_fp, dims, embedding) VALUES (?1, ?2, ?3, ?4)")?;
            for (seq, p) in pieces.iter().enumerate() {
                let chunk_id = format!("{lib_id}|{relpath}#{seq}");
                let content_hash = sha256_hex(p.text.as_bytes());
                let fts_text = match fts {
                    Some(t) => t.token_stream(&p.text),
                    None => p.text.clone(),
                };
                ins_chunk.execute(params![
                    chunk_id,
                    relpath,
                    p.kind,
                    p.strategy,
                    seq as i64,
                    p.src_start as i64,
                    p.src_end as i64,
                    p.start_line as i64,
                    p.end_line as i64,
                    content_hash,
                    model_fp,
                    dict_fp,
                    p.text,
                ])?;
                ins_fts.execute(params![chunk_id, fts_text])?;
                let bytes: Vec<u8> =
                    embeddings[seq].iter().flat_map(|x| x.to_le_bytes()).collect();
                ins_vec.execute(params![chunk_id, model_fp, embeddings[seq].len() as i64, bytes])?;
            }
        }
    tx.commit().context("提交文件级事务失败")?;
    Ok(())
}

/// 仅词典变化（内容未变）：逐 chunk 替换 FTS 文本并写 dict_fp；向量保留
fn replace_file_fts(
    conn: &mut Connection,
    relpath: &str,
    fts: &Option<FtsTokenizer>,
    dict_fp: &str,
) -> Result<()> {
    let tx = conn.transaction()
        .context("开启文件级事务失败")?;
    let mut rows: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT chunk_id, content FROM chunks WHERE file = ?1 ORDER BY seq")?;
        stmt.query_map([relpath], |row| Ok((row.get(0)?, row.get(1)?)))
            .context("读取 chunks 失败")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("读取 chunks 失败")?
    };
    {
        let mut del_fts = tx.prepare("DELETE FROM chunks_fts WHERE chunk_id = ?1")?;
        let mut ins_fts = tx.prepare("INSERT INTO chunks_fts (chunk_id, content) VALUES (?1, ?2)")?;
        for (chunk_id, content) in rows.drain(..) {
            let fts_text = match fts {
                Some(t) => t.token_stream(&content),
                None => content.clone(),
            };
            del_fts.execute(params![chunk_id.as_str()])?;
            ins_fts.execute(params![chunk_id, fts_text])?;
        }
    }
    tx.execute("UPDATE chunks SET dict_fp = ?1 WHERE file = ?2", params![dict_fp, relpath])?;
    tx.commit().context("提交文件级事务失败")?;
    Ok(())
}

fn delete_file_chunks(conn: &mut Connection, relpath: &str) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare("SELECT chunk_id FROM chunks WHERE file = ?1")?;
        let ids: Vec<String> = stmt
            .query_map([relpath], |row| row.get(0))
            .context("读取旧 chunk id 失败")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("读取旧 chunk id 失败")?;
        let mut del_fts = tx.prepare("DELETE FROM chunks_fts WHERE chunk_id = ?1")?;
        let mut del_vec = tx.prepare("DELETE FROM vectors WHERE chunk_id = ?1")?;
        for id in ids {
            del_fts.execute(params![id.as_str()])?;
            del_vec.execute(params![id.as_str()])?;
        }
    }
    tx.execute("DELETE FROM chunks WHERE file = ?1", [relpath])?;
    tx.execute("DELETE FROM files WHERE relpath = ?1", [relpath])?;
    tx.commit()?;
    Ok(())
}

/// 增量 reindex：只用内容哈希 diff
pub fn reindex(
    cfg: &LibConfig,
    db_path: &Path,
    model_dir: &Path,
    dict_dir: Option<&Path>,
    user_dict: Option<&Path>,
    full: bool,
) -> Result<ReindexStats> {
    if !cfg.root.is_dir() {
        anyhow::bail!("root 目录不存在: {}", cfg.root.display());
    }
    let model_tok = ModelTokenizer::load(model_dir)?;
    let fts_tok = match dict_dir {
        Some(p) => Some(FtsTokenizer::load(p, user_dict)?),
        None => None,
    };
    let dict_fp = crate::ftstok::dict_fingerprint(dict_dir, user_dict)?;
    let mut conn = crate::db::open(db_path)?;

    let files = walk(&cfg.root, &cfg.dir_filter)?;
    let existing: HashMap<String, String> = {
        let mut m = HashMap::new();
        let mut stmt = conn.prepare("SELECT relpath, content_hash FROM files")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (k, v) = row?;
            m.insert(k, v);
        }
        m
    };
    // 模型指纹变化或向量缺失的文件：即使内容哈希未变也需要重切块+重编码
    let stale_vec: std::collections::HashSet<String> = if full {
        std::collections::HashSet::new()
    } else {
        let mut s = std::collections::HashSet::new();
        {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT file FROM chunks WHERE model_fp IS NULL OR model_fp != ?1",
            )?;
            let rows = stmt.query_map([model_tok.fingerprint.as_str()], |r| {
                r.get::<_, String>(0)
            })?;
            for row in rows {
                s.insert(row?);
            }
        }
        {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT c.file FROM chunks c LEFT JOIN vectors v ON v.chunk_id = c.chunk_id
                 WHERE v.chunk_id IS NULL",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for row in rows {
                s.insert(row?);
            }
        }
        s
    };
    // 词典指纹变化（或旧库 NULL）：仅需重分词 FTS 文本；向量可保留（chunk_id 不变）
    let stale_fts: std::collections::HashSet<String> = if full {
        std::collections::HashSet::new()
    } else {
        let mut s = std::collections::HashSet::new();
        {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT file FROM chunks WHERE dict_fp IS NULL OR dict_fp != ?1",
            )?;
            let rows = stmt.query_map([dict_fp.as_str()], |r| r.get::<_, String>(0))?;
            for row in rows {
                s.insert(row?);
            }
        }
        s
    };

    let max_tokens = cfg.chunking.max_tokens as usize;
    let overlap_tokens = (cfg.chunking.max_tokens as usize)
        .mul_cfg_ratio(cfg.chunking.overlap_ratio);
    let mut stats = ReindexStats {
        scanned: files.len(),
        added: 0,
        changed: 0,
        fts_only: 0,
        unchanged: 0,
        removed: 0,
        chunks: 0,
    };
    let stdout = std::io::stdout();
    let mut embedder: Option<Embedder> = None;
    for (i, (rel, full_path)) in files.iter().enumerate() {
        {
            let mut lock = stdout.lock();
            let _ = write!(
                lock,
                "\rreindex {}: {i}/{} 文件",
                cfg.id,
                files.len()
            );
        }
        let bytes = std::fs::read(full_path)
            .with_context(|| format!("无法读取 {}", full_path.display()))?;
        let hash = sha256_hex(&bytes);
        let is_new = !existing.contains_key(rel);
        let hash_changed = !is_new && existing.get(rel) != Some(&hash);
        let needs_full = full || is_new || hash_changed || stale_vec.contains(rel);
        let fts_only = !needs_full && stale_fts.contains(rel);
        if !needs_full && !fts_only {
            stats.unchanged += 1;
            continue;
        }
        if fts_only {
            // 词典指纹变化（内容未变）：仅替换 FTS 文本并写 dict_fp，向量保留
            replace_file_fts(&mut conn, rel, &fts_tok, &dict_fp)?;
            stats.fts_only += 1;
            continue;
        }
        let content = String::from_utf8_lossy(&bytes).to_string();
        let kind = kind_of(rel).expect("受支持文件");
        let strategy = strategy_for(kind, &cfg.chunking);
        let pieces = chunking::chunk_file(
            &content,
            kind,
            &strategy,
            max_tokens,
            overlap_tokens,
            &model_tok,
        )?;
        if embedder.is_none() {
            let emb = Embedder::load(model_dir).context("加载嵌入模型失败")?;
            if emb.model_fp != model_tok.fingerprint {
                anyhow::bail!("嵌入模型指纹与 tokenizer 指纹不一致");
            }
            embedder = Some(emb);
        }
        let emb = embedder.as_mut().unwrap();
        let texts: Vec<&str> = pieces.iter().map(|p| p.text.as_str()).collect();
        let embeddings = emb.embed_batch(&texts)?;
        replace_file_chunks(&mut conn, &cfg.id, rel, &pieces, &embeddings, &model_tok.fingerprint, &dict_fp, &fts_tok)?;
        conn.execute(
            "INSERT INTO files (relpath, content_hash, size, indexed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(relpath) DO UPDATE SET content_hash = ?2, size = ?3, indexed_at = ?4",
            params![rel, hash, bytes.len() as i64, now_secs()],
        )?;
        if is_new {
            stats.added += 1;
        } else {
            stats.changed += 1;
        }
    }

    for rel in existing.keys() {
        if !files.iter().any(|(r, _)| r == rel) {
            delete_file_chunks(&mut conn, rel)?;
            stats.removed += 1;
        }
    }
    stats.chunks = conn
        .query_row("SELECT count(*) FROM chunks", [], |r| r.get::<_, i64>(0))?
        as usize;

    println!(
        "\rreindex {}: 扫描 {} 文件: 新增 {} 变更 {} 未变 {} 删除 {} fts-only {} chunks {}      ",
        cfg.id,
        stats.scanned,
        stats.added,
        stats.changed,
        stats.unchanged,
        stats.removed,
        stats.fts_only,
        stats.chunks
    );
    Ok(stats)
}

trait Ratio {
    fn mul_cfg_ratio(&self, ratio: f32) -> usize;
}

impl Ratio for usize {
    fn mul_cfg_ratio(&self, ratio: f32) -> usize {
        (*self as f32 * ratio) as usize
    }
}