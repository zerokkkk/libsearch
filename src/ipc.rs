//! Windows 命名管道 IPC（JSON Lines 帧）
//!
//! 管道名：`\\.\pipe\libsearch-<sha256(canonicalize(home)) 前 8 位>`
//! 协议：一行一个 JSON。请求 `{"op": ...}`；响应 `{"ok": bool, "payload": ..., "error": ...}`

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use named_pipe::{PipeClient, PipeOptions, PipeServer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::search::SearchOutput;

/// 客户端等待管道可用的预检超时（毫秒）
const WAIT_PIPE_MS: u32 = 2000;
/// 客户端读响应超时（search 首查询含模型加载，给足）
const CLIENT_READ_TIMEOUT: Duration = Duration::from_secs(120);
/// 客户端写请求超时
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 服务端读请求超时
const SERVER_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// 服务端写响应超时
const SERVER_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// 管道名：`\\.\pipe\libsearch-<sha256(canonicalize(home)) 前 8 位>`
pub fn pipe_name(home: &Path) -> Result<String> {
    let canon = std::fs::canonicalize(home)
        .with_context(|| format!("无法解析 home 路径 {}", home.display()))?;
    let s = canon.to_string_lossy().to_string();
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    Ok(format!("\\\\.\\pipe\\libsearch-{}", &digest[0..8]))
}

/// IPC 请求（serde tag = "op"）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    Reload,
    Search {
        lib: String,
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        json: Option<bool>,
    },
}

/// daemon 状态中的库信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibInfo {
    pub id: String,
    pub root: String,
    pub files: i64,
}

/// daemon 状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub home: String,
    pub pipe: String,
    pub uptime_secs: u64,
    pub libs: Vec<LibInfo>,
    /// 已缓存的嵌入模型路径
    pub cached_models: Vec<String>,
    /// 已缓存的 FTS 分词器缓存键
    pub cached_tokenizers: Vec<String>,
}

/// IPC 响应 payload（serde tag = "kind"）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    Ping { pong: String },
    Status(StatusInfo),
    Reload { cleared: Vec<String> },
    Search(SearchOutput),
}

/// IPC 响应（一行一个 JSON）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Payload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn WaitNamedPipeW(name: *const u16, timeout_ms: u32) -> i32;
}

/// 客户端：预检管道可用 → 连接 → 写请求行 → 读响应行
pub fn request(home: &Path, req: &Request) -> Result<Payload> {
    let name = pipe_name(home)?;
    // 预检：无 daemon（或独占实例正忙）时快速失败，而不是干等连接
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    if unsafe { WaitNamedPipeW(wide.as_ptr(), WAIT_PIPE_MS) } == 0 {
        bail!("daemon 未运行或正忙（pipe {name}）");
    }
    let mut client = PipeClient::connect(&name)
        .with_context(|| format!("连接管道 {name} 失败（daemon 未运行？）"))?;
    client.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
    client.set_read_timeout(Some(CLIENT_READ_TIMEOUT));
    client.write_all(serde_json::to_string(req)?.as_bytes())?;
    client.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(&mut client).read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        bail!("daemon 未返回响应（管道已断开）");
    }
    let resp: Response = serde_json::from_str(line)
        .with_context(|| format!("响应解析失败: {line}"))?;
    if !resp.ok {
        bail!("daemon 错误: {}", resp.error.unwrap_or_else(|| "（无详情）".into()));
    }
    resp.payload.ok_or_else(|| anyhow!("daemon 响应缺少 payload"))
}

/// 服务端：单线程同步 accept 循环（`.single()` 独占实例）
pub fn serve(home: &Path, mut handle: impl FnMut(&Request) -> Result<Payload>) -> Result<()> {
    let name = pipe_name(home)?;
    let mut connecting = PipeOptions::new(&name).single()?;
    loop {
        let mut server = connecting.wait()?;
        handle_conn(&mut server, &mut handle);
        connecting = server.disconnect()?;
    }
}

/// 处理单个连接：读一行请求 → 分发 → 写一行响应（错误不崩 daemon）
fn handle_conn(conn: &mut PipeServer, handle: &mut impl FnMut(&Request) -> Result<Payload>) {
    conn.set_read_timeout(Some(SERVER_READ_TIMEOUT));
    conn.set_write_timeout(Some(SERVER_WRITE_TIMEOUT));
    let mut line = String::new();
    let n = match BufReader::new(&mut *conn).read_line(&mut line) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("读取请求失败: {e}");
            return;
        }
    };
    if n == 0 {
        return; // 客户端已断开
    }
    let resp = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => match handle(&req) {
            Ok(payload) => Response { ok: true, payload: Some(payload), error: None },
            Err(e) => Response {
                ok: false,
                payload: None,
                error: Some(format!("{e:#}")),
            },
        },
        Err(e) => Response {
            ok: false,
            payload: None,
            error: Some(format!("请求解析失败: {e}")),
        },
    };
    let mut out = match serde_json::to_string(&resp) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("响应序列化失败: {e}");
            return;
        }
    };
    out.push('\n');
    if let Err(e) = conn.write_all(out.as_bytes()) {
        eprintln!("写响应失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serde_roundtrip() {
        let reqs = vec![
            Request::Ping,
            Request::Status,
            Request::Reload,
            Request::Search { lib: "demo".into(), query: "hello 世界".into(), json: Some(true) },
            Request::Search { lib: "x".into(), query: "y".into(), json: None },
        ];
        for req in reqs {
            let s = serde_json::to_string(&req).unwrap();
            let back: Request = serde_json::from_str(&s).unwrap();
            assert_eq!(s, serde_json::to_string(&back).unwrap());
        }
    }

    #[test]
    fn response_payload_serde_roundtrip() {
        let payload = Payload::Search(SearchOutput {
            lib: "demo".into(),
            query: "q".into(),
            methods: vec!["bm25".into()],
            count: 1,
            results: vec![crate::search::Entry {
                rank: 1,
                chunk_id: "c1".into(),
                file: "a.md".into(),
                kind: "md".into(),
                start_line: 1,
                end_line: 2,
                snippet: "s".into(),
                fusion: 0.5,
                bm25: Some(0.5),
                bm25_rank: Some(1),
                vector: None,
                vector_rank: None,
            }],
        });
        let resp = Response { ok: true, payload: Some(payload), error: None };
        let s = serde_json::to_string(&resp).unwrap();
        assert!(!s.contains("\"error\""));
        let back: Response = serde_json::from_str(&s).unwrap();
        match back.payload {
            Some(Payload::Search(out)) => {
                assert_eq!(out.count, 1);
                assert_eq!(out.results[0].chunk_id, "c1");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
        // error 分支：payload 缺省
        let err = Response { ok: false, payload: None, error: Some("boom".into()) };
        let s = serde_json::to_string(&err).unwrap();
        let back: Response = serde_json::from_str(&s).unwrap();
        assert!(!back.ok && back.payload.is_none() && back.error.as_deref() == Some("boom"));
    }

    #[test]
    fn pipe_name_deterministic_and_home_dependent() {
        let a = std::env::temp_dir().join(format!("libsearch_ipc_a_{}", std::process::id()));
        let b = std::env::temp_dir().join(format!("libsearch_ipc_b_{}", std::process::id()));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let na = pipe_name(&a).unwrap();
        let nb = pipe_name(&b).unwrap();
        assert_eq!(na, pipe_name(&a).unwrap());
        assert_ne!(na, nb);
        assert!(na.starts_with("\\\\.\\pipe\\libsearch-"));
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}