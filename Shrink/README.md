<p align="center">
<svg width="150" height="150" viewBox="0 0 200 200" xmlns="http://www.w3.org/2000/svg">
    <rect width="200" height="200" rx="35" fill="#0d1117" stroke="#30363d" stroke-width="4"/>
    <rect x="50" y="40" width="100" height="30" rx="5" fill="#8b949e" opacity="0.5"/>
    <line x1="60" y1="55" x2="140" y2="55" stroke="#c9d1d9" stroke-width="4" stroke-linecap="round" stroke-dasharray="8 8"/>
    <path d="M 30 100 L 70 100 L 55 85 M 70 100 L 55 115" stroke="#58a6ff" stroke-width="6" fill="none" stroke-linecap="round" stroke-linejoin="round"/>
    <path d="M 170 100 L 130 100 L 145 85 M 130 100 L 145 115" stroke="#58a6ff" stroke-width="6" fill="none" stroke-linecap="round" stroke-linejoin="round"/>
    <rect x="80" y="130" width="40" height="30" rx="5" fill="#2ea043"/>
    <line x1="90" y1="145" x2="110" y2="145" stroke="#ffffff" stroke-width="6" stroke-linecap="round"/>
  </svg>
</p> <h1 align="center">Shrink</h1> <p align="center">
  <strong>The Zero-Latency MCP Token Compactor & Multi-Server Gateway</strong>  

  <em>An invisible, low-overhead proxy that aggressively squashes verbose tool schemas, preserving context windows and dropping API costs by 44% to 87%.</em>
</p> <p align="center">
  <img src="https://img.shields.io/badge/Language-Rust-orange.svg" alt="Rust">
  <img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="License">
  <img src="https://img.shields.io/badge/Runtime-Tokio-blueviolet.svg" alt="Tokio">
</p>




💡 Why It Exists

When an AI agent attaches multiple Model Context Protocol (MCP ) servers, the initial tools/list negotiation payload scales linearly with complexity. A comprehensive set of upstream tools (e.g., GitHub, Jira, and databases combined) easily consumes 30,000 to 55,000 tokens before the agent even begins executing its first turn. This rapidly exhausts prompt context windows and heavily inflates inference overhead.

Shrink sits between your AI client agent and upstream servers as a drop-in replacement. It intercepts tool discovery payloads, maps complex JSON schemas to dense, atomic structural signatures, and passes a highly compacted registry to the model. Upon model execution, Shrink intercepts outbound tools/call payloads, performs signature validation locally, and rehydrates the original structures transparently before passing them to the destination servers.

Plain Text


                  ┌──────────────────────────────────────────────┐
                  │                 AI AGENT                     │
                  └──────────────────────┬───────────────────────┘
                                         │
                                         ▼ (Compacted Schemas / Low Token Cost)
                  ┌──────────────────────────────────────────────┐
                  │               SHRINK GATEWAY                 │
                  └──────────────┬───────┬───────┬───────────────┘
                                 │       │       │
            ┌────────────────────┘       │       └────────────────────┐
            ▼ (Raw JSON-RPC)             ▼ (Raw JSON-RPC)             ▼ (Raw JSON-RPC)
┌───────────────────────┐    ┌───────────────────────┐    ┌───────────────────────┐
│      GitHub MCP       │    │       Jira MCP        │    │      SQLite MCP       │
└───────────────────────┘    └───────────────────────┘    └───────────────────────┘






📊 Compression Performance

Measured benchmarks utilizing a standard two-server upstream layout containing roughly 45 complex schemas:

Compression Tier
Token Reduction
Schema Payload Delivered to Model
System Requirements
none
0%
Byte-exact, full verbose JSON definitions.
No translation layer.
balanced
44%
Stripped schema structures + hard truncated text descriptions (160 ch max).
Passthrough.
safe
80%
Complete, clean valid JSON Schema; all text descriptions omitted entirely.
Passthrough.
high
87%
Stripped JSON object structure containing a TypeScript-style signature string inside.
Requires local state rehydration.





[!NOTE]
Under the High compression tier, a local Phase 4 argument validator catches malformed tools/call parameters locally before network routing, returning an immediate JSON-RPC Invalid params flag (/jql: type mismatch) to trigger automatic agent self-correction safely.




🛠️ Installation

Homebrew (macOS / Linux)

Bash


brew tap KNambiarDJsc/shrink
brew install shrink



Zero-Install Script (Node / npx)

Bash


npx shrink --compression high -- npx -y @modelcontextprotocol/server-github



From Source (Cargo)

Bash


# Stdio-only compilation profile (Rust ≥ 1.75)
cargo install --git https://github.com/KNambiarDJsc/Shrink

# Compile with high-concurrency SSE/HTTP transport features
cargo install --git https://github.com/KNambiarDJsc/Shrink --features sse






🚀 Usage

1. Single Server Integration (Drop-in Mode )

Replace your agent's direct command execution with the shrink binary execution sequence.

Bash


shrink --compression high -- npx -y @modelcontextprotocol/server-github



2. Multi-Server Management (Routing Configuration)

Create a gateway.toml layout file to bundle, route, and isolate distinct server instances using dynamic prefixes:

Plain Text


# gateway.toml
compression = "high"

[[servers]]
name    = "github"                           # Automatically prefixes as: github__<tool_name>
command = "npx"
args    = ["-y", "@modelcontextprotocol/server-github"]

[[servers]]
name    = "jira"                             # Automatically prefixes as: jira__<tool_name>
command = "/usr/local/bin/jira-mcp"
args    = []



Fire up the multi-server orchestrator passing the config handle:

Bash


shrink --config gateway.toml



3. Server Sent Events (SSE) Transport Network Daemon

Bash


shrink --listen 0.0.0.0:3000 --config gateway.toml



The agent establishes an active channel targeting http://localhost:3000/sse. Requests arrive cleanly via POST /messages?sessionId=<id> and payloads stream back natively over SSE.




📐 Internal Architecture

Plain Text


 D:\Shrink\src\
 ├── main.rs          # CLI Parsing Engine, Mode Resolution (Stdio vs SSE )
 ├── session.rs       # Upstream Process Lifecycle & Router Binding
 ├── router.rs        # Concurrent Fan-out, Identity Translation, Prefix Routing
 ├── proxy.rs         # Async Tokio Read/Write Channel Primitives
 ├── compactor.rs     # AST Mutator: JSON Schema ➔ Dense TypeScript Signature
 ├── validator.rs     # Strict Structural JSON-RPC Schema Evaluation & Inbound Rehydration
 ├── ledger.rs        # Concurrent Shared State Engine (DashMap Global Registry)
 ├── config.rs        # Strict TOML Specification Parser & Validator
 ├── metrics.rs       # Atomic Operational Telemetry & Prometheus Formatting
 └── sse.rs           # Axum HTTP Core Routing Layer (Feature-Gated)



Data Lifecycle Matrix (Single Request Flow)

Plain Text


  Inbound Client Stream (stdin)
               │
               ▼
       [ proxy::read_pump ]
               │
               ▼
   [ router.on_client_frame() ]
               │
               ├───────────────────────────────┐
               │ If tools/list?                │ If tools/call?
               ▼                               ▼
     ┌───────────────────┐           ┌───────────────────┐
     │ Fan-out upstreams │           │ Isolate by Prefix │
     │  Merge Schemas    │           │ Validate Arguments│
     │ Mutate / Compact  │           │ Rehydrate Layout  │
     │ Populate Ledger   │           │ Route or Reject   │
     └───────────────────┘           └───────────────────┘
               │                               │
               └───────────────┬───────────────┘
                               │
                               ▼
                      [ proxy::write_pump ]
                               │
                               ▼
                 Outbound Client Stream (stdout)






📊 Telemetry & Observability

When running under high-concurrency network operations (--features sse), Shrink exports native monitoring variables ready for scraping.

Bash


# Query Prometheus telemetries
curl http://localhost:3000/metrics

# Base health checks
curl http://localhost:3000/health



Scrape Telemetry Structure

Plain Text


mcpgw_uptime_seconds 42
mcpgw_tools_list_total 1
mcpgw_tokens_before_total 529
mcpgw_tokens_after_total 67
mcpgw_tokens_saved_pct 87
mcpgw_tools_call_total 3
mcpgw_tools_call_forwarded_total 2
mcpgw_tools_call_rejected_total 1
mcpgw_active_sessions 1






🧪 Verification Matrix

Execute internal verification suites using the testing tools in your development environment:

Bash


# Execute unit testing infrastructure
cargo test

# Launch single-server simulation test
python3 tests/drive_agent.py

# Launch fully namespaced multi-server integration matrix
python3 tests/drive_multi.py



IDE Configuration Block (cursor.json / claude_desktop.json )

JSON


{
  "mcpServers": {
    "shrink-gateway": {
      "command": "shrink",
      "args": ["--compression", "high", "--config", "/path/to/gateway.toml"]
    }
  }
}






🗺️ Roadmap

•
Phase 6 (Completed): Multi-threaded SSE transport layer daemon, high-performance Prometheus integration telemetry, package management definitions (npm, Homebrew).

•
Phase 7 (In Development): High-efficiency persistent connection pool states (shared across isolated ephemeral SSE tasks), granular per-target configuration block variables (ENV parameter injection inside gateway.toml), streaming array evaluations for progressive parsing, and integrated secure TLS terminations.




📄 License

This infrastructure project is licensed under the terms of the MIT License.

