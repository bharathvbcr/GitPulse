#!/usr/bin/env node
/**
 * Bidirectional type-drift checker for the coverage IPC payload structs.
 *
 * The IPC contract checker (check-ipc-contract.mjs) only verifies command
 * NAMES; struct fields can drift silently until a runtime crash or a silently
 * undefined render. This parses the Rust serde payload structs in
 * src-tauri/src/analyzer/coverage.rs and their TS twins in
 * src/lib/coverage/types.ts, then fails on either direction of drift:
 *
 *   (a) a Rust field with no TS property (TS reads `undefined` for it), or
 *   (b) a TS property with no Rust field (Rust never sends it).
 *   (c) a shared field whose normalized wire type or backend-required
 *       presence no longer agrees.
 *
 * SCOPE: see CONTRACTS below for exactly what is checked — 35 contracts over
 * 57 structs, spanning both wire surfaces: command returns and event payloads.
 * Enums are still skipped here and covered separately, by
 * scripts/enum-variant-contract.test.ts. That is most, not all, of the named types crossing the IPC
 * boundary: the ones still missing declare their TypeScript interface inside a
 * component rather than a module, so there is no single file to point this at.
 * "Type contract holds" means the listed contracts hold. The remaining gap is
 * named in the CONTRACTS docstring rather than left for a reader to assume
 * away; PullRequestInfo is pinned separately in
 * scripts/pr-timing-contract.test.ts for the same reason.
 *
 * Serde awareness: per-field `#[serde(rename = "x")]` changes the wire name
 * and is honored; `#[serde(skip)]` drops the field from the wire; a checked
 * struct carrying `rename_all` is refused loudly unless it is "snake_case"
 * (identity for these snake_case idents). Enums are not part of this contract
 * and are skipped entirely — including rename_all-carrying enums like
 * CoverageFormat.
 *
 * Exit codes: 0 contract holds · 1 contract violation · 2 internal error.
 *
 * Flags (all optional, used by the tests to simulate drift on scratch copies):
 *   --rust <path>  alternate coverage.rs to parse
 *   --ts <path>    alternate types.ts to parse
 */
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { alignRows } from "./columns.mjs";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const DEFAULT_RUST_SOURCE = path.join(REPO_ROOT, "src-tauri", "src", "analyzer", "coverage.rs");
export const DEFAULT_TS_SOURCE = path.join(REPO_ROOT, "src", "lib", "coverage", "types.ts");

export const TERMINAL_RUST_SOURCE = path.join(REPO_ROOT, "src-tauri", "src", "terminal", "mod.rs");
export const TERMINAL_TS_SOURCE = path.join(REPO_ROOT, "src", "lib", "terminal", "runResult.ts");

/**
 * `TerminalRunResult` is the payload every command-running panel reads, and it
 * had no gate at all: it was declared three separate times in TypeScript
 * (twice named, once inlined anonymously at an `invoke` call), so a Rust field
 * rename would surface as a silently `undefined` property in whichever panels
 * had not been updated. It is now declared once and checked here.
 */
export const TERMINAL_STRUCTS = Object.freeze([
  "TerminalRunResult",
  "TerminalSpawned",
  "TerminalOutputPayload",
  "TerminalExitPayload",
]);

/**
 * The Rust structs whose serialized shape the frontend depends on, and the
 * TS interfaces that must mirror them field-for-field.
 */
export const CHECKED_STRUCTS = Object.freeze([
  "CoverageTotals",
  "CoverageFamilyStatus",
  "CoverageArtifact",
  "FileCoverageSummary",
  "CoveredLine",
  "FileCoverage",
  "CoverageLanguageSplit",
  "CoverageScanLimit",
  "CoverageReport",
]);

/**
 * Every Rust serde struct that crosses IPC and has a TypeScript twin, paired
 * with the module each side lives in.
 *
 * The two original entries (coverage, terminal) were the two contracts that
 * had already drifted. The rest were added once the checker stopped reporting
 * false drift on module-qualified type paths, `skip_serializing_if`-omitted
 * options, and `rename_all = "camelCase"` — limitations that, not coincidence,
 * were exactly what the uncovered types happened to use.
 *
 * A struct with no entry here is unchecked. That is a real gap, not an
 * implicit assertion of safety, and it is enumerated with a reason for each
 * type in scripts/ipc-type-coverage-contract.test.ts — so a newly added IPC
 * payload cannot join the unchecked set without someone saying so.
 *
 * Every type that was declared inside a component, or mirrored there under a
 * different name, has been moved into a module and listed here. What remains
 * unchecked is only what no caller consumes: payloads returned by the commands
 * in check-ipc-contract's ORPHAN_ALLOWLIST, which have no TypeScript type to
 * compare against because nothing invokes them.
 */
/** @param {...string} parts */
const rust = (...parts) => path.join(REPO_ROOT, "src-tauri", "src", ...parts);
/** @param {...string} parts */
const ts = (...parts) => path.join(REPO_ROOT, "src", "lib", ...parts);

export const CONTRACTS = Object.freeze([
  { label: "coverage", rustPath: DEFAULT_RUST_SOURCE, tsPath: DEFAULT_TS_SOURCE, structs: CHECKED_STRUCTS },
  { label: "terminal", rustPath: TERMINAL_RUST_SOURCE, tsPath: TERMINAL_TS_SOURCE, structs: TERMINAL_STRUCTS },
  { label: "ai", rustPath: rust("ai", "mod.rs"), tsPath: ts("stores", "harnessStore.ts"), structs: ["AiGeneration", "AiStatus"] },
  { label: "harness", rustPath: rust("harness", "mod.rs"), tsPath: ts("stores", "harnessStore.ts"), structs: ["HarnessStatus"] },
  { label: "policy", rustPath: rust("harness", "policy.rs"), tsPath: ts("stores", "harnessStore.ts"), structs: ["PolicyVerdict"] },
  { label: "ledger", rustPath: rust("ledger", "mod.rs"), tsPath: ts("ledger", "types.ts"), structs: ["LedgerEvent", "LedgerStatus", "LedgerAppended"] },
  { label: "ingest", rustPath: rust("ingest", "mod.rs"), tsPath: ts("ingest", "types.ts"), structs: ["CatchUp"] },
  { label: "grants", rustPath: rust("grants", "mod.rs"), tsPath: ts("grants", "types.ts"), structs: ["Grant", "Grantor", "GrantScope", "GrantView"] },
  { label: "local-scan", rustPath: rust("harness", "protocol.rs"), tsPath: ts("ai", "scan.ts"), structs: ["ScanModel", "ScanServer", "ScanResult"] },
  { label: "tasks", rustPath: rust("tasks", "mod.rs"), tsPath: ts("tasks", "types.ts"), structs: ["TaskScope", "TaskLease", "TaskView"] },
  { label: "ops", rustPath: rust("ops.rs"), tsPath: ts("ops", "model.ts"), structs: ["BranchCleanupPlan", "CommitReviewReport"] },
  { label: "release", rustPath: rust("commands", "mod.rs"), tsPath: ts("ops", "model.ts"), structs: ["ReleasePublishResult"] },
  // The envelope on every gated command: `policy` travels with `output` so the
  // UI can tell an approved action from one that ran with no gate available.
  // Renaming either field would have broken 33 commands at once, silently.
  { label: "guarded", rustPath: rust("commands", "mod.rs"), tsPath: ts("stores", "harnessStore.ts"), structs: ["Guarded"] },
  { label: "branches", rustPath: rust("engine", "git_reader.rs"), tsPath: ts("branches", "types.ts"), structs: ["BranchInfo", "TagInfo"] },
  { label: "commits", rustPath: rust("engine", "git_reader.rs"), tsPath: ts("stores", "graphStore.ts"), structs: ["CommitDetails", "CommitFileChange"] },
  { label: "graph", rustPath: rust("commands", "mod.rs"), tsPath: ts("stores", "graphStore.ts"), structs: ["CommitGraphPayload"] },
  { label: "refs", rustPath: rust("graph", "refs.rs"), tsPath: ts("stores", "graphStore.ts"), structs: ["RefDecoration"] },
  { label: "stack", rustPath: rust("stack", "stack_tree.rs"), tsPath: ts("stack", "types.ts"), structs: ["StackHierarchyPayload", "StackedBranchNode", "BranchAncestryChain"] },
  { label: "status", rustPath: rust("engine", "git_reader.rs"), tsPath: ts("stores", "repoStore.ts"), structs: ["FileStatus", "BranchStatsReport"] },
  { label: "file-content", rustPath: rust("engine", "git_reader.rs"), tsPath: ts("files", "types.ts"), structs: ["BlameLine", "FileBlob"] },
  { label: "language-detect", rustPath: rust("analyzer", "language.rs"), tsPath: ts("files", "types.ts"), structs: ["LanguageInfo"] },
  { label: "reflog", rustPath: rust("engine", "git_reader.rs"), tsPath: ts("branches", "types.ts"), structs: ["ReflogEntry"] },
  { label: "worktrees", rustPath: rust("engine", "worktree.rs"), tsPath: ts("branches", "types.ts"), structs: ["WorktreeInfo"] },
  { label: "languages", rustPath: rust("engine", "git_reader.rs"), tsPath: ts("language", "barStats.ts"), structs: ["LanguageStatsReport", "RepoLanguageStat"] },
  { label: "repo", rustPath: rust("engine", "git_cli.rs"), tsPath: ts("stores", "repoStore.ts"), structs: ["ResolvedRepo"] },
  { label: "ci-local", rustPath: rust("ci_local.rs"), tsPath: ts("github", "types.ts"), structs: ["CiLocalReport"] },
  { label: "workflows", rustPath: rust("github", "actions.rs"), tsPath: ts("github", "types.ts"), structs: ["WorkflowsReport"] },
  { label: "github", rustPath: rust("github", "mod.rs"), tsPath: ts("github", "types.ts"), structs: ["GitHubContext", "PullRequestInfo"] },
  { label: "dependabot", rustPath: rust("github", "mod.rs"), tsPath: ts("health", "types.ts"), structs: ["DependabotReport"] },
  { label: "deps", rustPath: rust("analyzer", "deps.rs"), tsPath: ts("health", "types.ts"), structs: ["DepsHealthReport"] },
  { label: "word-diff", rustPath: rust("diff", "word_diff.rs"), tsPath: ts("diff", "wordDiff.ts"), structs: ["IntraLineDiff"] },
  { label: "conflict", rustPath: rust("diff", "conflict.rs"), tsPath: ts("diff", "conflict.ts"), structs: ["ConflictDocument", "ConflictChunk"] },
  { label: "storage", rustPath: rust("storage", "mod.rs"), tsPath: ts("storage", "types.ts"), structs: ["StorageReport"] },
  { label: "updates", rustPath: rust("updates", "mod.rs"), tsPath: ts("updates", "updateCheck.ts"), structs: ["UpdateCheck"] },
  // The repository-surface payloads. Each has a hand-written TypeScript mirror
  // the UI branches on, so they are checked rather than excused: a renamed
  // Rust field would otherwise surface as a silently `undefined` property in
  // the operation banner, the stash list, or the remotes panel.
  { label: "repo-operation", rustPath: rust("engine", "repo_op.rs"), tsPath: ts("repos", "operation.ts"), structs: ["RepoOperation"] },
  { label: "stash", rustPath: rust("engine", "stash.rs"), tsPath: ts("repos", "stash.ts"), structs: ["StashEntry"] },
  { label: "remotes", rustPath: rust("engine", "remotes.rs"), tsPath: ts("repos", "remotes.ts"), structs: ["RemoteInfo"] },
  { label: "submodules", rustPath: rust("engine", "submodules.rs"), tsPath: ts("repos", "submodules.ts"), structs: ["SubmoduleInfo"] },
  // Events are a second wire surface: emitted payloads, not command returns.
  { label: "repo-events", rustPath: rust("watcher", "mod.rs"), tsPath: ts("repos", "events.ts"), structs: ["RepoChangedPayload"] },
  { label: "native-events", rustPath: rust("desktop", "mod.rs"), tsPath: ts("desktop", "nativeActions.ts"), structs: ["NativeEvent"] },
  { label: "codeintel", rustPath: rust("codeintel", "mod.rs"), tsPath: ts("codeintel", "types.ts"), structs: ["CodeintelSymbolHit", "CodeintelEdge", "CodeintelDeadSymbol", "CodeintelResponse", "CodeintelStatus"] },
]);

/**
 * Strip // and block comments so they cannot masquerade as code tokens.
 * @param {string} source
 */
function stripComments(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/[^\n]*/g, "");
}

/** Line/column-free index helper for diagnostics.
 * @param {string} source
 * @param {number} index
 */
function lineOf(source, index) {
  return source.slice(0, index).split("\n").length;
}

/**
 * Return the body between the first balanced `{...}` at/after `from`, plus
 * the declaration text just before it (attributes included via caller).
 *
 * @param {string} source comment-stripped source
 * @param {number} openIndex index of the opening `{`
 */
function balancedBody(source, openIndex) {
  let depth = 0;
  for (let i = openIndex; i < source.length; i += 1) {
    const ch = source[i];
    if (ch === "{") depth += 1;
    if (ch === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(openIndex + 1, i);
    }
  }
  throw new Error("unbalanced braces");
}

/**
 * Attributes immediately preceding an item (back to the previous item end).
 * @param {string} stripped comment-stripped source
 * @param {number} declIndex index of `pub struct` / `pub enum` keyword start
 */
function precedingAttributes(stripped, declIndex) {
  // -1, not 0: the floor is "nothing precedes it", and slicing from 0 + 1
  // ate the leading `#` of an attribute on the first item in a file. That
  // dropped the attribute silently — including a rename_all this checker is
  // supposed to refuse loudly, which would have reported an unverifiable
  // struct as verified.
  const prevEnd = Math.max(
    stripped.lastIndexOf("}", declIndex),
    stripped.lastIndexOf(";", declIndex),
    -1,
  );
  return stripped.slice(prevEnd + 1, declIndex);
}

/**
 * Parse one `#[serde(...)]` attribute string into its options.
 * @param {string} attr e.g. `#[serde(rename_all = "snake_case")]`
 */
function serdeOptions(attr) {
  const inner = /#\[\s*serde\s*\(([\s\S]*)\)\s*\]/.exec(attr);
  if (!inner) return null;
  /** @type {Map<string, string>} */
  const options = new Map();
  for (const pair of inner[1].split(",")) {
    const eq = pair.indexOf("=");
    if (eq === -1) {
      options.set(pair.trim(), "");
    } else {
      options.set(pair.slice(0, eq).trim(), pair.slice(eq + 1).trim().replace(/^['"]|['"]$/g, ""));
    }
  }
  return options;
}

/**
 * Apply serde's `rename_all` to a snake_case field ident.
 *
 * Only the rules this repo actually uses are implemented; anything else is
 * still refused loudly by the caller rather than guessed at. This mirrors
 * serde's own algorithm (segments capitalized and joined, then the first
 * character lowered for camelCase) rather than approximating it with a regex,
 * so `a__b` maps the way serde maps it and not one character differently.
 *
 * @param {string} ident
 * @param {string | undefined} rule
 */
export function applyRenameAll(ident, rule) {
  if (rule === undefined || rule === "snake_case") return ident;
  if (rule === "camelCase") {
    const pascal = ident
      .split("_")
      .map((part) => (part === "" ? "" : part[0].toUpperCase() + part.slice(1)))
      .join("");
    return pascal === "" ? "" : pascal[0].toLowerCase() + pascal.slice(1);
  }
  throw new Error(`unsupported rename_all rule: ${rule}`);
}

/** rename_all rules this checker can resolve statically. */
export const SUPPORTED_RENAME_ALL = Object.freeze(["snake_case", "camelCase"]);

/**
 * Normalize the Rust types used by the checked payloads to their JSON wire
 * equivalents. This is deliberately small and explicit: an unfamiliar type
 * is kept as a named token so a refactor cannot silently become `unknown`.
 *
 * @param {string} type
 * @returns {string}
 */
function normalizeRustType(type) {
  // Drop reference lifetimes before collapsing whitespace, or `&'static str`
  // survives as the token `&'staticstr` and never matches `&str`.
  const compact = type.replace(/&\s*'\w+\s+/g, "&").replace(/\s+/g, "");
  const option = /^Option<(.*)>$/.exec(compact);
  if (option) return [normalizeRustType(option[1]), "null"].sort().join("|");
  const vector = /^Vec<(.*)>$/.exec(compact);
  if (vector) return `${normalizeRustType(vector[1])}[]`;
  const array = /^\[(.*);\d+\]$/.exec(compact);
  if (array) return `${normalizeRustType(array[1])}[]`;
  if (compact === "String" || compact === "&str" || compact === "str") return "string";
  if (compact === "bool") return "boolean";
  if (/^(?:u|i)(?:8|16|32|64|128|size)$/.test(compact) || /^(?:f32|f64)$/.test(compact)) {
    return "number";
  }
  // `crate::graph::RefDecoration` and `RefDecoration` are one wire type; serde
  // never writes a struct's path, and TypeScript has no notion of one. Keeping
  // the qualifier made every cross-module field read as drift, which is why
  // this checker only ever covered types whose fields stayed in one module.
  return stripTypePath(compact);
}

/**
 * Drop leading `foo::bar::` module qualifiers from a named type.
 * @param {string} type
 */
function stripTypePath(type) {
  return type.replace(/^(?:[A-Za-z_]\w*::)+/, "");
}

/**
 * Split a type expression at top-level separators only.
 * @param {string} type
 * @param {string} separator
 * @returns {string[]}
 */
function splitTopLevel(type, separator) {
  const parts = [];
  let depth = 0;
  let current = "";
  for (const ch of type) {
    if ("<[{(".includes(ch)) depth += 1;
    if (">]})".includes(ch)) depth -= 1;
    if (ch === separator && depth === 0) {
      parts.push(current);
      current = "";
    } else {
      current += ch;
    }
  }
  parts.push(current);
  return parts;
}

/**
 * Normalize the TypeScript types used by the checked interfaces to the same
 * compact wire vocabulary as the Rust side.
 *
 * @param {string} type
 * @returns {string}
 */
function normalizeTsType(type) {
  const compact = type.replace(/\s+/g, "");
  const unionParts = splitTopLevel(compact, "|");
  if (unionParts.length > 1) {
    const unions = unionParts.map((part) => normalizeTsType(part));
    return [...new Set(unions)].sort().join("|");
  }
  const array = /^(.*)\[\]$/.exec(compact);
  if (array) return `${normalizeTsType(array[1])}[]`;
  const genericArray = /^Array<(.*)>$/.exec(compact);
  if (genericArray) return `${normalizeTsType(genericArray[1])}[]`;
  if (compact === "string" || compact === "boolean" || compact === "number" || compact === "null" || compact === "undefined") {
    return compact;
  }
  return compact;
}

/**
 * Extract field entries from a Rust struct body: top-level comma segments,
 * each optionally led by attributes, shaped `pub name: Type`.
 *
 * @param {string} body comment-stripped struct body
 * @param {string | undefined} [renameAll] struct-level serde rename_all rule
 * @returns {{ fields: Map<string, { type: string, optional: boolean }>, unparseable: string[] }}
 *          wire-name -> normalized wire type/presence; unparseable segments verbatim
 */
function parseRustFields(body, renameAll) {
  /** @type {string[]} */
  const segments = [];
  let depth = 0;
  let current = "";
  for (const ch of body) {
    if (ch === "<" || ch === "(" || ch === "[") depth += 1;
    if (ch === ">" || ch === ")" || ch === "]") depth -= 1;
    if (ch === "," && depth === 0) {
      segments.push(current);
      current = "";
    } else {
      current += ch;
    }
  }
  if (current.trim() !== "") segments.push(current);

  /** @type {Map<string, { type: string, optional: boolean }>} */
  const fields = new Map();
  const unparseable = [];
  for (const rawSegment of segments) {
    const segment = rawSegment.trim();
    if (segment === "") continue;
    /** @type {string[]} */
    const attrs = [];
    let rest = segment;
    for (;;) {
      const attr = /^#\[[^\]]*\]\s*/.exec(rest);
      if (!attr) break;
      attrs.push(attr[0]);
      rest = rest.slice(attr[0].length);
    }
    const fieldDecl = /^(?:pub(?:\([^)]*\))?\s+)?(?:r#)?([A-Za-z_]\w*)\s*:/.exec(rest.trim());
    if (!fieldDecl) {
      unparseable.push(segment.replace(/\s+/g, " ").slice(0, 80));
      continue;
    }
    // rename_all first; a per-field `rename` overrides it below, literally.
    let wireName = applyRenameAll(fieldDecl[1], renameAll);
    let skipped = false;
    let optional = false;
    /** @type {string | undefined} */
    let skipIf;
    for (const attr of attrs) {
      const options = serdeOptions(attr);
      if (!options) continue;
      if (options.has("skip")) skipped = true;
      if (options.has("skip_serializing_if")) skipIf = options.get("skip_serializing_if");
      // `skip_serializing_if` alone decides wire presence. `#[serde(default)]`
      // is a *deserialization* attribute — it supplies a value when a field is
      // missing from input, and never stops serde from writing the field out.
      // Treating it as wire-optional demanded that TypeScript mark fields
      // optional that Rust always sends, which is drift the checker invented.
      if (options.has("skip_serializing_if")) optional = true;
      if (options.has("rename") && typeof options.get("rename") === "string") {
        wireName = /** @type {string} */ (options.get("rename"));
      }
    }
    if (!skipped) {
      const type = rest.trim().slice(rest.trim().indexOf(":") + 1).trim();
      // `Option<T>` is normally nullable on the wire, but paired with
      // `skip_serializing_if = "Option::is_none"` the key is omitted instead —
      // absent-or-T, never null. That is exactly TypeScript's `p?: T`, so
      // normalizing it to `null|T` would report the idiomatic serde spelling
      // of an optional field as drift.
      const omitsNone = skipIf === "Option::is_none";
      const inner = /^Option<([\s\S]*)>$/.exec(type.replace(/\s+/g, ""));
      fields.set(wireName, {
        type: normalizeRustType(omitsNone && inner ? inner[1] : type),
        optional,
      });
    }
  }
  return { fields, unparseable };
}

/**
 * Locate each checked struct's declaration and body in the Rust source.
 *
 * @param {string} rustSource raw coverage.rs contents
 * @param {readonly string[]} [checked] struct names to parse
 * @param {{ requirePub?: boolean }} [options] `requirePub: false` reads structs
 *        that never cross IPC on their own, such as the `gh` JSON DTOs
 * @returns {{
 *   structs: Map<string, { fields: Map<string, { type: string, optional: boolean }>, line: number }>,
 *   unparseable: Array<{ struct: string, segment: string }>,
 *   violations: string[],
 * }}
 */
export function parseRustStructs(rustSource, checked = CHECKED_STRUCTS, { requirePub = true } = {}) {
  const stripped = stripComments(rustSource);
  const structs = new Map();
  /** @type {Array<{ struct: string, segment: string }>} */
  const unparseable = [];
  /** @type {string[]} */
  const violations = [];

  for (const name of checked) {
    // IPC payloads must be `pub` to be returned by a command, so a private one
    // is a real finding. Callers reading structs that never cross IPC on their
    // own — the `gh` JSON DTOs, say — opt out of that requirement.
    const declRe = new RegExp(`${requirePub ? "pub " : "(?:pub )?"}struct ${name}\\b`);
    const decl = declRe.exec(stripped);
    if (!decl) {
      violations.push(`rust: struct ${name} not found (renamed, removed${requirePub ? ", or made private" : ""}?)`);
      continue;
    }
    const openBrace = stripped.indexOf("{", decl.index);
    if (openBrace === -1) {
      violations.push(`rust: struct ${name} has no brace body`);
      continue;
    }

    // Struct-level serde renames change every wire name; only identity-safe
    // snake_case passes. Anything else must be resolved by hand, loudly.
    const attrs = precedingAttributes(stripped, decl.index);
    /** @type {string | undefined} */
    let renameAll;
    const serdeAttr = /#\[\s*serde\s*\([\s\S]*?\)\s*\]/.exec(attrs);
    if (serdeAttr) {
      const options = serdeOptions(serdeAttr[0]);
      const rule = options?.get("rename_all");
      if (rule !== undefined && !SUPPORTED_RENAME_ALL.includes(rule)) {
        violations.push(
          `rust: struct ${name} carries #[serde(rename_all = "${rule}")] — only ${SUPPORTED_RENAME_ALL.join(", ")} can be resolved statically; resolve manually`,
        );
        continue;
      }
      renameAll = rule;
    }

    try {
      const parsed = parseRustFields(balancedBody(stripped, openBrace), renameAll);
      for (const segment of parsed.unparseable) {
        unparseable.push({ struct: name, segment });
      }
      structs.set(name, { fields: parsed.fields, line: lineOf(rustSource, decl.index) });
    } catch (err) {
      violations.push(`rust: could not parse struct ${name}: ${/** @type {Error} */ (err).message}`);
    }
  }
  return { structs, unparseable, violations };
}

/**
 * Parse one interface body into wire-name -> type/presence.
 *
 * @param {string} body comment-stripped interface body
 * @param {string} name interface name, for diagnostics
 * @param {Array<{ interface: string, segment: string }>} unparseable sink
 */
function parseInterfaceBody(body, name, unparseable) {
  /** @type {Map<string, { type: string, optional: boolean }>} */
  const props = new Map();
  let depth = 0;
  let current = "";
  const flush = () => {
    const segment = current.trim();
    current = "";
    if (segment === "") return;
    const prop = /^(?:readonly\s+)?(\w+)\s*(\?)?:\s*([\s\S]+)$/.exec(segment);
    if (!prop) {
      unparseable.push({ interface: name, segment: segment.replace(/\s+/g, " ").slice(0, 80) });
      return;
    }
    props.set(prop[1], { type: normalizeTsType(prop[3]), optional: prop[2] === "?" });
  };
  for (const ch of body) {
    if (ch === "{" || ch === "(" || ch === "[") depth += 1;
    if (ch === "}" || ch === ")" || ch === "]") depth -= 1;
    if ((ch === ";" || ch === "\n") && depth === 0) flush();
    else current += ch;
  }
  flush();
  return props;
}

/**
 * Parse `export interface Name { ... }` bodies from the TS source.
 *
 * @param {string} tsSource raw types.ts contents
 * @returns {{
 *   interfaces: Map<string, { props: Map<string, { type: string, optional: boolean }>, line: number }>,
 *   unparseable: Array<{ interface: string, segment: string }>,
 *   violations: string[],
 * }}
 */
export function parseTsInterfaces(tsSource, checked = CHECKED_STRUCTS) {
  const stripped = stripComments(tsSource);
  const interfaces = new Map();
  /** @type {Array<{ interface: string, segment: string }>} */
  const unparseable = [];
  /** @type {string[]} */
  const violations = [];

  for (const name of checked) {
    const declRe = new RegExp(`export interface ${name}\\b`);
    const decl = declRe.exec(stripped);
    if (!decl) {
      violations.push(`ts: interface ${name} not found (renamed, removed, or not exported?)`);
      continue;
    }
    const openBrace = stripped.indexOf("{", decl.index);
    if (openBrace === -1) {
      violations.push(`ts: interface ${name} has no brace body`);
      continue;
    }
    // `interface A extends B` splits one wire shape across two declarations.
    // Inherited props are resolved from this same file; a base declared
    // elsewhere is reported rather than skipped, because skipping it would
    // surface later as a pile of "Rust sends a field TS lacks" violations
    // whose real cause is one unreadable base.
    const heritage = stripped.slice(decl.index, openBrace);
    const bases = /\bextends\b([^{]*)/.exec(heritage);
    /** @type {Map<string, { type: string, optional: boolean }>} */
    const inherited = new Map();
    if (bases) {
      for (const raw of bases[1].split(",")) {
        const base = raw.trim();
        if (base === "") continue;
        const baseDecl = new RegExp(`(?:export\\s+)?interface ${base}\\b`).exec(stripped);
        const baseBrace = baseDecl ? stripped.indexOf("{", baseDecl.index) : -1;
        if (!baseDecl || baseBrace === -1) {
          violations.push(
            `ts: interface ${name} extends ${base}, which is not declared in this file — the wire shape cannot be assembled`,
          );
          continue;
        }
        for (const [key, value] of parseInterfaceBody(
          balancedBody(stripped, baseBrace),
          base,
          unparseable,
        )) {
          inherited.set(key, value);
        }
      }
    }
    try {
      const body = balancedBody(stripped, openBrace);
      // Own props last: an interface narrowing an inherited one wins, exactly
      // as TypeScript resolves it.
      const props = new Map([
        ...inherited,
        ...parseInterfaceBody(body, name, unparseable),
      ]);
      interfaces.set(name, { props, line: lineOf(tsSource, decl.index) });
    } catch (err) {
      violations.push(`ts: could not parse interface ${name}: ${/** @type {Error} */ (err).message}`);
    }
  }
  return { interfaces, unparseable, violations };
}

/**
 * Field-for-field diff of both sides.
 *
 * @param {ReturnType<typeof parseRustStructs>} rust
 * @param {ReturnType<typeof parseTsInterfaces>} ts
 */
export function compareTypes(rust, ts, checked = CHECKED_STRUCTS) {
  /** @type {Array<{ struct: string, rustOnly: string[], tsOnly: string[] }>} */
  const drifts = [];
  /** @type {Array<{ struct: string, field: string, rustType: string, tsType: string, rustOptional: boolean, tsOptional: boolean }>} */
  const typeDrifts = [];
  let fieldCount = 0;
  for (const name of checked) {
    const rustStruct = rust.structs.get(name);
    const tsIface = ts.interfaces.get(name);
    if (!rustStruct || !tsIface) continue;
    fieldCount += rustStruct.fields.size;
    const rustOnly = [...rustStruct.fields.keys()].filter((f) => !tsIface.props.has(f)).sort();
    const tsOnly = [...tsIface.props.keys()].filter((f) => !rustStruct.fields.has(f)).sort();
    if (rustOnly.length > 0 || tsOnly.length > 0) drifts.push({ struct: name, rustOnly, tsOnly });
    for (const field of rustStruct.fields.keys()) {
      const tsField = tsIface.props.get(field);
      if (!tsField) continue;
      const rustField = rustStruct.fields.get(field);
      if (!rustField) continue;
      // A TS optional property is a safe compatibility widening for a Rust
      // field that is always serialized (Option<T> is nullable, not absent).
      // The dangerous direction is Rust being able to omit a field while TS
      // requires it; that would surface as undefined at runtime.
      if (rustField.type !== tsField.type || (rustField.optional && !tsField.optional)) {
        typeDrifts.push({
          struct: name,
          field,
          rustType: rustField.type,
          tsType: tsField.type,
          rustOptional: rustField.optional,
          tsOptional: tsField.optional,
        });
      }
    }
  }
  return { drifts, typeDrifts, fieldCount };
}

/**
 * @param {{ rustPath: string, tsPath: string, structs?: readonly string[] }} input
 */
export function runTypeCheck({ rustPath, tsPath, structs = CHECKED_STRUCTS }) {
  const rust = parseRustStructs(readFileSync(rustPath, "utf8"), structs);
  const ts = parseTsInterfaces(readFileSync(tsPath, "utf8"), structs);
  const { drifts, typeDrifts, fieldCount } = compareTypes(rust, ts, structs);

  /** @type {string[]} */
  const violations = [...rust.violations, ...ts.violations];
  for (const { struct, segment } of [...rust.unparseable.map((u) => ({ struct: u.struct, segment: u.segment }))]) {
    violations.push(`rust: unparseable entry in ${struct}: ${segment}`);
  }
  for (const { interface: iface, segment } of ts.unparseable) {
    violations.push(`ts: unparseable property in ${iface}: ${segment}`);
  }
  for (const drift of drifts) {
    for (const field of drift.rustOnly) {
      violations.push(`drift: ${drift.struct}.${field} exists in Rust but has no TS property (TS reads undefined)`);
    }
    for (const field of drift.tsOnly) {
      violations.push(`drift: ${drift.struct}.${field} exists in TS but Rust never sends it (renamed or deleted backend-side?)`);
    }
  }
  for (const drift of typeDrifts) {
    // Only when the types really differ: a presence-only mismatch used to
    // print "type mismatch (Rust ReleaseInfo[]; TS ReleaseInfo[])", naming two
    // identical types and hiding the actual problem in the next line.
    if (drift.rustType !== drift.tsType) {
      violations.push(
        `drift: ${drift.struct}.${drift.field} type mismatch (Rust ${drift.rustType}; TS ${drift.tsType})`,
      );
    }
    if (drift.rustOptional && !drift.tsOptional) {
      violations.push(
        `drift: ${drift.struct}.${drift.field} may be absent from Rust but is required in TS`,
      );
    }
  }

  return {
    rustCount: rust.structs.size,
    tsCount: ts.interfaces.size,
    fieldCount,
    driftCount: drifts.length,
    typeDriftCount: typeDrifts.length,
    typeDrifts,
    drifts,
    violations,
    ok: violations.length === 0,
  };
}

/**
 * @param {ReturnType<typeof runTypeCheck>} result
 * @param {string} rustLabel
 * @param {string} tsLabel
 */
export function formatReport(result, rustLabel, tsLabel, title = "Coverage IPC type check (Rust structs <-> TS interfaces)") {
  const lines = [
    title,
    "",
    // A3's aligner, same as check-ipc-contract: widths come from the labels,
    // so the "drifted field types" row can no longer be a space out of line.
    ...alignRows([
      { label: "rust structs checked", value: String(result.rustCount), note: rustLabel },
      { label: "ts interfaces checked", value: String(result.tsCount), note: tsLabel },
      { label: "fields compared", value: String(result.fieldCount) },
      { label: "drifted structs", value: String(result.driftCount) },
      { label: "drifted field types", value: String(result.typeDriftCount) },
    ]),
  ];
  /**
   * @param {string} title
   * @param {string[]} items
   */
  const detail = (title, items) => {
    if (items.length === 0) return;
    lines.push("", `  ${title}:`);
    for (const item of items) lines.push(`    - ${item}`);
  };
  detail("violations", result.violations);
  lines.push("", result.ok ? "OK: type contract holds." : "FAIL: type contract violated.");
  return lines.join("\n");
}

/**
 * Human title for a contract label, used only when a contract fails and its
 * full report is printed.
 * @param {string} label
 */
function titleFor(label) {
  return `${label.charAt(0).toUpperCase()}${label.slice(1)} IPC type check`;
}

/**
 * @param {string[]} argv
 */
function parseArgs(argv) {
  const opts = { rustPath: DEFAULT_RUST_SOURCE, tsPath: DEFAULT_TS_SOURCE, json: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--json") opts.json = true;
    else if (arg === "--rust") opts.rustPath = path.resolve(argv[++i]);
    else if (arg === "--ts") opts.tsPath = path.resolve(argv[++i]);
    else throw new Error(`unknown argument: ${arg}`);
  }
  return opts;
}

/**
 * @param {string[]} [argv]
 */

/** Backlog A2: asking for help is not an error, so this exits 0. */
export function usage() {
  return formatUsage({
    name: "check-coverage-types",
    summary: "Verify Rust serde structs and TypeScript interfaces agree field-for-field and wire-type-for-wire-type.",
    flags: [
      { flag: "--rust <path>".replace(/^"|"$/g, ""), description: "Rust source to read the serde structs from" },
      { flag: "--ts <path>".replace(/^"|"$/g, ""), description: "TypeScript source to read the interfaces from" },
      { flag: "--help, -h".replace(/^"|"$/g, ""), description: "print this message and exit 0" }
    ],
    exits: "0 the contract holds · 1 drift was found · 2 the check could not run",
  });
}

export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  /** @type {{ rustPath: string, tsPath: string, json: boolean }} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (err) {
    console.error(`check-coverage-types: ${/** @type {Error} */ (err).message}`);
    return 2;
  }

  // An explicit --rust/--ts pair checks only the coverage contract on those
  // scratch copies (how the tests simulate drift), and keeps the single
  // detailed report. With no flags every contract in CONTRACTS runs.
  const explicit = opts.rustPath !== DEFAULT_RUST_SOURCE || opts.tsPath !== DEFAULT_TS_SOURCE;
  const contracts = explicit
    ? [{ label: "coverage", ...opts, structs: CHECKED_STRUCTS }]
    : CONTRACTS;

  let code = 0;
  /** @type {unknown[]} */
  const collected = [];
  /** @type {Array<{ label: string, value: string, note: string }>} */
  const rows = [];
  /** @type {string[]} */
  const failures = [];
  const totals = { structs: 0, fields: 0, drifted: 0, typeDrifted: 0 };

  for (const contract of contracts) {
    const title = `${titleFor(contract.label)} (Rust structs <-> TS interfaces)`;
    /** @type {ReturnType<typeof runTypeCheck>} */
    let result;
    try {
      result = runTypeCheck(contract);
    } catch (err) {
      console.error(`check-coverage-types: internal error: ${/** @type {Error} */ (err).message}`);
      return 2;
    }
    totals.structs += result.rustCount;
    totals.fields += result.fieldCount;
    totals.drifted += result.driftCount;
    totals.typeDrifted += result.typeDriftCount;
    if (opts.json) collected.push({ title, label: contract.label, ...result });
    rows.push({
      label: contract.label,
      value: `${result.rustCount} struct${result.rustCount === 1 ? "" : "s"}, ${result.fieldCount} fields`,
      note: result.ok ? "ok" : "DRIFT",
    });
    // A passing contract needs no detail; a failing one needs all of it, so
    // the noise appears exactly where someone has to act on it.
    if (!result.ok) {
      failures.push(
        formatReport(
          result,
          path.relative(REPO_ROOT, contract.rustPath),
          path.relative(REPO_ROOT, contract.tsPath),
          title,
        ),
      );
      code = 1;
    }
  }

  if (!opts.json) {
    if (explicit) {
      // Single-contract mode keeps the original detailed report either way,
      // so the drift-simulation tests read the same output they always have.
      const only = contracts[0];
      console.log(
        failures[0] ??
          formatReport(
            runTypeCheck(only),
            path.relative(REPO_ROOT, only.rustPath),
            path.relative(REPO_ROOT, only.tsPath),
            `${titleFor(only.label)} (Rust structs <-> TS interfaces)`,
          ),
      );
    } else {
      for (const failure of failures) console.log(failure, "\n");
      console.log(
        [
          "IPC wire type check (Rust serde structs <-> TS interfaces)",
          "",
          ...alignRows([
            { label: "contracts checked", value: String(contracts.length) },
            { label: "structs checked", value: String(totals.structs) },
            { label: "fields compared", value: String(totals.fields) },
            { label: "drifted structs", value: String(totals.drifted) },
            { label: "drifted field types", value: String(totals.typeDrifted) },
          ]),
          "",
          ...alignRows(rows),
          "",
          code === 0 ? "OK: type contract holds." : "FAIL: type contract violated.",
        ].join("\n"),
      );
    }
  }
  if (opts.json) console.log(JSON.stringify(collected, null, 2));
  return code;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
