#!/usr/bin/env node
const { execFileSync } = require("child_process");
const path = require("path");

const bin = path.join(__dirname, "bin", process.platform === "win32" ? "mcp-token-gateway.exe" : "mcp-token-gateway");
try {
    execFileSync(bin, process.argv.slice(2), { stdio: "inherit" });
} catch (e) {
    process.exit(e.status || 1);
}