//! libsearch CLI：库管理 / reindex / search / daemon 客户端

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use libsearch::{db, dict, home, libs, model, reindex, search};

#[derive(Parser)]
#[command(
    name = "libsearch",
    version,
    about = "本地轻量多库文档搜索引擎：BM25 + 轻量向量 (ONNX CPU) + RRF 融合"
)]
struct Cli {
    /// 程序目录（默认当前可执行文件所在目录，可被环境变量 LIBSEARCH_HOME 覆盖）
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 库管理
    Lib {
        #[command(subcommand)]
        cmd: LibCmd,
    },

    /// 重建库索引（P1）
    Reindex {
        /// 库 id
        lib: String,
        /// 强制全量重建
        #[arg(long)]
        full: bool,
    },

    /// 检索（P2；--daemon 走常驻 daemon）
    Search {
        /// 库 id
        lib: String,
        /// 查询文本
        query: String,
        /// JSON 输出
        #[arg(long)]
        json: bool,
        /// 走常驻 daemon（未运行时快速报错）
        #[arg(long)]
        daemon: bool,
    },

    /// 嵌入模型管理
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// 词典管理
    Dict {
        #[command(subcommand)]
        cmd: DictCmd,
    },

    /// daemon 存活检查
    #[cfg(windows)]
    Ping,

    /// daemon 状态
    #[cfg(windows)]
    Status,

    /// daemon 清缓存重载
    #[cfg(windows)]
    Reload,
}

#[derive(Subcommand)]
enum LibCmd {
    /// 新增库：创建 config.json 并初始化 index.db
    Add {
        /// 库 id（目录名，须唯一）
        id: String,
        /// 库根目录（须存在）
        root: PathBuf,
        /// 包含仅根目录下的条目
        #[arg(short = 'r', long = "root")]
        include_root: bool,
        /// 包含文件夹及子文件夹（递归）
        #[arg(short = 'R', long)]
        recursive: bool,
        /// include 正则（可多次；为空 = 收遍历范围内全部文件）
        #[arg(short = 'i', long)]
        include: Vec<String>,
        /// exclude 正则（可多次；不指定则保留默认排除规则）
        #[arg(short = 'x', long)]
        exclude: Option<Vec<String>>,
    },

    /// 列出全部库
    List,

    /// 删除库（整个库目录连同索引）
    Remove {
        /// 库 id
        id: String,
        /// 跳过确认
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// 下载嵌入模型到 models/<name>（预设名或任意 HF repo）
    Pull {
        /// 预设模型名（fastembed 变体名，大小写不敏感，如 ParaphraseMLMiniLML12V2Q）
        preset: Option<String>,
        /// 任意 HF repo org/name（与 preset 互斥）
        #[arg(long)]
        repo: Option<String>,
        /// repo 内 onnx 文件路径（须与 --repo 同现）
        #[arg(long)]
        file: Option<String>,
        /// models/ 下目标目录名（默认：预设名 / repo 名）
        #[arg(long)]
        dir: Option<String>,
        /// 额外下载的文件（repo 相对路径，可重复）
        #[arg(long)]
        extra: Vec<String>,
    },
}

#[derive(Subcommand)]
enum DictCmd {
    /// 从 GitHub releases 下载预编译词典到 dict/<name>
    Download {
        /// 词典名（ipadic / ipadic-neologd / unidic / sudachidict / ko-dic / cc-cedict / jieba）
        name: String,
        /// 重新下载并替换已有词典
        #[arg(long)]
        force: bool,
        /// 词典版本（须与加载器 format_version 匹配）
        #[arg(long, default_value = "6.2.0")]
        version: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let home = home::program_dir(cli.home.as_deref())?;
    run(&cli, &home)
}

fn run(cli: &Cli, home: &std::path::Path) -> anyhow::Result<()> {
    match &cli.cmd {
        Cmd::Lib { cmd } => match cmd {
            LibCmd::Add { id, root, include_root, recursive, include, exclude } => {
                libs::add(home, id, root, *include_root, *recursive, include, exclude.as_deref())?;
            }
            LibCmd::List => {
                let libs = libs::list(home)?;
                if libs.is_empty() {
                    println!("无库（{} 为空）", libs::libs_dir(home).display());
                    return Ok(());
                }
                for cfg in &libs {
                    let count = {
                        let p = libs::lib_dir(home, &cfg.id).join("index.db");
                        match db::open(&p) {
                            Ok(conn) => conn
                                .query_row("SELECT count(*) FROM files", [], |r| {
                                    r.get::<_, i64>(0)
                                })
                                .unwrap_or(0),
                            Err(_) => 0,
                        }
                    };
                    println!("{:<16} {:<48} files: {count}", cfg.id, cfg.root.display());
                }
            }
            LibCmd::Remove { id, yes } => libs::remove(home, id, *yes)?,
        },
        Cmd::Reindex { lib, full } => {
            let cfg = libs::load(home, lib)?;
            let db_path = libs::lib_dir(home, lib).join("index.db");
            let (model_dir, dict_bin, user_dict) = home::resource_paths(home, &cfg);
            reindex::reindex(&cfg, &db_path, &model_dir, dict_bin.as_deref(), user_dict.as_deref(), *full)?;
        }
        Cmd::Search { lib, query, json, daemon } => {
            if *daemon {
                search_via_daemon(home, lib, query, *json)?;
            } else {
                let cfg = libs::load(home, lib)?;
                let db_path = libs::lib_dir(home, lib).join("index.db");
                let (model_dir, dict_bin, user_dict) = home::resource_paths(home, &cfg);
                search::search(&cfg, &db_path, &model_dir, dict_bin.as_deref(), user_dict.as_deref(), query, *json)?;
            }
        }
        Cmd::Model { cmd } => match cmd {
            ModelCmd::Pull { preset, repo, file, dir, extra } => {
                let spec = model::PullSpec {
                    preset: preset.clone(),
                    repo: repo.clone(),
                    file: file.clone(),
                    dir: dir.clone(),
                    extra: extra.clone(),
                };
                let target = model::pull(home, &spec)?;
                println!("完成: {}", target.display());
            }
        },
        Cmd::Dict { cmd } => match cmd {
            DictCmd::Download { name, force, version } => {
                let target = dict::download(home, name, version, *force)?;
                println!("完成: {}", target.display());
            }
        },
        #[cfg(windows)]
        Cmd::Ping => {
            let _ = libsearch::ipc::request(home, &libsearch::ipc::Request::Ping)?;
            println!("pong");
        }
        #[cfg(windows)]
        Cmd::Status => match libsearch::ipc::request(home, &libsearch::ipc::Request::Status)? {
            libsearch::ipc::Payload::Status(s) => println!("{}", serde_json::to_string_pretty(&s)?),
            other => anyhow::bail!("daemon 返回了非 Status 响应: {other:?}"),
        },
        #[cfg(windows)]
        Cmd::Reload => match libsearch::ipc::request(home, &libsearch::ipc::Request::Reload)? {
            libsearch::ipc::Payload::Reload { cleared } => {
                println!("已清除 {} 项缓存:", cleared.len());
                for c in &cleared {
                    println!("  {c}");
                }
            }
            other => anyhow::bail!("daemon 返回了非 Reload 响应: {other:?}"),
        },
    }
    Ok(())
}

/// 经 daemon 检索（Windows）
#[cfg(windows)]
fn search_via_daemon(home: &Path, lib: &str, query: &str, json: bool) -> anyhow::Result<()> {
    match libsearch::ipc::request(
        home,
        &libsearch::ipc::Request::Search {
            lib: lib.to_string(),
            query: query.to_string(),
            json: Some(json),
        },
    )? {
        libsearch::ipc::Payload::Search(out) => search::print_search_output(&out, json),
        other => anyhow::bail!("daemon 返回了非 Search 响应: {other:?}"),
    }
}