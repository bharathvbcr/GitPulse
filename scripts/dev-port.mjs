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

/**
 * Only Vite listeners at least this old are considered leftovers. A younger
 * listener is almost certainly another developer's just-started dev server,
 * and killing it would turn a concurrent start into a kill race.
 */
export const RECLAIM_GRACE_MS = 60_000;

export class DevPortError extends Error {
  /**
   * @param {number} port
   * @param {Array<{ pid: number, command?: string, cwd?: string }>} blockers
   * @param {{ message?: string, diagnostics?: string[] }} [options]
   */
  constructor(port, blockers = [], options = {}) {
    super(options.message ?? formatBlockers(port, blockers, options.diagnostics));
    this.name = "DevPortError";
    this.port = port;
    this.blockers = blockers;
    this.diagnostics = options.diagnostics ?? [];
  }
}

export function defaultRepoRoot() {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
}

/**
 * @param {NodeJS.ProcessEnv} [env]
 * @param {number} [fallback]
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
  // Deliberately broad on the "/vite/" branch: npm shim argv varies
  // (node_modules/.bin/vite, vite/bin/vite.js, pnpm/yarn wrappers), and a
  // missed real server just falls through to auto-port while a wrongly
  // killed process would be data loss. Reclaim additionally requires
  // repo-local cwd, non-self pid, and an age check before any signal.
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
 * Decide whether a listener may be reclaimed. Only repo-local Vite processes
 * qualify, and only when they are old enough to be leftovers (older than
 * `graceMs`, default {@link RECLAIM_GRACE_MS}) or the operator explicitly opted
 * in via `reclaimAll` (GITPULSE_RECLAIM=1). Processes with unknown start time
 * are never reclaimed implicitly.
 *
 * @param {{
 *   pid: number,
 *   command?: string,
 *   cwd?: string,
 *   repoRoot: string,
 *   selfPids: Set<number>,
 *   startedAgoMs?: number | null,
 *   graceMs?: number,
 *   reclaimAll?: boolean,
 * }} listener
 */
export function shouldReclaimListener(listener) {
  if (!isReclaimCandidate(listener)) return false;
  if (listener.reclaimAll) return true;
  if (listener.startedAgoMs == null) return false;
  return listener.startedAgoMs >= (listener.graceMs ?? RECLAIM_GRACE_MS);
}

/**
 * Shape check only: is this a repo-local Vite process that is not ourselves?
 * Age and opt-in are decided by {@link shouldReclaimListener}.
 *
 * @param {{
 *   pid: number,
 *   command?: string,
 *   cwd?: string,
 *   repoRoot: string,
 *   selfPids: Set<number>,
 *   [key: string]: unknown,
 * }} listener
 */
export function isReclaimCandidate(listener) {
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

/**
 * @param {string[]} args
 */
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
  /**
   * @param {NodeJS.Signals} signal
   * @returns {() => void}
   */
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
  child.on(
    "exit",
    /** @type {(code: number | null, signal: NodeJS.Signals | null) => void} */ (
      (code, signal) => {
        hooks.removeListener("SIGINT", onInt);
        hooks.removeListener("SIGTERM", onTerm);
        if (signal) hooks.exit(1);
        else hooks.exit(code ?? 1);
      }
    ),
  );
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
 * Probe whether `port` accepts a listener. Fail closed: only a successful
 * bind reports free. EADDRINUSE means taken; every other error (EACCES,
 * EADDRNOTAVAIL, ...) is also reported as unusable and its error code is
 * appended to `diagnostics` so callers can explain *why* a port was skipped.
 *
 * @param {number} port
 * @param {string} [host]
 * @param {string[]} [diagnostics]
 * @returns {Promise<boolean>}
 */
export function tryListen(port, host, diagnostics = []) {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.unref();
    /**
     * @param {Error} err
     */
    const failWith = (err) => {
      const code =
        /** @type {NodeJS.ErrnoException} */ (err).code ?? String(err);
      if (code !== "EADDRINUSE") {
        diagnostics.push(`${host ?? "*"}:${port} ${code}`);
      }
      resolve(false);
    };
    server.once("error", failWith);
    server.once("listening", () => {
      server.close(() => resolve(true));
    });
    try {
      if (host == null) server.listen(port);
      else server.listen(port, host);
    } catch (err) {
      // Node throws synchronously for invalid ports (ERR_SOCKET_BAD_PORT).
      failWith(/** @type {Error} */ (err));
    }
  });
}

/**
 * @param {number} port
 * @param {string[]} [diagnostics]
 */
export async function isPortFree(port, diagnostics = []) {
  /** @type {string[]} */
  const v4 = [];
  if (!(await tryListen(port, "127.0.0.1", v4))) {
    diagnostics.push(...v4);
    return false;
  }
  /** @type {string[]} */
  const v6 = [];
  if (!(await tryListen(port, "::1", v6))) {
    diagnostics.push(...v6);
    return false;
  }
  return true;
}

/**
 * @param {number} from
 * @param {number} to
 */
export async function findFreePort(from, to) {
  if (
    !Number.isInteger(from) ||
    !Number.isInteger(to) ||
    from <= 0 ||
    to > 65535 ||
    from > to
  ) {
    throw new Error(
      `invalid port range: from ${from} to ${to} (need 1-65535 with from <= to)`,
    );
  }
  /** @type {string[]} */
  const diagnostics = [];
  for (let port = from; port <= to; port += 1) {
    if (await isPortFree(port, diagnostics)) return port;
  }
  throw new DevPortError(from, [], {
    message: `Ports ${from}-${to} are all busy. Stop one of the processes holding them, or set GITPULSE_DEV_PORT.`,
    diagnostics,
  });
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
    processElapsedMs,
    selfPids: () => {
      const pids = new Set([process.pid]);
      if (process.ppid) pids.add(process.ppid);
      return pids;
    },
    kill: killPid,
    /** Identity-checked kill: refuses if the pid no longer runs a matching command. */
    killVerified:
      /**
       * @param {number} pid
       * @param {string | null} needle
       */
      (pid, needle) => killPid(pid, needle),
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
  const reclaimAll = parseReclaimEnv(env);

  const listeners = await describeListeners(target, io);
  const selfPids = io.selfPids();
  const reclaimable = [];
  for (const listener of listeners) {
    // Only pay for a start-time lookup when the process is otherwise a
    // reclaim candidate (repo-local Vite, not ourselves).
    const base = { ...listener, repoRoot, selfPids, reclaimAll };
    if (!isReclaimCandidate(base)) continue;
    const startedAgoMs = reclaimAll
      ? Number.POSITIVE_INFINITY
      : await io.processElapsedMs(listener.pid);
    if (shouldReclaimListener({ ...base, startedAgoMs })) {
      reclaimable.push(listener);
    }
  }

  if (reclaimable.length > 0) {
    for (const listener of reclaimable) {
      // Identity-checked: between the listing above and this kill, the pid
      // could have exited and been recycled by an unrelated process. The
      // needle is the command we vetted; a mismatch aborts the kill.
      const needle = (listener.command ?? "").trim().toLowerCase();
      await io.killVerified(listener.pid, needle || null);
    }
    if (await io.waitUntilFree(target)) {
      return {
        port: target,
        source: "cleaned",
        killed: reclaimable,
      };
    }
  }

  /** @type {string[]} */
  const probeDiagnostics = [];
  if (!(await io.isFree(target, probeDiagnostics))) {
    if (allowAutoport) {
      const port = await io.findFreePort(target + 1, target + range);
      return {
        port,
        source: "autoport",
        blockedBy: listeners,
      };
    }
    throw new DevPortError(target, listeners, {
      diagnostics: probeDiagnostics,
    });
  }

  return {
    port: target,
    source: preset != null ? "env" : "preferred",
  };
}

/**
 * @param {NodeJS.ProcessEnv} [env]
 */
export function parseReclaimEnv(env = process.env) {
  const raw = env.GITPULSE_RECLAIM;
  if (raw == null || raw === "") return false;
  const normalized = String(raw).toLowerCase();
  return normalized !== "0" && normalized !== "false" && normalized !== "off";
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

/**
 * @param {number} port
 * @param {Array<{ pid: number, command?: string, cwd?: string }>} blockers
 * @param {string[]} [diagnostics]
 */
function formatBlockers(port, blockers, diagnostics = []) {
  const lines = [`Port ${port} is already in use.`];
  for (const blocker of blockers) {
    const detail = blocker.command
      ? summarizeCommand(blocker.command)
      : blocker.cwd ?? "unknown process";
    lines.push(`  pid ${blocker.pid}: ${detail}`);
  }
  if (blockers.length === 0) {
    if (diagnostics.length > 0) {
      lines.push(`  (port probes failed: ${summarizeCommand(diagnostics.join("; "))})`);
    } else {
      lines.push("  (could not identify the process holding the port)");
    }
  }
  lines.push("Stop that process, or run `npm run tauri dev` to pick a free port.");
  return lines.join("\n");
}

/**
 * @param {string} command
 */
function summarizeCommand(command) {
  const trimmed = command.trim().replace(/\s+/g, " ");
  return trimmed.length > 120 ? `${trimmed.slice(0, 117)}...` : trimmed;
}

/**
 * @param {number} port
 * @returns {Promise<Array<{ pid: number }>>}
 */
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

/**
 * @param {number} pid
 * @returns {Promise<string>}
 */
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

/**
 * @param {number} pid
 * @returns {Promise<string | undefined>}
 */
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

/**
 * Milliseconds since the process started, or null when unknowable (Windows,
 * ps unavailable, process gone). Used to keep the reclaim grace window honest.
 *
 * @param {number} pid
 * @returns {Promise<number | null>}
 */
async function processElapsedMs(pid) {
  if (process.platform === "win32") return null;
  try {
    const { stdout } = await execFileAsync("ps", [
      "-o",
      "etime=",
      "-p",
      String(pid),
    ]);
    return parseEtimeToMs(stdout.trim());
  } catch {
    return null;
  }
}

/**
 * Parse `ps -o etime` output: `SS`, `MM:SS`, `HH:MM:SS`, or `DD-HH:MM:SS`.
 * Returns null for anything unparseable rather than guessing.
 *
 * @param {string} raw
 * @returns {number | null}
 */
export function parseEtimeToMs(raw) {
  const match = /^(?:(\d+)-)?(?:(\d+):)?(\d+):(\d+)$/.exec(raw.trim());
  if (!match) return null;
  const days = Number(match[1] ?? 0);
  const hours = Number(match[2] ?? 0);
  const minutes = Number(match[3]);
  const seconds = Number(match[4]);
  if (minutes > 59 || seconds > 59) return null;
  if (match[1] != null && hours > 23) return null;
  return (((days * 24 + hours) * 60 + minutes) * 60 + seconds) * 1000;
}

/**
 * Reads the current command line of `pid` (empty when gone or unreadable).
 *
 * @param {number} pid
 * @returns {Promise<string>}
 */
async function currentCommand(pid) {
  if (process.platform === "win32") return "";
  try {
    const { stdout } = await execFileAsync("ps", ["-p", String(pid), "-o", "command="]);
    return stdout.trim().toLowerCase();
  } catch {
    return "";
  }
}

/**
 * Kills `pid`, but only while it still runs the command it was vetted as.
 * `expectNeedle` closes most of the pid-reuse window between listing a stale
 * listener and delivering signals: a recycled pid running anything else is
 * left alone. Null/empty needle skips the check (used by non-reclaim paths).
 *
 * @param {number} pid
 * @param {string | null} [expectNeedle]
 * @returns {Promise<void>}
 * @internal exported for tests only
 */
export async function killPid(pid, expectNeedle = null) {
  if (!Number.isInteger(pid) || pid <= 1) return;
  if (pid === process.pid || pid === process.ppid) return;
  /** @returns {Promise<boolean>} false = do not signal this pid anymore */
  const stillUs = async () => {
    if (!expectNeedle) return true;
    const cmd = await currentCommand(pid);
    // Empty output means the process is already gone (fine) or ps could not
    // read it — either way we must not send it a signal.
    return cmd !== "" && cmd.includes(expectNeedle);
  };
  if (!(await stillUs())) return;
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
  if (!(await stillUs())) return;
  try {
    process.kill(pid, "SIGKILL");
  } catch (err) {
    if (/** @type {NodeJS.ErrnoException} */ (err).code !== "ESRCH") throw err;
  }
}

/**
 * @param {number} pid
 * @returns {boolean}
 */
function pidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/**
 * @param {number} ms
 * @returns {Promise<void>}
 */
function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
