//! Observability: atomic counters exposed as a Prometheus scrape endpoint.
//!
//! All fields are `AtomicU64` — no allocations in the hot path, safe to share
//! across every task via `Arc<Metrics>`, and serialisable to the standard
//! Prometheus text format without any runtime dependency.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

#[derive(Debug)]
pub struct Metrics {
    /// When the gateway process started (wall-clock reference for uptime).
    start: Instant,

    // ── Discovery ──────────────────────────────────────────────────────────
    /// Number of tools/list responses that were merged and forwarded.
    pub tools_list_total: AtomicU64,
    /// Cumulative token estimate of schemas *before* compaction.
    pub tokens_before_total: AtomicU64,
    /// Cumulative token estimate of schemas *after* compaction.
    pub tokens_after_total: AtomicU64,

    // ── Invocation ─────────────────────────────────────────────────────────
    /// Total tools/call requests received.
    pub tools_call_total: AtomicU64,
    /// tools/call requests forwarded to upstream (valid).
    pub tools_call_forwarded: AtomicU64,
    /// tools/call requests rejected locally (invalid params).
    pub tools_call_rejected: AtomicU64,

    // ── Session (SSE mode) ─────────────────────────────────────────────────
    /// Active SSE client sessions right now (can go negative transiently; i64).
    pub active_sessions: AtomicI64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            tools_list_total:    AtomicU64::new(0),
            tokens_before_total: AtomicU64::new(0),
            tokens_after_total:  AtomicU64::new(0),
            tools_call_total:    AtomicU64::new(0),
            tools_call_forwarded: AtomicU64::new(0),
            tools_call_rejected:  AtomicU64::new(0),
            active_sessions:      AtomicI64::new(0),
        }
    }

    pub fn record_discovery(&self, before_tokens: u64, after_tokens: u64) {
        self.tools_list_total.fetch_add(1, Relaxed);
        self.tokens_before_total.fetch_add(before_tokens, Relaxed);
        self.tokens_after_total.fetch_add(after_tokens, Relaxed);
    }

    pub fn record_call_forwarded(&self) {
        self.tools_call_total.fetch_add(1, Relaxed);
        self.tools_call_forwarded.fetch_add(1, Relaxed);
    }

    pub fn record_call_rejected(&self) {
        self.tools_call_total.fetch_add(1, Relaxed);
        self.tools_call_rejected.fetch_add(1, Relaxed);
    }

    pub fn session_start(&self) {
        self.active_sessions.fetch_add(1, Relaxed);
    }

    pub fn session_end(&self) {
        self.active_sessions.fetch_sub(1, Relaxed);
    }

    /// Render the Prometheus text exposition format.
    ///
    /// This is the only format the `/metrics` endpoint needs; no runtime
    /// dependency required.
    pub fn uptime_secs(&self) -> u64 { self.start.elapsed().as_secs() }

    pub fn render(&self) -> String {
        let uptime = self.start.elapsed().as_secs();
        let before = self.tokens_before_total.load(Relaxed);
        let after  = self.tokens_after_total.load(Relaxed);
        let saved  = before.saturating_sub(after);
        let pct    = if before > 0 { saved * 100 / before } else { 0 };

        format!(
            "# HELP mcpgw_uptime_seconds Seconds since the gateway process started.\n\
             # TYPE mcpgw_uptime_seconds gauge\n\
             mcpgw_uptime_seconds {uptime}\n\
             \n\
             # HELP mcpgw_tools_list_total tools/list responses merged and forwarded.\n\
             # TYPE mcpgw_tools_list_total counter\n\
             mcpgw_tools_list_total {tlt}\n\
             \n\
             # HELP mcpgw_tokens_before_total Cumulative ~tokens in schemas before compaction.\n\
             # TYPE mcpgw_tokens_before_total counter\n\
             mcpgw_tokens_before_total {before}\n\
             \n\
             # HELP mcpgw_tokens_after_total Cumulative ~tokens in schemas after compaction.\n\
             # TYPE mcpgw_tokens_after_total counter\n\
             mcpgw_tokens_after_total {after}\n\
             \n\
             # HELP mcpgw_tokens_saved_pct Cumulative token savings as a percentage.\n\
             # TYPE mcpgw_tokens_saved_pct gauge\n\
             mcpgw_tokens_saved_pct {pct}\n\
             \n\
             # HELP mcpgw_tools_call_total Total tools/call requests received.\n\
             # TYPE mcpgw_tools_call_total counter\n\
             mcpgw_tools_call_total {tct}\n\
             \n\
             # HELP mcpgw_tools_call_forwarded_total tools/call requests forwarded to upstream.\n\
             # TYPE mcpgw_tools_call_forwarded_total counter\n\
             mcpgw_tools_call_forwarded_total {fwd}\n\
             \n\
             # HELP mcpgw_tools_call_rejected_total tools/call requests rejected locally.\n\
             # TYPE mcpgw_tools_call_rejected_total counter\n\
             mcpgw_tools_call_rejected_total {rej}\n\
             \n\
             # HELP mcpgw_active_sessions Active SSE client sessions (0 in stdio mode).\n\
             # TYPE mcpgw_active_sessions gauge\n\
             mcpgw_active_sessions {sess}\n",
            tlt  = self.tools_list_total.load(Relaxed),
            tct  = self.tools_call_total.load(Relaxed),
            fwd  = self.tools_call_forwarded.load(Relaxed),
            rej  = self.tools_call_rejected.load(Relaxed),
            sess = self.active_sessions.load(Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_all_keys() {
        let m = Metrics::new();
        m.record_discovery(100, 20);
        m.record_call_forwarded();
        m.record_call_rejected();
        let out = m.render();
        for key in &[
            "mcpgw_uptime_seconds",
            "mcpgw_tools_list_total",
            "mcpgw_tokens_before_total",
            "mcpgw_tokens_after_total",
            "mcpgw_tokens_saved_pct",
            "mcpgw_tools_call_total",
            "mcpgw_tools_call_forwarded_total",
            "mcpgw_tools_call_rejected_total",
            "mcpgw_active_sessions",
        ] {
            assert!(out.contains(key), "missing metric: {key}");
        }
        // 80% savings: (100-20)/100 = 80
        assert!(out.contains("mcpgw_tokens_saved_pct 80"));
    }
}