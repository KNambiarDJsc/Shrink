#!/usr/bin/env python3
"""Agent-style driver for single-server backward-compat: request, await response, repeat."""
import json, subprocess, sys

GATEWAY = sys.argv[1] if len(sys.argv) > 1 else "./target/release/mcp-token-gateway"
TIER    = sys.argv[2] if len(sys.argv) > 2 else "high"

p = subprocess.Popen(
    [GATEWAY, "--compression", TIER, "--", "python3", "tests/mock_upstream.py"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
)
def send(req): p.stdin.write((json.dumps(req)+"\n").encode()); p.stdin.flush()
def recv():    return json.loads(p.stdout.readline())

send({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}})
r1 = recv()
print(f"id=1 tools/list           → tools={[t['name'] for t in r1['result']['tools']]}")

send({"jsonrpc":"2.0","id":2,"method":"tools/call",
      "params":{"name":"search_jira_issues","arguments":{"query":"project=AI"}}})
r2 = recv()
print(f"id=2 tools/call (valid)   → {'OK' if 'result' in r2 else 'ERR '+json.dumps(r2['error'])}")

send({"jsonrpc":"2.0","id":3,"method":"tools/call",
      "params":{"name":"search_jira_issues","arguments":{"limit":5}}})
r3 = recv()
print(f"id=3 tools/call (missing required) → {'OK' if 'result' in r3 else 'ERR  '+json.dumps(r3['error'])}")

send({"jsonrpc":"2.0","id":4,"method":"tools/call",
      "params":{"name":"search_jira_issues","arguments":{"query":42}}})
r4 = recv()
print(f"id=4 tools/call (wrong type)       → {'OK' if 'result' in r4 else 'ERR  '+json.dumps(r4['error'])}")

send({"jsonrpc":"2.0","id":5,"method":"tools/call",
      "params":{"name":"search_jira_issues","arguments":{"query":"x","limit":None}}})
r5 = recv()
print(f"id=5 tools/call (optional=null)    → {'OK' if 'result' in r5 else 'ERR '+json.dumps(r5['error'])}")

p.stdin.close(); p.wait(timeout=2)
err = p.stderr.read().decode()
print("\n--- methods the mock actually received ---")
for ln in err.splitlines():
    if "] received" in ln: print(" ", ln.strip())
print("\n--- gateway rejection logs ---")
import re
for ln in err.splitlines():
    if "rejecting tools/call" in ln:
        print(" ", re.sub(r'\x1b\[[0-9;]*m', '', ln).strip())