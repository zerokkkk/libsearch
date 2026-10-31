//! libsearch 引擎库：切块 / 索引 / 检索 / daemon（Windows 命名管道 IPC）

pub mod chunking;
pub mod config;
pub mod db;
pub mod dict;
pub mod embed;
pub mod ftstok;
pub mod home;
pub mod libs;
pub mod model;
pub mod model_tokenizer;
pub mod reindex;
pub mod search;

#[cfg(windows)]
pub mod daemon;
#[cfg(windows)]
pub mod ipc;