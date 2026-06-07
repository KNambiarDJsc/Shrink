//! Stream/channel I/O primitives.
//!
//! The router speaks in plain Strings via mpsc channels; this module bridges
//! those channels to and from `tokio` byte streams using a `LinesCodec`. Two
//! tiny primitives are all the new architecture needs:
//!
//! - [`read_pump`] reads framed lines from a stream and invokes a handler for
//!   each one (the router's `on_client_frame` / `on_upstream_frame`).
//! - [`write_pump`] drains a channel of pre-serialized frames into a writer.

use anyhow::Context;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

/// Maximum size of a single JSON-RPC line. MCP tool schemas are exactly the
/// bloated payloads we exist to shrink, so the default `LinesCodec` limit is
/// far too small — bump it to 16 MiB.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Build a `LinesCodec` sized for large MCP frames.
pub fn codec() -> LinesCodec {
    LinesCodec::new_with_max_length(MAX_LINE_BYTES)
}

/// Read framed lines and dispatch each through `handler`. Returns on EOF.
pub async fn read_pump<R, F>(
    mut reader: FramedRead<R, LinesCodec>,
    mut handler: F,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(String),
{
    while let Some(frame) = reader.next().await {
        let line = frame.context("failed reading frame from stream")?;
        handler(line);
    }
    Ok(())
}

/// Drain `rx` into `writer`, one frame per recv. Returns when the channel
/// closes (all senders dropped).
pub async fn write_pump<W>(
    mut rx: mpsc::UnboundedReceiver<String>,
    mut writer: FramedWrite<W, LinesCodec>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(line) = rx.recv().await {
        writer
            .send(line)
            .await
            .context("failed writing frame to stream")?;
    }
    let _ = futures::SinkExt::<String>::close(&mut writer).await;
    Ok(())
}