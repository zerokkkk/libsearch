//! libsearch daemon：长驻服务（Windows 命名管道 IPC）

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "libsearch-daemon",
    version,
    about = "libsearch 长驻 daemon：命名管道服务 ping/status/reload/search（进程内缓存模型与分词器）"
)]
struct Cli {
    /// 程序目录（默认当前可执行文件所在目录，可被环境变量 LIBSEARCH_HOME 覆盖）
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    /// 启动时预热指定库的模型与分词器
    #[arg(long)]
    warm: Option<String>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("daemon 失败: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let home = libsearch::home::program_dir(cli.home.as_deref())?;
    let mut daemon = libsearch::daemon::Daemon::new(home.clone());
    if let Some(lib) = &cli.warm {
        daemon.warm(lib)?;
        println!("已预热库 {lib}");
    }
    println!(
        "libsearch daemon 就绪（home={} pipe={}）",
        home.display(),
        libsearch::ipc::pipe_name(&home)?
    );
    libsearch::ipc::serve(&home, |req| daemon.handle(req))
}

#[cfg(not(windows))]
fn run() -> anyhow::Result<()> {
    anyhow::bail!("daemon 仅支持 Windows（named_pipe）")
}