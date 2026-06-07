//! Session management: spawn upstream processes, wire the Router, start pumps.
//!
//! Both *stdio mode* (one session per process) and *SSE mode* (one session
//! per connected client) use `launch()` to stand up a live session. The
//! caller supplies the `to_client_tx` channel so it can choose how to deliver
//! responses — write to stdout, stream via SSE, etc.

use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::config::ServerSpec;
use crate::metrics::Metrics;
use crate::proxy::{codec, read_pump, write_pump};
use crate::router::{Router, ServerInfo};
use crate::Compression;

/// Live handles returned by [`launch`].
///
/// Drop-and-`shutdown()` when the session ends; that aborts the tasks and
/// kills every upstream child process.
pub struct SessionHandle {
    pub router: Arc<Router>,
    upstream_reader_tasks: Vec<JoinHandle<anyhow::Result<()>>>,
    upstream_writer_tasks: Vec<JoinHandle<anyhow::Result<()>>>,
    children: Vec<Child>,
}

impl SessionHandle {
    /// Gracefully tear down the session: abort background tasks, kill children.
    pub async fn shutdown(mut self) {
        for t in &self.upstream_reader_tasks {
            t.abort();
        }
        for t in &self.upstream_writer_tasks {
            t.abort();
        }
        for child in &mut self.children {
            let _ = child.kill().await;
        }
    }
}

/// Spawn a single upstream MCP server child process with piped stdio.
///
/// The child's stderr is *inherited* so its own log lines flow straight to
/// the real terminal without going through the gateway.
pub fn spawn_child(spec: &ServerSpec) -> Result<Child> {
    Command::new(&spec.command)
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn upstream '{}' ({})", spec.name, spec.command))
}

/// Spawn every upstream, create the [`Router`], and wire all I/O pumps.
///
/// # Parameters
/// * `server_specs`   — one entry per upstream (command + args).
/// * `server_infos`   — matching routing metadata (name + prefix).
/// * `tier`           — schema compression level for this session.
/// * `to_client_tx`   — the caller's inbound channel for responses.
/// * `metrics`        — shared telemetry (may be `Arc<Metrics::new()>` for a
///   one-shot stdio session, or the global handle in SSE mode).
pub fn launch(
    server_specs: &[ServerSpec],
    server_infos: Vec<ServerInfo>,
    tier: Compression,
    to_client_tx: mpsc::UnboundedSender<String>,
    metrics: Arc<Metrics>,
) -> Result<SessionHandle> {
    let n = server_specs.len();

    // One unbounded channel *to* each upstream — the router writes here;
    // write_pump drains into the child's stdin.
    let mut to_upstream_tx = Vec::with_capacity(n);
    let mut to_upstream_rx = Vec::with_capacity(n);
    for _ in 0..n {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        to_upstream_tx.push(tx);
        to_upstream_rx.push(rx);
    }

    let router = Router::new(server_infos, tier, to_client_tx, to_upstream_tx, metrics);

    let mut children = Vec::with_capacity(n);
    let mut upstream_reader_tasks = Vec::with_capacity(n);
    let mut upstream_writer_tasks = Vec::with_capacity(n);

    for (idx, (spec, rx)) in server_specs
        .iter()
        .zip(to_upstream_rx.into_iter())
        .enumerate()
    {
        let mut child = spawn_child(spec)?;
        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to capture stdin for '{}'", spec.name))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to capture stdout for '{}'", spec.name))?;

        // Upstream stdout → router
        let r = router.clone();
        let reader = FramedRead::new(child_stdout, codec());
        upstream_reader_tasks.push(tokio::spawn(read_pump(reader, move |line| {
            r.on_upstream_frame(idx, line)
        })));

        // Channel → upstream stdin
        let writer = FramedWrite::new(child_stdin, codec());
        upstream_writer_tasks.push(tokio::spawn(write_pump(rx, writer)));

        children.push(child);
    }

    Ok(SessionHandle {
        router,
        upstream_reader_tasks,
        upstream_writer_tasks,
        children,
    })
}