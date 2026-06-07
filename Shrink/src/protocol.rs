//! MCP wire-protocol helpers.
//!
//! MCP speaks JSON-RPC 2.0 over a newline-delimited stdio transport: every
//! message is a single line of JSON terminated by `\n`, and messages MUST NOT
//! contain embedded newlines. We therefore frame the stream with a
//! `LinesCodec` and treat each line as one logical frame.
//!
//! IMPORTANT DESIGN RULE: the gateway must be able to forward *any* valid frame
//! untouched, including frames it doesn't understand. So we never deserialize
//! into a rigid struct and re-serialize for forwarding (that risks dropping or
//! reordering fields). Instead we parse into a `serde_json::Value` purely to
//! *inspect* / route, and forward the original line verbatim unless a later
//! phase explicitly chooses to rewrite it.

use serde_json::Value;

/// A coarse classification of a JSON-RPC frame, borrowed from a parsed `Value`.
///
/// JSON-RPC 2.0 distinguishes frames by which fields are present:
/// - request:      has `method` + `id`
/// - notification: has `method`, no `id`
/// - response:     has `id` + (`result` | `error`), no `method`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum FrameKind<'a> {
    Request { method: &'a str },
    Notification { method: &'a str },
    Response,
    /// Anything that doesn't fit the shapes above (still forwarded verbatim).
    Other,
}

/// Classify a parsed JSON-RPC frame without allocating.
#[allow(dead_code)]
pub fn classify(v: &Value) -> FrameKind<'_> {
    // An explicit JSON `null` id is treated as "no id" for routing purposes.
    let has_id = v.get("id").map(|x| !x.is_null()).unwrap_or(false);
    match (v.get("method").and_then(Value::as_str), has_id) {
        (Some(method), true) => FrameKind::Request { method },
        (Some(method), false) => FrameKind::Notification { method },
        (None, true) => FrameKind::Response,
        (None, false) => FrameKind::Other,
    }
}

/// Returns the `tools` array if this value is a `tools/list` *result*.
///
/// We detect by structure (`result.tools` is an array) rather than by matching
/// request ids, which keeps the proxy stateless and robust to out-of-order or
/// interleaved responses.
#[allow(dead_code)]
pub fn tools_list_result(v: &Value) -> Option<&Vec<Value>> {
    v.get("result")
        .and_then(|r| r.get("tools"))
        .and_then(Value::as_array)
}

/// Rough token estimate using the common ~4-characters-per-token heuristic.
///
/// This is intentionally cheap and approximate — it exists to *report* the
/// scale of schema bloat (and, from Phase 3, the savings), not for billing.
#[allow(dead_code)]
pub fn estimate_tokens(v: &Value) -> usize {
    // `to_string` is compact (no whitespace), matching how schemas travel on
    // the wire, so the estimate tracks real payload size closely.
    v.to_string().len() / 4
}