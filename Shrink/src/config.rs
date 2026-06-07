//! TOML config file for multi-server mode.
//!
//! Example:
//!
//! ```toml
//! compression = "high"
//!
//! [[servers]]
//! name    = "github"
//! command = "npx"
//! args    = ["-y", "@modelcontextprotocol/server-github"]
//!
//! [[servers]]
//! name    = "jira"
//! command = "/usr/local/bin/jira-mcp"
//! args    = []
//! ```
//!
//! `name` becomes the tool-prefix (`github__create_issue`), so it must be a
//! valid identifier — see [`GatewayConfig::validate`].

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    /// `"none" | "safe" | "balanced" | "high"`. Parsed to `Compression` by main.
    pub compression: Option<String>,
    #[serde(default)]
    pub servers: Vec<ServerSpec>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerSpec {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl GatewayConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let cfg: GatewayConfig =
            toml::from_str(&text).with_context(|| format!("invalid TOML in {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Enforce invariants the router relies on: ≥1 server, unique identifier-shaped names.
    pub fn validate(&self) -> Result<()> {
        if self.servers.is_empty() {
            bail!("config must declare at least one [[servers]] entry");
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.servers {
            if s.name.is_empty() {
                bail!("server `name` must be non-empty");
            }
            // Tool names will be `<name>__<original>`. Keep `name` to an
            // identifier so the result is still a valid tool-name token for
            // hosts that validate them.
            if !s
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
                || s.name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
            {
                bail!(
                    "server name '{}' must match [A-Za-z_][A-Za-z0-9_]* — it becomes a tool-name prefix",
                    s.name
                );
            }
            if !seen.insert(s.name.clone()) {
                bail!("duplicate server name: {}", s.name);
            }
            if s.command.is_empty() {
                bail!("server '{}' has empty command", s.name);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_server() {
        let text = r#"
            compression = "high"
            [[servers]]
            name = "github"
            command = "npx"
            args = ["-y", "@modelcontextprotocol/server-github"]
            [[servers]]
            name = "jira"
            command = "/usr/bin/jira-mcp"
        "#;
        let cfg: GatewayConfig = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.compression.as_deref(), Some("high"));
        assert_eq!(cfg.servers.len(), 2);
        assert_eq!(cfg.servers[0].name, "github");
        assert_eq!(cfg.servers[1].args, Vec::<String>::new());
    }

    #[test]
    fn rejects_invalid_names() {
        let bad: GatewayConfig = toml::from_str(
            r#"
                [[servers]]
                name = "has-dash"
                command = "x"
            "#,
        )
        .unwrap();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_names() {
        let dup: GatewayConfig = toml::from_str(
            r#"
                [[servers]]
                name = "a"
                command = "x"
                [[servers]]
                name = "a"
                command = "y"
            "#,
        )
        .unwrap();
        assert!(dup.validate().is_err());
    }

    #[test]
    fn rejects_empty_server_list() {
        let cfg: GatewayConfig = toml::from_str("").unwrap();
        assert!(cfg.validate().is_err());
    }
}