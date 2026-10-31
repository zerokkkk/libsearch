use std::path::Path;

use rusqlite::Connection;

/// 单库索引库 schema：files / chunks / chunks_fts / vectors 四张核心表
pub const SCHEMA: &str = r#"
-- 文件级哈希（增量 reindex 的 diff 依据）
CREATE TABLE IF NOT EXISTS files (
    relpath      TEXT PRIMARY KEY,      -- 相对 root 的路径
    content_hash TEXT NOT NULL,         -- 文件内容 sha256
    size         INTEGER NOT NULL DEFAULT 0,
    indexed_at   INTEGER NOT NULL DEFAULT 0
);

-- chunk 元数据（文件内位置 + 内容哈希 + 模型指纹）
CREATE TABLE IF NOT EXISTS chunks (
    chunk_id     TEXT PRIMARY KEY,      -- <lib_id>|<relpath>#<seq>
    file         TEXT NOT NULL,         -- 相对 root 的路径
    kind         TEXT NOT NULL,         -- markdown / txt / yaml / json
    strategy     TEXT NOT NULL,         -- breakpoints / template
    seq          INTEGER NOT NULL,      -- 文件内序号
    start_off    INTEGER NOT NULL,      -- 字符偏移半开区间 [start_off, end_off)
    end_off      INTEGER NOT NULL,
    start_line   INTEGER NOT NULL,      -- 1-based 闭区间行号
    end_line     INTEGER NOT NULL,
    content_hash TEXT NOT NULL,         -- chunk 文本 sha256
    model_fp     TEXT,                  -- 嵌入模型指纹（模型+版本）
    content      TEXT NOT NULL          -- chunk 文本（template 为渲染文本）
);
CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file);

-- BM25 全文索引（external content 于 P2 接线；v1 独立 content 存储）
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    chunk_id UNINDEXED,
    content,
    tokenize = 'unicode61'
);

-- chunk 向量（float32 little-endian 连续字节）
CREATE TABLE IF NOT EXISTS vectors (
    chunk_id  TEXT PRIMARY KEY,
    model_fp  TEXT NOT NULL,
    dims      INTEGER NOT NULL,
    embedding BLOB NOT NULL
);
"#;

/// 打开（不存在则创建）库索引库，并保证 schema 存在
pub fn open(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    init_schema(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// 旧库迁移：chunks 加 dict_fp 列（存量行 NULL，reindex 自动回填）
fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let has: bool = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('chunks') WHERE name = 'dict_fp'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)?;
    if !has {
        conn.execute("ALTER TABLE chunks ADD COLUMN dict_fp TEXT", [])?;
    }
    Ok(())
}
pub fn init_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}