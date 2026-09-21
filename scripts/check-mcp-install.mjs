#!/usr/bin/env node
/**
 * Agent install doctor — are the executables an agent would actually launch
 * the ones this repo builds?
 *
 * Two of them, because the plugin ships two. `.mcp.json` spawns
 * `gitpulse-mcp`; `hooks/hooks.json` spawns `gitpulse-hook` for the collision
 * guard, the command gate and the session brief. Checking only the server is
 * how this doctor came to report `ok` on a machine where every hook the plugin
 * declares was a `command not found`: the server's absence shows up as a
 * failed MCP connection, but a hook that cannot start is a non-blocking error
 * the host swallows, so the two gates simply stop running and nothing says so.
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
 *   ok           it reports this repo's version, and its schema or subcommands
 *   stale        it answered, with a different version, schema or subcommands
 *   unresponsive it did not provide a usable identity
 *   absent       nothing by that name is on PATH at all
 *
 * Version and schema are the *release* identity, and that is not the whole
 * question. Neither moves when a fix lands between releases, so a binary built
 * from older source reports a version that still matches and passes every
 * check above — which is exactly what happened: a `gitpulse-hook` built the day
 * before a repository-trust fix was reported `ok` here while it went on
 * emitting the pre-fix refusal. So a third half asks what source the binaries
 * were actually built from, recorded by `npm run mcp:install` and re-derived
 * here; see `install-identity.mjs`. Its verdicts add `unverifiable`, which is
 * never folded into `ok` — an install nobody recorded must not read the same
 * as one that was checked and matched.
 *
 * Each binary carries its own verdict and all three are printed. They are not
 * merged into one line: a healthy server reporting a clean pass over silently
 * disabled hooks is precisely the substitution this file exists to refuse.
 * The process exit code is the whole package — 0 only when all are ok.
 *
 * Not part of `ci:local`: CI has no reason to install either binary, and a
 * check that cannot run there must not be made to look like one that passed.
 * Refresh with `npm run mcp:install`.
 *
 * Exit codes: 0 ok · 1 absent/stale/unresponsive/unverifiable · 2 internal error.
 *
 * Flags:
 *   --bin <path>       probe this server instead of resolving PATH
 *   --hook-bin <path>  probe this hook binary instead of resolving PATH
 *   --expect <ver>     compare against this version instead of package.json
 *   --timeout <ms>     identity budget (default 10000)
 *   --json             machine-readable result
 */
import { spawn } from "node:child_process";
import { accessSync, constants, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";
import { readVendoredSchema } from "./check-vendor-schema.mjs";
import { fileDigest, readInstallRecord, recordPath, sourceDigest } from "./install-identity.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The executable name both MCP manifests spawn. */
export const SERVER_BIN = "gitpulse-mcp";

/**
 * The executable `plugins/gitpulse/hooks/hooks.json` spawns for every hook.
 *
 * It is checked here for the same reason the server is, and a sharper one. The
 * server's absence is loud: a client that cannot spawn it shows a failed MCP
 * connection. A hook's absence is silent by design — the host reports a
 * non-blocking error and lets the tool call proceed, which the Claude Code
 * hook reference states plainly: "a mistyped path in settings.json leaves the
 * gate silently disabled". So a repository whose collision guard and command
 * gate never run looks exactly like one where they ran and found nothing,
 * which is the single confusion this whole package is built to avoid.
 */
export const HOOK_BIN = "gitpulse-hook";

/** Where the shipped hook manifest lives, relative to the repo root. */
const HOOKS_MANIFEST = path.join("plugins", "gitpulse", "hooks", "hooks.json");

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
 * The hook subcommands the shipped manifest actually asks a host to spawn.
 *
 * Read from `hooks/hooks.json` rather than listed here, so a subcommand added
 * to the manifest is checked against the installed binary without anyone
 * remembering to update this file. A manifest that cannot be read is an
 * internal error, not an empty list: "we could not find out what the manifest
 * declares" must never be served as "it declares nothing".
 *
 * Only the *first* word after the binary is a subcommand. `notify` takes the
 * notification type as a second word — the matcher's own name — and counting
 * that as a subcommand would report every correctly installed binary as
 * drifted, because it serves `notify` and never served `permission_prompt`.
 *
 * @param {string} [root]
 * @returns {string[]}
 */
export function declaredHookSubcommands(root = REPO_ROOT) {
  const raw = readFileSync(path.join(root, HOOKS_MANIFEST), "utf8");
  /** @type {unknown} */
  const parsed = JSON.parse(raw);
  if (!isRecord(parsed) || !isRecord(parsed.hooks)) {
    throw new Error(`${HOOKS_MANIFEST} has no hooks object`);
  }
  /** @type {Set<string>} */
  const found = new Set();
  for (const groups of Object.values(parsed.hooks)) {
    if (!Array.isArray(groups)) continue;
    for (const group of groups) {
      if (!isRecord(group) || !Array.isArray(group.hooks)) continue;
      for (const handler of group.hooks) {
        if (!isRecord(handler) || typeof handler.command !== "string") continue;
        const [bin, subcommand] = handler.command.trim().split(/\s+/);
        if (bin !== HOOK_BIN || !subcommand) continue;
        found.add(subcommand);
      }
    }
  }
  if (found.size === 0) throw new Error(`${HOOKS_MANIFEST} declares no ${HOOK_BIN} subcommand`);
  return [...found].sort();
}

/**
 * Read `gitpulse-hook --version`: its version, and the subcommands it serves.
 *
 * Both halves must be present. A binary that printed one and not the other has
 * not identified itself, and guessing the missing half is how a partial answer
 * becomes a clean bill of health.
 *
 * @param {string} stdout
 * @returns {{ version: string | null, subcommands: string[] | null }}
 */
export function parseHookIdentity(stdout) {
  /** @type {string | null} */
  let version = null;
  /** @type {string[] | null} */
  let subcommands = null;
  for (const line of stdout.split(/\r?\n/)) {
    const trimmed = line.trim();
    const versioned = /^gitpulse-hook\s+(\S+)$/.exec(trimmed);
    if (versioned && version === null) version = versioned[1];
    const listed = /^subcommands:\s*(.+)$/.exec(trimmed);
    if (listed && subcommands === null) {
      subcommands = listed[1]
        .split(",")
        .map((name) => name.trim())
        .filter((name) => name.length > 0)
        .sort();
    }
  }
  return { version, subcommands };
}

/**
 * Ask the hook binary who it is.
 *
 * `--version` is the only argument that makes this binary answer at all: every
 * other invocation is a hook, and silence is a legitimate reply to all of them,
 * so no hook call can distinguish a working binary from a broken one. stdin is
 * closed rather than piped — the identity path reads none, and leaving a pipe
 * open would make this probe hang on a build that does.
 *
 * @param {string} binPath
 * @param {number} timeoutMs
 * @returns {Promise<{ version: string | null, subcommands: string[] | null, error: string | null }>}
 */
export function probeHook(binPath, timeoutMs = DEFAULT_TIMEOUT_MS) {
  return new Promise((resolve) => {
    // stdin is `null` here by construction: the identity path reads none, and
    // handing it an open pipe would hang this probe against a build that does.
    /** @type {import("node:child_process").ChildProcessByStdio<null, import("node:stream").Readable, import("node:stream").Readable>} */
    let child;
    try {
      child = spawn(binPath, ["--version"], { stdio: ["ignore", "pipe", "pipe"] });
    } catch (err) {
      resolve({ version: null, subcommands: null, error: /** @type {Error} */ (err).message });
      return;
    }
    let stdout = "";
    let receivedBytes = 0;
    let settled = false;
    const finish = (/** @type {{ version: string | null, subcommands: string[] | null, error: string | null }} */ outcome) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.kill("SIGKILL");
      resolve(outcome);
    };
    const timer = setTimeout(
      () => finish({ version: null, subcommands: null, error: `no identity response within ${timeoutMs}ms` }),
      timeoutMs,
    );
    child.on("error", (err) => finish({ version: null, subcommands: null, error: err.message }));
    child.stderr.resume();
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      if (settled) return;
      receivedBytes += Buffer.byteLength(chunk, "utf8");
      if (receivedBytes > MAX_RESPONSE_BYTES) {
        finish({ version: null, subcommands: null, error: `identity response exceeded ${MAX_RESPONSE_BYTES} bytes` });
        return;
      }
      stdout += chunk;
    });
    // Read to EOF rather than settling on the first chunk: the two identity
    // lines are not guaranteed to arrive in one write.
    child.on("close", () => {
      const { version, subcommands } = parseHookIdentity(stdout);
      finish({
        version,
        subcommands,
        error: version !== null && subcommands !== null
          ? null
          : "binary exited without a complete identity line",
      });
    });
  });
}

/**
 * @param {{ binPath: string | null, version: string | null, subcommands: string[] | null, error: string | null, expected: string, declared: string[] }} observed
 * @returns {{ status: "ok" | "stale" | "unresponsive" | "absent", violations: string[] }}
 */
export function classifyHook({ binPath, version, subcommands, error, expected, declared }) {
  if (binPath === null) {
    return {
      status: "absent",
      violations: [
        `no ${HOOK_BIN} on PATH — ${HOOKS_MANIFEST} spawns that bare name, so every hook it declares is a non-blocking error and the gate is silently disabled`,
        "install it with: npm run mcp:install",
      ],
    };
  }
  if (version === null || subcommands === null) {
    return {
      status: "unresponsive",
      violations: [`${binPath} did not identify itself${error ? ` (${error})` : ""}`],
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
  const missing = declared.filter((name) => !subcommands.includes(name));
  if (missing.length > 0) {
    return {
      status: "stale",
      violations: [
        `${binPath} does not serve ${missing.join(", ")}, which ${HOOKS_MANIFEST} declares — those hooks would run and decide nothing`,
        "refresh it with: npm run mcp:install",
      ],
    };
  }
  return { status: "ok", violations: [] };
}

/**
 * Whether the binaries on PATH were built from the source this tree holds.
 *
 * The version and schema checks above answer "is this the right release". They
 * cannot answer "is this the right code": both binaries report only
 * `CARGO_PKG_VERSION`, so every fix that lands between releases leaves them
 * reporting a version that still matches. That is not hypothetical — a
 * `gitpulse-hook` built the day before a repository-trust fix passed both
 * checks above while still emitting the pre-fix refusal to users.
 *
 * So the install writes down what it built from and this re-derives it. Three
 * distinct ways of not being able to say yes, kept distinct because they need
 * different actions:
 *
 * - **unverifiable** — no usable record, or a binary on PATH that this record
 *   does not describe. Nothing was compared. It is never `ok`: an install that
 *   was never recorded looks exactly like one that was, and that equivalence is
 *   the whole defect this closes.
 * - **stale** — the record describes these binaries, and this tree's sources
 *   have moved since. Reinstalling is the fix.
 * - **ok** — the digests agree.
 *
 * @param {{ record: import("./install-identity.mjs").InstallRecord | null, treeDigest: string, treeFileCount: number, binPaths: (string | null)[], root: string }} observed
 * @returns {{ status: "ok" | "stale" | "unverifiable", violations: string[] }}
 */
export function classifyProvenance({ record, treeDigest, treeFileCount, binPaths, root }) {
  const present = binPaths.filter((p) => /** @type {string | null} */ (p) !== null);
  if (present.length === 0) {
    return {
      status: "unverifiable",
      violations: ["neither binary is on PATH, so there is nothing to check provenance for"],
    };
  }
  if (record === null) {
    return {
      status: "unverifiable",
      violations: [
        `no usable install record at ${recordPath()} — the binaries on PATH cannot be traced to any source`,
        "record one with: npm run mcp:install",
      ],
    };
  }
  /** @type {string[]} */
  const unknown = [];
  /** @type {string[]} */
  const replaced = [];
  for (const bin of present) {
    const recorded = record.binaries[/** @type {string} */ (bin)];
    if (recorded === undefined) {
      unknown.push(/** @type {string} */ (bin));
      continue;
    }
    const actual = fileDigest(/** @type {string} */ (bin));
    if (actual === null || actual !== recorded) replaced.push(/** @type {string} */ (bin));
  }
  if (unknown.length > 0 || replaced.length > 0) {
    return {
      status: "unverifiable",
      violations: [
        ...unknown.map((bin) => `${bin} is on PATH but the install record does not describe it`),
        ...replaced.map((bin) => `${bin} has changed since it was recorded — something other than npm run mcp:install wrote it`),
        "re-record with: npm run mcp:install",
      ],
    };
  }
  if (record.sourceDigest !== treeDigest) {
    const from = record.sourceRoot === root ? "" : ` (installed from ${record.sourceRoot})`;
    return {
      status: "stale",
      violations: [
        `the binaries on PATH were built from source that differs from this tree${from} — installed ${record.installedAt}`,
        `recorded digest ${record.sourceDigest.slice(0, 16)}… over ${record.sourceFileCount} files; this tree is ${treeDigest.slice(0, 16)}… over ${treeFileCount}`,
        "refresh it with: npm run mcp:install",
      ],
    };
  }
  return { status: "ok", violations: [] };
}

/**
 * @param {{ binPath: string | null, version: string | null, storeSchema: number | null, expected: string, expectedSchema: number, status: string, violations: string[], hook?: { binPath: string | null, version: string | null, subcommands: string[] | null, declared: string[], status: string, violations: string[] }, provenance?: { status: string, violations: string[], treeDigest: string, treeFileCount: number, installedAt: string | null } }} result
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
  const hook = result.hook;
  if (hook) {
    lines.push("", `  ${HOOK_BIN}`, "");
    lines.push(`  ${"executable on PATH".padEnd(26)} : ${hook.binPath ?? "<not found>"}`);
    lines.push(`  ${"version it reports".padEnd(26)} : ${hook.version ?? "<no identity>"}`);
    lines.push(`  ${"subcommands it serves".padEnd(26)} : ${hook.subcommands?.join(", ") ?? "<unavailable>"}`);
    lines.push(`  ${"subcommands hooks.json needs".padEnd(26)} : ${hook.declared.join(", ")}`);
    if (hook.violations.length > 0) {
      lines.push("", "  violations:");
      for (const violation of hook.violations) lines.push(`    - ${violation}`);
    }
  }
  const provenance = result.provenance;
  if (provenance) {
    lines.push("", "  source provenance", "");
    lines.push(`  ${"this tree's source digest".padEnd(26)} : ${provenance.treeDigest.slice(0, 16)}… (${provenance.treeFileCount} files)`);
    lines.push(`  ${"recorded at install".padEnd(26)} : ${provenance.installedAt ?? "<never recorded>"}`);
    if (provenance.violations.length > 0) {
      lines.push("", "  violations:");
      for (const violation of provenance.violations) lines.push(`    - ${violation}`);
    }
  }
  // Each half gets its own verdict line. Collapsing them into one would let a
  // healthy server report a clean pass over silently disabled hooks, which is
  // the exact substitution this doctor exists to refuse.
  lines.push(
    "",
    result.status === "ok"
      ? `OK: the ${SERVER_BIN} on PATH matches version ${result.expected} and store schema ${result.expectedSchema}.`
      : `FAIL (${result.status}): the server an agent would connect to is not this tree's build.`,
  );
  if (hook) {
    lines.push(
      hook.status === "ok"
        ? `OK: the ${HOOK_BIN} on PATH is version ${result.expected} and serves every hook the plugin declares.`
        : `FAIL (${hook.status}): the hooks the plugin declares would not run against this tree's build.`,
    );
  }
  if (provenance) {
    lines.push(
      provenance.status === "ok"
        ? "OK: both binaries were built from the source this tree holds."
        : `FAIL (${provenance.status}): the code running inside those binaries is not this tree's — version alone cannot see this.`,
    );
  }
  return lines.join("\n");
}

/** @param {string[]} argv */
export function parseArgs(argv) {
  /** @type {string | undefined} */
  let bin;
  /** @type {string | undefined} */
  let hookBin;
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
    else if (arg === "--hook-bin") hookBin = path.resolve(next(arg));
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
  return { bin, hookBin, expect, timeoutMs, json };
}

export function usage() {
  return formatUsage({
    name: "check-mcp-install",
    summary: `Assert the ${SERVER_BIN} and ${HOOK_BIN} on PATH are the ones this tree builds, not stale copies.`,
    flags: [
      { flag: "--bin <path>", description: `probe this executable instead of resolving ${SERVER_BIN} on PATH` },
      { flag: "--hook-bin <path>", description: `probe this executable instead of resolving ${HOOK_BIN} on PATH` },
      { flag: "--expect <ver>", description: "version to require instead of package.json's" },
      { flag: "--timeout <ms>", description: `handshake budget (default ${DEFAULT_TIMEOUT_MS})` },
      { flag: "--json", description: "print the result as JSON" },
      { flag: "--help, -h", description: "print this message and exit 0" },
    ],
    exits: "0 the installed server matches · 1 absent, stale, unresponsive, or unverifiable · 2 the check could not run",
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

    // Derived from the shipped manifest, so a hook added there is checked
    // against the installed binary without this file being touched.
    const declared = declaredHookSubcommands();
    const hookPath = opts.hookBin ?? resolveOnPath(HOOK_BIN);
    const hookProbe = hookPath === null
      ? { version: null, subcommands: null, error: null }
      : await probeHook(hookPath, opts.timeoutMs);
    const hookVerdict = classifyHook({ binPath: hookPath, ...hookProbe, expected, declared });
    const hook = {
      binPath: hookPath,
      version: hookProbe.version,
      subcommands: hookProbe.subcommands,
      declared,
      status: hookVerdict.status,
      violations: hookVerdict.violations,
      ok: hookVerdict.status === "ok",
    };

    // Asked of the tree, not of the binaries: they have no channel for it.
    const { digest: treeDigest, fileCount: treeFileCount } = sourceDigest(REPO_ROOT);
    const record = readInstallRecord();
    const provenanceVerdict = classifyProvenance({
      record, treeDigest, treeFileCount, binPaths: [binPath, hookPath], root: REPO_ROOT,
    });
    const provenance = {
      status: provenanceVerdict.status,
      violations: provenanceVerdict.violations,
      treeDigest,
      treeFileCount,
      installedAt: record?.installedAt ?? null,
      ok: provenanceVerdict.status === "ok",
    };

    const result = {
      binPath, version: probe.version, storeSchema: probe.storeSchema, expected, expectedSchema,
      status, violations,
      // Kept as the server's own verdict so an existing reader of this field
      // is not silently told something new; `ok` below is the whole package.
      serverOk: status === "ok",
      hook,
      provenance,
      ok: status === "ok" && hook.ok && provenance.ok,
    };
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
