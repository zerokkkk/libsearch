use std::time::Instant;

use anyhow::{Context, Result};

/// 暴力余弦扫描探针：读取库 vectors 表，可选复制到目标规模，计时单查询全扫描。
/// 用法: probe_scan <home> <lib> [target_count]
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: probe_scan <home> <lib> [target_count]");
        return Ok(());
    }
    let home = std::path::Path::new(&args[1]);
    let lib = &args[2];
    let db_path = home.join("database").join("libs").join(lib).join("index.db");
    let target: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100_000);

    let conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("打开 {} 失败", db_path.display()))?;
    let mut stmt = conn
        .prepare("SELECT dims, embedding FROM vectors ORDER BY rowid")
        .context("准备语句失败")?;
    let rows = stmt.query_map([], |row| {
        let dims: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        Ok((dims, blob))
    }).context("查询 vectors 失败")?;

    let mut vecs: Vec<Vec<f32>> = Vec::new();
    let mut dims = 0usize;
    for r in rows {
        let (d, blob) = r.context("读取行失败")?;
        if vecs.is_empty() {
            dims = d as usize;
        }
        let v: Vec<f32> = blob
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        vecs.push(v);
    }
    if vecs.is_empty() {
        anyhow::bail!("vectors 表为空，先 reindex");
    }

    let q: Vec<f32> = normalize(&vecs[0]);
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    println!("lib={} dims={} real_chunks={} profile={}", lib, dims, vecs.len(), profile);

    // 实际规模：单次计时
    let t = Instant::now();
    let (ri, rs) = scan_once(&q, &vecs);
    let real_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("real:   n={:<7} top1=idx {} score={:.4}  t={:.2} ms", vecs.len(), ri, rs, real_ms);

    // 复制（循环）到目标规模
    let mut all: Vec<Vec<f32>> = vecs.clone();
    if all.len() < target {
        let base = all.len();
        all.reserve(target - base);
        let mut i = 0;
        while all.len() < target {
            all.push(vecs[i % base].clone());
            i += 1;
        }
    }
    let iters = 5;
    let _ = scan_once(&q, &all); // 预热
    let t = Instant::now();
    let mut best = (0usize, f32::MIN);
    for _ in 0..iters {
        let b = scan_once(&q, &all);
        if b.1 > best.1 {
            best = b;
        }
    }
    let avg_ms = t.elapsed().as_secs_f64() * 1000.0 / iters as f64;
    println!(
        "padded: n={:<7} top1=idx {} score={:.4}  iters={} avg={:.2} ms/scan  ({:.2} ms/1k chunks)",
        all.len(), best.0, best.1, iters, avg_ms, avg_ms * 1000.0 / (all.len() as f64 / 1000.0)
    );
    Ok(())
}

fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / norm).collect()
}

fn scan_once(q: &[f32], vecs: &[Vec<f32>]) -> (usize, f32) {
    let mut best_i = 0usize;
    let mut best_s = f32::MIN;
    for (i, v) in vecs.iter().enumerate() {
        let mut dot = 0.0f32;
        for j in 0..q.len() {
            dot += q[j] * v[j];
        }
        if dot > best_s {
            best_s = dot;
            best_i = i;
        }
    }
    (best_i, best_s)
}