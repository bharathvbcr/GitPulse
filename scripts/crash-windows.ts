import { withoutComments } from "./check-schema-parity.mjs";

/**
 * The crash-window table's checking logic, as pure functions over strings.
 *
 * `scripts/crash-windows.json` lists the crash windows of three write paths,
 * each tied to its injection point and to the test that proves recovery. This
 * module is what lets a test fail when that list and the Rust source drift.
 * Nothing here reads a file: `crash-windows-contract.test.ts` loads the real
 * sources, and `crash-windows.test.ts` feeds in small invented ones.
 *
 * Comments are removed with `withoutComments` from `check-schema-parity.mjs`,
 * the repository's one comment-aware Rust scanner (nested block comments, doc
 * comments, raw strings and char literals). It keeps string contents, so an env
 * hook can be read; `mask` then blanks the contents of strings where only
 * structure matters, so a brace or a semicolon inside an attribute string is
 * never read as code.
 *
 * What counts as an injection hook. The guard recognises exactly these shapes
 * in the scanned files, and says so in its summary:
 *
 *   env   a whole string literal `"GITPULSE_TEST_<NAME>"`. It is the spelling
 *         of every test-only environment switch (`GITPULSE_TEST_KILL_AFTER_
 *         RECORDED`); a name quoted inside a longer string, as in an
 *         `#[ignore = "..."]` message, is not a hook.
 *   fn    a function whose name is `inject_*`, `*_for_test`, contains
 *         `crash_point` or `failpoint`/`fail_point`, or ends in `_with`,
 *         `_with_seam` or `_with_seams` while taking a `dyn Fn`/`FnMut`/`FnOnce`
 *         parameter (a closure seam). A `#[test]` function is never a hook.
 *
 * A hook of any other shape (a planted SQL trigger, a poisoned row, state a
 * test builds by hand, a `static` flag, a `thread_local` hook such as
 * `before_publish_hook`, a test-only counter such as `LEDGER_SIBLING_OPENS`) is
 * invisible to this scan. Rows whose injection is `state` or `data` say so, and
 * the guard checks only their tests.
 *
 * What the guard reads of a test. It reads one file at a time: the attributes on
 * the `fn`, on every `mod` block that encloses it, and a `#![cfg(...)]` at the
 * top of the file, plus `#[ignore]` and `#[cfg_attr(..., ignore)]`. It cannot
 * see a `cfg` on the `mod x;` line in the file that declares an out-of-line
 * test module. For the files this table cites, that declaration was read by
 * hand (ledger/mod.rs, ledger/bindings.rs, ingest/mod.rs, engine/mod.rs and
 * engine/object_quarantine.rs: all plain `#[cfg(test)]`); a new cited file whose
 * parent declaration is gated needs the same look, and the cfg it finds goes in
 * the row.
 */

export type HookKind = "env" | "fn";

export interface Hook {
  kind: HookKind;
  name: string;
  file: string;
}

export interface TestRef {
  file: string;
  name: string;
  /** Every non-trivial `cfg(...)` predicate on the test, whitespace removed. */
  cfg?: string[];
  /** The test carries `#[ignore]`. */
  ignored?: boolean;
}

export interface Row {
  id: string;
  path: string;
  /**
   * Which engine of the path the row belongs to, where a path has more than
   * one. Worktree merge has two: the pipeline behind the MCP tool and the
   * teardown engine behind the panel and the wave. They share removal code but
   * not their sequence, so coverage of one never stands for the other.
   */
  engine?: string;
  boundary: { durableBefore: string; notYetWritten: string };
  injection: { kind: "hook" | "state" | "data" | "none"; detail: string; hooks: Hook[] };
  tests: TestRef[];
  /** Tests that touch the window without pinning it. Checked to exist, never counted as coverage. */
  nearestTests?: TestRef[];
  recovery: string;
  status: "covered" | "uncovered";
  reason?: string;
}

export interface Table {
  scope: { dirs: string[]; files: string[] };
  paths: string[];
  /** Hooks that match the patterns but are not crash windows, each with the reason. */
  ignoredHooks: Array<Hook & { reason: string }>;
  rows: Row[];
}

export interface Report {
  errors: string[];
  covered: number;
  uncovered: number;
  byPath: Record<string, { covered: number; uncovered: number }>;
  /** Keyed `<path>/<engine>`, for the rows that name an engine. */
  byEngine: Record<string, { covered: number; uncovered: number }>;
  scopeFiles: string[];
  hooksFound: Hook[];
}

export interface TestFacts {
  /** How many `fn <name>` declarations the file holds (0: not found, more than 1: ambiguous). */
  count: number;
  isTest: boolean;
  ignored: boolean;
  /** Non-trivial cfg predicates on the function, its enclosing modules and the file. */
  cfgs: string[];
}

export const HOOK_PATTERNS =
  'env: a whole string literal "GITPULSE_TEST_*"; fn: inject_*, *_for_test, *crash_point*, *failpoint*, ' +
  "or a *_with / *_with_seam(s) function taking a dyn Fn parameter";

const STATUSES = ["covered", "uncovered"];
const ENV_LITERAL = /"(GITPULSE_TEST_[A-Z0-9_]+)"/g;
const FN_DECL = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)/g;
const FN_NAME_HOOKS = [/^inject_/, /_for_test$/, /crash_point/, /fail_?point/];
const SEAM_NAME = /(_with|_with_seams?)$/;
const DYN_FN = /\bdyn\s+Fn(?:Mut|Once)?\s*\(/;

/** `code` with the contents of every string and char literal blanked, length kept. */
function mask(code: string): string {
  const out = code.split("");
  const blank = (from: number, to: number) => {
    for (let i = from; i < to; i += 1) if (out[i] !== "\n") out[i] = " ";
  };
  let i = 0;
  while (i < code.length) {
    const rest = code.slice(i, i + 40);
    const raw = /^r(#*)"/.exec(rest);
    if (raw && !/[A-Za-z0-9_]/.test(code[i - 1] ?? "")) {
      const open = i + raw[0].length;
      const close = code.indexOf(`"${raw[1]}`, open);
      const end = close === -1 ? code.length : close;
      blank(open, end);
      i = end === code.length ? end : end + 1 + raw[1].length;
    } else if (code[i] === '"') {
      let end = i + 1;
      while (end < code.length && code[end] !== '"') end += code[end] === "\\" ? 2 : 1;
      blank(i + 1, Math.min(end, code.length));
      i = end + 1;
    } else if (code[i] === "'" && /^'(?:\\.[^']*|[^\\'])'/.test(rest.slice(0, 12))) {
      const literal = /^'(?:\\.[^']*|[^\\'])'/.exec(rest.slice(0, 12))?.[0] ?? "'";
      blank(i + 1, i + literal.length - 1);
      i += literal.length;
    } else {
      i += 1;
    }
  }
  return out.join("");
}

interface Source {
  code: string;
  masked: string;
}

function prepare(source: string): Source {
  const code = withoutComments(source);
  return { code, masked: mask(code) };
}

/** Matching close of the bracket opened at `open`, or -1. Brackets in strings are already blanked. */
function closeOf(masked: string, open: number): number {
  const pair: Record<string, string> = { "(": ")", "[": "]", "{": "}", "<": ">" };
  const opener = masked[open];
  const closer = pair[opener];
  let depth = 0;
  for (let i = open; i < masked.length; i += 1) {
    if (masked[i] === opener) depth += 1;
    else if (masked[i] === closer) {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

interface Attribute {
  name: string;
  body: string;
}

/** The outer attributes `#[...]` lying in `code[from, to)`. */
function attributesIn(src: Source, from: number, to: number): Attribute[] {
  const found: Attribute[] = [];
  for (let i = from; i < to; i += 1) {
    if (src.masked[i] !== "#" || src.masked[i + 1] !== "[") continue;
    const close = closeOf(src.masked, i + 1);
    if (close === -1 || close >= to) break;
    const inner = src.code.slice(i + 2, close).trim();
    const name = /^[A-Za-z_][\w:]*/.exec(inner)?.[0] ?? "";
    found.push({ name, body: inner.slice(name.length).trim() });
    i = close;
  }
  return found;
}

/** Start of the item whose head ends at `at`: just after the last `{`, `}` or `;` before it. */
function itemStart(masked: string, at: number): number {
  let i = at - 1;
  while (i >= 0 && masked[i] !== "{" && masked[i] !== "}" && masked[i] !== ";") i -= 1;
  return i + 1;
}

const compact = (text: string) => text.replace(/\s+/g, "");

/** The predicate of a `cfg(...)` attribute, whitespace removed, or null when it is `cfg(test)` or not a cfg. */
function cfgPredicate(attribute: Attribute): string | null {
  if (attribute.name !== "cfg") return null;
  const body = attribute.body;
  if (!body.startsWith("(") || !body.endsWith(")")) return null;
  const predicate = compact(body.slice(1, -1));
  return predicate === "test" ? null : predicate;
}

/** `#![cfg(...)]` inner attributes: they gate the whole file, wherever the first item sits. */
function fileCfgs(src: Source): string[] {
  const found: string[] = [];
  for (const match of src.masked.matchAll(/#!\[/g)) {
    const bracket = (match.index ?? 0) + 2;
    const close = closeOf(src.masked, bracket);
    if (close === -1) continue;
    const inner = src.code.slice(bracket + 1, close).trim();
    const body = inner.replace(/^cfg\s*/, "");
    if (inner.startsWith("cfg") && body.startsWith("(") && body.endsWith(")")) {
      const predicate = compact(body.slice(1, -1));
      if (predicate !== "test") found.push(predicate);
    }
  }
  return found;
}

/** The cfg predicates of every `mod` block that encloses `at`. */
function moduleCfgs(src: Source, at: number): string[] {
  const stack: Array<string[] | null> = [];
  for (let i = 0; i < at; i += 1) {
    const ch = src.masked[i];
    if (ch === "{") {
      const start = itemStart(src.masked, i);
      const head = src.masked.slice(start, i);
      if (/\bmod\s+[A-Za-z_]\w*\s*$/.test(head)) {
        stack.push(
          attributesIn(src, start, i)
            .map(cfgPredicate)
            .filter((predicate): predicate is string => predicate !== null),
        );
      } else {
        stack.push(null);
      }
    } else if (ch === "}") {
      stack.pop();
    }
  }
  return stack.flat().filter((entry): entry is string => entry !== null);
}

function declarations(src: Source, name: string): number[] {
  const found: number[] = [];
  for (const match of src.masked.matchAll(FN_DECL)) {
    if (match[1] === name) found.push(match.index ?? 0);
  }
  return found;
}

function factsAt(src: Source, at: number): Omit<TestFacts, "count"> {
  const start = itemStart(src.masked, at);
  const attributes = attributesIn(src, start, at);
  const cfgs = [
    ...attributes.map(cfgPredicate).filter((predicate): predicate is string => predicate !== null),
    ...moduleCfgs(src, at),
    ...fileCfgs(src),
  ];
  return {
    isTest: attributes.some((attribute) => attribute.name === "test" || attribute.name.endsWith("::test")),
    ignored: attributes.some(
      (attribute) =>
        attribute.name === "ignore" || (attribute.name === "cfg_attr" && /[(,\s]ignore\b/.test(attribute.body)),
    ),
    cfgs: [...new Set(cfgs)].sort(),
  };
}

/** What the attributes around `fn <name>` in `source` say about it. */
export function testFacts(source: string, name: string): TestFacts {
  const src = prepare(source);
  const at = declarations(src, name);
  if (at.length === 0) return { count: 0, isTest: false, ignored: false, cfgs: [] };
  return { count: at.length, ...factsAt(src, at[0]) };
}

function envLiterals(src: Source): string[] {
  return [...new Set([...src.code.matchAll(ENV_LITERAL)].map((match) => match[1]))];
}

/** Every injection hook the documented patterns find in one file. */
export function findHooks(file: string, source: string): Hook[] {
  const src = prepare(source);
  const hooks: Hook[] = envLiterals(src).map((name) => ({ kind: "env" as const, name, file }));
  const seen = new Set<string>();
  for (const match of src.masked.matchAll(FN_DECL)) {
    const name = match[1];
    if (seen.has(name)) continue;
    const at = match.index ?? 0;
    let hook = FN_NAME_HOOKS.some((pattern) => pattern.test(name));
    if (!hook && SEAM_NAME.test(name)) {
      const open = src.masked.indexOf("(", at + match[0].length);
      const close = open === -1 ? -1 : closeOf(src.masked, open);
      hook = close !== -1 && DYN_FN.test(src.masked.slice(open, close));
    }
    if (hook && !factsAt(src, at).isTest) {
      seen.add(name);
      hooks.push({ kind: "fn", name, file });
    }
  }
  return hooks;
}

function hookExists(sources: Record<string, string>, hook: Hook): "yes" | "no" | "unloaded" {
  const source = sources[hook.file];
  if (source === undefined) return "unloaded";
  const src = prepare(source);
  return hook.kind === "env" ? (envLiterals(src).includes(hook.name) ? "yes" : "no") : declarations(src, hook.name).length > 0 ? "yes" : "no";
}

const hookKey = (hook: Hook) => `${hook.kind}:${hook.file}:${hook.name}`;

function sameSet(left: string[], right: string[]): boolean {
  const a = [...new Set(left.map(compact))].sort();
  const b = [...new Set(right.map(compact))].sort();
  return a.length === b.length && a.every((entry, index) => entry === b[index]);
}

function checkTest(id: string, ref: TestRef, sources: Record<string, string>, errors: string[]): void {
  const source = sources[ref.file];
  if (source === undefined) {
    errors.push(`${id}: cited file ${ref.file} was not loaded`);
    return;
  }
  const facts = testFacts(source, ref.name);
  if (facts.count === 0) {
    errors.push(`${id}: test '${ref.name}' not found in ${ref.file}`);
    return;
  }
  if (facts.count > 1) {
    errors.push(`${id}: '${ref.name}' is declared ${facts.count} times in ${ref.file}; the citation is ambiguous`);
    return;
  }
  if (!facts.isTest) {
    errors.push(`${id}: '${ref.name}' in ${ref.file} is not a test (it has no #[test])`);
    return;
  }
  const declared = ref.cfg ?? [];
  if (!sameSet(facts.cfgs, declared)) {
    const shown = (list: string[]) => (list.length === 0 ? "none" : list.map((entry) => `cfg(${compact(entry)})`).join(", "));
    errors.push(
      `${id}: test '${ref.name}' in ${ref.file} is gated by ${shown(facts.cfgs)} but the row declares cfg ${shown(declared)}`,
    );
  }
  if (facts.ignored && ref.ignored !== true) {
    errors.push(`${id}: test '${ref.name}' in ${ref.file} is #[ignore]d but the row does not say ignored`);
  }
  if (!facts.ignored && ref.ignored === true) {
    errors.push(`${id}: row claims '${ref.name}' is ignored but ${ref.file} has no #[ignore] on it`);
  }
}

/** Every rule the table must keep. Returns the errors and the counts; throws nothing. */
export function checkTable(table: Table, sources: Record<string, string>, scopeFiles: string[]): Report {
  const errors: string[] = [];
  const byPath: Report["byPath"] = Object.fromEntries(table.paths.map((path) => [path, { covered: 0, uncovered: 0 }]));
  const byEngine: Report["byEngine"] = {};
  let covered = 0;
  let uncovered = 0;
  if (scopeFiles.length === 0) errors.push("the scope is empty: the guard scanned no file, so it can vouch for nothing");

  const ids = new Map<string, number>();
  for (const row of table.rows) ids.set(row.id, (ids.get(row.id) ?? 0) + 1);
  for (const [id, count] of ids) {
    if (count > 1) errors.push(`duplicate id ${id}: ${count} rows share it`);
  }

  const named = new Set<string>();
  const counted = new Set<string>();
  for (const row of table.rows) {
    const id = row.id;
    const problems = errors.length;
    if (!/^[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+$/.test(id)) errors.push(`${id}: id must be UPPER-KEBAB with a path prefix`);
    if (!table.paths.includes(row.path)) errors.push(`${id}: unknown path '${row.path}'`);
    if (!STATUSES.includes(row.status)) errors.push(`${id}: unknown status '${row.status}'`);

    const tests = row.tests ?? [];
    if (row.status === "covered") {
      if (tests.length === 0) errors.push(`${id}: covered row cites no test`);
      if (row.injection.kind === "none") {
        errors.push(`${id}: covered row has injection none; a window nothing can inject at is not covered`);
      }
      if (row.reason !== undefined && row.reason.trim() !== "") errors.push(`${id}: covered row carries a reason for being uncovered`);
    } else if (row.status === "uncovered") {
      if (row.reason === undefined || row.reason.trim() === "") errors.push(`${id}: uncovered row has no reason`);
      if (tests.length > 0) errors.push(`${id}: uncovered row lists tests; cover it, or move them to nearestTests`);
    }

    const { kind, hooks } = row.injection;
    if (kind === "hook" && hooks.length === 0) errors.push(`${id}: injection kind hook names no hook`);
    if (kind !== "hook" && hooks.length > 0) errors.push(`${id}: injection kind ${kind} lists hooks; use kind hook`);
    for (const hook of hooks) {
      named.add(hookKey(hook));
      const where = hookExists(sources, hook);
      if (where === "unloaded") errors.push(`${id}: hook '${hook.name}' cites ${hook.file}, which was not loaded`);
      else if (where === "no") errors.push(`${id}: hook '${hook.name}' (${hook.kind}) no longer exists in ${hook.file}`);
    }

    for (const ref of tests) checkTest(id, ref, sources, errors);
    for (const ref of row.nearestTests ?? []) checkTest(id, ref, sources, errors);

    const valid = errors.length === problems && !counted.has(id);
    counted.add(id);
    if (valid && byPath[row.path]) {
      const tallies = [byPath[row.path]];
      if (row.engine !== undefined) {
        const key = `${row.path}/${row.engine}`;
        byEngine[key] ??= { covered: 0, uncovered: 0 };
        tallies.push(byEngine[key]);
      }
      for (const tally of tallies) tally[row.status] += 1;
      if (row.status === "covered") covered += 1;
      else uncovered += 1;
    }
  }

  const setAside = new Set<string>();
  for (const hook of table.ignoredHooks) {
    setAside.add(hookKey(hook));
    if (hook.reason.trim() === "") errors.push(`${hook.file}: hook '${hook.name}' is set aside with no reason`);
    if (hookExists(sources, hook) === "no") errors.push(`${hook.file}: set-aside hook '${hook.name}' no longer exists`);
  }

  const hooksFound: Hook[] = [];
  for (const file of scopeFiles) {
    const source = sources[file];
    if (source === undefined) {
      errors.push(`${file}: in scope but not loaded`);
      continue;
    }
    for (const hook of findHooks(file, source)) {
      hooksFound.push(hook);
      if (!named.has(hookKey(hook)) && !setAside.has(hookKey(hook))) {
        errors.push(`${file}: ${hook.kind} hook '${hook.name}' is in scope but no row names it`);
      }
    }
  }

  return { errors, covered, uncovered, byPath, byEngine, scopeFiles, hooksFound };
}

/** What a reader of the test output needs: the two counts apart, and what was looked at. */
export function summary(report: Report): string {
  const count = report.scopeFiles.length;
  const lines = [
    `crash windows: covered ${report.covered}, uncovered ${report.uncovered}`,
    ...Object.entries(report.byPath).map(
      ([path, tally]) => `  ${path}: covered ${tally.covered}, uncovered ${tally.uncovered}`,
    ),
    ...Object.entries(report.byEngine).map(
      ([engine, tally]) => `    ${engine}: covered ${tally.covered}, uncovered ${tally.uncovered}`,
    ),
    `hook scan: scanned ${count} file${count === 1 ? "" : "s"} for injection hooks (${HOOK_PATTERNS}); found ${report.hooksFound.length}`,
    "scope: the ledger write path, the ops log in git_writer.rs and worktree merge only, not the whole codebase;",
    "an injection that is not one of the hook shapes above is not seen by the scan",
    ...report.scopeFiles.map((file) => `  scanned ${file}`),
  ];
  return lines.join("\n");
}
