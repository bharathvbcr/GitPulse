#!/usr/bin/env node
/**
 * MCP install doctor — is the `gitpulse-mcp` an agent would actually launch
 * the one this repo builds?
 *
 * `plugins/gitpulse/mcp.json` and `plugins/gitpulse/.mcp.json` both spawn the bare token
 * `gitpulse-mcp`, resolved off PATH. That indirection is correct for a
 * published plugin — the client should run the installed server, not a path
 * baked into a manifest — but it means the connected server is whatever
 * happens to be on PATH, which no build step owns. A binary copied there by
 * hand keeps answering handshakes long after the repo has moved on, and the
 * handshake it answers *looks* healthy: the staleness is only visible if you
 * compare the version it reports against the version this tree carries.
 *
 * So the comparison is the check. The four outcomes are kept distinct on
 * purpose — "no binary on PATH" and "binary matches" are the two that a
 * naive check would collapse into one silent pass:
 *
 *   ok           the server on PATH reports this repo's version
 *   stale        it answered, with a different version than this tree
 *   unresponsive it is on PATH but did not complete an MCP handshake
 *   absent       nothing named gitpulse-mcp is on PATH at all
 *
 * Not part of `ci:local`: CI has no reason to install the server, and a check
 * that cannot run there must not be made to look like one that passed.
 * Refresh with `npm run mcp:install`.
 *
 * Exit codes: 0 ok · 1 absent/stale/unresponsive · 2 internal error.
 *
 * Flags:
 *   --bin <path>     probe this executable instead of resolving PATH
 *   --expect <ver>   compare against this version instead of package.json
 *   --timeout <ms>   handshake budget (default 10000)
 *   --json           machine-readable result
 */
import { spawn } from "node:child_process";
import { accessSync, constants, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The executable name both MCP manifests spawn. */
export const SERVER_BIN = "gitpulse-mcp";

/** Handshake budget. An unresponsive server must fail, never hang a release. */
export const DEFAULT_TIMEOUT_MS = 10_000;

/**
 * Resolve `name` against PATH without shelling out to `which`, so the answer
 * does not depend on a shell being present or on its builtins.
 *
 * @param {string} name
 * @param {{ pathValue?: string, platform?: string, access?: (full: string, mode: number) => void }} [env]
 * @returns {string | null}
 */
export function resolveOnPath(name, env = {}) {
  const pathValue = env.pathValue ?? process.env.PATH ?? "";
  const platform = env.platform ?? process.platform;
  // Node's X_OK is existence on Windows (execute bits are not a filesystem
  // concept there). Tests that need Unix "not executable" inject `access`.
  const access = env.access ?? ((full, mode) => accessSync(full, mode));
  // Windows resolves a bare name through PATHEXT; the other suffixes are not
  // meaningful for a Rust binary, so .exe is the only one worth trying.
  const candidates = platform === "win32" ? [`${name}.exe`, name] : [name];
  for (const dir of pathValue.split(path.delimiter)) {
    if (!dir) continue;
    for (const candidate of candidates) {
      const full = path.join(dir, candidate);
      try {
        access(full, constants.X_OK);
        return full;
      } catch {
        // Not here, or not executable — keep looking rather than reporting the
        // first near-miss as the answer.
      }
    }
  }
  return null;
}

/** @returns {string} */
export function expectedVersion() {
  const pkg = JSON.parse(readFileSync(path.join(REPO_ROOT, "package.json"), "utf8"));
  if (typeof pkg.version !== "string" || !pkg.version) {
    throw new Error("package.json has no usable version");
  }
  return pkg.version;
}

/**
 * Pull `result.serverInfo.version` out of a stream of JSON-RPC lines.
 *
 * Only the response to our own id is accepted: a server is free to emit
 * notifications first, and matching on "the first line that has a version"
 * would read one of those.
 *
 * @param {string} stdout
 * @param {number} id
 * @returns {string | null}
 */
export function parseServerVersion(stdout, id) {
  for (const line of stdout.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("{")) continue;
    /** @type {any} */
    let message;
    try {
      message = JSON.parse(trimmed);
    } catch {
      continue;
    }
    if (message.id !== id) continue;
    const version = message?.result?.serverInfo?.version;
    return typeof version === "string" ? version : null;
  }
  return null;
}

/**
 * Complete a legacy `initialize` handshake and report the version claimed.
 *
 * The legacy era is used deliberately: it is the smaller of the two contracts
 * the server speaks, so this doctor keeps working if the modern `_meta` shape
 * gains required fields.
 *
 * @param {string} binPath
 * @param {number} timeoutMs
 * @returns {Promise<{ version: string | null, error: string | null }>}
 */
export function probeServer(binPath, timeoutMs = DEFAULT_TIMEOUT_MS) {
  return new Promise((resolve) => {
    /** @type {import("node:child_process").ChildProcessWithoutNullStreams} */
    let child;
    try {
      child = spawn(binPath, [], { stdio: ["pipe", "pipe", "pipe"] });
    } catch (err) {
      resolve({ version: null, error: /** @type {Error} */ (err).message });
      return;
    }
    let stdout = "";
    let settled = false;
    const finish = (/** @type {{ version: string | null, error: string | null }} */ outcome) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.kill("SIGKILL");
      resolve(outcome);
    };
    const timer = setTimeout(
      () => finish({ version: null, error: `no handshake response within ${timeoutMs}ms` }),
      timeoutMs,
    );

    child.on("error", (err) => finish({ version: null, error: err.message }));
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      const version = parseServerVersion(stdout, 1);
      if (version !== null) finish({ version, error: null });
    });
    child.on("close", () => {
      const version = parseServerVersion(stdout, 1);
      finish(
        version !== null
          ? { version, error: null }
          : { version: null, error: "server exited without a usable initialize response" },
      );
    });

    child.stdin.on("error", () => {
      // A server that closed stdin is reported by the close handler; writing
      // into the closed pipe must not take the process down with EPIPE.
    });
    child.stdin.write(
      `${JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: {
          protocolVersion: "2025-06-18",
          capabilities: {},
          clientInfo: { name: "gitpulse-mcp-doctor", version: "1" },
        },
      })}\n`,
    );
  });
}

/**
 * @param {{ binPath: string | null, version: string | null, error: string | null, expected: string }} observed
 * @returns {{ status: "ok" | "stale" | "unresponsive" | "absent", violations: string[] }}
 */
export function classify({ binPath, version, error, expected }) {
  if (binPath === null) {
    return {
      status: "absent",
      violations: [
        `no ${SERVER_BIN} on PATH — the MCP manifests spawn that bare name, so no client can start the server`,
        "install it with: npm run mcp:install",
      ],
    };
  }
  if (version === null) {
    return {
      status: "unresponsive",
      violations: [`${binPath} did not complete an MCP handshake${error ? ` (${error})` : ""}`],
    };
  }
  if (version !== expected) {
    return {
      status: "stale",
      violations: [
        `${binPath} reports version ${JSON.stringify(version)} but this tree is ${JSON.stringify(expected)}`,
        "refresh it with: npm run mcp:install",
      ],
    };
  }
  return { status: "ok", violations: [] };
}

/**
 * @param {{ binPath: string | null, version: string | null, expected: string, status: string, violations: string[] }} result
 */
export function formatReport(result) {
  const lines = ["MCP install doctor", ""];
  lines.push(`  ${"executable on PATH".padEnd(26)} : ${result.binPath ?? "<not found>"}`);
  lines.push(`  ${"version it reports".padEnd(26)} : ${result.version ?? "<no handshake>"}`);
  lines.push(`  ${"version this tree carries".padEnd(26)} : ${result.expected}`);
  if (result.violations.length > 0) {
    lines.push("", "  violations:");
    for (const violation of result.violations) lines.push(`    - ${violation}`);
  }
  lines.push(
    "",
    result.status === "ok"
      ? `OK: the ${SERVER_BIN} on PATH is this tree's ${result.expected}.`
      : `FAIL (${result.status}): the server an agent would connect to is not this tree's build.`,
  );
  return lines.join("\n");
}

/** @param {string[]} argv */
export function parseArgs(argv) {
  /** @type {string | undefined} */
  let bin;
  /** @type {string | undefined} */
  let expect;
  let timeoutMs = DEFAULT_TIMEOUT_MS;
  let json = false;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    /** @param {string} flag */
    const next = (flag) => {
      const value = argv[++i];
      if (value === undefined) throw new Error(`${flag} requires a value`);
      return value;
    };
    if (arg === "--json") json = true;
    else if (arg === "--bin") bin = path.resolve(next(arg));
    else if (arg === "--expect") expect = next(arg);
    else if (arg === "--timeout") {
      const raw = next(arg);
      const parsed = Number(raw);
      if (!Number.isFinite(parsed) || parsed <= 0) {
        throw new Error(`--timeout must be a positive number of milliseconds, got ${JSON.stringify(raw)}`);
      }
      timeoutMs = parsed;
    } else throw new Error(`unknown argument: ${arg}`);
  }
  return { bin, expect, timeoutMs, json };
}

export function usage() {
  return formatUsage({
    name: "check-mcp-install",
    summary: `Assert the ${SERVER_BIN} on PATH is the server this tree builds, not a stale copy.`,
    flags: [
      { flag: "--bin <path>", description: `probe this executable instead of resolving ${SERVER_BIN} on PATH` },
      { flag: "--expect <ver>", description: "version to require instead of package.json's" },
      { flag: "--timeout <ms>", description: `handshake budget (default ${DEFAULT_TIMEOUT_MS})` },
      { flag: "--json", description: "print the result as JSON" },
      { flag: "--help, -h", description: "print this message and exit 0" },
    ],
    exits: "0 the installed server matches · 1 absent, stale, or unresponsive · 2 the check could not run",
  });
}

/** @param {string[]} [argv] */
export async function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  /** @type {ReturnType<typeof parseArgs>} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (err) {
    console.error(`check-mcp-install: ${/** @type {Error} */ (err).message}`);
    return 2;
  }

  try {
    const expected = opts.expect ?? expectedVersion();
    const binPath = opts.bin ?? resolveOnPath(SERVER_BIN);
    const probe = binPath === null ? { version: null, error: null } : await probeServer(binPath, opts.timeoutMs);
    const { status, violations } = classify({ binPath, version: probe.version, error: probe.error, expected });
    const result = { binPath, version: probe.version, expected, status, violations, ok: status === "ok" };
    if (opts.json) console.log(JSON.stringify(result, null, 2));
    else console.log(formatReport(result));
    return result.ok ? 0 : 1;
  } catch (err) {
    console.error(`check-mcp-install: internal error: ${/** @type {Error} */ (err).message}`);
    return 2;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then((code) => process.exit(code));
}
