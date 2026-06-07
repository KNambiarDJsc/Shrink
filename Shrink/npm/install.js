#!/usr/bin/env node
/**
 * Downloads the right pre-compiled binary from GitHub Releases on `npm install`.
 * Run via the `postinstall` hook in package.json.
 */
const { execSync } = require("child_process");
const { createWriteStream, chmodSync, existsSync, mkdirSync } = require("fs");
const { get } = require("https");
const path = require("path");
const zlib = require("zlib");

const REPO = "YOUR_ORG/mcp-token-gateway";
const VERSION = require("./package.json").version;
const BIN_DIR = path.join(__dirname, "bin");
const BIN = path.join(BIN_DIR, process.platform === "win32" ? "mcp-token-gateway.exe" : "mcp-token-gateway");

if (existsSync(BIN)) process.exit(0);
if (!existsSync(BIN_DIR)) mkdirSync(BIN_DIR);

const PLATFORM_MAP = {
    "darwin-arm64": "aarch64-apple-darwin",
    "darwin-x64": "x86_64-apple-darwin",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "win32-x64": "x86_64-pc-windows-msvc",
};

const key = `${process.platform}-${process.arch}`;
const target = PLATFORM_MAP[key];
if (!target) { console.error(`mcp-token-gateway: unsupported platform ${key}`); process.exit(1); }

const ext = process.platform === "win32" ? ".zip" : ".tar.gz";
const asset = `mcp-token-gateway-v${VERSION}-${target}${ext}`;
const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${asset}`;

console.log(`mcp-token-gateway: downloading binary for ${target}...`);

function followRedirects(url, cb) {
    get(url, res => {
        if (res.statusCode === 301 || res.statusCode === 302) return followRedirects(res.headers.location, cb);
        cb(res);
    });
}

followRedirects(url, res => {
    if (res.statusCode !== 200) {
        console.error(`mcp-token-gateway: download failed (HTTP ${res.statusCode}). Try manual install from ${url}`);
        process.exit(1);
    }
    if (ext === ".tar.gz") {
        // Extract the single binary from the tarball.
        const tar = require("child_process").spawn("tar", ["-xz", "-C", BIN_DIR], { stdio: ["pipe", "inherit", "inherit"] });
        res.pipe(tar.stdin);
        tar.on("close", code => {
            if (code !== 0) process.exit(code);
            chmodSync(BIN, 0o755);
            console.log("mcp-token-gateway: binary installed");
        });
    } else {
        // Windows: write zip then extract with PowerShell.
        const tmp = path.join(BIN_DIR, asset);
        const ws = createWriteStream(tmp);
        res.pipe(ws);
        ws.on("finish", () => {
            execSync(`powershell -Command "Expand-Archive -Path '${tmp}' -DestinationPath '${BIN_DIR}' -Force"`);
            console.log("mcp-token-gateway: binary installed");
        });
    }
});