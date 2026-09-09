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
 * compare its version and store schema against this tree.
 *
 * So the comparison is the check. The four outcomes are kept distinct on
 * purpose — "no binary on PATH" and "binary matches" are the two that a
 * naive check would collapse into one silent pass:
 *
 *   ok           the server reports this repo's version and store schema
 *   stale        it answered, with a different version or store schema
 *   unresponsive it did not provide a usable handshake and schema identity
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
import { readVendoredSchema } from "./check-vendor-schema.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The executable name both MCP manifests spawn. */
export const SERVER_BIN = "gitpulse-mcp";

/** Handshake budget. An unresponsive server must fail, never hang a release. */
export const DEFAULT_TIMEOUT_MS = 10_000;
const MAX_RESPONSE_BYTES = 1_048_576;
const MANIFEST_URI = "gitpulse://server/manifest";

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

/** @param {unknown} value @returns {value is Record<string, unknown>} */
function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * A missing reply is distinct from a reply whose schema identity is unavailable.
 * @param {string} stdout
 * @param {number} id
 * @returns {{ storeSchema: number | null, error: string | null } | null}
 */
export function parseServerManifest(stdout, id) {
  for (const line of stdout.split(/\r?\n/)) {
    /** @type {unknown} */
    let message;
    try { message = JSON.parse(line); } catch { continue; }
    if (!isRecord(message) || message.id !== id) continue;
    if (!isRecord(message.result) || !Array.isArray(message.result.contents)
        || message.result.contents.length !== 1) {
      return { storeSchema: null, error: "server did not return its manifest resource" };
    }
    const content = message.result.contents[0];
    if (!isRecord(content) || content.uri !== MANIFEST_URI
        || content.mimeType !== "application/json" || typeof content.text !== "string") {
      return { storeSchema: null, error: "server manifest is missing or truncated" };
    }
    /** @type {unknown} */
    let manifest;
    try { manifest = JSON.parse(content.text); } catch {
      return { storeSchema: null, error: "server manifest is invalid JSON" };
    }
    const schema = isRecord(manifest) ? manifest.storeSchemaVersion : null;
    if (typeof schema !== "number" || !Number.isSafeInteger(schema) || schema <= 0) {
      return { storeSchema: null, error: "server manifest has no valid storeSchemaVersion" };
    }
    return { storeSchema: schema, error: null };
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
 * @returns {Promise<{ version: string | null, storeSchema: number | null, error: string | null }>}
 */
export function probeServer(binPath, timeoutMs = DEFAULT_TIMEOUT_MS) {
  return new Promise((resolve) => {
    /** @type {import("node:child_process").ChildProcessWithoutNullStreams} */
    let child;
    try {
      child = spawn(binPath, [], { stdio: ["pipe", "pipe", "pipe"] });
    } catch (err) {
      resolve({ version: null, storeSchema: null, error: /** @type {Error} */ (err).message });
      return;
    }
    let stdout = "";
    let receivedBytes = 0;
    /** @type {string | null} */
    let version = null;
    let requestedManifest = false;
    let settled = false;
    const finish = (/** @type {{ version: string | null, storeSchema: number | null, error: string | null }} */ outcome) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.kill("SIGKILL");
      resolve(outcome);
    };
    const timer = setTimeout(
      () => finish({ version, storeSchema: null, error: `no complete identity response within ${timeoutMs}ms` }),
      timeoutMs,
    );

    child.on("error", (err) => finish({ version, storeSchema: null, error: err.message }));
    child.stderr.resume();
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      if (settled) return;
      receivedBytes += Buffer.byteLength(chunk, "utf8");
      if (receivedBytes > MAX_RESPONSE_BYTES) {
        finish({ version, storeSchema: null, error: `identity response exceeded ${MAX_RESPONSE_BYTES} bytes` });
        return;
      }
      stdout += chunk;
      version ??= parseServerVersion(stdout, 1);
      if (version !== null && !requestedManifest) {
        requestedManifest = true;
        child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);
        child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 2, method: "resources/read", params: { uri: MANIFEST_URI } })}\n`);
      }
      const manifest = requestedManifest ? parseServerManifest(stdout, 2) : null;
      if (manifest !== null) finish({ version, ...manifest });
    });
    child.on("close", () => {
      finish({ version, storeSchema: null, error: version === null
        ? "server exited without a usable initialize response"
        : "server exited without its manifest schema identity" });
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
          protocolVersion: "2024-11-05",
          capabilities: {},
          clientInfo: { name: "gitpulse-mcp-doctor", version: "1" },
        },
      })}\n`,
    );
  });
}

/**
 * @param {{ binPath: string | null, version: string | null, storeSchema: number | null, error: string | null, expected: string, expectedSchema: number }} observed
 * @returns {{ status: "ok" | "stale" | "unresponsive" | "absent", violations: string[] }}
 */
export function classify({ binPath, version, storeSchema, error, expected, expectedSchema }) {
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
  if (storeSchema === null || error !== null) {
    return {
      status: "unresponsive",
      violations: [`${binPath} did not provide a usable schema identity${error ? ` (${error})` : ""}`],
    };
  }
  if (storeSchema !== expectedSchema) {
    return {
      status: "stale",
      violations: [`${binPath} reads store schema ${storeSchema} but this tree reads ${expectedSchema}`, "refresh it with: npm run mcp:install"],
    };
  }
  return { status: "ok", violations: [] };
}

/**
 * @param {{ binPath: string | null, version: string | null, storeSchema: number | null, expected: string, expectedSchema: number, status: string, violations: string[] }} result
 */
export function formatReport(result) {
  const lines = ["MCP install doctor", ""];
  lines.push(`  ${"executable on PATH".padEnd(26)} : ${result.binPath ?? "<not found>"}`);
  lines.push(`  ${"version it reports".padEnd(26)} : ${result.version ?? "<no handshake>"}`);
  lines.push(`  ${"version this tree carries".padEnd(26)} : ${result.expected}`);
  lines.push(`  ${"store schema it reports".padEnd(26)} : ${result.storeSchema ?? "<unavailable>"}`);
  lines.push(`  ${"store schema this tree reads".padEnd(26)} : ${result.expectedSchema}`);
  if (result.violations.length > 0) {
    lines.push("", "  violations:");
    for (const violation of result.violations) lines.push(`    - ${violation}`);
  }
  lines.push(
    "",
    result.status === "ok"
      ? `OK: the ${SERVER_BIN} on PATH matches version ${result.expected} and store schema ${result.expectedSchema}.`
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
    const expectedSchema = readVendoredSchema();
    const binPath = opts.bin ?? resolveOnPath(SERVER_BIN);
    const probe = binPath === null ? { version: null, storeSchema: null, error: null } : await probeServer(binPath, opts.timeoutMs);
    const { status, violations } = classify({ binPath, ...probe, expected, expectedSchema });
    const result = { binPath, version: probe.version, storeSchema: probe.storeSchema, expected, expectedSchema, status, violations, ok: status === "ok" };
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
