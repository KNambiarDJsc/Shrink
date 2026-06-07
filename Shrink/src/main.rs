mod compactor;
mod config;
mod ledger;
mod metrics;
mod protocol;
mod proxy;
mod router;
mod session;
mod validator;

#[cfg(feature = "sse")]
mod sse;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::Level;

use crate::config::{GatewayConfig, ServerSpec};
use crate::metrics::Metrics;
use crate::proxy::codec;
use crate::router::ServerInfo;
use crate::session::launch;

const HELP: &str = "mcp-token-gateway -- transparent MCP proxy

USAGE (stdio):
    mcp-token-gateway [--compression TIER] -- <CMD> [ARGS...]
    mcp-token-gateway --config <PATH>

USAGE (SSE, build with --features sse):
    mcp-token-gateway --listen 0.0.0.0:3000 --config <PATH>

OPTIONS:
    --compression none|safe|balanced|high  [default: none]
    --config <PATH>   TOML config (multi-server)
    --listen <ADDR>   bind address for SSE mode
    -v                verbose logging (-vv = trace)
    -h, --help
    -V, --version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None, Safe, Balanced, High,
}

impl Compression {
    fn parse(s: &str) -> anyhow::Result<Self> {
        Ok(match s {
            "none" => Compression::None, "safe" => Compression::Safe,
            "balanced" => Compression::Balanced, "high" => Compression::High,
            other => bail!("unknown compression tier '{other}'"),
        })
    }
}

struct Cli {
    compression: Option<Compression>,
    config:  Option<PathBuf>,
    listen:  Option<String>,
    verbose: u8,
    command: Vec<String>,
}

fn parse_args() -> anyhow::Result<Cli> {
    let mut compression = None; let mut config = None;
    let mut listen = None; let mut verbose = 0u8; let mut command = Vec::new();
    let mut after_dd = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        if after_dd { command.push(arg); continue; }
        match arg.as_str() {
            "--"             => after_dd = true,
            "-h"|"--help"    => { println!("{HELP}"); std::process::exit(0); }
            "-V"|"--version" => { println!("mcp-token-gateway {}", env!("CARGO_PKG_VERSION")); std::process::exit(0); }
            "-v"|"--verbose" => verbose = verbose.saturating_add(1),
            "-vv"            => verbose = verbose.saturating_add(2),
            "--compression"  => { let v = it.next().ok_or_else(|| anyhow!("--compression needs a value"))?; compression = Some(Compression::parse(&v)?); }
            s if s.starts_with("--compression=") => { compression = Some(Compression::parse(&s["--compression=".len()..])?); }
            "--config"       => { config = Some(PathBuf::from(it.next().ok_or_else(|| anyhow!("--config needs a path"))?)); }
            s if s.starts_with("--config=") => { config = Some(PathBuf::from(&s["--config=".len()..])); }
            "--listen"       => { listen = Some(it.next().ok_or_else(|| anyhow!("--listen needs an address"))?); }
            s if s.starts_with("--listen=") => { listen = Some(s["--listen=".len()..].to_string()); }
            other => bail!("unexpected argument '{other}'\n\n{HELP}"),
        }
    }
    if config.is_none() && command.is_empty() {
        bail!("either --config <PATH> or -- <CMD> is required\n\n{HELP}");
    }
    if config.is_some() && !command.is_empty() {
        bail!("--config and -- <CMD> are mutually exclusive");
    }
    Ok(Cli { compression, config, listen, verbose, command })
}

fn init_tracing(verbose: u8) {
    let level = match verbose { 0 => Level::INFO, 1 => Level::DEBUG, _ => Level::TRACE };
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_max_level(level).with_target(false).init();
}

fn resolve(cli: &Cli) -> anyhow::Result<(Vec<ServerSpec>, Vec<ServerInfo>, Compression)> {
    if let Some(path) = &cli.config {
        let cfg = GatewayConfig::load(path)?;
        let tier = match cli.compression {
            Some(c) => c,
            None => cfg.compression.as_deref().map(Compression::parse).transpose()?.unwrap_or(Compression::None),
        };
        let (specs, infos): (Vec<_>, Vec<_>) = cfg.servers.into_iter().map(|s| {
            let info = ServerInfo { name: s.name.clone(), prefix: s.name.clone() };
            (s, info)
        }).unzip();
        Ok((specs, infos, tier))
    } else {
        let tier = cli.compression.unwrap_or(Compression::None);
        let spec = ServerSpec { name: "upstream".into(), command: cli.command[0].clone(), args: cli.command[1..].to_vec() };
        let info = ServerInfo { name: "upstream".into(), prefix: String::new() };
        Ok((vec![spec], vec![info], tier))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = parse_args()?;
    init_tracing(cli.verbose);
    let (specs, infos, tier) = resolve(&cli)?;

    if let Some(addr) = &cli.listen {
        #[cfg(feature = "sse")]
        {
            let metrics = Arc::new(Metrics::new());
            let app = sse::SseApp::new(specs, infos, tier, metrics);
            let router = sse::build_axum_router(app);
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("failed to bind to {addr}"))?;
            tracing::info!(address=%addr, "SSE gateway listening");
            axum::serve(listener, router).await.context("axum server error")?;
            return Ok(());
        }
        #[cfg(not(feature = "sse"))]
        {
            let _ = addr;
            bail!("--listen requires `--features sse`: rebuild with cargo build --features sse");
        }
    }

    // ── Stdio mode ──────────────────────────────────────────────────────────
    let metrics = Arc::new(Metrics::new());
    tracing::info!(
        servers = specs.len(), tier = ?tier,
        names = ?infos.iter().map(|i| &i.name).collect::<Vec<_>>(),
        "starting gateway (stdio)"
    );

    let (to_client_tx, to_client_rx) = mpsc::unbounded_channel::<String>();
    let session = launch(&specs, infos, tier, to_client_tx, metrics)
        .context("failed to launch session")?;

    let client_reader = FramedRead::new(tokio::io::stdin(), codec());
    let r = session.router.clone();
    let reader_task = tokio::spawn(proxy::read_pump(client_reader, move |line| r.on_client_frame(line)));

    let client_writer = FramedWrite::new(tokio::io::stdout(), codec());
    let writer_task = tokio::spawn(proxy::write_pump(to_client_rx, client_writer));

    let _ = reader_task.await;
    tracing::info!("client closed; draining");
    tokio::time::sleep(Duration::from_millis(500)).await;
    writer_task.abort();
    session.shutdown().await;
    tracing::info!("gateway shut down cleanly");
    Ok(())
}