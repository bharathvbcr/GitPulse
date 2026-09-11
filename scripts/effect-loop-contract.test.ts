import { readdirSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { scriptBlocks } from "../src/lib/dom/markupText.ts";
import { escapeRegExp } from "../src/lib/text/lineSearch.ts";

/**
 * A pane that crashes with `effect_update_depth_exceeded` takes its whole
 * surface down, and the shape that causes it is invisible on inspection.
 *
 * Three shapes, all scanned from the tree rather than listed here:
 *
 * 1. A `$state` read+write inside a synchronous callback registered from
 *    `$effect`. `Metric.subscribe` (src/lib/metrics/freshness.ts) delivers the
 *    CURRENT snapshot from inside `subscribe()` itself, so the first callback
 *    runs while the effect is still being tracked. PulseView's workspace LOC
 *    strip (`const next = [...workspaceLoc]`) and StoragePanel's
 *    `historyVersion += 1` (the compound assignment IS the read) shipped this.
 *
 * 2. A `$state` read+write in the `$effect` body itself, after `untrack(...)`
 *    and after stripping those sync-callback arguments so they stay check (1).
 *    PulseView's `loadedPath = $state` load effect is this shape.
 *
 * 3. A `$storeName` auto-subscription plus `name.set` / `name.update` /
 *    `name.setError` in the same effect after `untrack`. App's
 *    `$repoStore.error` forwarding that called `repoStore.setError(null)` is
 *    this shape. Svelte runes (`state`, `derived`, `effect`, `props`,
 *    `bindable`, `inspect`, `host`) are not stores.
 *
 * Neither is visible in `npm test` on its own: vitest runs
 * `environment: "node"`, where `$effect` compiles out entirely.
 *
 * Every roster below is derived from the tree rather than listed here. The
 * counters at the end exist for the same reason: a regex that silently stops
 * matching would otherwise report a clean scan of nothing at all.
 */

const SRC = fileURLToPath(new URL("../src", import.meta.url));

/** Runes live in .svelte AND in .svelte.ts/.svelte.js modules. Scanning only
 *  components would pass forever while the real set grew past it. */
const RUNE_FILE = /\.svelte$|\.svelte\.[jt]s$/;

function runeFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = `${dir}/${entry}`;
    if (statSync(full).isDirectory()) out.push(...runeFiles(full));
    else if (RUNE_FILE.test(entry) && !/\.(test|spec)\./.test(entry)) out.push(full);
  }
  return out.sort();
}

/**
 * Blank comments and string/template literals, preserving length and lines.
 *
 * Without this the scan matches identifiers inside prose and string values:
 * StoragePanel writes `loading = snap.state === "loading"`, and the word in
 * that string reads exactly like a variable read. Every such false positive is
 * a check that eventually gets deleted for crying wolf.
 */
export function blankNonCode(src: string): string {
  const out = src.split("");
  let i = 0;
  const blank = (from: number, to: number) => {
    for (let k = from; k < to && k < out.length; k++) if (out[k] !== "\n") out[k] = " ";
  };
  while (i < src.length) {
    const two = src.slice(i, i + 2);
    if (two === "//") {
      const end = src.indexOf("\n", i);
      const stop = end === -1 ? src.length : end;
      blank(i, stop);
      i = stop;
    } else if (two === "/*") {
      const end = src.indexOf("*/", i + 2);
      const stop = end === -1 ? src.length : end + 2;
      blank(i, stop);
      i = stop;
    } else if (src[i] === '"' || src[i] === "'" || src[i] === "`") {
      const quote = src[i];
      let j = i + 1;
      while (j < src.length) {
        if (src[j] === "\\") j += 2;
        else if (src[j] === quote) break;
        else j++;
      }
      blank(i + 1, j);
      // Template holes carry real code; restore them.
      if (quote === "`") {
        for (const hole of src.slice(i, j).matchAll(/\$\{[^}]*\}/g)) {
          const start = i + (hole.index ?? 0);
          for (let k = 0; k < hole[0].length; k++) out[start + k] = hole[0][k];
        }
      }
      i = j + 1;
    } else i++;
  }
  return out.join("");
}

/** Text of the balanced pair starting at `open`. */
export function balanced(src: string, open: number, o = "(", c = ")"): string {
  let i = open;
  let depth = 0;
  do {
    if (src[i] === o) depth++;
    else if (src[i] === c) depth--;
    i++;
  } while (i < src.length && depth > 0);
  return src.slice(open, i);
}

const word = (name: string) => escapeRegExp(name);

export function countWrites(region: string, name: string): { plain: number; compound: number } {
  const n = word(name);
  const prop = `(?:\\.[A-Za-z_$][\\w$]*|\\[[^\\]]+\\])+`;
  const plain = [...region.matchAll(new RegExp(`(?<![=!<>+\\-*/%&|^])\\b${n}\\s*=(?!=)`, "g"))].length;
  const compound =
    [...region.matchAll(new RegExp(`\\b${n}\\s*(?:\\+\\+|--|(?:\\+|-|\\*|/|%|\\||&|\\^|\\?\\?|\\|\\||&&)=)`, "g"))].length +
    [...region.matchAll(new RegExp(`(?:\\+\\+|--)\\s*\\b${n}\\b`, "g"))].length +
    // `$state` is a proxy: `scanned.path = x` reads and writes the same
    // binding. Counting only `scanned =` missed the HealthPanel shape.
    [...region.matchAll(new RegExp(`\\b${n}${prop}\\s*(?:\\+\\+|--|(?:\\+|-|\\*|/|%|\\||&|\\^|\\?\\?|\\|\\||&&)?=(?!=))`, "g"))].length +
    [...region.matchAll(new RegExp(`(?:\\+\\+|--)\\s*\\b${n}${prop}`, "g"))].length;
  return { plain, compound };
}

/** Occurrences that are not a plain assignment target — i.e. tracked reads.
 *  A compound assignment counts as a read, because it is one. */
export function countReads(region: string, name: string): number {
  const all = [...region.matchAll(new RegExp(`\\b${word(name)}\\b`, "g"))].length;
  return all - countWrites(region, name).plain;
}

/** Blank `untrack(...)` regions: reading there is deliberately not a dependency. */
function stripUntracked(region: string): string {
  let out = region;
  for (;;) {
    const at = out.indexOf("untrack(");
    if (at === -1) return out;
    const call = balanced(out, at + "untrack".length);
    const width = "untrack".length + call.length;
    out = out.slice(0, at) + " ".repeat(width) + out.slice(at + width);
  }
}

/** Blank the argument lists of synchronous-callback APIs so a subscribe /
 *  map body stays check (1) and is not also reported as an effect-body loop. */
export function stripSyncCallbackArgs(region: string): string {
  const out = region.split("");
  const re = new RegExp(SYNC_CALLBACK_APIS.source, "g");
  for (const sc of region.matchAll(re)) {
    const paren = (sc.index ?? 0) + sc[0].length - 1;
    const call = balanced(region, paren);
    for (let k = 1; k < call.length - 1; k++) {
      if (out[paren + k] !== "\n") out[paren + k] = " ";
    }
  }
  return out.join("");
}

/** `$foo` auto-subscriptions that are Svelte runes, not stores. */
export const SVELTE_RUNES = new Set([
  "state",
  "derived",
  "effect",
  "props",
  "bindable",
  "inspect",
  "host",
]);

const STORE_WRITE = /\b([A-Za-z_$][\w$]*)\.(set|update|setError)\s*\(/g;

/**
 * Synchronous-callback APIs: a callback handed to one of these can run while
 * the registering effect is still tracking. `.subscribe` is the metric and
 * store seam that actually bit; the others are the same hazard by shape, and
 * naming them here is cheaper than discovering the next one in production.
 */
const SYNC_CALLBACK_APIS = /\.(subscribe|forEach|map|watch|listen|on)\s*\(/g;

/** Every rune-bearing script region of a file — both <script> blocks, since
 *  taking only the first would hide the instance script behind `<script module>`. */
function scriptRegions(file: string, raw: string): { code: string; offset: number }[] {
  if (!file.endsWith(".svelte")) return [{ code: blankNonCode(raw), offset: 0 }];
  return scriptBlocks(raw).map((block) => ({
    code: blankNonCode(block.inner),
    offset: block.innerStart,
  }));
}

/** Component-local helper bodies, so a callback that delegates to a helper
 *  (StoragePanel's `applySnapshot`) is analysed through it. Arrow-function
 *  consts count: a callback delegating to one would otherwise be invisible. */
function helperBodies(code: string): Map<string, string> {
  const helpers = new Map<string, string>();
  for (const m of code.matchAll(/function\s+([A-Za-z_$][\w$]*)\s*\(/g)) {
    const brace = code.indexOf("{", (m.index ?? 0) + m[0].length);
    if (brace > -1) helpers.set(m[1], balanced(code, brace, "{", "}"));
  }
  for (const m of code.matchAll(
    /(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]*)?=\s*(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*=>\s*\{/g,
  )) {
    const brace = code.indexOf("{", (m.index ?? 0) + m[0].length - 1);
    if (brace > -1) helpers.set(m[1], balanced(code, brace, "{", "}"));
  }
  return helpers;
}

interface Violation {
  file: string;
  line: number;
  state: string;
  api: string;
  reads: number;
  writes: number;
}

interface ScanResult {
  violations: Violation[];
  files: number;
  runeModules: number;
  effects: number;
  callbackSites: number;
  storeSites: number;
  statesSeen: number;
}

function relPath(file: string): string {
  return file.startsWith(SRC + "/") ? file.slice(SRC.length + 1) : file;
}

function collectFromScript(
  file: string,
  raw: string,
  code: string,
  offset: number,
): Pick<ScanResult, "violations" | "effects" | "callbackSites" | "storeSites" | "statesSeen" | "runeModules"> {
  const violations: Violation[] = [];
  let effects = 0;
  let callbackSites = 0;
  let storeSites = 0;
  const stateNames = [
    ...code.matchAll(/(?:let|const|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]*)?=\s*\$state\b/g),
  ].map((m) => m[1]);
  const statesSeen = stateNames.length;
  const hasEffect = code.includes("$effect");
  const runeModules = hasEffect && stateNames.length > 0 ? 1 : 0;
  if (!hasEffect) {
    return { violations, effects, callbackSites, storeSites, statesSeen, runeModules };
  }
  const helpers = helperBodies(code);

  for (const em of code.matchAll(/\$effect(?:\.pre)?\s*\(/g)) {
    effects++;
    const effectBody = balanced(code, code.indexOf("(", em.index ?? 0));
    const line = raw.slice(0, offset + (em.index ?? 0)).split("\n").length;
    const fileName = relPath(file);

    const callbackRe = new RegExp(SYNC_CALLBACK_APIS.source, "g");
    for (const sc of effectBody.matchAll(callbackRe)) {
      callbackSites++;
      const call = balanced(effectBody, effectBody.indexOf("(", sc.index ?? 0));
      let region = call;
      for (const [name, body] of helpers) {
        if (new RegExp(`\\b${word(name)}\\s*\\(`).test(call)) region += "\n" + body;
      }
      region = stripUntracked(region);
      for (const state of stateNames) {
        const writes = countWrites(region, state);
        const total = writes.plain + writes.compound;
        const reads = countReads(region, state);
        if (total > 0 && reads > 0) {
          violations.push({
            file: fileName,
            line,
            state,
            api: sc[1],
            reads,
            writes: total,
          });
        }
      }
    }

    const body = stripSyncCallbackArgs(stripUntracked(effectBody));
    for (const state of stateNames) {
      const writes = countWrites(body, state);
      const total = writes.plain + writes.compound;
      const reads = countReads(body, state);
      if (total > 0 && reads > 0) {
        violations.push({
          file: fileName,
          line,
          state,
          api: "body",
          reads,
          writes: total,
        });
      }
    }

    const storeRegion = stripUntracked(effectBody);
    const storeReads = new Set(
      [...storeRegion.matchAll(/\$([A-Za-z_$][\w$]*)/g)]
        .map((m) => m[1])
        .filter((name) => !SVELTE_RUNES.has(name)),
    );
    storeSites += storeReads.size;
    const writeRe = new RegExp(STORE_WRITE.source, "g");
    for (const wm of storeRegion.matchAll(writeRe)) {
      const name = wm[1];
      const method = wm[2];
      if (!storeReads.has(name)) continue;
      const reads = [...storeRegion.matchAll(new RegExp(`\\$${word(name)}\\b`, "g"))].length;
      violations.push({
        file: fileName,
        line,
        state: name,
        api: method,
        reads,
        writes: 1,
      });
    }
  }

  return { violations, effects, callbackSites, storeSites, statesSeen, runeModules };
}

export function scanSnippet(src: string): ScanResult {
  const code = blankNonCode(src);
  const part = collectFromScript("snippet.svelte.ts", src, code, 0);
  return {
    violations: part.violations,
    files: 1,
    runeModules: part.runeModules,
    effects: part.effects,
    callbackSites: part.callbackSites,
    storeSites: part.storeSites,
    statesSeen: part.statesSeen,
  };
}

export function scan(): ScanResult {
  const violations: Violation[] = [];
  let effects = 0;
  let callbackSites = 0;
  let storeSites = 0;
  let statesSeen = 0;
  let runeModules = 0;
  const files = runeFiles(SRC);

  for (const file of files) {
    const raw = readFileSync(file, "utf8");
    for (const { code, offset } of scriptRegions(file, raw)) {
      const part = collectFromScript(file, raw, code, offset);
      violations.push(...part.violations);
      effects += part.effects;
      callbackSites += part.callbackSites;
      storeSites += part.storeSites;
      statesSeen += part.statesSeen;
      runeModules += part.runeModules;
    }
  }
  return { violations, files: files.length, runeModules, effects, callbackSites, storeSites, statesSeen };
}

function formatViolation(v: Violation): string {
  if (v.api === "body") {
    return `${v.file}:${v.line} — $effect writes ${v.state} and reads it back in its body (${v.reads} read/${v.writes} write)`;
  }
  if (v.api === "set" || v.api === "update" || v.api === "setError") {
    return `${v.file}:${v.line} — $effect auto-subscribes $${v.state} and calls ${v.state}.${v.api}() (${v.reads} read/${v.writes} write)`;
  }
  return `${v.file}:${v.line} — $effect writes ${v.state} and reads it back inside .${v.api}() (${v.reads} read/${v.writes} write)`;
}

describe("no $effect reads the state it writes through a synchronous callback", () => {
  const result = scan();

  it("finds no self-invalidating effect", () => {
    expect(result.violations.map(formatViolation)).toEqual([]);
  });

  it("actually examined the panes it claims to cover", () => {
    // A check that could not run must never report the same result as a check
    // that ran and passed.
    expect(result.files).toBeGreaterThan(50);
    expect(result.runeModules).toBeGreaterThan(20);
    expect(result.effects).toBeGreaterThan(40);
    expect(result.callbackSites).toBeGreaterThan(3);
    expect(result.storeSites).toBeGreaterThan(3);
    expect(result.statesSeen).toBeGreaterThan(100);
  });

  it("recognises both real defects, including the compound-assignment read", () => {
    const pulse = `
      let workspaceLoc = $state([]);
      $effect(() => {
        workspaceLoc = rows;
        locMetric.subscribe(p, (snap) => {
          const next = [...workspaceLoc];
          workspaceLoc = next;
        });
      });`;
    const storage = `
      let historyVersion = $state(0);
      $effect(() => {
        storageMetric.subscribe(p, (snap) => { historyVersion += 1; });
      });`;
    for (const sample of [pulse, storage]) {
      const code = blankNonCode(sample);
      const name = code.match(/let\s+([A-Za-z_$][\w$]*)\s*=\s*\$state\b/)![1];
      const call = balanced(code, code.indexOf("(", code.indexOf(".subscribe")));
      const writes = countWrites(call, name);
      expect(writes.plain + writes.compound).toBeGreaterThan(0);
      expect(countReads(call, name)).toBeGreaterThan(0);
    }
  });

  it("sees through an arrow-function helper, not just a function declaration", () => {
    // StoragePanel's real defect was reached through a helper. Had that helper
    // been `const apply = (snap) => {...}` the earlier scan would have missed
    // it entirely and reported the tree clean.
    const sample = blankNonCode(`
      const apply = (snap) => { historyVersion += 1; };
      function alsoApply(snap) { historyVersion += 1; }
    `);
    const helpers = helperBodies(sample);
    expect([...helpers.keys()].sort()).toEqual(["alsoApply", "apply"]);
  });

  it("does not count identifiers inside comments or string literals", () => {
    const sample = blankNonCode(`
      // a previous report survives: report the failure
      loading = snap.state === "loading";
      report = snap.value;`);
    expect(countReads(sample, "loading")).toBe(0);
    expect(countReads(sample, "report")).toBe(0);
  });

  it("escapes backslash so a name cannot become a character-class in the identifier regex", () => {
    // Pre-fix `word` only escaped `$`. `x\d` compiled as "x, then a digit" and
    // counted `x0 = 1` as a write of the identifier `x\d`.
    expect(countWrites("x0 = 1", "x\\d").plain).toBe(0);
    expect(countWrites("x\\d = 1", "x\\d").plain).toBe(1);
  });

  it("does not flag a read that is explicitly untracked", () => {
    const sample = stripUntracked(
      blankNonCode(`rows.subscribe(p, () => { const n = [...untrack(() => rows)]; rows = n; })`),
    );
    expect(countReads(sample, "rows")).toBe(1); // the .subscribe receiver only
  });

  it("flags $state read+write in an $effect body after untrack and subscribe args are stripped", () => {
    const pulseLoaded = `
      let loadedPath = $state(null);
      $effect(() => {
        if (path === loadedPath) return;
        loadedPath = path;
      });`;
    const storageBody = `
      let historyVersion = $state(0);
      $effect(() => { historyVersion += 1; });`;
    const pulse = scanSnippet(pulseLoaded);
    expect(pulse.violations.some((v) => v.api === "body" && v.state === "loadedPath")).toBe(true);
    const storage = scanSnippet(storageBody);
    expect(storage.violations.some((v) => v.api === "body" && v.state === "historyVersion")).toBe(true);
    const fixed = scanSnippet(`
      let loadedPath = null;
      $effect(() => {
        if (path === loadedPath) return;
        loadedPath = path;
      });`);
    expect(fixed.violations.filter((v) => v.state === "loadedPath")).toEqual([]);
  });

  it("does not treat a subscribe callback as an effect-body loop", () => {
    const sample = scanSnippet(`
      let workspaceLoc = $state([]);
      $effect(() => {
        locMetric.subscribe(p, (snap) => {
          const next = [...workspaceLoc];
          workspaceLoc = next;
        });
      });`);
    expect(sample.violations.filter((v) => v.api === "body")).toEqual([]);
    expect(sample.violations.some((v) => v.api === "subscribe" && v.state === "workspaceLoc")).toBe(
      true,
    );
  });

  it("flags $repoStore.error auto-sub plus repoStore.setError in the same effect", () => {
    const looping = scanSnippet(`
      $effect(() => {
        const err = $repoStore.error;
        if (err) repoStore.setError(null);
      });`);
    expect(looping.violations.some((v) => v.state === "repoStore" && v.api === "setError")).toBe(
      true,
    );
    const fixed = scanSnippet(`
      $effect(() => {
        const err = $repoStore.error;
        if (err) untrack(() => { repoStore.setError(null); });
      });`);
    expect(fixed.violations.filter((v) => v.state === "repoStore")).toEqual([]);
    const setShape = scanSnippet(`
      $effect(() => { const v = $items; items.set(v); });`);
    expect(setShape.violations.some((v) => v.state === "items" && v.api === "set")).toBe(true);
    const updateShape = scanSnippet(`
      $effect(() => { const v = $items; items.update((n) => n); });`);
    expect(updateShape.violations.some((v) => v.state === "items" && v.api === "update")).toBe(true);
  });

  it("flags $state property mutation in an $effect body", () => {
    const looping = scanSnippet(`
      let scanned = $state({ path: "" });
      $effect(() => {
        if (path === scanned.path) return;
        scanned.path = path;
      });`);
    expect(looping.violations.some((v) => v.api === "body" && v.state === "scanned")).toBe(true);
    const indexed = scanSnippet(`
      let statuses = $state({});
      $effect(() => {
        statuses[key] = { running: true };
      });`);
    expect(indexed.violations.some((v) => v.api === "body" && v.state === "statuses")).toBe(true);
    const plain = scanSnippet(`
      const scanned = { path: "" };
      $effect(() => {
        if (path === scanned.path) return;
        scanned.path = path;
      });`);
    expect(plain.violations.filter((v) => v.state === "scanned")).toEqual([]);
  });

  it("does not treat Svelte runes as store auto-subscriptions", () => {
    const sample = scanSnippet(`
      $effect(() => {
        let n = $state(0);
        const d = $derived(1);
        $inspect(n);
        $host();
        state.set(1);
        derived.update(() => 0);
        effect.setError(null);
        props.set(1);
        bindable.update(() => 0);
        inspect.setError(null);
        host.set(1);
      });`);
    expect(sample.violations.filter((v) => SVELTE_RUNES.has(v.state))).toEqual([]);
  });
});
