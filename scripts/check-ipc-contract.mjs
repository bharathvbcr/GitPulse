#!/usr/bin/env node
/**
 * Bidirectional IPC contract checker for the Tauri `cmd_*` seam.
 *
 * Parses the Rust command registry (`generate_handler![...]` in
 * src-tauri/src/lib.rs) and every frontend `invoke()` call site under src/,
 * then fails on either direction of drift:
 *
 *   (a) a TS/Svelte invoke of a command the backend never registered
 *       (guaranteed runtime crash the moment that code path runs), or
 *   (b) a registered handler with zero production callers (silent dead API
 *       surface), unless it is explicitly listed in ORPHAN_ALLOWLIST, or
 *   (c) an invoke whose command name cannot be statically resolved (dynamic
 *       call site): reported as manual-review and failed loudly instead of
 *       silently passing an unverified seam.
 *
 * Exit codes: 0 contract holds · 1 contract violation · 2 internal error.
 *
 * Flags (all optional, used by the tests to simulate drift):
 *   --lib <path>       alternate lib.rs to parse
 *   --src <dir>        alternate frontend root to scan
 *   --extra-dir <dir>  additional directory scanned for invoke call sites
 *                      (repeatable)
 */
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const DEFAULT_LIB_RS = path.join(REPO_ROOT, "src-tauri", "src", "lib.rs");
export const DEFAULT_SRC_DIR = path.join(REPO_ROOT, "src");

/**
 * Registered handlers intentionally kept Rust-only today. Every entry needs a
 * justification; any NEW orphan still fails the check, so this list cannot
 * quietly grow. Stale entries (handler removed from the registry) also fail,
 * keeping the list honest.
 */
export const ORPHAN_ALLOWLIST = Object.freeze({
  // CommitDetails renders the file list from cmd_get_commit_details' payload;
  // this dedicated endpoint is unused surplus until a view needs it alone.
  cmd_get_commit_files: "superseded by cmd_get_commit_details payload; kept for API parity",
  // Word-diff engine exposed for future parity/perf work.
  cmd_compute_word_diff: "frontend computes word diffs client-side; Rust engine reserved",
  // Language LOC endpoint without a UI surface yet.
  cmd_count_loc: "utility endpoint; no LOC display wired yet",
  // The commit-type filter parses conventional prefixes locally
  // (lib/filter/parseQuery.ts CONVENTIONAL_TYPES); parser endpoint unused.
  cmd_parse_conventional_commit: "frontend parses conventional types locally",
  // The canvas renderer solves connector geometry internally; endpoint kept
  // in case layout moves back to Rust.
  cmd_get_bezier_connector: "canvas draws connectors internally; geometry endpoint reserved",
  // Tag management actions: tags are listed (cmd_list_tags) but create/delete
  // have no UI affordance yet.
  cmd_create_tag: "tag creation backend-complete; UI pending",
  cmd_delete_tag: "tag deletion backend-complete; UI pending",
  // Direct policy verdicts flow through runMutating()'s gate; this explicit
  // per-command check is reserved for harness diagnostics surfaces.
  cmd_policy_check_command: "policy gate reached via store mutations; direct endpoint reserved",
});

// "coverage" is deliberately absent: src/lib/coverage holds production invoke
// sites, while vitest's coverage/ output lives at the repo root, which is
// never a scan root.
const SKIP_DIRS = new Set(["node_modules", "dist", ".git", ".svelte-kit", "target"]);
// Tests mock invoke rather than exercise the real seam; production reachability
// is the invariant this checker enforces, so test files are not callers.
const NON_PRODUCTION_FILE = /(^|[\\/])__tests__([\\/])|\.(test|spec)\.[cm]?[jt]s$|\.d\.ts$/i;
const FRONTEND_EXT = new Set([".ts", ".svelte"]);
const HANDLER_ENTRY = /^(?:[A-Za-z_]\w*::)*[A-Za-z_]\w*$/;
/**
 * Callee names that count as IPC invocation. Anchored and exact: an unanchored
 * substring match would let helpers like `safeInvoke` or `reinvoke` fabricate
 * contract entries, masking real orphans (false pass) or inventing phantom
 * missing commands (false fail). A new wrapper name must be added here
 * explicitly — fail-closed, like every other list in this checker.
 */
const INVOKE_CALLEE = /^(?:invoke|invokeFn)$/;

/**
 * Extract the token list inside `generate_handler![ ... ]`.
 *
 * @param {string} source
 * @returns {string} comma-separated raw entries
 */
export function extractHandlerList(source) {
  const marker = "generate_handler![";
  const start = source.indexOf(marker);
  if (start === -1) {
    throw new Error("could not find `generate_handler![` in lib.rs");
  }
  let depth = 0;
  for (let i = start + marker.length - 1; i < source.length; i += 1) {
    const ch = source[i];
    if (ch === "[") depth += 1;
    if (ch === "]") {
      depth -= 1;
      if (depth === 0) return source.slice(start + marker.length, i);
    }
  }
  throw new Error("`generate_handler![...]` is never closed in lib.rs");
}

/**
 * Parse registered command ids. Entries may be bare (`cmd_x`) or qualified
 * (`module::cmd_x`); the command id is the last path segment.
 *
 * @param {string} libRsSource
 * @returns {{ handlers: Map<string, { raw: string, line: number }>, errors: string[] }}
 */
export function parseRegisteredHandlers(libRsSource) {
  const block = extractHandlerList(libRsSource);
  const prefix = libRsSource.slice(0, libRsSource.indexOf(block));
  const baseLine = prefix.split("\n").length;
  const handlers = new Map();
  const errors = [];
  for (const rawEntry of block.split(",")) {
    const raw = rawEntry.replace(/\/\/.*$/gm, "").trim();
    if (raw === "") continue;
    if (!HANDLER_ENTRY.test(raw)) {
      errors.push(`unparseable generate_handler entry: ${JSON.stringify(raw.trim())}`);
      continue;
    }
    const name = raw.split("::").pop();
    if (!handlers.has(name)) {
      handlers.set(name, { raw, line: baseLine + rawEntry.split("\n").length - 1 });
    }
  }
  return { handlers, errors };
}

/**
 * Recursively collect production .ts/.svelte files under each root.
 *
 * @param {string[]} roots
 * @returns {string[]}
 */
export function collectFrontendFiles(roots) {
  /** @type {string[]} */
  const files = [];
  /**
   * @param {string} dir
   */
  const visit = (dir) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return; // unreadable/missing optional dir: nothing to scan
    }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!SKIP_DIRS.has(entry.name)) visit(full);
        continue;
      }
      if (!entry.isFile()) continue;
      if (!FRONTEND_EXT.has(path.extname(entry.name))) continue;
      if (NON_PRODUCTION_FILE.test(entry.name)) continue;
      files.push(full);
    }
  };
  for (const root of roots) visit(root);
  return files.sort();
}

/**
 * @param {string} content
 * @param {number} index
 */
function lineNumber(content, index) {
  return content.slice(0, index).split("\n").length;
}

/** @param {string} name */
function escapeRegex(name) {
  return name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

const IDENT_CHAR = /[A-Za-z0-9_$]/;

/**
 * Returns the index just past the string / template literal / comment that
 * starts at `start`. Their contents are opaque to generic detection: text
 * like `"a<b"` must never open a span that swallows unrelated code.
 *
 * @param {string} content
 * @param {number} start
 * @returns {number}
 */
function skipOpaque(content, start) {
  const n = content.length;
  const opener = content[start];
  if (opener === "/" && content[start + 1] === "/") {
    const nl = content.indexOf("\n", start);
    return nl === -1 ? n : nl; // the newline itself stays
  }
  if (opener === "/" && content[start + 1] === "*") {
    const end = content.indexOf("*/", start + 2);
    return end === -1 ? n : end + 2;
  }
  let i = start + 1;
  while (i < n) {
    const ch = content[i];
    if (ch === "\\") {
      i += 2;
      continue;
    }
    if (ch === opener) return i + 1;
    if (opener === "`" && ch === "$" && content[i + 1] === "{") {
      // Template interpolation: ride out the balanced braces.
      let depth = 0;
      i += 1;
      while (i < n) {
        if (content[i] === "{") depth += 1;
        else if (content[i] === "}") {
          depth -= 1;
          if (depth === 0) break;
        }
        i += 1;
      }
    }
    i += 1;
  }
  return i;
}

/**
 * Blanks every balanced generic parameter list attached to an identifier
 * (`invoke<Guarded<string>>` → `invoke` + spaces), leaving all other
 * characters — and every character offset — exactly where they were, so line
 * numbers computed from match indexes stay correct.
 *
 * Depth-counted rather than nesting-budgeted: the earlier one-level regex
 * made `invoke<Guarded<string[]>>` visible but three levels invisible, so
 * real call sites silently vanished and their handlers read as orphaned. A
 * `<` only opens a span when it directly hugs identifier characters, which
 * leaves comparison operators (`a < b`) alone; string literals and comments
 * are opaque, and an unterminated span is left untouched. Spans may cross
 * newlines (multi-line generics are real call sites); the newline characters
 * themselves survive blanking so line numbering holds.
 *
 * @param {string} content
 * @returns {string}
 */
export function blankGenericParameters(content) {
  const chars = content.split("");
  const n = content.length;
  let i = 0;
  while (i < n) {
    const ch = content[i];
    if (ch === '"' || ch === "'" || ch === "`" || (ch === "/" && (content[i + 1] === "/" || content[i + 1] === "*"))) {
      i = skipOpaque(content, i);
      continue;
    }
    if (!IDENT_CHAR.test(ch)) {
      i += 1;
      continue;
    }
    while (i < n && IDENT_CHAR.test(content[i])) i += 1;
    // A generic opens only immediately after an identifier.
    if (content[i] !== "<") continue;
    let depth = 0;
    let close = -1;
    let j = i;
    while (j < n) {
      const c = content[j];
      if (c === '"' || c === "'" || c === "`") {
        // String-literal types inside a generic stay opaque.
        j = skipOpaque(content, j);
        continue;
      }
      if (c === "<") depth += 1;
      else if (c === ">") {
        depth -= 1;
        if (depth === 0) {
          close = j;
          break;
        }
      }
      j += 1;
    }
    // Unterminated: not a generic; leave the text untouched.
    if (close === -1) continue;
    for (let k = i; k <= close; k += 1) {
      if (chars[k] !== "\n") chars[k] = " ";
    }
    i = close + 1;
  }
  return chars.join("");
}

/**
 * Scan frontend files for invoke-style call sites.
 *
 * Recognized shapes: `invoke("x")`, `invoke<T>("x")`, aliased direct calls
 * whose identifier contains "invoke" (e.g. the stores' `invokeFn`), plus
 * single-identifier arguments (`invoke(NAME)`), which are resolved against
 * same-file `const/let/var NAME = "literal"` declarations. Anything else
 * (interpolated templates, cross-module indirection) becomes a manual-review
 * item instead of a guess.
 *
 * @param {string[]} files
 * @returns {{
 *   invoked: Map<string, Array<{ file: string, line: number }>>,
 *   manualReviews: Array<{ file: string, line: number, detail: string }>,
 *   siteCount: number,
 * }}
 */
export function scanInvokeCallSites(files) {
  /** @type {Map<string, Array<{ file: string, line: number }>>} */
  /** @type {Map<string, Array<{ file: string, line: number }>>} */
  const invoked = new Map();
  /** @type {Array<{ file: string, line: number, detail: string }>} */
  const manualReviews = [];
  let siteCount = 0;
  /**
   * @param {string} name
   * @param {{ file: string, line: number }} site
   */
  const record = (name, site) => {
    const sites = invoked.get(name);
    if (sites) sites.push(site);
    else invoked.set(name, [site]);
    siteCount += 1;
  };
  // Literal first argument: 'x' / "x" / `x` (backticks rejected if interpolated).
  // Generic parameter lists are stripped from each line before matching, so
  // `invoke<Guarded<string>>("x")` matches the same shape as `invoke("x")`
  // at any nesting depth.
  const literalCall = /\b([A-Za-z_$][\w$]*)\s*\(\s*(['"`])([^'"`\n]*)\2/g;
  // Identifier first argument: possibly-dynamic command name.
  const dynamicCall = /\b([A-Za-z_$][\w$]*)\s*\(\s*([A-Za-z_$][\w$]*)\s*[,)\n]/g;

  for (const file of files) {
    const content = readFileSync(file, "utf8");
    const relFile = path.relative(REPO_ROOT, file);
    // Offset-preserving, so line numbers derived from match indexes remain
    // exact against the original file.
    const callable = blankGenericParameters(content);

    for (const match of callable.matchAll(literalCall)) {
      const [, callee, , arg] = match;
      if (!INVOKE_CALLEE.test(callee)) continue;
      const line = lineNumber(callable, match.index);
      if (arg.includes("${")) {
        manualReviews.push({
          file: relFile,
          line,
          detail: `interpolated template passed to ${callee}()`,
        });
        continue;
      }
      if (!invoked.has(arg)) invoked.set(arg, []);
      record(arg, { file: relFile, line });
    }

    for (const match of callable.matchAll(dynamicCall)) {
      const [, callee, argIdent] = match;
      if (!INVOKE_CALLEE.test(callee)) continue;
      const declRe = new RegExp(
        `(?:^|[^\\w$.])(?:const|let|var)\\s+${escapeRegex(argIdent)}\\s*(?::[^=;\\n]+)?=\\s*(['"\`])([^'"\\\`\n]+)\\1`,
      );
      const decl = declRe.exec(callable);
      const line = lineNumber(callable, match.index);
      if (decl && !decl[2].includes("${")) {
        record(decl[2], { file: relFile, line });
      } else {
        manualReviews.push({
          file: relFile,
          line,
          detail: `dynamic command name ${argIdent} could not be resolved locally`,
        });
      }
    }
  }
  return { invoked, manualReviews, siteCount };
}

/**
 * Compare both sides of the seam.
 *
 * @param {Map<string, { raw: string, line: number }>} registered
 * @param {Map<string, Array<{ file: string, line: number }>>} invoked
 * @returns {{ missing: string[], orphans: string[], allowedOrphans: string[], staleAllowlist: string[] }}
 */
export function compareContract(registered, invoked) {
  const missing = [...invoked.keys()].filter((name) => !registered.has(name)).sort();
  const orphans = [];
  const allowedOrphans = [];
  for (const name of registered.keys()) {
    if (invoked.has(name)) continue;
    if (Object.hasOwn(ORPHAN_ALLOWLIST, name)) allowedOrphans.push(name);
    else orphans.push(name);
  }
  const staleAllowlist = Object.keys(ORPHAN_ALLOWLIST)
    .filter((name) => !registered.has(name))
    .sort();
  return {
    missing: missing.sort(),
    orphans: orphans.sort(),
    allowedOrphans: allowedOrphans.sort(),
    staleAllowlist,
  };
}

/**
 * @param {{
 *   libPath: string,
 *   srcDirs: string[],
 * }} input
 */
export function runContractCheck({ libPath, srcDirs }) {
  const libSource = readFileSync(libPath, "utf8");
  const { handlers: registered, errors } = parseRegisteredHandlers(libSource);
  const files = collectFrontendFiles(srcDirs);
  const { invoked, manualReviews, siteCount } = scanInvokeCallSites(files);
  const { missing, orphans, allowedOrphans, staleAllowlist } = compareContract(
    registered,
    invoked,
  );

  /** @type {string[]} */
  const violations = [];
  for (const error of errors) violations.push(`registry: ${error}`);
  for (const name of missing) {
    const sites = invoked.get(name) ?? [];
    violations.push(
      `missing: "${name}" is invoked at ${sites.map((s) => `${s.file}:${s.line}`).join(", ")} but never registered`,
    );
  }
  for (const name of orphans) {
    const site = registered.get(name);
    violations.push(
      `orphan: "${name}" is registered (${path.relative(REPO_ROOT, libPath)}:${site?.line}) but no frontend caller exists`,
    );
  }
  for (const name of staleAllowlist) {
    violations.push(`allowlist: "${name}" is listed in ORPHAN_ALLOWLIST but not registered`);
  }
  for (const review of manualReviews) {
    violations.push(`manual-review needed: ${review.file}:${review.line} ${review.detail}`);
  }

  return {
    registeredCount: registered.size,
    invokedCount: invoked.size,
    siteCount,
    productionFiles: files.length,
    missing,
    orphans,
    allowedOrphans,
    staleAllowlist,
    manualReviews,
    violations,
    ok: violations.length === 0,
  };
}

/**
 * @param {ReturnType<typeof runContractCheck>} result
 * @param {string} libLabel
 */
export function formatReport(result, libLabel) {
  const lines = [
    "IPC contract check (Rust registry <-> frontend invoke)",
    "",
    `  registered handlers     : ${result.registeredCount}  (${libLabel})`,
    `  invoked commands        : ${result.invokedCount}  (${result.siteCount} call sites across ${result.productionFiles} production files)`,
    `  orphaned handlers       : ${result.orphans.length}  (registered, never invoked, NOT allowlisted)`,
    `  missing commands        : ${result.missing.length}  (invoked, never registered)`,
    `  allowed Rust-only       : ${result.allowedOrphans.length}  (see ORPHAN_ALLOWLIST justifications)`,
    `  manual-review sites     : ${result.manualReviews.length}  (dynamic call sites needing human eyes)`,
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
  detail("missing commands", result.missing);
  detail("orphaned handlers", result.orphans);
  detail("manual-review sites", result.manualReviews.map((r) => `${r.file}:${r.line} ${r.detail}`));
  lines.push("", result.ok ? "OK: IPC contract holds." : "FAIL: IPC contract violated.");
  return lines.join("\n");
}

/**
 * @param {string[]} argv
 */
function parseArgs(argv) {
  const opts = /** @type {{libPath: string, srcDirs: string[], extraDirs: string[]}} */ ({
    libPath: DEFAULT_LIB_RS,
    srcDirs: [DEFAULT_SRC_DIR],
    extraDirs: [],
  });
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    // Same contract as check-release-version.mjs: a value-taking flag without
    // one is a clean usage error, not a raw Node TypeError.
    /** @param {string} flag */
    const next = (flag) => {
      const value = argv[++i];
      if (value === undefined) throw new Error(`${flag} requires a value`);
      return value;
    };
    if (arg === "--lib") opts.libPath = path.resolve(next(arg));
    else if (arg === "--src") opts.srcDirs = [path.resolve(next(arg))];
    else if (arg === "--extra-dir") opts.extraDirs.push(path.resolve(next(arg)));
    else throw new Error(`unknown argument: ${arg}`);
  }
  return { ...opts, srcDirs: [...opts.srcDirs, ...opts.extraDirs] };
}

/**
 * @param {string[]} [argv]
 */
export function main(argv = process.argv.slice(2)) {
  /** @type {{libPath: string, srcDirs: string[]}} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (err) {
    console.error(`check-ipc-contract: ${/** @type {Error} */ (err).message}`);
    return 2;
  }
  /** @type {ReturnType<typeof runContractCheck>} */
  let result;
  try {
    result = runContractCheck(opts);
  } catch (err) {
    console.error(`check-ipc-contract: internal error: ${/** @type {Error} */ (err).message}`);
    return 2;
  }
  console.log(formatReport(result, path.relative(REPO_ROOT, opts.libPath)));
  if (!result.ok) {
    return 1;
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
