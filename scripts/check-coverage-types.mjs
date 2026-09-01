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
export const TERMINAL_STRUCTS = Object.freeze(["TerminalRunResult"]);

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
  const prevEnd = Math.max(
    stripped.lastIndexOf("}", declIndex),
    stripped.lastIndexOf(";", declIndex),
    0,
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
 * Normalize the Rust types used by the checked payloads to their JSON wire
 * equivalents. This is deliberately small and explicit: an unfamiliar type
 * is kept as a named token so a refactor cannot silently become `unknown`.
 *
 * @param {string} type
 * @returns {string}
 */
function normalizeRustType(type) {
  const compact = type.replace(/\s+/g, "");
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
  return compact;
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
 * @returns {{ fields: Map<string, { type: string, optional: boolean }>, unparseable: string[] }}
 *          wire-name -> normalized wire type/presence; unparseable segments verbatim
 */
function parseRustFields(body) {
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
    let wireName = fieldDecl[1];
    let skipped = false;
    let optional = false;
    for (const attr of attrs) {
      const options = serdeOptions(attr);
      if (!options) continue;
      if (options.has("skip")) skipped = true;
      if (options.has("default") || options.has("skip_serializing_if")) optional = true;
      if (options.has("rename") && typeof options.get("rename") === "string") {
        wireName = /** @type {string} */ (options.get("rename"));
      }
    }
    if (!skipped) {
      const type = rest.trim().slice(rest.trim().indexOf(":") + 1).trim();
      fields.set(wireName, { type: normalizeRustType(type), optional });
    }
  }
  return { fields, unparseable };
}

/**
 * Locate each checked struct's declaration and body in the Rust source.
 *
 * @param {string} rustSource raw coverage.rs contents
 * @returns {{
 *   structs: Map<string, { fields: Map<string, { type: string, optional: boolean }>, line: number }>,
 *   unparseable: Array<{ struct: string, segment: string }>,
 *   violations: string[],
 * }}
 */
export function parseRustStructs(rustSource, checked = CHECKED_STRUCTS) {
  const stripped = stripComments(rustSource);
  const structs = new Map();
  /** @type {Array<{ struct: string, segment: string }>} */
  const unparseable = [];
  /** @type {string[]} */
  const violations = [];

  for (const name of checked) {
    const declRe = new RegExp(`pub struct ${name}\\b`);
    const decl = declRe.exec(stripped);
    if (!decl) {
      violations.push(`rust: struct ${name} not found (renamed, removed, or made private?)`);
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
    const serdeAttr = /#\[\s*serde\s*\([\s\S]*?\)\s*\]/.exec(attrs);
    if (serdeAttr) {
      const options = serdeOptions(serdeAttr[0]);
      const renameAll = options?.get("rename_all");
      if (renameAll !== undefined && renameAll !== "snake_case") {
        violations.push(
          `rust: struct ${name} carries #[serde(rename_all = "${renameAll}")] — wire names cannot be verified statically; resolve manually`,
        );
        continue;
      }
    }

    try {
      const parsed = parseRustFields(balancedBody(stripped, openBrace));
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
    try {
      const body = balancedBody(stripped, openBrace);
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
    violations.push(
      `drift: ${drift.struct}.${drift.field} type mismatch (Rust ${drift.rustType}; TS ${drift.tsType})`,
    );
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
    `  rust structs checked    : ${result.rustCount}  (${rustLabel})`,
    `  ts interfaces checked   : ${result.tsCount}  (${tsLabel})`,
    `  fields compared         : ${result.fieldCount}`,
    `  drifted structs         : ${result.driftCount}`,
    `  drifted field types      : ${result.typeDriftCount}`,
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
 * @param {string[]} argv
 */
function parseArgs(argv) {
  const opts = { rustPath: DEFAULT_RUST_SOURCE, tsPath: DEFAULT_TS_SOURCE };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--rust") opts.rustPath = path.resolve(argv[++i]);
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
  /** @type {{ rustPath: string, tsPath: string }} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (err) {
    console.error(`check-coverage-types: ${/** @type {Error} */ (err).message}`);
    return 2;
  }

  // An explicit --rust/--ts pair checks only the coverage contract on those
  // scratch copies (how the tests simulate drift). With no flags, every
  // contract is checked.
  const explicit = opts.rustPath !== DEFAULT_RUST_SOURCE || opts.tsPath !== DEFAULT_TS_SOURCE;
  const contracts = explicit
    ? [{ title: "Coverage IPC type check (Rust structs <-> TS interfaces)", ...opts, structs: CHECKED_STRUCTS }]
    : [
        {
          title: "Coverage IPC type check (Rust structs <-> TS interfaces)",
          rustPath: DEFAULT_RUST_SOURCE,
          tsPath: DEFAULT_TS_SOURCE,
          structs: CHECKED_STRUCTS,
        },
        {
          title: "Terminal IPC type check (Rust structs <-> TS interfaces)",
          rustPath: TERMINAL_RUST_SOURCE,
          tsPath: TERMINAL_TS_SOURCE,
          structs: TERMINAL_STRUCTS,
        },
      ];

  let code = 0;
  for (const contract of contracts) {
    /** @type {ReturnType<typeof runTypeCheck>} */
    let result;
    try {
      result = runTypeCheck(contract);
    } catch (err) {
      console.error(`check-coverage-types: internal error: ${/** @type {Error} */ (err).message}`);
      return 2;
    }
    console.log(
      formatReport(
        result,
        path.relative(REPO_ROOT, contract.rustPath),
        path.relative(REPO_ROOT, contract.tsPath),
        contract.title,
      ),
    );
    if (!result.ok) code = 1;
  }
  return code;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
