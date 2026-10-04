#!/usr/bin/env node
// l8bin-mcp — npm shim for the LiteBin MCP server.
//
// Runs `l8b mcp`, installing the platform binary on first use:
//   1. L8B_BIN env var, if set (explicit override)
//   2. an existing l8b on PATH (respects manual installs)
//   3. cached download in ~/.l8b/shim/, fetching the latest GitHub release
//
// All diagnostics go to stderr — stdout belongs to the MCP protocol.

import { spawn, spawnSync } from "node:child_process";
import { pipeline } from "node:stream/promises";
import { createWriteStream } from "node:fs";
import fs from "node:fs/promises";
import path from "node:path";
import { Readable } from "node:stream";
import os from "node:os";

const REPO = "mtsandeep/l8bin";
const SHIM_DIR = path.join(os.homedir(), ".l8b", "shim");

const say = (msg) => process.stderr.write(`l8bin-mcp: ${msg}\n`);

function platformAsset() {
  const arch = process.arch === "x64" ? "x86_64" : process.arch === "arm64" ? "aarch64" : null;
  if (!arch) throw new Error(`unsupported architecture: ${process.arch}`);
  const exeSuffix = process.platform === "win32" ? ".exe" : "";
  let osName;
  if (process.platform === "linux") osName = "linux";
  else if (process.platform === "darwin") osName = "macos";
  else if (process.platform === "win32") osName = "windows";
  else throw new Error(`unsupported platform: ${process.platform}`);
  return { asset: `l8b-${arch}-${osName}${exeSuffix}`, exe: `l8b${exeSuffix}` };
}

function onPath(exe) {
  const probe = process.platform === "win32" ? spawnSync("where", [exe]) : spawnSync("which", [exe]);
  if (probe.status !== 0 || !probe.stdout) return null;
  const found = probe.stdout.toString().trim().split(/\r?\n/)[0];
  return found || null;
}

async function latestTag() {
  const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
    headers: { "user-agent": "l8bin-mcp-shim" },
  });
  if (!res.ok) throw new Error(`could not resolve the latest LiteBin release (HTTP ${res.status})`);
  const tag = (await res.json()).tag_name;
  if (!tag) throw new Error("latest release carries no tag");
  return tag;
}

async function download(url, dest) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok || !res.body) throw new Error(`download failed (HTTP ${res.status}) for ${url}`);
  await fs.mkdir(path.dirname(dest), { recursive: true });
  const tmp = `${dest}.tmp`;
  await pipeline(Readable.fromWeb(res.body), createWriteStream(tmp));
  await fs.rename(tmp, dest);
  if (process.platform !== "win32") await fs.chmod(dest, 0o755);
}

async function resolveBinary() {
  if (process.env.L8B_BIN) {
    await fs.access(process.env.L8B_BIN);
    return process.env.L8B_BIN;
  }

  const { asset, exe } = platformAsset();

  const existing = onPath(exe);
  if (existing) return existing;

  let tag;
  try {
    tag = await latestTag();
  } catch (e) {
    // Offline with no cache is fatal; offline with a cache still works.
    try {
      const cached = path.join(SHIM_DIR, "current");
      await fs.access(cached);
      return path.join(cached, exe);
    } catch {
      throw e;
    }
  }

  const dest = path.join(SHIM_DIR, tag, exe);
  try {
    await fs.access(dest);
  } catch {
    say(`fetching l8b ${tag} for ${process.platform}/${process.arch}...`);
    await download(`https://github.com/${REPO}/releases/download/${tag}/${asset}`, dest);
    const current = path.join(SHIM_DIR, "current");
    await fs.rm(current, { recursive: true, force: true }).catch(() => {});
    await fs.symlink(path.join(SHIM_DIR, tag), current, process.platform === "win32" ? "junction" : "dir").catch(() => {});
  }
  return dest;
}

try {
  const bin = await resolveBinary();
  const child = spawn(bin, ["mcp", ...process.argv.slice(2)], { stdio: "inherit" });
  child.on("error", (e) => {
    say(`failed to start l8b: ${e.message}`);
    process.exit(1);
  });
  child.on("exit", (code, signal) => {
    if (signal) process.kill(process.pid, signal);
    else process.exit(code ?? 1);
  });
} catch (e) {
  say(e.message);
  say("install l8b manually with: curl -fsSL https://l8b.in | bash -s cli");
  process.exit(1);
}
