#!/usr/bin/env node
/**
 * Does every installed reader agree with the schemas DevCouncil writes?
 *
 * DevCouncil owns five formats: the devmap store (`CURRENT_SCHEMA_VERSION`),
 * dc-store (`SCHEMA_VERSION`), the workbench (`WORKBENCH_SCHEMA`), the
 * extraction payload (devmap-extract's `EXTRACTION_SCHEMA_VERSION`, a string
 * constant; a stored graph written at another version is reported obsolete) and
 * the stacked-worktree lease (devmap-query's `STACK_FORMAT_VERSION`, which no
 * schema number tracks: a devmap that reads another lease format rejects every
 * stacked lane GitPulse provisions while the schemas all agree). Every
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
 * - GitPulse serves 11 `devmap_*` tools through gitpulse-mcp without vendoring
 *   devmap-serve, so no Rust test can see upstream's `TOOL_NAMES`. That row
 *   asserts GitPulse's names are a subset of devmap-serve's at the vendored
 *   DevCouncil commit.
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
import { failOnGitBound, findSibling } from "./vendor-crates.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The schemas DevCouncil owns: where each constant is declared, by key. */
export const SCHEMAS = Object.freeze({
  store: { label: "devmap store", constant: "CURRENT_SCHEMA_VERSION", file: "devmap-store/src/schema.rs" },
  dcStore: { label: "dc-store", constant: "SCHEMA_VERSION", file: "dc-store/src/lib.rs" },
  workbench: { label: "workbench", constant: "WORKBENCH_SCHEMA", file: "dc-store/src/workbench/mod.rs" },
  extract: { label: "devmap-extract", constant: "EXTRACTION_SCHEMA_VERSION", file: "devmap-extract/src/cache.rs" },
  lease: { label: "stack lease", constant: "STACK_FORMAT_VERSION", file: "devmap-query/src/stacked.rs" },
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
 * Rust source with its comments blanked: line comments (including doc
 * comments) to end of line, and block comments (which nest in Rust) to their
 * close, each replaced by one space with
 * newlines kept. String and char literals are copied through untouched, so a
 * `//` inside a string is not a comment. Only live declarations remain.
 *
 * With `blankStrings`, string contents are blanked as well (quotes and
 * newlines kept), so a declaration quoted inside a string is not one. The one
 * literal kept is a bare `"123"` right after `=`: devmap-extract declares its
 * version as such a string.
 *
 * @param {string} text
 * @param {boolean} [blankStrings]
 * @returns {string}
 */
export function withoutComments(text, blankStrings = false) {
  const blank = (/** @type {string} */ literal, /** @type {string} */ before) => {
    if (!blankStrings || (/^"\d+"$/.test(literal) && /=\s*$/.test(before))) return literal;
    const open = literal.indexOf('"');
    const close = literal.lastIndexOf('"');
    return close <= open ? literal : literal.slice(0, open + 1) + literal.slice(open + 1, close).replace(/[^\n]/g, " ") + literal.slice(close);
  };
  let out = "";
  let i = 0;
  while (i < text.length) {
    const two = text.slice(i, i + 2);
    if (two === "//") {
      while (i < text.length && text[i] !== "\n") i++;
      out += " ";
    } else if (two === "/*") {
      let depth = 0;
      do {
        const pair = text.slice(i, i + 2);
        if (pair === "/*") { depth++; i += 2; }
        else if (pair === "*/") { depth--; i += 2; }
        else { if (text[i] === "\n") out += "\n"; i++; }
      } while (depth > 0 && i < text.length);
      out += " ";
    } else if (text[i] === "r" && /^r(#*)"/.test(text.slice(i, i + 40)) && !/[A-Za-z0-9_]/.test(text[i - 1] ?? "")) {
      const hashes = /^r(#*)"/.exec(text.slice(i, i + 40))?.[1] ?? "";
      const close = text.indexOf(`"${hashes}`, i + 2 + hashes.length);
      const end = close === -1 ? text.length : close + 1 + hashes.length;
      out += blank(text.slice(i, end), out);
      i = end;
    } else if (text[i] === '"') {
      let end = i + 1;
      while (end < text.length && text[end] !== '"') end += text[end] === "\\" ? 2 : 1;
      out += blank(text.slice(i, end + 1), out);
      i = end + 1;
    } else if (text[i] === "'" && /^'(?:\\.[^']*|[^\\'])'/.test(text.slice(i, i + 12))) {
      const literal = /^'(?:\\.[^']*|[^\\'])'/.exec(text.slice(i, i + 12))?.[0] ?? "'";
      out += literal;
      i += literal.length;
    } else {
      out += text[i++];
    }
  }
  return out;
}

/**
 * Rust source (comments and string contents already blanked) without the
 * items gated by `#[cfg(test)]`: a test module's copy of a constant is not the
 * one the build ships. The item runs to its matching `}`, or to `;` when it
 * has no body.
 *
 * @param {string} text
 * @returns {string}
 */
function withoutTestItems(text) {
  const gate = /#\[cfg\(\s*test\s*\)\]/g;
  let out = "";
  let at = 0;
  for (let match = gate.exec(text); match !== null; match = gate.exec(text)) {
    if (match.index < at) continue;
    out += text.slice(at, match.index);
    let i = match.index + match[0].length;
    while (i < text.length && text[i] !== "{" && text[i] !== ";") i++;
    if (text[i] === "{") {
      let depth = 0;
      do {
        const quoted = /^'(?:\\.[^']*|[^\\'])'/.exec(text.slice(i, i + 12));
        if (text[i] === "'" && quoted) i += quoted[0].length;
        else {
          if (text[i] === "{") depth++;
          else if (text[i] === "}") depth--;
          i++;
        }
      } while (depth > 0 && i < text.length);
    } else i++;
    at = i;
    gate.lastIndex = i;
  }
  return out + text.slice(at);
}

/**
 * One `pub const NAME: <int type> = N;` from a Rust source file, or the
 * `pub const NAME: &str = "N";` form devmap-extract declares its version in.
 * A string that is not a whole number is not found, and so is a constant
 * declared twice with different values: either would be a guess.
 *
 * @param {string} text
 * @param {string} constant
 * @returns {number | null}
 */
export function rustConstant(text, constant) {
  text = withoutTestItems(withoutComments(text, true));
  const integer = String.raw`[iu](?:8|16|32|64|size)\s*=\s*(\d+)`;
  const string = String.raw`&(?:'static\s+)?str\s*=\s*"(\d+)"`;
  const found = [...text.matchAll(new RegExp(String.raw`pub const ${constant}:\s*(?:${integer}|${string})\s*;`, "g"))].map((match) => Number(match[1] ?? match[2]));
  if (found.length === 0 || found.some((value) => value !== found[0] || !Number.isSafeInteger(value))) return null;
  return found[0];
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

export const TOOL_NAMES_READER = "GitPulse devmap tools (vs devmap-serve TOOL_NAMES)";
const UPSTREAM_TOOLS_FILE = "devmap-serve/src/mcp.rs";
const GITPULSE_TOOLS_FILE = path.join("src-tauri", "src", "mcp", "devmap_parity.rs");

/**
 * The quoted strings in the `[...]` that follows `declaration`, or null when
 * the declaration is absent or its list is empty or unterminated. Comments are
 * blanked first, so a commented-out entry is not a tool.
 *
 * @param {string} text
 * @param {RegExp} declaration matches up to and including the opening `[`
 * @param {RegExp} entry captures one name per match
 * @returns {string[] | null}
 */
function rustListNames(text, declaration, entry) {
  const code = withoutComments(text);
  const open = declaration.exec(code);
  if (open === null) return null;
  const start = open.index + open[0].length;
  const end = code.indexOf("]", start);
  if (end === -1) return null;
  const names = [...code.slice(start, end).matchAll(entry)].map((m) => m[1]);
  return names.length > 0 ? names : null;
}

/**
 * The tool names devmap-serve publishes, from its `TOOLS` table of
 * `("devmap_x", "cli_name")` pairs (`TOOL_NAMES` is built from it).
 *
 * @param {string} source
 * @returns {string[] | null}
 */
export function upstreamToolNames(source) {
  return rustListNames(source, /\bconst\s+TOOLS\b[^=]*=\s*&\[/, /\(\s*"([^"]+)"/g);
}

/**
 * The tool names GitPulse serves, from `TOOL_NAMES` in devmap_parity.rs.
 *
 * @param {string} source
 * @returns {string[] | null}
 */
export function gitpulseToolNames(source) {
  return rustListNames(source, /\bconst\s+TOOL_NAMES\b[^=]*=\s*&\[/, /"([^"]+)"/g);
}

/**
 * Is every tool GitPulse serves one devmap-serve publishes? A subset, not
 * equality: GitPulse serves 11 of upstream's tools by design. Order is not
 * compared: a `tools/list` client looks tools up by name and `tools/call`
 * dispatches by name, so order carries no behavior.
 *
 * A side that could not be read is `unverifiable`, never a match: a check that
 * could not run must not read as one that passed.
 *
 * @param {string[] | null} gitpulse
 * @param {{ names: string[] } | { error: string }} upstream
 * @returns {Row}
 */
export function toolNamesRow(gitpulse, upstream) {
  const reader = TOOL_NAMES_READER;
  if (gitpulse === null) return { reader, verdict: "unverifiable", detail: `no tool list in ${GITPULSE_TOOLS_FILE}` };
  if ("error" in upstream) return { reader, verdict: "unverifiable", detail: `upstream tool list unreadable: ${upstream.error}` };
  const published = new Set(upstream.names);
  const missing = gitpulse.filter((name) => !published.has(name));
  if (missing.length > 0) {
    return { reader, verdict: "stale", detail: `${missing.join(", ")} not in devmap-serve TOOL_NAMES; GitPulse serves a tool upstream does not` };
  }
  return { reader, verdict: "match", detail: `${gitpulse.length} of upstream's ${upstream.names.length} tools` };
}

/**
 * The tool-list row for a DevCouncil checkout: GitPulse's names against
 * devmap-serve's at `rev`, read from DevCouncil's object store.
 *
 * @param {string} devcouncil
 * @param {string | null} rev the DevCouncil commit GitPulse vendored
 * @param {typeof spawnSync} [run]
 * @returns {Row}
 */
export function toolNamesRowAt(devcouncil, rev, run = spawnSync) {
  /** @type {string | null} */
  let gitpulse = null;
  try {
    gitpulse = readFileSync(path.join(REPO_ROOT, GITPULSE_TOOLS_FILE), "utf8");
  } catch {
    // reported by toolNamesRow as an unreadable GitPulse list
  }
  const ours = gitpulse === null ? null : gitpulseToolNames(gitpulse);
  if (rev === null) return toolNamesRow(ours, { error: "VENDOR.json names no DevCouncil commit for devmap-query" });
  const shown = run("git", ["show", `${rev}:${OWNER_PREFIX}/${UPSTREAM_TOOLS_FILE}`], { cwd: devcouncil, encoding: "utf8", timeout: 15_000 });
  if (shown.error || shown.status !== 0) {
    return toolNamesRow(ours, { error: `git show ${rev}:${OWNER_PREFIX}/${UPSTREAM_TOOLS_FILE} failed: ${(shown.error?.message || String(shown.stderr)).trim()}` });
  }
  const names = upstreamToolNames(String(shown.stdout));
  return toolNamesRow(ours, names === null ? { error: `no TOOLS table in ${UPSTREAM_TOOLS_FILE} at ${rev.slice(0, 8)}` } : { names });
}

/**
 * The DevCouncil commit GitPulse vendored devmap-query at: devmap-serve is not
 * vendored, and devmap-query is the crate its tools call into.
 *
 * @returns {string | null}
 */
export function vendoredDevmapRev() {
  try {
    const manifest = JSON.parse(readFileSync(path.join(REPO_ROOT, VENDORED_PREFIX, "VENDOR.json"), "utf8"));
    const commit = manifest?.crates?.find((/** @type {{ name?: string }} */ c) => c?.name === "devmap-query")?.origin?.commit;
    return typeof commit === "string" && /^[0-9a-f]{40}$/.test(commit) ? commit : null;
  } catch {
    return null;
  }
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
  // A reading that is not a safe integer (NaN, a float) did not say; it is
  // never compared, so it cannot read as stale or as a match.
  const reported = (/** @type {SchemaKey} */ key) => Number.isSafeInteger(schemas[key]);
  const missing = reading.keys.filter((key) => !reported(key));
  const unreported = missing.length > 0 ? `does not report ${missing.map((key) => SCHEMAS[key].label).join(", ")}` : "";
  // A known mismatch outranks an unknown: a reader that is provably behind on
  // one schema is stale whatever it failed to say about another.
  const differs = reading.keys.filter((key) => reported(key) && schemas[key] !== owner[key]);
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
  if (probe.extractSchema !== null) schemas.extract = probe.extractSchema;
  // The manifest names no lease format yet, so this row is unverifiable for it
  // until gitpulse-mcp's manifest reports `STACK_FORMAT_VERSION`.
  return { reader, keys: ALL_KEYS, schemas, note: "the gitpulse-mcp manifest does not name its stack lease format" };
}

/**
 * `devmap` on PATH. Its `--version` line names the store schema, and names the
 * extraction schema and the lease format only once DevCouncil prints them
 * (`extract schema N`, `stack lease format N`); a build that does not is
 * unverifiable for them, never assumed to match.
 *
 * @param {ReturnType<typeof readCliSchema> | null} [cli]
 * @returns {Reading}
 */
export function devmapReading(cli = resolveOnPath("devmap") === null ? null : readCliSchema()) {
  const reader = "devmap (PATH)";
  /** @type {SchemaKey[]} */
  const keys = ["store", "extract", "lease"];
  if (cli === null) return { reader, keys, absent: "not on PATH" };
  if (!cli.available) return { reader, keys, error: cli.reason };
  /** @type {Schemas} */
  const schemas = { store: cli.expected };
  if (typeof cli.extract === "number") schemas.extract = cli.extract;
  if (typeof cli.lease === "number") schemas.lease = cli.lease;
  const unprinted = [
    ...(schemas.extract === undefined ? ["the extraction schema (`extract schema N`)"] : []),
    ...(schemas.lease === undefined ? ["the stack lease format (`stack lease format N`)"] : []),
  ];
  return {
    reader,
    keys,
    schemas,
    ...(unprinted.length > 0 ? { note: `devmap --version does not print ${unprinted.join(" or ")}; DevCouncil must add it` } : {}),
  };
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
  rows.push(toolNamesRowAt(devcouncil, vendoredDevmapRev()));
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
  return failOnGitBound(() => report(args));
}

/** @param {{ app: string, timeoutMs: number, json: boolean }} args */
async function report(args) {
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
