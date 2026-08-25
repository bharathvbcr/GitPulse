import { execFile, spawn } from "node:child_process";
import { readFile, readlink } from "node:fs/promises";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

/** Preferred Vite / Tauri `devUrl` port. Keep in sync with `src-tauri/tauri.conf.json`. */
export const PREFERRED_DEV_PORT = 5173;
export const DEV_PORT_RANGE = 20;

export class DevPortError extends Error {
  /**
   * @param {number} port
   * @param {Array<{ pid: number, command?: string, cwd?: string }>} blockers
   */
  constructor(port, blockers = []) {
    super(formatBlockers(port, blockers));
    this.name = "DevPortError";
    this.port = port;
    this.blockers = blockers;
  }
}

export function defaultRepoRoot() {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
}

/**
 * @param {NodeJS.ProcessEnv} [env]
 * @param {number | null} [fallback]
 */
export function portFromEnv(env = process.env, fallback = PREFERRED_DEV_PORT) {
  const parsed = parseOptionalPort(env.GITPULSE_DEV_PORT);
  return parsed ?? fallback;
}

/**
 * @param {unknown} raw
 * @returns {number | null}
 */
export function parseOptionalPort(raw) {
  if (raw == null || raw === "") return null;
  const n = Number(raw);
  if (!Number.isInteger(n) || n <= 0 || n > 65535) {
    throw new Error(
      `GITPULSE_DEV_PORT must be an integer 1-65535, got ${JSON.stringify(raw)}`,
    );
  }
  return n;
}

export function isTauriHookEnv(env = process.env) {
  return (
    env.TAURI_ENV_PLATFORM != null ||
    env.TAURI_ENV_DEBUG != null ||
    env.TAURI_ENV_ARCH != null
  );
}

/**
 * @param {string} command
 */
export function isDevServerCommand(command) {
  if (!command) return false;
  const normalized = command.replace(/\\/g, "/").toLowerCase();
  return (
    /(?:^|[/\s"'=])vite(?:\.js)?(?:\s|$)/.test(normalized) ||
    normalized.includes("/vite/")
  );
}

/**
 * @param {string | undefined} cwd
 * @param {string} repoRoot
 */
export function isInsideRepo(cwd, repoRoot) {
  if (!cwd || !repoRoot) return false;
  const resolvedCwd = path.resolve(cwd);
  const resolvedRoot = path.resolve(repoRoot);
  const rel = path.relative(resolvedRoot, resolvedCwd);
  return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel));
}

/**
 * @param {{ pid: number, command?: string, cwd?: string, repoRoot: string, selfPids: Set<number> }} listener
 */
export function shouldReclaimListener(listener) {
  if (!Number.isInteger(listener.pid) || listener.pid <= 1) return false;
  if (listener.selfPids.has(listener.pid)) return false;
  if (!isDevServerCommand(listener.command ?? "")) return false;
  const command = listener.command ?? "";
  const repoRoot = path.resolve(listener.repoRoot);
  return (
    isInsideRepo(listener.cwd, repoRoot) ||
    command.replace(/\\/g, "/").includes(repoRoot.replace(/\\/g, "/"))
  );
}

/**
 * @param {string} stdout
 * @returns {number[]}
 */
export function parseLsofPids(stdout) {
  return [
    ...new Set(
      stdout
        .split(/\s+/)
        .map((token) => Number(token))
        .filter((pid) => Number.isInteger(pid) && pid > 1),
    ),
  ];
}

/**
 * @param {string} stdout
 * @param {number} port
 * @returns {number[]}
 */
export function parseNetstatPids(stdout, port) {
  const pids = new Set();
  const pattern = new RegExp(`[:.]${port}\\s+\\S+\\s+LISTENING\\s+(\\d+)`, "i");
  for (const line of stdout.split(/\r?\n/)) {
    const match = line.match(pattern);
    if (match) pids.add(Number(match[1]));
  }
  return [...pids].filter((pid) => Number.isInteger(pid) && pid > 1);
}

/**
 * @param {number} port
 */
export function tauriConfigForPort(port) {
  return { build: { devUrl: `http://localhost:${port}` } };
}

/**
 * @param {string[]} args
 * @param {number} port
 * @param {number} [preferred]
 */
export function withTauriDevUrl(args, port, preferred = PREFERRED_DEV_PORT) {
  if (port === preferred) return [...args];
  return [...args, "--config", JSON.stringify(tauriConfigForPort(port))];
}

export function isTauriDevArgs(args) {
  return args[0] === "dev" && !args.includes("--help") && !args.includes("-h");
}

/**
 * Forward Ctrl-C / SIGTERM to a spawned bin so Vite cannot outlive `npm run dev`
 * and hold 5173 for the next launch.
 *
 * @param {import("node:child_process").ChildProcess} child
 * @param {Pick<NodeJS.Process, "on" | "removeListener" | "exit">} [hooks]
 */
export function attachChildLifetime(child, hooks = process) {
  const shutdown = (signal) => () => {
    if (child.exitCode != null || child.signalCode) return;
    try {
      child.kill(signal);
    } catch {
      // already gone
    }
  };
  const onInt = shutdown("SIGINT");
  const onTerm = shutdown("SIGTERM");
  hooks.on("SIGINT", onInt);
  hooks.on("SIGTERM", onTerm);
  child.on("exit", (code, signal) => {
    hooks.removeListener("SIGINT", onInt);
    hooks.removeListener("SIGTERM", onTerm);
    if (signal) hooks.exit(1);
    else hooks.exit(code ?? 1);
  });
  child.on("error", (err) => {
    console.error(err);
    hooks.exit(1);
  });
}

/**
 * @param {string} repoRoot
 * @param {string} name
 * @param {string[]} args
 * @param {NodeJS.ProcessEnv} [env]
 */
export function spawnLocalBin(repoRoot, name, args, env = process.env) {
  const bin = path.join(
    repoRoot,
    "node_modules",
    ".bin",
    process.platform === "win32" ? `${name}.cmd` : name,
  );
  const child = spawn(bin, args, {
    cwd: repoRoot,
    env,
    stdio: "inherit",
    shell: process.platform === "win32",
  });
  attachChildLifetime(child);
  return child;
}

/**
 * @param {{ source: string, port: number, killed?: Array<{ pid: number }>, blockedBy?: Array<{ pid: number, command?: string }> }} result
 * @param {number} [preferred]
 */
export function formatResolveMessage(result, preferred = PREFERRED_DEV_PORT) {
  if (result.source === "cleaned") {
    const pids = (result.killed ?? []).map((k) => k.pid).join(", ");
    return `[gitpulse] Reclaimed port ${result.port} (stopped leftover Vite pid ${pids})`;
  }
  if (result.source === "autoport") {
    const blocker = (result.blockedBy ?? [])[0];
    const who = blocker
      ? `pid ${blocker.pid}${blocker.command ? ` (${summarizeCommand(blocker.command)})` : ""}`
      : "another process";
    return `[gitpulse] Port ${preferred} in use by ${who}; using ${result.port}`;
  }
  return null;
}

/**
 * @param {number} port
 * @param {string} [host]
 * @returns {Promise<boolean>}
 */
export function tryListen(port, host) {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.unref();
    server.once("error", (err) => {
      resolve(/** @type {NodeJS.ErrnoException} */ (err).code === "EADDRINUSE" ? false : true);
    });
    server.once("listening", () => {
      server.close(() => resolve(true));
    });
    if (host == null) server.listen(port);
    else server.listen(port, host);
  });
}

/**
 * @param {number} port
 */
export async function isPortFree(port) {
  if (!(await tryListen(port, "127.0.0.1"))) return false;
  if (!(await tryListen(port, "::1"))) return false;
  return true;
}

/**
 * @param {number} from
 * @param {number} to
 */
export async function findFreePort(from, to) {
  for (let port = from; port <= to; port += 1) {
    if (await isPortFree(port)) return port;
  }
  throw new DevPortError(from, []);
}

/**
 * @param {number} port
 * @param {number} [timeoutMs]
 */
export async function waitUntilFree(port, timeoutMs = 2000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    if (await isPortFree(port)) return true;
    await sleep(50);
  }
  return isPortFree(port);
}

export function createDefaultIo() {
  return {
    listListeners,
    processCommand,
    processCwd,
    selfPids: () => {
      const pids = new Set([process.pid]);
      if (process.ppid) pids.add(process.ppid);
      return pids;
    },
    kill: killPid,
    isFree: isPortFree,
    waitUntilFree,
    findFreePort,
  };
}

/**
 * @param {{
 *   preferred?: number,
 *   env?: NodeJS.ProcessEnv,
 *   repoRoot?: string,
 *   allowAutoport?: boolean,
 *   range?: number,
 *   io?: ReturnType<typeof createDefaultIo>,
 * }} [options]
 */
export async function resolveDevPort(options = {}) {
  const env = options.env ?? process.env;
  const preferred = options.preferred ?? PREFERRED_DEV_PORT;
  const preset = parseOptionalPort(env.GITPULSE_DEV_PORT);
  const target = preset ?? preferred;
  const repoRoot = options.repoRoot ?? defaultRepoRoot();
  const io = options.io ?? createDefaultIo();
  const range = options.range ?? DEV_PORT_RANGE;
  const allowAutoport =
    options.allowAutoport ?? (preset == null && !isTauriHookEnv(env));

  const listeners = await describeListeners(target, io);
  const selfPids = io.selfPids();
  const reclaimable = listeners.filter((listener) =>
    shouldReclaimListener({ ...listener, repoRoot, selfPids }),
  );

  if (reclaimable.length > 0) {
    for (const listener of reclaimable) {
      await io.kill(listener.pid);
    }
    if (await io.waitUntilFree(target)) {
      return {
        port: target,
        source: "cleaned",
        killed: reclaimable,
      };
    }
  }

  if (await io.isFree(target)) {
    return {
      port: target,
      source: preset != null ? "env" : "preferred",
    };
  }

  if (allowAutoport) {
    const port = await io.findFreePort(target + 1, target + range);
    return {
      port,
      source: "autoport",
      blockedBy: listeners,
    };
  }

  throw new DevPortError(target, listeners);
}

/**
 * @param {number} port
 * @param {ReturnType<typeof createDefaultIo>} io
 */
async function describeListeners(port, io) {
  const listed = await io.listListeners(port);
  return Promise.all(
    listed.map(async (listener) => ({
      pid: listener.pid,
      command: await io.processCommand(listener.pid),
      cwd: await io.processCwd(listener.pid),
    })),
  );
}

function formatBlockers(port, blockers) {
  const lines = [`Port ${port} is already in use.`];
  for (const blocker of blockers) {
    const detail = blocker.command
      ? summarizeCommand(blocker.command)
      : blocker.cwd ?? "unknown process";
    lines.push(`  pid ${blocker.pid}: ${detail}`);
  }
  if (blockers.length === 0) {
    lines.push("  (could not identify the process holding the port)");
  }
  lines.push("Stop that process, or run `npm run tauri dev` to pick a free port.");
  return lines.join("\n");
}

function summarizeCommand(command) {
  const trimmed = command.trim().replace(/\s+/g, " ");
  return trimmed.length > 120 ? `${trimmed.slice(0, 117)}...` : trimmed;
}

async function listListeners(port) {
  if (process.platform === "win32") {
    try {
      const { stdout } = await execFileAsync("netstat", ["-ano", "-p", "tcp"]);
      return parseNetstatPids(stdout, port).map((pid) => ({ pid }));
    } catch {
      return [];
    }
  }
  try {
    const { stdout } = await execFileAsync("lsof", [
      "-nP",
      `-iTCP:${port}`,
      "-sTCP:LISTEN",
      "-t",
    ]);
    return parseLsofPids(stdout).map((pid) => ({ pid }));
  } catch (err) {
    if (/** @type {NodeJS.ErrnoException} */ (err).code === "ENOENT") return [];
    if (/** @type {{ status?: number }} */ (err).status === 1) return [];
    return [];
  }
}

async function processCommand(pid) {
  if (process.platform === "linux") {
    try {
      const cmdline = await readFile(`/proc/${pid}/cmdline`);
      return cmdline.toString().replace(/\0/g, " ").trim();
    } catch {
      // fall through to ps
    }
  }
  try {
    const { stdout } = await execFileAsync("ps", ["-o", "command=", "-p", String(pid)]);
    return stdout.trim();
  } catch {
    return "";
  }
}

async function processCwd(pid) {
  if (process.platform === "linux") {
    try {
      return await readlink(`/proc/${pid}/cwd`);
    } catch {
      return undefined;
    }
  }
  if (process.platform === "win32") return undefined;
  try {
    const { stdout } = await execFileAsync("lsof", [
      "-a",
      "-p",
      String(pid),
      "-d",
      "cwd",
      "-Fn",
    ]);
    const line = stdout.split(/\r?\n/).find((entry) => entry.startsWith("n"));
    return line ? line.slice(1) : undefined;
  } catch {
    return undefined;
  }
}

async function killPid(pid) {
  if (!Number.isInteger(pid) || pid <= 1) return;
  if (pid === process.pid || pid === process.ppid) return;
  if (process.platform === "win32") {
    try {
      await execFileAsync("taskkill", ["/PID", String(pid), "/T"]);
    } catch {
      try {
        await execFileAsync("taskkill", ["/PID", String(pid), "/T", "/F"]);
      } catch {
        // already gone
      }
    }
    return;
  }
  try {
    process.kill(pid, "SIGTERM");
  } catch (err) {
    if (/** @type {NodeJS.ErrnoException} */ (err).code === "ESRCH") return;
    throw err;
  }
  const start = Date.now();
  while (Date.now() - start < 1000) {
    if (!pidAlive(pid)) return;
    await sleep(50);
  }
  try {
    process.kill(pid, "SIGKILL");
  } catch (err) {
    if (/** @type {NodeJS.ErrnoException} */ (err).code !== "ESRCH") throw err;
  }
}

function pidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
