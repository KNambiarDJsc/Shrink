#!/usr/bin/env python3
"""Multi-server end-to-end: gateway fronts TWO upstreams via TOML config.

Verifies:
  - tools/list returns tools from BOTH upstreams, prefixed (alpha__*, beta__*).
  - tools/call routes by prefix to the right upstream — and the upstream
    receives the BARE tool name (the prefix is stripped on the way down).
  - Each upstream only sees calls intended for it.
  - Invalid args still rejected locally even after routing.
"""
import json, subprocess, sys, re

GATEWAY = "./target/release/mcp-token-gateway"
CONFIG  = "tests/two_servers.toml"

p = subprocess.Popen(
    [GATEWAY, "--config", CONFIG],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
)
def send(req): p.stdin.write((json.dumps(req)+"\n").encode()); p.stdin.flush()
def recv():    return json.loads(p.stdout.readline())

# 1) initialize — should fan out and return a single merged response.
send({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"test"}}})
r1 = recv()
info = r1["result"]["serverInfo"]
print(f"id=1 initialize → server='{info['name']}' aggregating={info['aggregating']}")

# 2) tools/list — should return BOTH tools, namespaced.
send({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
r2 = recv()
names = sorted(t["name"] for t in r2["result"]["tools"])
print(f"id=2 tools/list → namespaced names: {names}")

# 3) Call alpha's tool.
send({"jsonrpc":"2.0","id":3,"method":"tools/call",
      "params":{"name":"alpha__search_alpha","arguments":{"query":"galaxies"}}})
r3 = recv()
print(f"id=3 alpha__search_alpha → {r3.get('result') or r3.get('error')}")

# 4) Call beta's tool.
send({"jsonrpc":"2.0","id":4,"method":"tools/call",
      "params":{"name":"beta__lookup_beta","arguments":{"query":"omega"}}})
r4 = recv()
print(f"id=4 beta__lookup_beta   → {r4.get('result') or r4.get('error')}")

# 5) Malformed args — still rejected locally.
send({"jsonrpc":"2.0","id":5,"method":"tools/call",
      "params":{"name":"beta__lookup_beta","arguments":{"limit":1}}})  # missing required 'query'
r5 = recv()
print(f"id=5 beta__lookup_beta (missing required) → {'OK' if 'result' in r5 else 'REJECTED  '+json.dumps(r5['error'])}")

# 6) Unknown tool — rejected locally.
send({"jsonrpc":"2.0","id":6,"method":"tools/call",
      "params":{"name":"gamma__nonsense","arguments":{}}})
r6 = recv()
print(f"id=6 gamma__nonsense (unknown)  → {'OK' if 'result' in r6 else 'REJECTED  '+json.dumps(r6['error'])}")

p.stdin.close(); p.wait(timeout=2)
err = p.stderr.read().decode()

print("\n--- what each upstream actually received (per-mock stderr) ---")
for ln in err.splitlines():
    if "] received" in ln: print(" ", ln.strip())

print("\n--- gateway logs of interest ---")
for ln in err.splitlines():
    s = re.sub(r'\x1b\[[0-9;]*m', '', ln).strip()
    if any(k in s for k in ("merged + compacted", "rejecting tools/call", "starting gateway")):
        print(" ", s)