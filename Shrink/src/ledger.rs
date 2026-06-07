//! The bidirectional schema ledger.
//!
//! When the upstream server advertises its tools (`tools/list`), we stash each
//! tool's *original, verbose* `inputSchema` here, keyed by tool name. Later,
//! when the agent invokes a tool against the *compacted* signature, the
//! rehydrator (Phase 4) looks the original schema back up so it can rebuild the
//! exact JSON payload the upstream server expects.
//!
//! The map is `DashMap`-backed and cloneable: every clone shares the same
//! underlying storage via an `Arc`, so the two proxy pumps (which run as
//! separate tasks) can read/write the same ledger without locks.

use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;

/// The original tool definition exactly as the upstream server published it.
#[derive(Clone, Debug)]
pub struct VerboseToolDefinition {
    pub name: String,
    /// The untouched JSON Schema for the tool's arguments. This is the source
    /// of truth used to validate and rehydrate invocations.
    pub input_schema: Value,
}

/// A cheap-to-clone handle over a shared, concurrent tool registry.
#[derive(Clone, Default)]
pub struct SchemaLedger {
    inner: Arc<DashMap<String, VerboseToolDefinition>>,
}

impl SchemaLedger {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Record (or overwrite) the verbose definition for a tool.
    pub fn record(&self, name: impl Into<String>, input_schema: Value) {
        let name = name.into();
        self.inner.insert(
            name.clone(),
            VerboseToolDefinition { name, input_schema },
        );
    }

    /// Fetch a cloned copy of a stored definition (used by the rehydrator in Phase 4).
    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<VerboseToolDefinition> {
        self.inner.get(name).map(|e| e.value().clone())
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// (Used by validation reporting in later phases.)
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}