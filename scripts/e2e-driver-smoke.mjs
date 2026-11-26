#!/usr/bin/env node
/**
 * Real-binary Tauri WebDriver (`tauri-driver`) E2E smoke + screenshot suite
 * for Linux (X11/Xvfb and headless Wayland/Weston) and Windows (WebView2).
 *
 * Drives the real compiled `gitpulse` binary through W3C WebDriver HTTP
 * against an isolated profile directory and verifies five real desktop
 * invariants:
 *   1. Cold start with isolated profile (real UI boot, zero uncaught crashes)
 *   2. Real Git repository + OS file watcher + live diff update
 *   3. Real terminal PTY (Linux) / ConPTY (Windows) command round-trip
 *   4. `tauri-plugin-single-instance` second-launch handoff
 *   5. Clean shutdown with zero orphaned app processes
 *
 * Captures verified PNG screenshots at each milestone (and on failure) plus a
 * structured `summary.json` into the artifacts directory.
 *
 * Exit codes: 0 all checks passed · 1 a smoke assertion failed · 2 prerequisites
 * missing or unsupported platform (fail closed; never silently passes).
 */

import { spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PNG_MAGIC = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

/**
 * @typedef {{
 *   binary: string,
 *   seedBin: string,
 *   artifactsDir: string,
 *   driver: string,
 *   nativeDriver: string | null,
 *   port: number | null,
 *   timeoutSeconds: number,
 *   keepTemp: boolean,
 *   help: boolean,
 * }} SmokeOptions
 */

/**
 * Resolves the default compiled `gitpulse` binary path, honoring `CARGO_TARGET_DIR`.
 * @param {NodeJS.Platform} [platform]
 * @param {Record<string, string | undefined>} [env]
 * @param {string} [repoRoot]
 */
export function defaultAppBinary(platform = process.platform, env = process.env, repoRoot = REPO_ROOT) {
  const exe = platform === "win32" ? "gitpulse.exe" : "gitpulse";
  const targetDir = env.CARGO_TARGET_DIR
    ? path.resolve(repoRoot, env.CARGO_TARGET_DIR)
    : path.join(repoRoot, "src-tauri", "target");
  return path.join(targetDir, "debug", exe);
}

/**
 * Resolves the default compiled `e2e_seed_trust` example helper path.
 * @param {NodeJS.Platform} [platform]
 * @param {Record<string, string | undefined>} [env]
 * @param {string} [repoRoot]
 */
export function defaultSeedBinary(platform = process.platform, env = process.env, repoRoot = REPO_ROOT) {
  const exe = platform === "win32" ? "e2e_seed_trust.exe" : "e2e_seed_trust";
  const targetDir = env.CARGO_TARGET_DIR
    ? path.resolve(repoRoot, env.CARGO_TARGET_DIR)
    : path.join(repoRoot, "src-tauri", "target");
  return path.join(targetDir, "debug", "examples", exe);
}

/**
 * The `tauri:options.webviewOptions` for a session: on Windows, the isolated
 * WebView2 profile folder, which tauri-driver forwards to Edge WebDriver as
 * `ms:edgeOptions.webviewOptions` (the documented way to choose it).
 *
 * @param {string} profileRoot
 * @param {NodeJS.Platform} [platform]
 * @returns {{ userDataFolder: string } | undefined}
 */
export function webviewOptionsFor(profileRoot, platform = process.platform) {
  return platform === "win32" ? { userDataFolder: path.join(profileRoot, "webview2") } : undefined;
}

/**
 * Builds an isolated process environment so the E2E run never reads or writes
 * the developer's or runner's real GitPulse config, data, cache, gitconfig, or
 * WebView2 user-data folder.
 *
 * @param {string} profileRoot
 * @param {Record<string, string | undefined>} baseEnv
 * @param {NodeJS.Platform} [platform]
 * @returns {Record<string, string | undefined>}
 */
export function buildIsolatedEnv(profileRoot, baseEnv, platform = process.platform) {
  const home = path.join(profileRoot, "home");
  const config = path.join(profileRoot, "config");
  const data = path.join(profileRoot, "data");
  const cache = path.join(profileRoot, "cache");
  const logs = path.join(profileRoot, "logs");
  const gitconfig = path.join(profileRoot, "gitconfig");
  /** @type {Record<string, string | undefined>} */
  const env = {
    ...baseEnv,
    HOME: home,
    GIT_CONFIG_GLOBAL: gitconfig,
    GIT_CONFIG_NOSYSTEM: "1",
    GITPULSE_LOG_DIR: logs,
  };
  if (platform === "win32") {
    env.USERPROFILE = home;
    // The app resolves its data directory through the known-folder API,
    // which derives AppData from the profile, not from these variables.
    // Point both at the folders that lookup would find under the isolated
    // USERPROFILE (runSmoke creates them), so either route lands on the same
    // existing directory: a missing one left tauri logging "failed to
    // resolve the app local data directory: unknown path".
    env.APPDATA = path.join(home, "AppData", "Roaming");
    env.LOCALAPPDATA = path.join(home, "AppData", "Local");
    // The WebView2 profile goes to Edge WebDriver as
    // webviewOptions.userDataFolder (webviewOptionsFor), never through
    // WEBVIEW2_USER_DATA_FOLDER: that variable overrides the folder the
    // driver chose for the app, and session creation then hung for 60 s.
    delete env.WEBVIEW2_USER_DATA_FOLDER;
  } else {
    env.XDG_CONFIG_HOME = config;
    env.XDG_DATA_HOME = data;
    env.XDG_CACHE_HOME = cache;
    env.WEBKIT_DISABLE_DMABUF_RENDERER ??= "1";
    env.LIBGL_ALWAYS_SOFTWARE ??= "1";
  }
  return env;
}

/**
 * Verifies a base64 WebDriver screenshot payload is a valid non-empty PNG.
 * @param {string} base64
 * @returns {Buffer}
 */
export function decodeAndVerifyPng(base64) {
  if (typeof base64 !== "string" || base64.length < 32) {
    throw new Error("WebDriver screenshot returned an empty or truncated payload");
  }
  const bytes = Buffer.from(base64, "base64");
  if (bytes.length < PNG_MAGIC.length || !bytes.subarray(0, PNG_MAGIC.length).equals(PNG_MAGIC)) {
    throw new Error(`WebDriver screenshot is not a valid PNG (${bytes.length} bytes)`);
  }
  return bytes;
}

/**
 * Classifies the active display backend for diagnostics and summary output.
 * @param {NodeJS.Platform} platform
 * @param {Record<string, string | undefined>} env
 */
export function detectDisplayBackend(platform, env) {
  if (platform === "win32") return "win32";
  if (platform === "darwin") return "cocoa";
  const gdk = (env.GDK_BACKEND ?? "").toLowerCase();
  if (gdk === "wayland" || (env.WAYLAND_DISPLAY && gdk !== "x11")) return "wayland";
  if (env.DISPLAY) return "x11";
  return "none";
}

/**
 * Computes the remaining time budget against an absolute deadline, capped at `capMs`.
 * Throws once the deadline has elapsed so callers never hang past `--timeout`.
 * @param {number} deadlineMs
 * @param {number} capMs
 * @param {number} [now]
 */
export function remainingBudgetMs(deadlineMs, capMs, now = Date.now()) {
  if (!Number.isFinite(deadlineMs)) return capMs;
  const remaining = deadlineMs - now;
  if (remaining <= 0) {
    throw new Error("E2E smoke overall timeout exceeded");
  }
  return Math.min(capMs, remaining);
}

/**
 * Minimal W3C WebDriver HTTP client over Node 22 `fetch`.
 */
export class WebDriverClient {
  /**
   * @param {string} baseUrl
   * @param {typeof fetch} [fetchImpl]
   * @param {number} [deadlineMs]
   */
  constructor(baseUrl, fetchImpl = fetch, deadlineMs = Infinity) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.fetchImpl = fetchImpl;
    this.deadlineMs = deadlineMs;
    /** @type {string | null} */
    this.sessionId = null;
  }

  /**
   * @param {string} method
   * @param {string} endpoint
   * @param {unknown} [body]
   * @param {number} [timeoutMs]
   * @param {boolean} [enforceOverallDeadline]
   * @returns {Promise<any>}
   */
  async request(method, endpoint, body, timeoutMs = 45_000, enforceOverallDeadline = true) {
    const effectiveTimeout = enforceOverallDeadline
      ? remainingBudgetMs(this.deadlineMs, timeoutMs)
      : timeoutMs;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), effectiveTimeout);
    try {
      const response = await this.fetchImpl(`${this.baseUrl}${endpoint}`, {
        method,
        headers: body === undefined ? undefined : { "Content-Type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: controller.signal,
      });
      const text = await response.text();
      let payload;
      try {
        payload = text ? JSON.parse(text) : {};
      } catch {
        throw new Error(`WebDriver ${method} ${endpoint} returned non-JSON HTTP ${response.status}: ${text.slice(0, 200)}`);
      }
      if (!response.ok) {
        const errName = payload?.value?.error ?? `HTTP ${response.status}`;
        const errMsg = payload?.value?.message ?? text.slice(0, 300);
        throw new Error(`WebDriver ${method} ${endpoint} failed (${errName}): ${errMsg}`);
      }
      return payload?.value;
    } finally {
      clearTimeout(timer);
    }
  }

  /** @param {string} application @param {string[]} [args] @param {{ userDataFolder: string } | undefined} [webviewOptions] */
  async createSession(application, args = [], webviewOptions = undefined) {
    const result = await this.request(
      "POST",
      "/session",
      {
        capabilities: {
          alwaysMatch: {
            "tauri:options": {
              application,
              args,
              ...(webviewOptions ? { webviewOptions } : {}),
            },
          },
        },
      },
      60_000,
    );
    const sessionId = result?.sessionId;
    if (typeof sessionId !== "string" || !sessionId) {
      throw new Error(`WebDriver /session returned no sessionId: ${JSON.stringify(result)}`);
    }
    this.sessionId = sessionId;
    await this.request("POST", `/session/${sessionId}/timeouts`, {
      script: 30_000,
      pageLoad: 30_000,
      implicit: 5_000,
    });
    return sessionId;
  }

  /** @param {string} script @param {unknown[]} [args] @param {number} [timeoutMs] */
  async executeScript(script, args = [], timeoutMs = 30_000) {
    if (!this.sessionId) throw new Error("No active WebDriver session");
    return this.request("POST", `/session/${this.sessionId}/execute/sync`, { script, args }, timeoutMs);
  }

  /** @param {string} script @param {unknown[]} [args] @param {number} [timeoutMs] */
  async executeAsyncScript(script, args = [], timeoutMs = 35_000) {
    if (!this.sessionId) throw new Error("No active WebDriver session");
    return this.request("POST", `/session/${this.sessionId}/execute/async`, { script, args }, timeoutMs);
  }

  /** @param {string} destPath */
  async captureScreenshot(destPath) {
    if (!this.sessionId) throw new Error("No active WebDriver session");
    const encoded = await this.request("GET", `/session/${this.sessionId}/screenshot`, undefined, 30_000);
    const png = decodeAndVerifyPng(String(encoded ?? ""));
    mkdirSync(path.dirname(destPath), { recursive: true });
    writeFileSync(destPath, png);
    return { path: destPath, bytes: png.length };
  }

  async deleteSession() {
    if (!this.sessionId) return;
    const id = this.sessionId;
    this.sessionId = null;
    await this.request("DELETE", `/session/${id}`, undefined, 15_000, false);
  }
}

/**
 * @param {string[]} argv
 * @param {NodeJS.Platform} [platform]
 * @param {Record<string, string | undefined>} [env]
 * @returns {SmokeOptions}
 */
export function parseArgs(argv, platform = process.platform, env = process.env) {
  /** @type {SmokeOptions} */
  const opts = {
    binary: env.GITPULSE_E2E_BINARY ? path.resolve(env.GITPULSE_E2E_BINARY) : defaultAppBinary(platform, env),
    seedBin: env.GITPULSE_E2E_SEED_BIN ? path.resolve(env.GITPULSE_E2E_SEED_BIN) : defaultSeedBinary(platform, env),
    artifactsDir: path.resolve(env.GITPULSE_E2E_ARTIFACTS_DIR || path.join(REPO_ROOT, ".gitpulse", "scratch", "e2e-artifacts")),
    driver: env.TAURI_DRIVER_BIN || "tauri-driver",
    nativeDriver: env.TAURI_NATIVE_DRIVER_BIN || null,
    port: null,
    timeoutSeconds: 180,
    keepTemp: false,
    help: wantsHelp(argv),
  };
  if (opts.help) return opts;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--keep-temp") {
      opts.keepTemp = true;
      continue;
    }
    const next = () => {
      const value = argv[++i];
      if (value === undefined || value.startsWith("--")) throw new Error(`${arg} requires a value`);
      return value;
    };
    if (arg === "--binary") opts.binary = path.resolve(next());
    else if (arg === "--seed-bin") opts.seedBin = path.resolve(next());
    else if (arg === "--artifacts") opts.artifactsDir = path.resolve(next());
    else if (arg === "--driver") opts.driver = next();
    else if (arg === "--native-driver") opts.nativeDriver = next();
    else if (arg === "--port") {
      const port = Number(next());
      if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("--port must be an integer 1..65535");
      opts.port = port;
    } else if (arg === "--timeout") {
      const timeout = Number(next());
      if (!Number.isFinite(timeout) || timeout < 30 || timeout > 900) throw new Error("--timeout must be between 30 and 900 seconds");
      opts.timeoutSeconds = timeout;
    } else {
      throw new Error(`Unknown option: ${arg}`);
    }
  }
  return opts;
}

export function usage() {
  return formatUsage({
    name: "e2e-driver-smoke",
    summary: "Run the real-binary tauri-driver E2E smoke and screenshot suite on Linux (X11/Wayland) and Windows.",
    flags: [
      { flag: "--binary <path>", description: "Path to the compiled gitpulse executable (default: debug target)" },
      { flag: "--seed-bin <path>", description: "Path to the compiled e2e_seed_trust example binary" },
      { flag: "--artifacts <dir>", description: "Directory for PNG screenshots and summary.json" },
      { flag: "--driver <path>", description: "Path to tauri-driver binary (default: tauri-driver on PATH)" },
      { flag: "--native-driver <path>", description: "Explicit WebKitWebDriver or msedgedriver path" },
      { flag: "--port <n>", description: "Loopback port for tauri-driver (default: ephemeral free port)" },
      { flag: "--timeout <sec>", description: "Overall smoke timeout in seconds, 30..900 (default: 180)" },
      { flag: "--keep-temp", description: "Preserve the isolated profile and fixture repos after exit" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits: "0 all E2E checks passed · 1 an E2E assertion failed · 2 missing prerequisite or unsupported OS",
  });
}

/** Allocates an ephemeral free loopback TCP port. */
async function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      if (!addr || typeof addr === "string") {
        server.close(() => reject(new Error("Could not allocate loopback port")));
        return;
      }
      const { port } = addr;
      server.close(() => resolve(port));
    });
  });
}

/** @param {string} command @param {string[]} args @param {import("node:child_process").SpawnSyncOptions} [options] */
function mustExec(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    timeout: 30_000,
    ...options,
  });
  if (result.error) throw new Error(`${command} failed to launch: ${result.error.message}`);
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} exited ${result.status}: ${String(result.stderr || result.stdout).trim()}`);
  }
  return String(result.stdout ?? "");
}

/** @param {string} program */
function commandAvailable(program) {
  if (path.isAbsolute(program) || program.includes("/") || program.includes("\\")) return existsSync(program);
  const probe = spawnSync(process.platform === "win32" ? "where" : "which", [program], { stdio: "ignore" });
  return probe.status === 0;
}

/**
 * Builds shell input line(s) that emit `${prefix}${suffix}` when executed,
 * without the typed command itself containing the concatenated marker (so local
 * PTY/ConPTY echo alone cannot satisfy the round-trip assertion).
 * @param {string} prefix
 * @param {string} suffix
 * @param {NodeJS.Platform} [platform]
 * @param {Record<string, string | undefined>} [env]
 * @returns {string[]}
 */
export function buildPtyProbeCommand(prefix, suffix, platform = process.platform, env = process.env) {
  if (platform === "win32") {
    const comspec = (env.COMSPEC || "powershell.exe").toLowerCase();
    if (comspec.includes("powershell") || comspec.includes("pwsh")) {
      return [`Write-Output ("${prefix}" + "${suffix}")\r`];
    }
    return [`set "_gp_m=${suffix}"\r`, `echo ${prefix}%_gp_m%\r`];
  }
  return [`printf '%s%s\\n' ${prefix} ${suffix}\n`];
}

/**
 * Creates a real temporary Git repository with one tracked commit.
 * @param {string} dir
 * @param {string} fileName
 * @param {string} content
 * @param {Record<string, string | undefined>} env
 */
function initFixtureRepo(dir, fileName, content, env) {
  mkdirSync(dir, { recursive: true });
  const gitArgs = ["-c", "user.name=GitPulse E2E", "-c", "user.email=e2e@gitpulse.local", "-c", "commit.gpgsign=false"];
  mustExec("git", [...gitArgs, "init", "-b", "main"], { cwd: dir, env });
  mustExec("git", ["config", "--local", "user.name", "GitPulse E2E"], { cwd: dir, env });
  mustExec("git", ["config", "--local", "user.email", "e2e@gitpulse.local"], { cwd: dir, env });
  mustExec("git", ["config", "--local", "commit.gpgsign", "false"], { cwd: dir, env });
  writeFileSync(path.join(dir, fileName), content);
  mustExec("git", [...gitArgs, "add", fileName], { cwd: dir, env });
  mustExec("git", [...gitArgs, "commit", "-m", `initial ${fileName}`], { cwd: dir, env });
}

/**
 * Terminates a spawned process and its process group/tree.
 * On Windows, always uses `taskkill /T /F` because `child.kill()` only kills
 * `tauri-driver.exe` and leaves `msedgedriver.exe` alive holding `workRoot`.
 * @param {import("node:child_process").ChildProcess} child
 * @param {"SIGTERM" | "SIGKILL"} signal
 */
function killProcessTree(child, signal) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  if (typeof child.pid === "number" && child.pid > 1) {
    if (process.platform === "win32") {
      try {
        const res = spawnSync("taskkill", ["/T", "/F", "/PID", String(child.pid)], { stdio: "ignore", timeout: 5_000 });
        if (res.status === 0) return;
      } catch { /* fall through */ }
    } else {
      try { process.kill(-child.pid, signal); return; } catch { /* fall through */ }
    }
  }
  try { child.kill(signal); } catch { /* already exited */ }
}

/**
 * @param {string} url
 * @param {number} deadlineMs
 * @param {import("node:child_process").ChildProcess} [driverProc]
 * @param {() => string} [getDriverOutput]
 */
async function waitForPortReady(url, deadlineMs, driverProc, getDriverOutput) {
  const start = Date.now();
  while (Date.now() - start < deadlineMs) {
    if (driverProc && (driverProc.exitCode !== null || driverProc.signalCode !== null)) {
      const detail = getDriverOutput?.().trim();
      throw new Error(`tauri-driver exited prematurely (${driverProc.exitCode ?? driverProc.signalCode})${detail ? `: ${detail}` : ""}`);
    }
    try {
      const response = await fetch(`${url}/status`, { signal: AbortSignal.timeout(1500) });
      if (response.status >= 200 && response.status < 500) return;
    } catch {
      // driver still starting
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`tauri-driver did not listen at ${url} within ${deadlineMs}ms`);
}

/** @param {number} ms */
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Lists live processes referencing `marker` across Unix (`ps`) and Windows
 * (`Win32_Process` via PowerShell).
 * @param {string} marker
 * @returns {string[]}
 */
function findOrphanProcesses(marker) {
  if (process.platform === "win32") {
    const escaped = marker.replace(/'/g, "''");
    const ps = `$m = '${escaped}'; Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($m) -and -not $_.CommandLine.Contains('e2e-driver-smoke') -and -not $_.CommandLine.Contains('Win32_Process') } | ForEach-Object { "$($_.ProcessId) $($_.Name) $($_.CommandLine)" }`;
    const res = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", ps], {
      encoding: "utf8",
      timeout: 10_000,
    });
    if (res.status !== 0) return [];
    return String(res.stdout ?? "")
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
  }
  const res = spawnSync("ps", ["-axo", "pid=,comm=,args="], { encoding: "utf8", timeout: 5_000 });
  if (res.status !== 0) return [];
  return String(res.stdout ?? "")
    .split("\n")
    .filter((line) => line.includes(marker) && !line.includes("e2e-driver-smoke"))
    .map((line) => line.trim());
}

/**
 * The Edge WebDriver, app and WebView2 browser processes on Windows, each as
 * "pid name parent command-line" with the command line cut at 700 characters,
 * at most 24 rows. A failed query is reported as such, not as no processes.
 * @returns {string[]}
 */
function windowsAttachProcesses() {
  // PowerShell 7 (pwsh, on every GitHub Windows image) exposes CommandLine on
  // Get-Process, which starts far faster than a CIM query: the CIM form ran
  // past its 15 s bound on a runner and showed nothing.
  const ps = "Get-Process -Name msedgedriver,gitpulse,msedgewebview2 -ErrorAction SilentlyContinue | Select-Object -First 24 | ForEach-Object { \"$($_.Id) $($_.ProcessName) parent=$($_.Parent.Id) $($_.CommandLine)\" }";
  const res = spawnSync("pwsh", ["-NoProfile", "-NonInteractive", "-Command", ps], {
    encoding: "utf8",
    timeout: 45_000,
  });
  if (res.error || res.status !== 0) {
    const why = res.error ? res.error.message : `exit ${res.status}: ${String(res.stderr ?? "").trim()}`;
    return [`(process query failed: ${why.slice(0, 300)})`];
  }
  return String(res.stdout ?? "")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => (line.length > 700 ? `${line.slice(0, 700)}…` : line));
}

/** @param {SmokeOptions} opts */
async function runSmoke(opts) {
  if (process.platform === "darwin") {
    console.error("FAIL: tauri-driver E2E smoke is supported on Linux and Windows only (macOS uses WKWebView via `bun run test:webkit:all`).");
    return 2;
  }
  const display = detectDisplayBackend(process.platform, process.env);
  if (process.platform === "linux" && display === "none") {
    console.error("FAIL: Linux E2E smoke requires an X11 (xvfb-run) or Wayland (weston --backend=headless-backend.so) display.");
    return 2;
  }
  if (!commandAvailable(opts.driver)) {
    console.error(`FAIL: tauri-driver not found (${opts.driver}). Install with: cargo install --locked tauri-driver`);
    return 2;
  }
  const defaultNative = process.platform === "win32" ? "msedgedriver" : "WebKitWebDriver";
  const nativeDriver = opts.nativeDriver ?? defaultNative;
  if (!commandAvailable(nativeDriver)) {
    const hint = process.platform === "win32"
      ? "Install msedgedriver matching the installed Edge/WebView2 version and add it to PATH (or pass --native-driver)."
      : "Install webkit2gtk-driver (`sudo apt-get install -y webkit2gtk-driver`) or pass --native-driver.";
    console.error(`FAIL: native WebDriver not found (${nativeDriver}). ${hint}`);
    return 2;
  }
  if (!existsSync(opts.binary)) {
    console.error(`FAIL: gitpulse binary not found at ${opts.binary}. Build it with: cargo build --manifest-path src-tauri/Cargo.toml --locked --bin gitpulse`);
    return 2;
  }
  if (!existsSync(opts.seedBin)) {
    console.error(`FAIL: e2e_seed_trust helper not found at ${opts.seedBin}. Build it with: cargo build --manifest-path src-tauri/Cargo.toml --locked --example e2e_seed_trust`);
    return 2;
  }

  mkdirSync(opts.artifactsDir, { recursive: true });
  rmSync(path.join(opts.artifactsDir, "failure.png"), { force: true });
  const workRoot = mkdtempSync(path.join(tmpdir(), "gitpulse-e2e-"));
  const profileRoot = path.join(workRoot, "profile");
  const repoAlpha = path.join(workRoot, "repo-alpha");
  const repoBeta = path.join(workRoot, "repo-beta");
  for (const sub of ["home", "config", "data", "cache", "logs", "webview2"]) {
    mkdirSync(path.join(profileRoot, sub), { recursive: true });
  }
  writeFileSync(path.join(profileRoot, "gitconfig"), "[init]\n\tdefaultBranch = main\n");
  const env = buildIsolatedEnv(profileRoot, process.env);
  for (const dir of [env.APPDATA, env.LOCALAPPDATA]) {
    if (process.platform === "win32" && dir) mkdirSync(dir, { recursive: true });
  }

  initFixtureRepo(repoAlpha, "hello.txt", "line 1\nline 2\n", env);
  initFixtureRepo(repoBeta, "beta.txt", "beta repo\n", env);
  mustExec(opts.seedBin, [repoAlpha, repoBeta], { env });

  const port = opts.port ?? (await freePort());
  const nativePort = await freePort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const driverArgs = ["--port", String(port), "--native-port", String(nativePort)];
  if (opts.nativeDriver) driverArgs.push("--native-driver", opts.nativeDriver);

  let driverStdout = "";
  let driverStderr = "";
  const driverProc = spawn(opts.driver, driverArgs, {
    env,
    stdio: ["ignore", "pipe", "pipe"],
    detached: process.platform !== "win32",
  });
  driverProc.stdout?.on("data", (chunk) => {
    driverStdout = (driverStdout + String(chunk)).slice(-4000);
  });
  driverProc.stderr?.on("data", (chunk) => {
    driverStderr = (driverStderr + String(chunk)).slice(-4000);
  });

  const overallDeadlineMs = Date.now() + opts.timeoutSeconds * 1000;
  const client = new WebDriverClient(baseUrl, fetch, overallDeadlineMs);
  /** @type {() => void} */
  const emergencyCleanup = () => {
    killProcessTree(driverProc, "SIGKILL");
    if (!opts.keepTemp) {
      try { rmSync(workRoot, { recursive: true, force: true, maxRetries: 3 }); } catch { /* best-effort */ }
    }
  };
  const onSignal = (/** @type {NodeJS.Signals} */ sig) => {
    emergencyCleanup();
    process.exit(sig === "SIGINT" ? 130 : 143);
  };
  process.once("SIGINT", onSignal);
  process.once("SIGTERM", onSignal);
  /** @type {{ platform: string, display: string, startedAt: string, scenarios: Array<{ name: string, ok: boolean, ms: number, detail?: string }>, screenshots: Array<{ name: string, path: string, bytes: number }> }} */
  const summary = {
    platform: process.platform,
    display,
    startedAt: new Date().toISOString(),
    scenarios: [],
    screenshots: [],
  };

  /** @param {string} name */
  const snap = async (name) => {
    const dest = path.join(opts.artifactsDir, `${name}.png`);
    const info = await client.captureScreenshot(dest);
    summary.screenshots.push({ name, path: dest, bytes: info.bytes });
    console.log(`  screenshot: ${name}.png (${info.bytes} bytes)`);
  };

  /** @param {string} name @param {() => Promise<string | void>} fn */
  const step = async (name, fn) => {
    const t0 = Date.now();
    try {
      const detail = await fn();
      summary.scenarios.push({ name, ok: true, ms: Date.now() - t0, ...(detail ? { detail } : {}) });
      console.log(`PASS [${name}] (${Date.now() - t0}ms)${detail ? ` — ${detail}` : ""}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      summary.scenarios.push({ name, ok: false, ms: Date.now() - t0, detail: message });
      console.error(`FAIL [${name}] (${Date.now() - t0}ms): ${message}`);
      try {
        await snap("failure");
      } catch {
        // best-effort failure screenshot
      }
      throw error;
    }
  };

  let exitCode = 0;
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let overallTimer;
  const overallTimeout = new Promise((_, reject) => {
    overallTimer = setTimeout(() => {
      reject(new Error(`E2E smoke exceeded overall --timeout of ${opts.timeoutSeconds}s`));
    }, opts.timeoutSeconds * 1000);
    overallTimer.unref?.();
  });
  void overallTimeout.catch(() => {});

  const runScenarios = async () => {
    await waitForPortReady(
      baseUrl,
      remainingBudgetMs(overallDeadlineMs, 20_000),
      driverProc,
      () => [driverStderr, driverStdout].filter(Boolean).join("\n"),
    );

    await step("cold-start", async () => {
      await client.createSession(opts.binary, [repoAlpha], webviewOptionsFor(profileRoot));
      const boot = await client.executeAsyncScript(`
        const done = arguments[arguments.length - 1];
        const deadline = Date.now() + 20000;
        window.__gpE2eErrors = [];
        window.addEventListener("error", (e) => window.__gpE2eErrors.push(String(e.message || e.error)));
        window.addEventListener("unhandledrejection", (e) => window.__gpE2eErrors.push(String(e.reason)));
        const poll = async () => {
          const root = document.getElementById("app");
          if (window.__TAURI_INTERNALS__ && root && root.innerHTML.trim().length > 0) {
            try {
              const platform = await window.__TAURI_INTERNALS__.invoke("cmd_host_platform");
              done({ ok: true, title: document.title, platform, errors: window.__gpE2eErrors });
              return;
            } catch (err) {
              done({ ok: false, error: String(err) });
              return;
            }
          }
          if (Date.now() > deadline) {
            done({ ok: false, error: "Timed out waiting for #app and __TAURI_INTERNALS__" });
            return;
          }
          setTimeout(poll, 150);
        };
        poll();
      `);
      if (!boot?.ok) throw new Error(boot?.error ?? "App failed cold-start probe");
      if (Array.isArray(boot.errors) && boot.errors.length > 0) {
        throw new Error(`Uncaught page errors at cold start: ${boot.errors.join("; ")}`);
      }
      await snap("01-cold-start");
      return `title=${JSON.stringify(boot.title)} platform=${JSON.stringify(boot.platform)}`;
    });

    await step("repo-watcher-diff", async () => {
      const nonce = `e2e-live-edit-${randomUUID().slice(0, 8)}`;
      const armed = await client.executeAsyncScript(
        `
        const repoPath = arguments[0];
        const done = arguments[arguments.length - 1];
        (async () => {
          try {
            window.__gpWatcherHits = [];
            const handler = window.__TAURI_INTERNALS__.transformCallback((event) => {
              window.__gpWatcherHits.push(event);
            });
            await window.__TAURI_INTERNALS__.invoke("plugin:event|listen", {
              event: "repo-changed",
              target: { kind: "Any" },
              handler,
            });
            const watchId = await window.__TAURI_INTERNALS__.invoke("cmd_watch_repo", {
              repoPath,
              scope: "whole",
            });
            done({ ok: true, watchId });
          } catch (err) {
            done({ ok: false, error: String(err) });
          }
        })();
        `,
        [repoAlpha],
      );
      if (!armed?.ok) throw new Error(armed?.error ?? "Failed to arm real repo watcher");

      // Mutate tracked file on disk while the real app watcher is live.
      writeFileSync(path.join(repoAlpha, "hello.txt"), `line 1\nline 2\n${nonce}\n`);

      const observed = await client.executeAsyncScript(
        `
        const repoPath = arguments[0];
        const nonce = arguments[1];
        const done = arguments[arguments.length - 1];
        const deadline = Date.now() + 15000;
        const poll = async () => {
          try {
            const hits = (window.__gpWatcherHits || []).length;
            if (hits > 0 || Date.now() > deadline - 2000) {
              const diff = await window.__TAURI_INTERNALS__.invoke("cmd_get_file_diff", {
                repoPath,
                filePath: "hello.txt",
                isStaged: false,
                ignoreWhitespace: false,
              });
              const text = String(diff?.text ?? "");
              if (hits > 0 && text.includes("+" + nonce)) {
                done({ ok: true, hits, diffLength: text.length });
                return;
              }
              if (Date.now() > deadline) {
                done({ ok: false, error: "Watcher/diff check timed out (hits=" + hits + ", diffHasNonce=" + text.includes("+" + nonce) + ")" });
                return;
              }
            }
          } catch (err) {
            done({ ok: false, error: String(err) });
            return;
          }
          setTimeout(poll, 200);
        };
        poll();
        `,
        [repoAlpha, nonce],
      );
      if (!observed?.ok) throw new Error(observed?.error ?? "Watcher/diff verification failed");
      await snap("02-repo-watcher-diff");
      return `watcherHits=${observed.hits} diffBytes=${observed.diffLength}`;
    });

    await step("terminal-pty-roundtrip", async () => {
      const prefix = "GITPULSE_E2E_PTY_";
      const suffix = randomUUID().slice(0, 8);
      const marker = `${prefix}${suffix}`;
      const probeLines = buildPtyProbeCommand(prefix, suffix, process.platform, env);
      const result = await client.executeAsyncScript(
        `
        const repoPath = arguments[0];
        const marker = arguments[1];
        const probeLines = arguments[2];
        const done = arguments[arguments.length - 1];
        (async () => {
          let sessionId = null;
          try {
            const decoder = new TextDecoder();
            let collected = "";
            const cbId = window.__TAURI_INTERNALS__.transformCallback((raw) => {
              const msg = raw && typeof raw === "object" && "message" in raw ? raw.message : raw;
              let text = "";
              if (msg instanceof ArrayBuffer) {
                text = decoder.decode(new Uint8Array(msg));
              } else if (ArrayBuffer.isView(msg)) {
                text = decoder.decode(new Uint8Array(msg.buffer, msg.byteOffset, msg.byteLength));
              } else if (Array.isArray(msg)) {
                text = decoder.decode(Uint8Array.from(msg));
              } else if (typeof msg === "string") {
                text = msg;
              } else if (msg && typeof msg === "object" && typeof msg.data_b64 === "string") {
                try { text = atob(msg.data_b64); } catch { text = msg.data_b64; }
              }
              if (text) collected += text;
              if (sessionId && text.length > 0) {
                window.__TAURI_INTERNALS__.invoke("cmd_terminal_ack", { sessionId, bytes: text.length }).catch(() => {});
              }
            });
            const spawned = await window.__TAURI_INTERNALS__.invoke("cmd_terminal_spawn", {
              repoPath,
              rows: 24,
              cols: 80,
              onOutput: "__CHANNEL__:" + cbId,
            });
            sessionId = spawned.id;
            // Wait briefly for initial shell prompt/banner (cold cmd.exe/PowerShell
            // on Windows can take >400ms before reading ConPTY input).
            const readyDeadline = Date.now() + 3000;
            while (collected.length === 0 && Date.now() < readyDeadline) {
              await new Promise((r) => setTimeout(r, 100));
            }
            for (const line of probeLines) {
              await window.__TAURI_INTERNALS__.invoke("cmd_terminal_write", {
                sessionId,
                data: line,
                binary: false,
              });
              if (probeLines.length > 1) {
                await new Promise((r) => setTimeout(r, 150));
              }
            }
            const deadline = Date.now() + 15000;
            while (!collected.includes(marker) && Date.now() < deadline) {
              await new Promise((r) => setTimeout(r, 150));
            }
            const matched = collected.includes(marker);
            await window.__TAURI_INTERNALS__.invoke("cmd_terminal_kill", { sessionId });
            if (!matched) {
              done({ ok: false, error: "PTY output did not contain marker " + marker + "; got " + collected.slice(-300) });
              return;
            }
            done({ ok: true, sessionId, bytes: collected.length });
          } catch (err) {
            if (sessionId) {
              try { await window.__TAURI_INTERNALS__.invoke("cmd_terminal_kill", { sessionId }); } catch {}
            }
            done({ ok: false, error: String(err) });
          }
        })();
        `,
        [repoAlpha, marker, probeLines],
      );
      if (!result?.ok) throw new Error(result?.error ?? "Terminal PTY round-trip failed");
      await snap("03-terminal-pty");
      return `sessionId=${result.sessionId} capturedBytes=${result.bytes}`;
    });

    await step("single-instance-handoff", async () => {
      const listenerArmed = await client.executeAsyncScript(`
        const done = arguments[arguments.length - 1];
        (async () => {
          try {
            window.__gpOpenEvents = [];
            const handler = window.__TAURI_INTERNALS__.transformCallback((event) => {
              window.__gpOpenEvents.push(event?.payload ?? event);
            });
            await window.__TAURI_INTERNALS__.invoke("plugin:event|listen", {
              event: "gitpulse-open-repo",
              target: { kind: "Any" },
              handler,
            });
            done({ ok: true });
          } catch (err) {
            done({ ok: false, error: String(err) });
          }
        })();
      `);
      if (!listenerArmed?.ok) throw new Error(listenerArmed?.error ?? "Failed to subscribe to gitpulse-open-repo");

      // Spawn a second copy of the real gitpulse binary targeting repoBeta under the exact same isolated profile.
      const second = spawnSync(opts.binary, [repoBeta], {
        env,
        encoding: "utf8",
        timeout: 15_000,
      });
      if (second.error) {
        throw new Error(`Second instance failed or hung (single-instance did not exit): ${second.error.message}`);
      }
      if (second.status !== 0) {
        throw new Error(`Second instance exited ${second.status}: ${String(second.stderr || second.stdout).trim().slice(0, 300)}`);
      }

      const handoff = await client.executeAsyncScript(
        `
        const expectedLeaf = arguments[0];
        const done = arguments[arguments.length - 1];
        const deadline = Date.now() + 10000;
        const describePayload = (p) => {
          if (typeof p === "string") return p;
          if (p && typeof p === "object") {
            if (typeof p.path === "string") return p.path;
            if (typeof p.repo_path === "string") return p.repo_path;
            return JSON.stringify(p);
          }
          return String(p);
        };
        const poll = async () => {
          const events = window.__gpOpenEvents || [];
          const hit = events.find((p) => describePayload(p).includes(expectedLeaf));
          if (hit) {
            done({ ok: true, received: describePayload(hit) });
            return;
          }
          try {
            const pending = await window.__TAURI_INTERNALS__.invoke("cmd_take_pending_open");
            if (pending && describePayload(pending).includes(expectedLeaf)) {
              done({ ok: true, received: describePayload(pending) });
              return;
            }
          } catch {}
          if (Date.now() > deadline) {
            done({ ok: false, error: "First instance did not observe handoff for " + expectedLeaf + "; events=" + JSON.stringify(events) });
            return;
          }
          setTimeout(poll, 150);
        };
        poll();
        `,
        ["repo-beta"],
      );
      if (!handoff?.ok) throw new Error(handoff?.error ?? "Single-instance handoff was not observed");
      await snap("04-single-instance-handoff");
      return `handoff=${handoff.received}`;
    });

    await step("clean-shutdown", async () => {
      await client.deleteSession();
      const isPtyHost = (/** @type {string} */ line) => /\bgitpulse-ptyd\b/.test(line);
      const exitDeadline = Date.now() + Math.min(5_000, Math.max(1_000, overallDeadlineMs - Date.now()));
      let remaining = findOrphanProcesses(workRoot);
      let leaked = remaining.filter((line) => !isPtyHost(line));
      while (leaked.length > 0 && Date.now() < exitDeadline) {
        await sleep(200);
        remaining = findOrphanProcesses(workRoot);
        leaked = remaining.filter((line) => !isPtyHost(line));
      }
      if (leaked.length > 0) {
        throw new Error(`Orphaned process(es) remained after shutdown: ${leaked.join(" | ")}`);
      }
      // `gitpulse-ptyd` is intentionally detached with a 30s idle-exit timer so
      // terminals survive GUI restarts. Verify it obeys SIGTERM cleanly before
      // the isolated profile directory is removed.
      const ptyHosts = remaining.filter(isPtyHost);
      for (const line of ptyHosts) {
        const pid = Number(line.trim().split(/\s+/)[0]);
        if (Number.isInteger(pid) && pid > 1) {
          try { process.kill(pid, "SIGTERM"); } catch { /* already exited */ }
        }
      }
      if (ptyHosts.length > 0) {
        const deadline = Date.now() + 5000;
        while (findOrphanProcesses(workRoot).length > 0 && Date.now() < deadline) {
          await sleep(150);
        }
        const stuck = findOrphanProcesses(workRoot);
        if (stuck.length > 0) {
          throw new Error(`gitpulse-ptyd did not exit cleanly on SIGTERM: ${stuck.join(" | ")}`);
        }
      }
      return ptyHosts.length > 0
        ? `session closed cleanly; ${ptyHosts.length} detached gitpulse-ptyd exited cleanly on SIGTERM`
        : "session closed cleanly, zero orphaned processes";
    });
  };

  try {
    await Promise.race([runScenarios(), overallTimeout]);
  } catch (error) {
    exitCode = 1;
    if (driverStdout.trim()) {
      console.error(`tauri-driver stdout:\n${driverStdout.trim()}`);
    }
    if (driverStderr.trim()) {
      console.error(`tauri-driver stderr:\n${driverStderr.trim()}`);
    }
    if (process.platform === "win32") {
      // Edge WebDriver logs nothing here, so show what it was attaching to:
      // whether a WebView2 browser process exists, and with which
      // --remote-debugging-port and --user-data-dir.
      const rows = windowsAttachProcesses();
      console.error(`WebView2 attach processes (${rows.length}):\n${rows.join("\n") || "(none)"}`);
    }
    if (error instanceof Error) console.error(`E2E smoke failed: ${error.message}`);
  } finally {
    clearTimeout(overallTimer);
    process.off("SIGINT", onSignal);
    process.off("SIGTERM", onSignal);
    try {
      await client.deleteSession();
    } catch {
      // session already closed
    }
    if (driverProc.exitCode === null && driverProc.signalCode === null) {
      killProcessTree(driverProc, "SIGTERM");
      await sleep(300);
      if (driverProc.exitCode === null && driverProc.signalCode === null) {
        killProcessTree(driverProc, "SIGKILL");
      }
    }
    driverProc.stdout?.destroy();
    driverProc.stderr?.destroy();
    writeFileSync(path.join(opts.artifactsDir, "summary.json"), JSON.stringify(summary, null, 2));
    if (!opts.keepTemp) {
      try {
        rmSync(workRoot, { recursive: true, force: true, maxRetries: 10, retryDelay: 150 });
      } catch (error) {
        const code = /** @type {NodeJS.ErrnoException} */ (error)?.code;
        if (process.platform === "win32" && (code === "EBUSY" || code === "EPERM" || code === "ENOTEMPTY")) {
          console.warn(`Warning: could not fully remove E2E work root ${workRoot} (${code}); continuing.`);
        } else {
          throw error;
        }
      }
    }
  }
  return exitCode;
}

/** @param {string[]} [argv] */
export async function main(argv = process.argv.slice(2)) {
  /** @type {SmokeOptions} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (error) {
    console.error(`e2e-driver-smoke: ${error instanceof Error ? error.message : String(error)}`);
    return 2;
  }
  if (opts.help) {
    console.log(usage());
    return 0;
  }
  return runSmoke(opts);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then(
    (code) => { process.exitCode = code; },
    (error) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; },
  );
}
