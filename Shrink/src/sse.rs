//! SSE/HTTP transport — compiled only when `--features sse`.
//!
//! Endpoints:
//!   GET  /sse           first SSE event is `event: endpoint / data: /messages?sessionId=<id>`
//!   POST /messages      agent sends JSON-RPC here; routed via the session Router
//!   GET  /health        `{"status":"ok","uptime_seconds":N,...}`
//!   GET  /metrics       Prometheus text exposition

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
};
use dashmap::DashMap;
use futures::Stream;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tower_http::cors::{Any, CorsLayer};

use crate::config::ServerSpec;
use crate::metrics::Metrics;
use crate::router::{Router as McpRouter, ServerInfo};
use crate::session::{launch, SessionHandle};
use crate::Compression;

// ─── Application state ────────────────────────────────────────────────────────

pub struct SseApp {
    pub server_specs: Vec<ServerSpec>,
    pub server_infos: Vec<ServerInfo>,
    pub tier: Compression,
    pub metrics: Arc<Metrics>,
    sessions: DashMap<String, Arc<SseSession>>,
    session_counter: AtomicU64,
}

/// Per-connection data shared between the SSE stream and POST handler.
struct SseSession {
    /// The Router for this connection — POST /messages dispatches here.
    router: Arc<McpRouter>,
}

impl SseApp {
    pub fn new(
        server_specs: Vec<ServerSpec>,
        server_infos: Vec<ServerInfo>,
        tier: Compression,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        Arc::new(Self {
            server_specs,
            server_infos,
            tier,
            metrics,
            sessions: DashMap::new(),
            session_counter: AtomicU64::new(1),
        })
    }

    fn next_id(&self) -> String {
        format!("{:016x}", self.session_counter.fetch_add(1, Relaxed))
    }
}

// ─── Build the axum Router ────────────────────────────────────────────────────

pub fn build_axum_router(app: Arc<SseApp>) -> axum::Router {
    axum::Router::new()
        .route("/sse",      get(sse_handler))
        .route("/messages", post(message_handler))
        .route("/health",   get(health_handler))
        .route("/metrics",  get(metrics_handler))
        .with_state(app)
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn sse_handler(
    State(app): State<Arc<SseApp>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let session_id = app.next_id();

    // Channel: router writes responses here; the SSE stream drains it.
    let (to_client_tx, to_client_rx) = mpsc::unbounded_channel::<String>();

    // Launch upstream processes and wire the Router.
    let handle_result = launch(
        &app.server_specs,
        app.server_infos.clone(),
        app.tier,
        to_client_tx.clone(),
        app.metrics.clone(),
    );
    let handle: SessionHandle = match handle_result {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(%session_id, error=%e, "upstream launch failed");
            let _ = to_client_tx.send(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":e.to_string()}}).to_string(),
            );
            return Sse::new(
                UnboundedReceiverStream::new(to_client_rx).map(|l| Ok(Event::default().data(l)))
            );
        }
    };

    let router = handle.router.clone();

    // Cleanup oneshot: when GuardedStream drops (client disconnect), the Sender
    // drops, the Receiver resolves, and the cleanup task fires.
    let (drop_tx, drop_rx) = oneshot::channel::<()>();

    let sessions_c = Arc::new(app.sessions.clone()); // clone the ref, not entries
    let sid_c = session_id.clone();
    let metrics_c = app.metrics.clone();
    // Store directly from `app` -- simplify by referencing app in task.
    let app_c = app.clone();
    tokio::spawn(async move {
        let _ = drop_rx.await;
        app_c.sessions.remove(&sid_c);
        handle.shutdown().await;
        metrics_c.session_end();
        tracing::info!(session_id=%sid_c, "SSE session torn down");
    });
    drop(sessions_c); // not needed separately

    // Register session for POST routing.
    app.sessions.insert(
        session_id.clone(),
        Arc::new(SseSession { router }),
    );
    app.metrics.session_start();
    tracing::info!(%session_id, "SSE session established");

    // MCP SSE spec: first event must be type "endpoint" pointing to the POST URL.
    let endpoint_event = Event::default()
        .event("endpoint")
        .data(format!("/messages?sessionId={session_id}"));

    let json_events = UnboundedReceiverStream::new(to_client_rx)
        .map(|line| Ok::<_, Infallible>(Event::default().data(line)));

    let stream = GuardedStream::new(
        futures::stream::once(async move { Ok(endpoint_event) }).chain(json_events),
        drop_tx,
    );

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// POST /messages?sessionId=<id>
/// Body is a raw JSON-RPC frame; the router dispatches it to the right upstream.
async fn message_handler(
    State(app): State<Arc<SseApp>>,
    Query(params): Query<HashMap<String, String>>,
    body: String,
) -> impl IntoResponse {
    let Some(sid) = params.get("sessionId") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(session) = app.sessions.get(sid.as_str()).map(|e| e.clone()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Dispatch the client's JSON-RPC frame through the Router.
    session.router.on_client_frame(body);
    // Per MCP spec: response arrives via SSE, NOT in the HTTP reply body.
    StatusCode::ACCEPTED.into_response()
}

async fn health_handler(State(app): State<Arc<SseApp>>) -> impl IntoResponse {
    axum::Json(json!({
        "status": "ok",
        "uptime_seconds": app.metrics.uptime_secs(),
        "active_sessions": app.metrics.active_sessions.load(Relaxed),
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn metrics_handler(State(app): State<Arc<SseApp>>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        app.metrics.render(),
    )
}

// ─── GuardedStream ────────────────────────────────────────────────────────────

/// Wraps a Stream and holds a `oneshot::Sender`. When axum drops this stream
/// (client disconnect) the Sender is dropped, which resolves the Receiver in
/// the cleanup task, triggering graceful session teardown.
struct GuardedStream<S> {
    inner: S,
    _guard: oneshot::Sender<()>,
}

impl<S> GuardedStream<S> {
    fn new(inner: S, guard: oneshot::Sender<()>) -> Self {
        Self { inner, _guard: guard }
    }
}

impl<S: Stream + Unpin> Stream for GuardedStream<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}