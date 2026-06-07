#!/usr/bin/env python3
"""Minimal mock MCP server (newline-delimited JSON-RPC over stdio).

Usage:  mock_upstream.py [TOOL_NAME]
The tool's argument shape is fixed (a `query: string` + optional `limit: int`)
so the mock works for both single- and multi-server tests.
"""
import sys, json

TOOL = sys.argv[1] if len(sys.argv) > 1 else "search_jira_issues"
FAT = "x" * 240  # stand-in for verbose, token-hungry descriptions

TOOLS = [{
    "name": TOOL,
    "description": FAT,
    "inputSchema": {
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": FAT},
            "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": FAT},
        },
        "required": ["query"],
        "additionalProperties": False,
    },
}]

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    mid = msg.get("id")
    method = msg.get("method")
    # Log to stderr so harnesses can verify which calls actually arrived.
    sys.stderr.write(f"[{TOOL}] received {method}\n"); sys.stderr.flush()
    if method == "initialize":
        resp = {"jsonrpc": "2.0", "id": mid,
                "result": {"protocolVersion": "2025-06-18",
                           "serverInfo": {"name": f"mock-{TOOL}", "version": "0.1"},
                           "capabilities": {"tools": {}}}}
    elif method == "tools/list":
        resp = {"jsonrpc": "2.0", "id": mid, "result": {"tools": TOOLS}}
    elif method == "tools/call":
        # Echo which tool was actually invoked.
        called = msg.get("params", {}).get("name", "?")
        resp = {"jsonrpc": "2.0", "id": mid, "result": {"called": called, "by": TOOL}}
    else:
        resp = {"jsonrpc": "2.0", "id": mid, "result": {"ok": True, "echo": method}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()