//! daemon：长驻进程，进程内缓存 Embedder / FtsTokenizer，经 ipc 服务请求

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;

use crate::db;
use crate::embed::Embedder;
use crate::ftstok::FtsTokenizer;
use crate::home::resource_paths;
use crate::ipc::{self, LibInfo, Payload, Request, StatusInfo};
use crate::libs;
use crate::search;

/// FtsTokenizer 缓存键：(dict_dir, user_dict)
type FtsKey = (Option<PathBuf>, Option<PathBuf>);

/// 长驻 daemon：懒加载 + 进程内缓存
pub struct Daemon {
    pub home: PathBuf,
    pub started_at: Instant,
    embed_cache: HashMap<PathBuf, Embedder>,
    fts_cache: HashMap<FtsKey, FtsTokenizer>,
}

impl Daemon {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            started_at: Instant::now(),
            embed_cache: HashMap::new(),
            fts_cache: HashMap::new(),
        }
    }

    /// 预热指定库的模型与分词器（启动时可选）
    pub fn warm(&mut self, lib_id: &str) -> Result<()> {
        let cfg = libs::load(&self.home, lib_id)?;
        let (model_dir, dict_dir, user_dict) = resource_paths(&self.home, &cfg);
        self.ensure_embedder(&model_dir)?;
        self.ensure_fts(dict_dir.as_deref(), user_dict.as_deref())?;
        Ok(())
    }

    /// 处理一条 IPC 请求（每次现读库配置，保证 lib add/remove 即时生效）
    pub fn handle(&mut self, req: &Request) -> Result<Payload> {
        match req {
            Request::Ping => Ok(Payload::Ping { pong: "pong".to_string() }),
            Request::Status => Ok(Payload::Status(self.status())),
            Request::Reload => Ok(Payload::Reload { cleared: self.reload() }),
            Request::Search { lib, query, .. } => {
                let cfg = libs::load(&self.home, lib)?;
                let db_path = libs::lib_dir(&self.home, lib).join("index.db");
                let (model_dir, dict_dir, user_dict) = resource_paths(&self.home, &cfg);
                let need_vector = cfg.retrieval.methods.iter().any(|m| m == "vector");
                if need_vector {
                    self.ensure_embedder(&model_dir)?;
                }
                self.ensure_fts(dict_dir.as_deref(), user_dict.as_deref())?;
                let fts_key = (dict_dir, user_dict);
                let fts = match &fts_key.0 {
                    Some(_) => self.fts_cache.get_mut(&fts_key),
                    None => None,
                };
                let mut embedder = if need_vector {
                    self.embed_cache.get_mut(&model_dir)
                } else {
                    None
                };
                let out = search::search_core(
                    &cfg,
                    &db_path,
                    &model_dir,
                    fts.as_deref(),
                    embedder.as_deref_mut(),
                    query,
                )?;
                Ok(Payload::Search(out))
            }
        }
    }

    /// 当前状态快照
    pub fn status(&self) -> StatusInfo {
        let libs = libs::list(&self.home)
            .map(|cs| {
                cs.iter()
                    .map(|c| {
                        let dbp = libs::lib_dir(&self.home, &c.id).join("index.db");
                        let files = db::open(&dbp)
                            .and_then(|conn| {
                                conn.query_row("SELECT count(*) FROM files", [], |r| {
                                    r.get::<_, i64>(0)
                                })
                                .map_err(anyhow::Error::from)
                            })
                            .unwrap_or(0);
                        LibInfo {
                            id: c.id.clone(),
                            root: c.root.to_string_lossy().into_owned(),
                            files,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        StatusInfo {
            home: self.home.to_string_lossy().into_owned(),
            pipe: ipc::pipe_name(&self.home).unwrap_or_default(),
            uptime_secs: self.started_at.elapsed().as_secs(),
            libs,
            cached_models: self
                .embed_cache
                .keys()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            cached_tokenizers: self.fts_cache.keys().map(describe_fts_key).collect(),
        }
    }

    /// 清空全部缓存（模型/词典更新后调用），返回被清键的描述
    pub fn reload(&mut self) -> Vec<String> {
        let cleared: Vec<String> = self
            .embed_cache
            .keys()
            .map(|p| format!("model: {}", p.display()))
            .chain(self.fts_cache.keys().map(|k| format!("fts: {}", describe_fts_key(k))))
            .collect();
        self.embed_cache.clear();
        self.fts_cache.clear();
        cleared
    }

    fn ensure_embedder(&mut self, model_dir: &Path) -> Result<()> {
        if !self.embed_cache.contains_key(model_dir) {
            let e = Embedder::load(model_dir).map_err(|err| {
                anyhow::anyhow!("加载嵌入模型失败（{}）: {err:#}", model_dir.display())
            })?;
            self.embed_cache.insert(model_dir.to_path_buf(), e);
        }
        Ok(())
    }

    fn ensure_fts(&mut self, dict_dir: Option<&Path>, user_dict: Option<&Path>) -> Result<()> {
        let key = (dict_dir.map(Path::to_path_buf), user_dict.map(Path::to_path_buf));
        let dict = match &key.0 {
            Some(d) => d,
            None => return Ok(()),
        };
        if !self.fts_cache.contains_key(&key) {
            let t = FtsTokenizer::load(dict, key.1.as_deref()).map_err(|err| {
                anyhow::anyhow!("加载 FTS 分词器失败（{}）: {err:#}", dict.display())
            })?;
            self.fts_cache.insert(key, t);
        }
        Ok(())
    }
}

/// 缓存键 → 可读描述
fn describe_fts_key(k: &FtsKey) -> String {
    match (&k.0, &k.1) {
        (Some(d), Some(u)) => format!("{} + {}", d.display(), u.display()),
        (Some(d), None) => format!("{}", d.display()),
        (None, _) => "none".to_string(),
    }
}