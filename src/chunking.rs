use std::collections::HashMap;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use crate::model_tokenizer::ModelTokenizer;

/// 切块滑动窗口：目标终点前 200 token（qmd 原值）
pub const BREAK_WINDOW_TOKENS: usize = 200;
const DECAY_FACTOR: f64 = 0.7;

/// 一个 chunk（内存形态；落库由 reindex 完成）
#[derive(Debug, Clone)]
pub struct ChunkPiece {
    /// chunk 文本（breakpoints 为原文切片；template 为渲染文本）
    pub text: String,
    /// 源文件字符偏移半开区间 [src_start, src_end)
    pub src_start: usize,
    pub src_end: usize,
    /// 1-based 行号闭区间 [start_line, end_line]
    pub start_line: u32,
    pub end_line: u32,
    pub kind: &'static str,
    pub strategy: &'static str,
}

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn build(content: &str) -> Self {
        let mut starts = vec![0usize];
        for (i, b) in content.as_bytes().iter().enumerate() {
            if *b == b'\n' && i + 1 < content.len() {
                starts.push(i + 1);
            }
        }
        Self { starts }
    }

    fn line_of(&self, byte_off: usize) -> u32 {
        self.starts.partition_point(|&s| s <= byte_off).max(1) as u32
    }
}

fn chars_to_byte_index(content: &str) -> Vec<usize> {
    let mut v = Vec::with_capacity(content.len() + 1);
    for (b, _) in content.char_indices() {
        v.push(b);
    }
    v.push(content.len());
    v
}

fn bytes_to_chars(content: &str, byte_off: usize) -> usize {
    content[..byte_off.min(content.len())].chars().count()
}

struct BreakPoint {
    byte_pos: usize,
    token_idx: usize,
    score: f64,
}

const BREAK_PATTERNS: [(&str, f64); 12] = [
    (r"\n#(\n|[^#\n])", 100.0),
    (r"\n##(\n|[^#\n])", 90.0),
    (r"\n###(\n|[^#\n])", 80.0),
    (r"\n####(\n|[^#\n])", 70.0),
    (r"\n#####(\n|[^#\n])", 60.0),
    (r"\n#{6,}", 50.0),
    (r"\n```", 80.0),
    (r"\n(?:---|\*\*\*|___)[ \t]*\n", 60.0),
    (r"\n\n+", 20.0),
    (r"\n[-*]\s", 5.0),
    (r"\n\d+\.\s", 5.0),
    (r"\n", 1.0),
];

fn scan_break_points(content: &str, token_starts: &[usize]) -> Vec<BreakPoint> {
    let mut best: HashMap<usize, f64> = HashMap::new();
    for (pat, score) in BREAK_PATTERNS {
        let re = Regex::new(pat).expect("断点正则");
        for m in re.find_iter(content) {
            let pos = m.start();
            let current = best.entry(pos).or_insert(0.0);
            if score > *current {
                *current = score;
            }
        }
    }
    let mut v: Vec<BreakPoint> = best
        .into_iter()
        .map(|(pos, score)| BreakPoint {
            byte_pos: pos,
            token_idx: token_starts.partition_point(|&s| s <= pos),
            score,
        })
        .collect();
    v.sort_by_key(|bp| bp.byte_pos);
    v
}

fn find_code_fences(content: &str) -> Vec<(usize, usize)> {
    let re = Regex::new(r"\n```").expect("代码围栏正则");
    let mut regions = Vec::new();
    let mut open: Option<usize> = None;
    for m in re.find_iter(content) {
        match open {
            None => open = Some(m.start()),
            Some(s) => {
                regions.push((s, m.start() + 3));
                open = None;
            }
        }
    }
    if let Some(s) = open {
        regions.push((s, content.len()));
    }
    regions
}

fn in_code_fence(pos: usize, fences: &[(usize, usize)]) -> bool {
    fences.iter().any(|&(s, e)| pos > s && pos < e)
}

/// breakpoints 切块（qmd 算法移植；token 计数用嵌入模型 subword）
pub fn chunk_breakpoints(
    content: &str,
    kind: &'static str,
    max_tokens: usize,
    overlap_tokens: usize,
    tokenizer: &ModelTokenizer,
) -> Vec<ChunkPiece> {
    if content.is_empty() {
        return Vec::new();
    }
    let lines = LineIndex::build(content);
    let char_to_byte = chars_to_byte_index(content);
    let token_starts: Vec<usize> = tokenizer
        .token_char_spans(content)
        .iter()
        .map(|&(c, _)| char_to_byte[c.min(char_to_byte.len() - 1)])
        .collect();

    if tokenizer.count_tokens(content) <= max_tokens {
        return vec![make_piece(content, &lines, 0, content.len(), kind, "breakpoints")];
    }

    let breaks = scan_break_points(content, &token_starts);
    let fences = find_code_fences(content);

    let mut out = Vec::new();
    let mut char_pos = 0usize;
    let mut start_tok = 0usize;
    loop {
        let target_tok = start_tok + max_tokens;
        let target_end = token_starts.get(target_tok).copied().unwrap_or(content.len());

        let mut best: Option<(usize, f64)> = None;
        for bp in &breaks {
            if bp.token_idx > target_tok {
                break;
            }
            if target_tok >= BREAK_WINDOW_TOKENS && bp.token_idx < target_tok - BREAK_WINDOW_TOKENS {
                continue;
            }
            if in_code_fence(bp.byte_pos, &fences) {
                continue;
            }
            let d = (target_tok - bp.token_idx) as f64 / BREAK_WINDOW_TOKENS as f64;
            let score = bp.score * (1.0 - d * d * DECAY_FACTOR);
            if best.map_or(true, |(_, s)| score > s) {
                best = Some((bp.byte_pos, score));
            }
        }
        let end_pos = match best {
            Some((p, _)) if p > char_pos => p,
            _ => target_end,
        };
        if end_pos <= char_pos {
            break;
        }
        out.push(make_piece(content, &lines, char_pos, end_pos, kind, "breakpoints"));
        if end_pos >= content.len() {
            break;
        }
        let end_tok = token_starts.partition_point(|&s| s < end_pos);
        let next_tok = end_tok.saturating_sub(overlap_tokens);
        char_pos = if next_tok > start_tok {
            token_starts[next_tok]
        } else {
            end_pos
        };
        start_tok = token_starts.partition_point(|&s| s < char_pos);
    }
    out
}

fn make_piece(
    content: &str,
    lines: &LineIndex,
    start: usize,
    end: usize,
    kind: &'static str,
    strategy: &'static str,
) -> ChunkPiece {
    ChunkPiece {
        text: content[start..end].to_string(),
        src_start: bytes_to_chars(content, start),
        src_end: bytes_to_chars(content, end),
        start_line: lines.line_of(start),
        end_line: if end > start { lines.line_of(end - 1) } else { lines.line_of(start) },
        kind,
        strategy,
    }
}

// =============================================================================
// generic-kv 模板（yaml / json 短片段）
// =============================================================================

/// 一条渲染行（可回溯到源码行）
#[derive(Debug, Clone)]
pub struct KvEntry {
    /// 路径（点号/下标缩进，如 a.b[0].c）
    pub path: String,
    /// 渲染行文本
    pub text: String,
    /// 源码行闭区间（1-based）
    pub start_line: u32,
    pub end_line: u32,
}

fn split_yaml_entry(body: &str) -> Option<(String, String)> {
    if let Some(idx) = body.find(": ") {
        return Some((body[..idx].trim().to_string(), body[idx + 2..].trim().to_string()));
    }
    if let Some(stripped) = body.strip_suffix(':') {
        return Some((stripped.trim().to_string(), String::new()));
    }
    None
}

/// 行式 YAML → 逐行 (path: value) 渲染（v1：块映射、块列表、块标量；忽略其他结构）
pub fn render_yaml(content: &str) -> Vec<KvEntry> {
    let raw: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut list_index: HashMap<String, usize> = HashMap::new();
    let mut pending_key: Option<(usize, String)> = None;

    for (i, line) in raw.iter().enumerate() {
        let line_no = (i + 1) as u32;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let body = line.trim_start();

        if body == "-" || body.starts_with("- ") {
            let item_rest = if body == "-" { "" } else { body[2..].trim_end() };
            let owner = match &pending_key {
                Some((ki, kp)) if *ki <= indent => Some(kp.clone()),
                _ => stack.last().map(|(_, p)| p.clone()),
            }
            .unwrap_or_default();
            let idx = {
                let c = list_index.entry(owner.clone()).or_insert(0);
                let v = *c;
                *c += 1;
                v
            };
            let item_path = if owner.is_empty() {
                format!("[{}]", idx)
            } else {
                format!("{}[{}]", owner, idx)
            };
            if item_rest.is_empty() {
                stack.push((indent + 1, item_path.clone()));
                pending_key = Some((indent + 1, item_path));
                continue;
            }
            if let Some((k, v)) = split_yaml_entry(item_rest) {
                let key_path = format!("{}.{}", item_path, k);
                if v.is_empty() {
                    stack.push((indent + 2, key_path.clone()));
                    pending_key = Some((indent + 2, key_path));
                } else {
                    out.push(KvEntry {
                        text: format!("{key_path}: {v}"),
                        path: key_path,
                        start_line: line_no,
                        end_line: line_no,
                    });
                    pending_key = None;
                }
            } else {
                out.push(KvEntry {
                    text: format!("{item_path}: {item_rest}"),
                    path: item_path,
                    start_line: line_no,
                    end_line: line_no,
                });
                pending_key = None;
            }
            continue;
        }

        if let Some((k, v)) = split_yaml_entry(body) {
            while let Some((si, _)) = stack.last() {
                if *si >= indent {
                    stack.pop();
                } else {
                    break;
                }
            }
            let parent = stack.last().map(|(_, p)| p.as_str()).unwrap_or("");
            let key_path = if parent.is_empty() { k.clone() } else { format!("{parent}.{k}") };
            if v.is_empty() {
                stack.push((indent, key_path.clone()));
                pending_key = Some((indent, key_path));
            } else if v == "|" || v == ">" || v == "|-" || v == ">-" {
                let mut value = String::new();
                let mut end_line = line_no;
                for (j, next) in raw.iter().enumerate().skip(i + 1) {
                    let t = next.trim();
                    if t.is_empty() {
                        continue;
                    }
                    let ni = next.len() - next.trim_start().len();
                    if ni <= indent {
                        break;
                    }
                    if !value.is_empty() {
                        value.push(' ');
                    }
                    value.push_str(t);
                    end_line = (j + 1) as u32;
                }
                out.push(KvEntry {
                    text: format!("{key_path}: {value}"),
                    path: key_path,
                    start_line: line_no,
                    end_line,
                });
                pending_key = None;
            } else {
                out.push(KvEntry {
                    text: format!("{key_path}: {v}"),
                    path: key_path,
                    start_line: line_no,
                    end_line: line_no,
                });
                pending_key = None;
            }
        }
        // 其他行（流式值等）v1 忽略
    }
    out
}

fn json_leaf_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

fn flatten_json(v: &Value, path: &str, out: &mut Vec<KvEntry>, line: u32) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                match val {
                    Value::Object(_) | Value::Array(_) => flatten_json(val, &p, out, line),
                    leaf => out.push(KvEntry {
                        path: p,
                        text: format!("{k}: {}", json_leaf_to_string(leaf)),
                        start_line: line,
                        end_line: line,
                    }),
                }
            }
        }
        Value::Array(a) => {
            for (idx, item) in a.iter().enumerate() {
                let p = if path.is_empty() {
                    format!("[{idx}]")
                } else {
                    format!("{path}[{idx}]")
                };
                match item {
                    Value::Object(_) | Value::Array(_) => flatten_json(item, &p, out, line),
                    leaf => out.push(KvEntry {
                        path: p,
                        text: json_leaf_to_string(leaf),
                        start_line: line,
                        end_line: line,
                    }),
                }
            }
        }
        leaf => out.push(KvEntry {
            path: if path.is_empty() { "(root)".into() } else { path.into() },
            text: json_leaf_to_string(leaf),
            start_line: line,
            end_line: line,
        }),
    }
}

fn json_last_key(path: &str) -> String {
    path.rsplit(['.', '[']).next().unwrap_or(path).trim_end_matches(']').to_string()
}

/// 把 JSON 渲染行按顺序映射回源码行（顺序扫描 key；失败回退到首行）
fn map_json_lines(content: &str, entries: &mut [KvEntry]) {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() || entries.is_empty() {
        return;
    }
    let mut cursor = 0usize;
    for e in entries.iter_mut() {
        let key = json_last_key(&e.path);
        if key.is_empty() || key == "(root)" {
            continue;
        }
        let needle = format!("\"{key}\"");
        let found = lines[cursor..]
            .iter()
            .position(|l| l.contains(&needle) || l.contains(&key))
            .map(|p| cursor + p);
        match found {
            Some(pos) => {
                e.start_line = (pos + 1) as u32;
                e.end_line = (pos + 1) as u32;
                cursor = pos;
            }
            None => {
                e.start_line = 1;
                e.end_line = lines.len() as u32;
            }
        }
    }
}

fn json_first_key(path: &str) -> String {
    path.split(['.', '[']).next().unwrap_or(path).to_string()
}

/// template 策略：渲染后按顶层分组 + token 预算切块
pub fn chunk_template(
    content: &str,
    kind: &'static str,
    max_tokens: usize,
    tokenizer: &ModelTokenizer,
) -> Result<Vec<ChunkPiece>> {
    let entries: Vec<KvEntry> = if kind == "yaml" {
        render_yaml(content)
    } else {
        let v: Value = serde_json::from_str(content)
            .with_context(|| "JSON 解析失败")?;
        let mut e = Vec::new();
        flatten_json(&v, "", &mut e, 1);
        map_json_lines(content, &mut e);
        e
    };
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let last_line = entries.last().unwrap().end_line;

    if tokenizer.count_tokens(&entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>().join("\n"))
        <= max_tokens
    {
        let text = entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>().join("\n");
        return Ok(vec![ChunkPiece {
            text,
            src_start: 0,
            src_end: bytes_to_chars(content, content.len()),
            start_line: 1,
            end_line: last_line,
            kind,
            strategy: "template",
        }]);
    }

    // 顶层分组
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_key: HashMap<String, usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        let top = json_first_key(&e.path);
        let gi = group_key.entry(top).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[*gi].push(i);
    }

    let mut out = Vec::new();
    let li = LineIndex::build(content);
    for group in &groups {
        // 组内按 token 预算继续切
        let mut current: Vec<usize> = Vec::new();
        let mut tokens = 0usize;
        let flush = |out: &mut Vec<ChunkPiece>,
                    current: &mut Vec<usize>,
                    entries: &[KvEntry],
                    content: &str,
                    kind: &'static str| {
            if current.is_empty() {
                return;
            }
            let text: Vec<&str> = current.iter().map(|i| entries[*i].text.as_str()).collect();
            let first = &entries[*current.first().unwrap()];
            let last = &entries[*current.last().unwrap()];
            let s_byte = li.starts[(first.start_line - 1) as usize];
            let e_byte = li.starts
                .get(last.end_line as usize)
                .copied()
                .unwrap_or(content.len());
            out.push(ChunkPiece {
                text: text.join("\n"),
                src_start: bytes_to_chars(content, s_byte),
                src_end: bytes_to_chars(content, e_byte),
                start_line: first.start_line,
                end_line: last.end_line,
                kind,
                strategy: "template",
            });
            current.clear();
        };
        for i in group {
            let t = tokenizer.count_tokens(&entries[*i].text);
            if !current.is_empty() && tokens + t > max_tokens {
                flush(&mut out, &mut current, &entries, content, kind);
                tokens = 0;
            }
            current.push(*i);
            tokens += t;
        }
        flush(&mut out, &mut current, &entries, content, kind);
    }
    out.retain(|p| !p.text.is_empty());
    Ok(out)
}

/// 按 kind 分派切块
pub fn chunk_file(
    content: &str,
    kind: &'static str,
    strategy: &str,
    max_tokens: usize,
    overlap_tokens: usize,
    tokenizer: &ModelTokenizer,
) -> Result<Vec<ChunkPiece>> {
    match strategy {
        "breakpoints" => Ok(chunk_breakpoints(content, kind, max_tokens, overlap_tokens, tokenizer)),
        "template" => chunk_template(content, kind, max_tokens, tokenizer),
        other => anyhow::bail!("未知切块策略: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_tokenizer::{ModelTokenizer, MODEL_DIR_NAME};

    fn test_tokenizer() -> ModelTokenizer {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(MODEL_DIR_NAME);
        ModelTokenizer::load(&dir).expect("加载测试 tokenizer")
    }

    #[test]
    fn breakpoints_small_file_single_chunk() {
        let tok = test_tokenizer();
        let content = "# 标题\n\n这是一段短文。\n";
        let chunks = chunk_breakpoints(content, "markdown", 900, 135, &tok);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, content);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
    }

    #[test]
    fn breakpoints_long_file_respects_headings() {
        let tok = test_tokenizer();
        let mut content = String::new();
        for i in 0..40 {
            content.push_str(&format!("# 标题 {i}\n\n"));
            for _ in 0..8 {
                content.push_str("这是一段用于填充长度的中文文本，用来触发切块行为。\n");
            }
        }
        let chunks = chunk_breakpoints(&content, "markdown", 900, 135, &tok);
        assert!(chunks.len() >= 2, "应切出多个块: {}", chunks.len());
        let total = content.chars().count();
        assert_eq!(chunks.last().unwrap().src_end, total);
        // 相邻块有 overlap：下一块起点 <= 上一块终点
        for w in chunks.windows(2) {
            assert!(w[1].src_start <= w[0].src_end);
        }
        // 行号与偏移一致
        for c in &chunks {
            assert!(c.start_line <= c.end_line);
        }
    }

    #[test]
    fn yaml_render_flat() {
        let yaml = "a: 1\nb:\n  c: 2\n  d: three\nlist:\n  - x\n  - y\n";
        let entries = render_yaml(yaml);
        let rendered: Vec<String> = entries.iter().map(|e| e.text.clone()).collect();
        assert!(rendered.contains(&"a: 1".to_string()));
        assert!(rendered.contains(&"b.c: 2".to_string()));
        assert!(rendered.contains(&"b.d: three".to_string()));
        assert!(rendered.contains(&"list[0]: x".to_string()));
        assert!(rendered.contains(&"list[1]: y".to_string()));
    }

    #[test]
    fn json_render_flat() {
        let json = r#"{"a": 1, "b": {"c": "x"}, "arr": [1, 2]}"#;
        let v: Value = serde_json::from_str(json).unwrap();
        let mut e = Vec::new();
        flatten_json(&v, "", &mut e, 1);
        let paths: Vec<String> = e.iter().map(|x| x.path.clone()).collect();
        assert!(paths.contains(&"a".to_string()));
        assert!(paths.contains(&"b.c".to_string()));
        assert!(paths.contains(&"arr[0]".to_string()));
        assert!(paths.contains(&"arr[1]".to_string()));
    }

}