#!/usr/bin/env node
/**
 * Does every installed reader agree with the schemas DevCouncil writes?
 *
 * DevCouncil owns three schemas: the devmap store (`CURRENT_SCHEMA_VERSION`),
 * dc-store (`SCHEMA_VERSION`) and the workbench (`WORKBENCH_SCHEMA`). Every
 * other app reads at least one of them, and each reads it through something
 * that can fall behind on its own:
 *
 * - GitPulse vendors copies of the crates, and its binaries embed them: the
 *   `gitpulse-mcp` on PATH, and the app, whose bundle ships a `gitpulse-mcp`
 *   from the same build.
 * - `devmap` and `dcstore` on PATH are what Manvi, Jarvis and GitPulse shell
 *   out to. Manvi's own binary links no store crate (its Rust engine is
 *   dc-glob), so its schema is whatever these two report.
 * - Manvi pins a DevCouncil commit in `scripts/module-pins.txt`, and Jarvis
 *   pins one in `upstream.lock.json`. A pin is a commit, not a schema, so
 *   those rows compare the constants *at the pinned commit* with the owner's.
 *   A pin behind HEAD with the same constants still reads what DevCouncil
 *   writes.
 *
 * - Manvi owns the serve contract (`contracts/serve.ops.schema.json`), and
 *   GitPulse keeps a copy it builds its side of the seam against. That row
 *   compares the two files byte for byte.
 *
 * Source parity alone is not enough. On 2026-10-11 the vendored copy said 27
 * while the installed `gitpulse-mcp` still read 26. This check exists so the
 * installed readers are asked as well as the source.
 *
 * Each row ends in one of four verdicts:
 * - `match`: it reports what the owner writes.
 * - `stale`: it reports something else.
 * - `unverifiable`: it is there but could not say.
 * - `absent`: it is not installed or not checked out here.
 *
 * Only `match` and `absent` pass. A row that could not be checked never
 * reads as one that matched, and the summary names absent rows rather than
 * folding them into a pass. `MIN_READER_SCHEMA_VERSION` is the oldest store
 * the writer still opens; it is not a tolerance for an old reader. A reader
 * below `CURRENT_SCHEMA_VERSION` is stale.
 *
 * Exit: 0 every present reader matches · 1 a reader is stale or unverifiable,
 * or the owner could not be read · 2 usage.
 */
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";
import { readCliSchema } from "./check-vendor-schema.mjs";
import { DEFAULT_TIMEOUT_MS, SERVER_BIN, probeServer, resolveOnPath } from "./check-mcp-install.mjs";
import { findSibling } from "./vendor-crates.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The schemas DevCouncil owns: where each constant is declared, by key. */
export const SCHEMAS = Object.freeze({
  store: { label: "devmap store", constant: "CURRENT_SCHEMA_VERSION", file: "devmap-store/src/schema.rs" },
  dcStore: { label: "dc-store", constant: "SCHEMA_VERSION", file: "dc-store/src/lib.rs" },
  workbench: { label: "workbench", constant: "WORKBENCH_SCHEMA", file: "dc-store/src/workbench/mod.rs" },
});

/** @typedef {keyof typeof SCHEMAS} SchemaKey */
/** @typedef {Partial<Record<SchemaKey, number>>} Schemas */
/** @typedef {"match" | "stale" | "unverifiable" | "absent"} Verdict */
/**
 * @typedef {{ reader: string, keys: SchemaKey[], schemas?: Schemas, error?: string, absent?: string, note?: string }} Reading
 * @typedef {{ reader: string, verdict: Verdict, detail: string }} Row
 */

const OWNER_PREFIX = "rust";
const VENDORED_PREFIX = path.join("src-tauri", "vendored");
const ALL_KEYS = /** @type {SchemaKey[]} */ (Object.keys(SCHEMAS));

/**
 * One `pub const NAME: <int type> = N;` from a Rust source file.
 *
 * @param {string} text
 * @param {string} constant
 * @returns {number | null}
 */
export function rustConstant(text, constant) {
  const match = new RegExp(`pub const ${constant}:\\s*[iu](?:8|16|32|64|size)\\s*=\\s*(\\d+)\\s*;`).exec(text);
  return match ? Number(match[1]) : null;
}

/**
 * Every schema constant, read through `read(relativeFile)`. A file that is
 * missing or a constant that is not found throws: an unreadable owner must
 * stop the check rather than leave a key undefined.
 *
 * @param {(file: string) => string} read
 * @returns {Required<Schemas>}
 */
export function readSchemas(read) {
  /** @type {Schemas} */
  const out = {};
  for (const key of ALL_KEYS) {
    const { constant, file } = SCHEMAS[key];
    const value = rustConstant(read(file), constant);
    if (value === null) throw new Error(`${constant} not found in ${file}`);
    out[key] = value;
  }
  return /** @type {Required<Schemas>} */ (out);
}

/**
 * @param {string} root a checkout whose crates live under `prefix`
 * @param {string} prefix
 */
function schemasInTree(root, prefix) {
  return readSchemas((file) => readFileSync(path.join(root, prefix, file), "utf8"));
}

/**
 * The owner's constants as they were at `rev`, read from DevCouncil's own
 * object store.
 *
 * A commit the local clone does not have throws: it is reported as such, not
 * guessed at. A constant the commit does not declare is left out of the
 * answer, because older DevCouncil commits predate some of these schemas (the
 * workbench was not versioned before late September 2026). That leaves the
 * key for `judge` to report, and the constants the commit does declare are
 * still compared.
 *
 * @param {string} devcouncil
 * @param {string} rev
 * @param {typeof spawnSync} [run]
 * @returns {Schemas}
 */
export function schemasAt(devcouncil, rev, run = spawnSync) {
  const exists = run("git", ["cat-file", "-e", `${rev}^{commit}`], { cwd: devcouncil, encoding: "utf8", timeout: 15_000 });
  if (exists.error || exists.status !== 0) {
    throw new Error(`DevCouncil has no commit ${rev} here; fetch it to check this pin`);
  }
  /** @type {Schemas} */
  const out = {};
  for (const key of ALL_KEYS) {
    const { constant, file } = SCHEMAS[key];
    const shown = run("git", ["show", `${rev}:${OWNER_PREFIX}/${file}`], { cwd: devcouncil, encoding: "utf8", timeout: 15_000 });
    if (shown.error) throw new Error(`git show ${rev}:${OWNER_PREFIX}/${file} failed: ${shown.error.message}`);
    // A nonzero exit here means the path does not exist at that commit.
    const value = shown.status === 0 ? rustConstant(String(shown.stdout), constant) : null;
    if (value !== null) out[key] = value;
  }
  return out;
}

/**
 * The verdict for one reading against the owner's constants.
 *
 * @param {Required<Schemas>} owner
 * @param {Reading} reading
 * @returns {Row}
 */
export function judge(owner, reading) {
  const { reader } = reading;
  if (reading.absent) return { reader, verdict: "absent", detail: reading.absent };
  if (reading.error) return { reader, verdict: "unverifiable", detail: reading.error };
  const schemas = reading.schemas ?? {};
  const missing = reading.keys.filter((key) => typeof schemas[key] !== "number");
  const unreported = missing.length > 0 ? `does not report ${missing.map((key) => SCHEMAS[key].label).join(", ")}` : "";
  // A known mismatch outranks an unknown: a reader that is provably behind on
  // one schema is stale whatever it failed to say about another.
  const differs = reading.keys.filter((key) => typeof schemas[key] === "number" && schemas[key] !== owner[key]);
  if (differs.length > 0) {
    const why = [
      ...differs.map((key) => `${SCHEMAS[key].label} ${schemas[key]}, DevCouncil writes ${owner[key]}`),
      ...(unreported ? [unreported] : []),
    ].join("; ");
    return { reader, verdict: "stale", detail: reading.note ? `${why} (${reading.note})` : why };
  }
  if (missing.length > 0) {
    return { reader, verdict: "unverifiable", detail: reading.note ? `${unreported} (${reading.note})` : unreported };
  }
  const shown = reading.keys.map((key) => `${SCHEMAS[key].label} ${schemas[key]}`).join(", ");
  return { reader, verdict: "match", detail: reading.note ? `${shown} (${reading.note})` : shown };
}

/**
 * @param {Row[]} rows
 * @returns {{ ok: boolean, summary: string }}
 */
export function summarize(rows) {
  /** @type {Record<Verdict, number>} */
  const counts = { match: 0, stale: 0, unverifiable: 0, absent: 0 };
  for (const row of rows) counts[row.verdict] += 1;
  const ok = counts.stale === 0 && counts.unverifiable === 0 && counts.match > 0;
  const parts = /** @type {Verdict[]} */ (["match", "stale", "unverifiable", "absent"])
    .filter((verdict) => counts[verdict] > 0)
    .map((verdict) => `${counts[verdict]} ${verdict === "absent" ? "absent (not checked)" : verdict}`);
  return { ok, summary: `${ok ? "OK" : "FAIL"}: ${parts.join(" · ") || "nothing checked"}` };
}

/** @param {string} pins text of Manvi's scripts/module-pins.txt */
export function manviPin(pins) {
  for (const line of pins.split(/\r?\n/)) {
    const [name, , rev] = line.trim().split("\t");
    if (name === "DevCouncil" && /^[0-9a-f]{40}$/.test(rev ?? "")) return rev;
  }
  return null;
}

/** @param {string} lock text of Jarvis's upstream.lock.json */
export function jarvisPin(lock) {
  const rev = JSON.parse(lock)?.devcouncil?.revision;
  return typeof rev === "string" && /^[0-9a-f]{40}$/.test(rev) ? rev : null;
}

/**
 * A pin row: the constants at the pinned commit.
 *
 * @param {string} reader
 * @param {string | null} devcouncil
 * @param {string} head
 * @param {string | null} checkout the consumer's checkout, or null when not found
 * @param {string} file the pin file, relative to the checkout
 * @param {(text: string) => string | null} parse
 * @returns {Reading}
 */
export function pinReading(reader, devcouncil, head, checkout, file, parse) {
  if (checkout === null) return { reader, keys: ALL_KEYS, absent: "no checkout found" };
  const full = path.join(checkout, file);
  if (!existsSync(full)) return { reader, keys: ALL_KEYS, error: `${full} is missing` };
  let rev;
  try {
    rev = parse(readFileSync(full, "utf8"));
  } catch (err) {
    return { reader, keys: ALL_KEYS, error: `${file} is unreadable: ${err instanceof Error ? err.message : String(err)}` };
  }
  if (rev === null) return { reader, keys: ALL_KEYS, error: `${file} names no DevCouncil commit` };
  if (devcouncil === null) return { reader, keys: ALL_KEYS, error: "no DevCouncil checkout to read the pin from" };
  try {
    const note = rev === head ? `pins DevCouncil HEAD ${rev.slice(0, 8)}` : `pins ${rev.slice(0, 8)}, DevCouncil HEAD is ${head.slice(0, 8)}`;
    return { reader, keys: ALL_KEYS, schemas: schemasAt(devcouncil, rev), note };
  } catch (err) {
    return { reader, keys: ALL_KEYS, error: err instanceof Error ? err.message : String(err) };
  }
}

/** The contract Manvi owns and GitPulse copies, relative to either checkout. */
export const SERVE_CONTRACT = path.join("contracts", "serve.ops.schema.json");

/**
 * A copied contract against its owner's file, byte for byte. Unlike a schema
 * number, there is nothing to compare but the text, so any difference is
 * stale: the copy no longer describes what the owner speaks.
 *
 * @param {string} reader
 * @param {string} copy GitPulse's file
 * @param {string | null} ownerCheckout the owning app's checkout, or null when not found
 * @returns {Row}
 */
export function contractRow(reader, copy, ownerCheckout) {
  if (ownerCheckout === null) return { reader, verdict: "absent", detail: "no owner checkout found" };
  const owned = path.join(ownerCheckout, SERVE_CONTRACT);
  if (!existsSync(owned)) return { reader, verdict: "unverifiable", detail: `${owned} is missing` };
  if (!existsSync(copy)) return { reader, verdict: "unverifiable", detail: `${copy} is missing` };
  return readFileSync(copy, "utf8") === readFileSync(owned, "utf8")
    ? { reader, verdict: "match", detail: `identical to ${owned}` }
    : { reader, verdict: "stale", detail: `differs from ${owned}; copy Manvi's file over GitPulse's and update the Rust side to it` };
}

/**
 * An installed `gitpulse-mcp`: what its manifest says it reads. A binary from
 * before the manifest named dc-store and the workbench reports only the store,
 * and is unverifiable for the other two rather than assumed to match.
 *
 * @param {string} reader
 * @param {string | null} bin
 * @param {string} absent
 * @param {number} timeoutMs
 * @returns {Promise<Reading>}
 */
async function serverReading(reader, bin, absent, timeoutMs) {
  if (bin === null || !existsSync(bin)) return { reader, keys: ALL_KEYS, absent };
  const probe = await probeServer(bin, timeoutMs);
  if (probe.error !== null && probe.storeSchema === null) return { reader, keys: ALL_KEYS, error: `${bin}: ${probe.error}` };
  /** @type {Schemas} */
  const schemas = {};
  if (probe.storeSchema !== null) schemas.store = probe.storeSchema;
  if (probe.dcStoreSchema !== null) schemas.dcStore = probe.dcStoreSchema;
  if (probe.workbenchSchema !== null) schemas.workbench = probe.workbenchSchema;
  return { reader, keys: ALL_KEYS, schemas };
}

/** @returns {Reading} */
function devmapReading() {
  const reader = "devmap (PATH)";
  if (resolveOnPath("devmap") === null) return { reader, keys: ["store"], absent: "not on PATH" };
  const cli = readCliSchema();
  if (!cli.available) return { reader, keys: ["store"], error: cli.reason };
  return { reader, keys: ["store"], schemas: { store: cli.expected } };
}

/**
 * `dcstore --version` names the workbench schema it opens; it does not name
 * dc-store's own `SCHEMA_VERSION`, so that key is source parity only.
 *
 * @returns {Reading}
 */
function dcstoreReading() {
  const reader = "dcstore (PATH)";
  const bin = resolveOnPath("dcstore");
  if (bin === null) return { reader, keys: ["workbench"], absent: "not on PATH" };
  const out = spawnSync(bin, ["--version"], { encoding: "utf8", timeout: 15_000 });
  if (out.error || out.status !== 0) return { reader, keys: ["workbench"], error: `${bin} --version failed: ${(out.stderr || out.error?.message || "").toString().trim()}` };
  try {
    const parsed = JSON.parse(String(out.stdout).trim().split("\n").pop() ?? "");
    const workbench = parsed?.workbench_schema;
    return typeof workbench === "number"
      ? { reader, keys: ["workbench"], schemas: { workbench } }
      : { reader, keys: ["workbench"], error: `${bin} --version names no workbench_schema` };
  } catch {
    return { reader, keys: ["workbench"], error: `${bin} --version is not JSON` };
  }
}

/** @param {string} devcouncil */
function headOf(devcouncil) {
  const out = spawnSync("git", ["rev-parse", "HEAD"], { cwd: devcouncil, encoding: "utf8", timeout: 15_000 });
  return out.status === 0 ? String(out.stdout).trim() : "";
}

/** @param {string} name @param {string | undefined} override */
function checkout(name, override) {
  const found = override ?? findSibling(name, REPO_ROOT);
  return existsSync(found) ? found : null;
}

/**
 * @param {{ app: string, timeoutMs: number, env?: NodeJS.ProcessEnv }} options
 * @returns {Promise<{ owner: Required<Schemas> | null, ownerError: string | null, devcouncil: string | null, rows: Row[] }>}
 */
export async function collect({ app, timeoutMs, env = process.env }) {
  const devcouncil = checkout("DevCouncil", env.GITPULSE_DEVCOUNCIL_ROOT);
  if (devcouncil === null) {
    return { owner: null, ownerError: "no DevCouncil checkout (set GITPULSE_DEVCOUNCIL_ROOT)", devcouncil, rows: [] };
  }
  /** @type {Required<Schemas>} */
  let owner;
  try {
    owner = schemasInTree(devcouncil, OWNER_PREFIX);
  } catch (err) {
    return { owner: null, ownerError: `DevCouncil's schema constants are unreadable: ${err instanceof Error ? err.message : String(err)}`, devcouncil, rows: [] };
  }
  const head = headOf(devcouncil);

  /** @type {Reading[]} */
  const readings = [];
  try {
    readings.push({ reader: "GitPulse vendored crates", keys: ALL_KEYS, schemas: schemasInTree(REPO_ROOT, VENDORED_PREFIX) });
  } catch (err) {
    readings.push({ reader: "GitPulse vendored crates", keys: ALL_KEYS, error: err instanceof Error ? err.message : String(err) });
  }
  readings.push(devmapReading());
  readings.push(dcstoreReading());
  readings.push(await serverReading(`${SERVER_BIN} (PATH)`, resolveOnPath(SERVER_BIN), "not on PATH", timeoutMs));
  readings.push(await serverReading(
    `GitPulse.app (its bundled ${SERVER_BIN})`,
    path.join(app, "Contents", "MacOS", SERVER_BIN),
    `no ${app}`,
    timeoutMs,
  ));
  const manvi = checkout("Manvi", env.GITPULSE_MANVI_ROOT);
  readings.push(pinReading("Manvi pin (scripts/module-pins.txt)", devcouncil, head, manvi, path.join("scripts", "module-pins.txt"), manviPin));
  readings.push(pinReading("Jarvis pin (upstream.lock.json)", devcouncil, head, checkout("Jarvis", env.GITPULSE_JARVIS_ROOT), "upstream.lock.json", jarvisPin));

  const rows = readings.map((reading) => judge(owner, reading));
  rows.push(contractRow("GitPulse copy of Manvi's serve contract", path.join(REPO_ROOT, SERVE_CONTRACT), manvi));
  return { owner, ownerError: null, devcouncil, rows };
}

export function usage() {
  return formatUsage({
    name: "check-schema-parity",
    summary: "Ask every installed reader which DevCouncil schemas it reads, and compare them with what DevCouncil writes.",
    flags: [
      { flag: "--app <path>", description: "The GitPulse.app bundle to ask (default /Applications/GitPulse.app)" },
      { flag: "--timeout <ms>", description: `Per-server handshake budget (default ${DEFAULT_TIMEOUT_MS})` },
      { flag: "--json", description: "Machine-readable result" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits: "0 every present reader matches · 1 stale, unverifiable, or owner unreadable · 2 usage",
  });
}

/**
 * @param {string[]} argv
 * @returns {{ app: string, timeoutMs: number, json: boolean } | { error: string }}
 */
export function parseArgs(argv) {
  let app = "/Applications/GitPulse.app";
  let timeoutMs = DEFAULT_TIMEOUT_MS;
  let json = false;
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--json") json = true;
    else if (arg === "--app" && argv[i + 1]) app = argv[++i];
    else if (arg === "--timeout" && argv[i + 1]) {
      timeoutMs = Number(argv[++i]);
      if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) return { error: "--timeout takes a positive whole number of milliseconds" };
    } else return { error: `unknown option ${JSON.stringify(arg)}` };
  }
  return { app, timeoutMs, json };
}

/** @param {string[]} argv */
export async function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  const args = parseArgs(argv);
  if ("error" in args) {
    console.error(`FAIL: ${args.error}\n`);
    console.error(usage());
    return 2;
  }
  const result = await collect(args);
  if (result.owner === null) {
    if (args.json) console.log(JSON.stringify({ ok: false, error: result.ownerError, rows: [] }));
    else console.error(`FAIL: ${result.ownerError}`);
    return 1;
  }
  const { ok, summary } = summarize(result.rows);
  if (args.json) {
    console.log(JSON.stringify({ ok, owner: result.owner, devcouncil: result.devcouncil, rows: result.rows, summary }));
    return ok ? 0 : 1;
  }
  const owner = ALL_KEYS.map((key) => `${SCHEMAS[key].label} ${result.owner?.[key]}`).join(", ");
  console.log(`DevCouncil writes ${owner} (${result.devcouncil})\n`);
  const width = Math.max(...result.rows.map((row) => row.reader.length));
  for (const row of result.rows) {
    console.log(`  ${row.reader.padEnd(width)}  ${row.verdict.padEnd(12)}  ${row.detail}`);
  }
  console.log(`\n${summary}`);
  return ok ? 0 : 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = await main();
}
