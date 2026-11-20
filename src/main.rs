//! uunet — 扫码登录 + 端口映射的终端组网工具(类 tailnet,基于 ratatui-kit)。
//!
//! 模块边界:`backend` 是唯一的后端接缝(现为 mock 实现),界面层不直接
//! 碰网络;后续接入真实 UU 账号 API 与隧道时只替换 backend 内部实现。

use anyhow::Result;
use clap::Parser;

mod app;
mod backend;
mod capability;
mod devices;
mod fec;
mod forward;
mod host;
mod kcp;
mod login;
mod pb;
mod peer;
mod portmap;
mod rtc;
mod signal;

#[derive(Parser)]
#[command(name = "uunet", version, about = "uunet — 扫码登录 + 端口映射的终端组网工具")]
struct Cli {
    /// 使用离线 mock 后端(不访问网络,二维码自动走完登录流程)
    #[arg(long)]
    mock: bool,
    /// 登录成功后启动被控服务(在线 + 仅端口转发)
    #[arg(long)]
    host: bool,
    /// 日志过滤器,例如 info、debug 或 uunet_host=trace
    #[arg(long, global = true)]
    log_level: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.log_level.as_deref());
    // reqwest 用 rustls-no-provider,必须先装好加密提供者(ring)。
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("安装 rustls ring 加密提供者失败");
    backend::init(cli.mock);
    tracing::info!("uunet 启动:mock={} host={}", cli.mock, cli.host);
    if cli.host {
        host::enable_at_startup();
    }
    app::run().await
}

/// 日志写 `~/.uunet/logs/uunet.log`(按天滚动),仿 vibegate 的
/// ~/.vibegate/logs 模式;RUST_LOG / --log-level 照常过滤。
fn init_tracing(filter: Option<&str>) {
    use tracing_subscriber::EnvFilter;
    let filter = filter
        .map(str::to_owned)
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| "info".into());
    let dir = std::env::var("HOME")
        .map(|home| std::path::PathBuf::from(home).join(".uunet/logs"))
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/uunet-logs"));
    let _ = std::fs::create_dir_all(&dir);
    let appender = tracing_appender::rolling::daily(dir, "uunet.log");
    // NonBlocking 的 worker guard 必须活到进程结束,否则日志丢失。
    let (writer, guard) = tracing_appender::non_blocking(appender);
    static GUARD: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
        std::sync::OnceLock::new();
    let _ = GUARD.set(guard);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(writer)
        .try_init();
}
