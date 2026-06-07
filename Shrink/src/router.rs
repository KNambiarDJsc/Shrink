//! Multi-server router.
//!
//! Replaces Phase 4's single-direction `Interceptor` with a hub that fans
//! `tools/list` and `initialize` out to N upstreams, merges the responses,
//! and routes `tools/call` to the right child by the tool name's prefix.
//!
//! Two pieces of state make this work concurrently:
//!
//! - **Per-upstream ID translation** (`upstream_pending`, `upstream_next_id`).
//!   Each upstream owns its own JSON-RPC id space; the router allocates a
//!   fresh internal id for every request it sends, records what to do with
//!   the reply, and translates back to the client's original id on the way
//!   out.
//!
//! - **Fan-out tracking** (`fanouts`). When the client asks for `tools/list`,
//!   the router issues N parallel requests, collects responses by upstream
//!   index, and (once all are in) merges them — namespacing each tool's
//!   `name` with its server's prefix and recording the mapping in `routing`
//!   so subsequent `tools/call`s can be routed back.
//!
//! Frame I/O happens via mpsc channels supplied at construction; the router
//! itself is transport-agnostic and synchronous — calls into it never await.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::compactor;
use crate::metrics::Metrics;
use crate::ledger::SchemaLedger;
use crate::validator::{self, ValidationError};
use crate::Compression;

const INVALID_PARAMS: i32 = -32602;
/// Separator between server prefix and the upstream's bare tool name.
pub const PREFIX_SEP: &str = "__";
const BALANCED_PARAM_DESC: usize = 80;
const BALANCED_TOOL_DESC: usize = 160;

/// Static description of one upstream as seen by the router.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub name: String,
    /// Tool-name prefix. `""` for single-server CLI mode; `<name>` in multi-
    /// server mode. Empty prefix means no namespacing at all.
    pub prefix: String,
}

#[derive(Clone, Debug)]
struct ToolRoute {
    server_idx: usize,
    /// The bare tool name as the upstream knows it (before prefixing).
    original_name: String,
}

#[derive(Debug)]
enum PendingResp {
    /// Translate the upstream's reply id back to `client_id` and forward.
    Forward { client_id: Value },
    /// Collect into this fan-out; finalize when all members arrive.
    FanOutMember { fanout_id: u64 },
}

#[derive(Debug)]
enum FanOutKind {
    ToolsList,
    Initialize,
}

#[derive(Debug)]
struct FanOut {
    client_id: Value,
    kind: FanOutKind,
    remaining: usize,
    collected: Vec<Option<Value>>,
}

pub struct Router {
    servers: Vec<ServerInfo>,
    tier: Compression,
    ledger: SchemaLedger,
    to_client: mpsc::UnboundedSender<String>,
    to_upstream: Vec<mpsc::UnboundedSender<String>>,
    upstream_pending: Vec<DashMap<i64, PendingResp>>,
    upstream_next_id: Vec<AtomicI64>,
    fanouts: DashMap<u64, Mutex<FanOut>>,
    fanout_next_id: AtomicU64,
    /// Namespaced tool-name → which upstream owns it (and its original name).
    routing: DashMap<String, ToolRoute>,
    /// Headline savings logged once.
    reported: AtomicBool,
    metrics: Arc<Metrics>,
}

impl Router {
    pub fn new(
        servers: Vec<ServerInfo>,
        tier: Compression,
        to_client: mpsc::UnboundedSender<String>,
        to_upstream: Vec<mpsc::UnboundedSender<String>>,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        assert_eq!(
            servers.len(),
            to_upstream.len(),
            "to_upstream channel count must equal server count"
        );
        let n = servers.len();
        Arc::new(Self {
            servers,
            tier,
            ledger: SchemaLedger::new(),
            to_client,
            to_upstream,
            upstream_pending: (0..n).map(|_| DashMap::new()).collect(),
            upstream_next_id: (0..n).map(|_| AtomicI64::new(1)).collect(),
            fanouts: DashMap::new(),
            fanout_next_id: AtomicU64::new(1),
            routing: DashMap::new(),
            reported: AtomicBool::new(false),
            metrics,
        })
    }

    // -----------------------------------------------------------------
    // Client-side ingress
    // -----------------------------------------------------------------

    /// Handle a frame just read from the client.
    pub fn on_client_frame(&self, line: String) {
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(bytes = line.len(), "non-JSON from client; broadcasting verbatim");
                self.broadcast(&line);
                return;
            }
        };
        let method = v.get("method").and_then(Value::as_str);
        let client_id = v.get("id").cloned();

        match (method, client_id) {
            (Some("tools/list"), Some(cid)) => self.fanout(cid, FanOutKind::ToolsList, json!({})),
            (Some("initialize"), Some(cid)) => self.fanout(
                cid,
                FanOutKind::Initialize,
                v.get("params").cloned().unwrap_or(json!({})),
            ),
            (Some("tools/call"), Some(cid)) => self.handle_tools_call(cid, &v),
            // Notifications (no id): broadcast to every server.
            (Some(_), None) => self.broadcast(&line),
            // Unrecognized request: route to server 0 by default. Real
            // multi-server installs typically only use the few well-known
            // methods plus tools/*; this keeps unknown methods working with
            // *some* upstream rather than failing hard.
            (Some(_), Some(cid)) => self.forward_to(0, cid, v),
            (None, _) => {
                tracing::warn!("client frame with no method; broadcasting verbatim");
                self.broadcast(&line);
            }
        }
    }

    fn broadcast(&self, line: &str) {
        for tx in &self.to_upstream {
            let _ = tx.send(line.to_string());
        }
    }

    /// Forward a generic request to one upstream with id-translation in place.
    fn forward_to(&self, server_idx: usize, client_id: Value, mut frame: Value) {
        let internal_id = self.upstream_next_id[server_idx].fetch_add(1, Ordering::Relaxed);
        self.upstream_pending[server_idx]
            .insert(internal_id, PendingResp::Forward { client_id });
        if let Some(obj) = frame.as_object_mut() {
            obj.insert("id".into(), json!(internal_id));
        }
        let _ = self.to_upstream[server_idx].send(frame.to_string());
    }

    /// Issue a request to every upstream, collecting responses for one merged reply.
    fn fanout(&self, client_id: Value, kind: FanOutKind, params: Value) {
        let n = self.servers.len();
        let fanout_id = self.fanout_next_id.fetch_add(1, Ordering::Relaxed);
        let method = match kind {
            FanOutKind::ToolsList => "tools/list",
            FanOutKind::Initialize => "initialize",
        };
        self.fanouts.insert(
            fanout_id,
            Mutex::new(FanOut {
                client_id,
                kind,
                remaining: n,
                collected: vec![None; n],
            }),
        );
        for idx in 0..n {
            let internal_id = self.upstream_next_id[idx].fetch_add(1, Ordering::Relaxed);
            self.upstream_pending[idx]
                .insert(internal_id, PendingResp::FanOutMember { fanout_id });
            let frame = json!({
                "jsonrpc":"2.0",
                "id": internal_id,
                "method": method,
                "params": params.clone(),
            });
            let _ = self.to_upstream[idx].send(frame.to_string());
        }
    }

    fn handle_tools_call(&self, client_id: Value, v: &Value) {
        let params = v.get("params").and_then(Value::as_object);
        let Some(name) = params.and_then(|p| p.get("name")).and_then(Value::as_str) else {
            self.reply_invalid_params(
                client_id,
                "?",
                &ValidationError {
                    path: "/params/name".into(),
                    reason: "tools/call requires a string 'name'".into(),
                },
            );
            return;
        };
        let args = params
            .and_then(|p| p.get("arguments"))
            .cloned()
            .unwrap_or_else(|| json!({}));

        // Resolve which upstream owns this tool.
        let route = match self.routing.get(name).map(|r| r.value().clone()) {
            Some(r) => r,
            None => {
                // No mapping: in single-server-no-prefix mode it's just the
                // bare tool name. Otherwise, the agent is calling something
                // we never advertised — reject locally.
                if self.servers.len() == 1 && self.servers[0].prefix.is_empty() {
                    ToolRoute {
                        server_idx: 0,
                        original_name: name.to_string(),
                    }
                } else {
                    self.reply_invalid_params(
                        client_id,
                        name,
                        &ValidationError {
                            path: "/params/name".into(),
                            reason: format!("unknown tool '{name}'"),
                        },
                    );
                    return;
                }
            }
        };

        // Validate & rehydrate against the original schema (when we have it).
        let final_args = if self.tier == Compression::None {
            args
        } else if let Some(schema) = self.ledger.get(name).map(|d| d.input_schema) {
            let aggressive = self.tier == Compression::High;
            match validator::rehydrate(&args, &schema, aggressive) {
                Ok(rehydrated) => rehydrated,
                Err(e) => {
                    tracing::warn!(tool=%name, path=%e.path, reason=%e.reason, "rejecting tools/call locally");
                    self.reply_invalid_params(client_id, name, &e);
                    return;
                }
            }
        } else {
            tracing::warn!(tool=%name, "no schema in ledger; forwarding without validation");
            args
        };

        // Forward with the original (un-prefixed) name and a translated id.
        let internal_id =
            self.upstream_next_id[route.server_idx].fetch_add(1, Ordering::Relaxed);
        self.upstream_pending[route.server_idx]
            .insert(internal_id, PendingResp::Forward { client_id });
        let frame = json!({
            "jsonrpc":"2.0",
            "id": internal_id,
            "method": "tools/call",
            "params": { "name": route.original_name, "arguments": final_args },
        });
        let _ = self.to_upstream[route.server_idx].send(frame.to_string());
        self.metrics.record_call_forwarded();
    }

    fn reply_invalid_params(&self, client_id: Value, tool: &str, e: &ValidationError) {
        self.metrics.record_call_rejected();
        let frame = json!({
            "jsonrpc":"2.0",
            "id": client_id,
            "error": {
                "code": INVALID_PARAMS,
                "message": "Invalid params",
                "data": { "tool": tool, "path": e.path, "reason": e.reason },
            }
        });
        let _ = self.to_client.send(frame.to_string());
    }

    // -----------------------------------------------------------------
    // Upstream-side ingress
    // -----------------------------------------------------------------

    pub fn on_upstream_frame(&self, server_idx: usize, line: String) {
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(server_idx, "non-JSON from upstream; forwarding to client");
                let _ = self.to_client.send(line);
                return;
            }
        };

        // Server-initiated requests/notifications: forward to client as-is.
        // (Bidirectional id translation for server-initiated requests is out
        // of scope for Phase 5; notifications are id-less so always safe.)
        if v.get("method").is_some() {
            let _ = self.to_client.send(line);
            return;
        }

        // Response: look up what we should do with it.
        let Some(internal_id) = v.get("id").and_then(Value::as_i64) else {
            tracing::warn!(server_idx, "response with no integer id; dropping");
            return;
        };
        let Some((_, pending)) = self.upstream_pending[server_idx].remove(&internal_id) else {
            tracing::warn!(server_idx, internal_id, "unrecognized response id; dropping");
            return;
        };

        match pending {
            PendingResp::Forward { client_id } => {
                let mut out = v;
                if let Some(obj) = out.as_object_mut() {
                    obj.insert("id".into(), client_id);
                }
                let _ = self.to_client.send(out.to_string());
            }
            PendingResp::FanOutMember { fanout_id } => {
                self.collect_fanout(server_idx, fanout_id, v);
            }
        }
    }

    /// Record one upstream's response into a fan-out; finalize on the last arrival.
    fn collect_fanout(&self, server_idx: usize, fanout_id: u64, response: Value) {
        // Scope the DashMap Ref + inner Mutex tightly so we drop them before
        // calling `remove` (which takes a write lock on the same shard).
        let (done, finalized) = {
            let entry = match self.fanouts.get(&fanout_id) {
                Some(e) => e,
                None => {
                    tracing::warn!(fanout_id, "fanout already finalized; dropping member");
                    return;
                }
            };
            let mut fo = entry.value().lock().expect("fanout mutex poisoned");
            if let Some(slot) = fo.collected.get_mut(server_idx) {
                *slot = Some(response);
            }
            fo.remaining = fo.remaining.saturating_sub(1);
            let done = fo.remaining == 0;
            let finalized = if done {
                let collected = std::mem::take(&mut fo.collected);
                let client_id = std::mem::replace(&mut fo.client_id, Value::Null);
                let kind = std::mem::replace(&mut fo.kind, FanOutKind::ToolsList);
                Some((collected, client_id, kind))
            } else {
                None
            };
            (done, finalized)
        }; // <- entry and fo dropped here

        if !done {
            return;
        }
        self.fanouts.remove(&fanout_id);
        let (collected, client_id, kind) = finalized.expect("checked done");
        let reply = match kind {
            FanOutKind::ToolsList => self.merge_tools_list(client_id, collected),
            FanOutKind::Initialize => self.merge_initialize(client_id, collected),
        };
        let _ = self.to_client.send(reply);
    }

    // -----------------------------------------------------------------
    // Merging
    // -----------------------------------------------------------------

    fn merge_tools_list(&self, client_id: Value, collected: Vec<Option<Value>>) -> String {
        let mut merged: Vec<Value> = Vec::new();
        for (idx, resp) in collected.into_iter().enumerate() {
            let Some(resp) = resp else { continue };
            let server = &self.servers[idx];
            let Some(tools) = resp
                .get("result")
                .and_then(|r| r.get("tools"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for tool in tools {
                let Some(bare_name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let namespaced = if server.prefix.is_empty() {
                    bare_name.to_string()
                } else {
                    format!("{}{}{}", server.prefix, PREFIX_SEP, bare_name)
                };
                let schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object"}));
                self.ledger.record(&namespaced, schema.clone());
                self.routing.insert(
                    namespaced.clone(),
                    ToolRoute {
                        server_idx: idx,
                        original_name: bare_name.to_string(),
                    },
                );
                let mut nt = tool.clone();
                if let Some(obj) = nt.as_object_mut() {
                    obj.insert("name".into(), Value::String(namespaced));
                }
                merged.push(nt);
            }
        }

        let before_bytes: usize = merged.iter().map(|t| t.to_string().len()).sum();
        let compacted = compact_tools_for_tier(&merged, self.tier);
        let frame = json!({
            "jsonrpc":"2.0",
            "id": client_id,
            "result": { "tools": compacted },
        });
        let line = frame.to_string();
        self.report(merged.len(), before_bytes, line.len());
        line
    }

    fn merge_initialize(&self, client_id: Value, collected: Vec<Option<Value>>) -> String {
        let mut protocol_version: Option<Value> = None;
        let mut capabilities = serde_json::Map::new();
        for resp in collected.into_iter().flatten() {
            let Some(result) = resp.get("result") else {
                continue;
            };
            if protocol_version.is_none() {
                if let Some(pv) = result.get("protocolVersion").cloned() {
                    protocol_version = Some(pv);
                }
            }
            if let Some(caps) = result.get("capabilities").and_then(Value::as_object) {
                for (k, v) in caps {
                    // First server wins per capability key.
                    capabilities.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        let frame = json!({
            "jsonrpc":"2.0",
            "id": client_id,
            "result": {
                "protocolVersion": protocol_version.unwrap_or(json!("2025-06-18")),
                "serverInfo": {
                    "name": "mcp-token-gateway",
                    "version": env!("CARGO_PKG_VERSION"),
                    "aggregating": self.servers.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                },
                "capabilities": Value::Object(capabilities),
            },
        });
        frame.to_string()
    }

    fn report(&self, tools: usize, before_bytes: usize, after_bytes: usize) {
        self.metrics.record_discovery((before_bytes / 4) as u64, (after_bytes / 4) as u64);
        if self.reported.swap(true, Ordering::Relaxed) {
            return;
        }
        let before_tokens = before_bytes / 4;
        let after_tokens = after_bytes / 4;
        let saved = before_bytes.saturating_sub(after_bytes);
        let pct = if before_bytes > 0 {
            saved * 100 / before_bytes
        } else {
            0
        };
        tracing::info!(
            servers = self.servers.len(),
            tools,
            tier = ?self.tier,
            before_tokens,
            after_tokens,
            saved_pct = pct,
            "merged + compacted tools/list across upstreams"
        );
    }
}

/// Apply the same per-tier rewrite as Phase 3, but to an already-merged tools list.
fn compact_tools_for_tier(tools: &[Value], tier: Compression) -> Vec<Value> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        let schema = tool.get("inputSchema").cloned().unwrap_or_else(|| json!({}));
        let mut nt = tool.clone();
        let Some(obj) = nt.as_object_mut() else {
            out.push(nt);
            continue;
        };
        match tier {
            Compression::None => {}
            Compression::Safe => {
                obj.insert(
                    "inputSchema".into(),
                    compactor::compact_jsonschema(&schema, false, 0),
                );
                obj.remove("description");
            }
            Compression::Balanced => {
                obj.insert(
                    "inputSchema".into(),
                    compactor::compact_jsonschema(&schema, true, BALANCED_PARAM_DESC),
                );
                if let Some(d) = obj.get("description").and_then(Value::as_str) {
                    let t = compactor::truncate(d, BALANCED_TOOL_DESC);
                    obj.insert("description".into(), Value::String(t));
                }
            }
            Compression::High => {
                let signature = compactor::compact_schema(&schema);
                obj.insert("inputSchema".into(), json!({"type":"object"}));
                obj.insert(
                    "description".into(),
                    Value::String(format!("(p:{signature})")),
                );
            }
        }
        out.push(nt);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_join() {
        assert_eq!(format!("github{}{}", PREFIX_SEP, "create_issue"), "github__create_issue");
    }

    #[test]
    fn compact_tools_for_tier_high_collapses_schema() {
        let tools = vec![json!({
            "name":"x",
            "description":"long",
            "inputSchema":{"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}
        })];
        let out = compact_tools_for_tier(&tools, Compression::High);
        assert_eq!(out[0]["inputSchema"], json!({"type":"object"}));
        assert_eq!(out[0]["description"], json!("(p:{a:string})"));
    }

    #[test]
    fn compact_tools_for_tier_safe_keeps_valid_schema() {
        let tools = vec![json!({
            "name":"x",
            "description":"d",
            "inputSchema":{
                "$schema":"http://json-schema.org/draft-07/schema#",
                "type":"object",
                "properties":{"a":{"type":"string","description":"d"}},
                "required":["a"]
            }
        })];
        let out = compact_tools_for_tier(&tools, Compression::Safe);
        // metadata stripped, structure kept
        assert!(out[0]["inputSchema"].get("$schema").is_none());
        assert_eq!(out[0]["inputSchema"]["type"], "object");
        assert_eq!(out[0]["inputSchema"]["required"], json!(["a"]));
        // tool-level description removed
        assert!(out[0].get("description").is_none());
    }
}