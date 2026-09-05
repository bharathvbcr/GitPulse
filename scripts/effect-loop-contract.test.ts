import { readdirSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A pane that crashes with `effect_update_depth_exceeded` takes its whole
 * surface down, and the shape that causes it is invisible on inspection.
 *
 * `Metric.subscribe` (src/lib/metrics/freshness.ts) delivers the CURRENT
 * snapshot synchronously, from inside `subscribe()` itself. So when an
 * `$effect` subscribes, that first callback runs while the effect is still
 * being tracked. If the callback reads a `$state` the effect also writes, the
 * read registers as a dependency of the effect that writes it, every write
 * re-invalidates the effect, and Svelte aborts the pane after ~1000 passes.
 *
 * Two panes shipped that shape:
 *
 *   PulseView's workspace LOC strip (`const next = [...workspaceLoc]`), which
 *   arms as soon as two repositories are open.
 *
 *   StoragePanel's usage history (`historyVersion += 1`, where the compound
 *   assignment IS the read). This one needs BOTH a cached measurement and a
 *   re-run of the effect: on a cold mount the snapshot is idle, the `if
 *   (snap.value && ...)` branch does not execute, and the tracked read never
 *   happens. Switching the active repository back to an already-measured one
 *   is what arms it — a mount-only check reports a false clean here, which is
 *   exactly what the browser A/B run showed before the scenario was widened.
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

const word = (name: string) => name.replace(/\$/g, "\\$");

export function countWrites(region: string, name: string): { plain: number; compound: number } {
  const n = word(name);
  const plain = [...region.matchAll(new RegExp(`(?<![=!<>+\\-*/%&|^])\\b${n}\\s*=(?!=)`, "g"))].length;
  const compound =
    [...region.matchAll(new RegExp(`\\b${n}\\s*(?:\\+\\+|--|(?:\\+|-|\\*|/|%|\\||&|\\^|\\?\\?|\\|\\||&&)=)`, "g"))].length +
    [...region.matchAll(new RegExp(`(?:\\+\\+|--)\\s*\\b${n}\\b`, "g"))].length;
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
  const out: { code: string; offset: number }[] = [];
  for (const m of raw.matchAll(/<script[^>]*>/g)) {
    const start = (m.index ?? 0) + m[0].length;
    const end = raw.indexOf("</script>", start);
    if (end > start) out.push({ code: blankNonCode(raw.slice(start, end)), offset: start });
  }
  return out;
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
  statesSeen: number;
}

export function scan(): ScanResult {
  const violations: Violation[] = [];
  let effects = 0;
  let callbackSites = 0;
  let statesSeen = 0;
  let runeModules = 0;
  const files = runeFiles(SRC);

  for (const file of files) {
    const raw = readFileSync(file, "utf8");
    for (const { code, offset } of scriptRegions(file, raw)) {
      const stateNames = [
        ...code.matchAll(/(?:let|const|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]*)?=\s*\$state\b/g),
      ].map((m) => m[1]);
      if (stateNames.length === 0) continue;
      statesSeen += stateNames.length;
      if (code.includes("$effect")) runeModules++;
      const helpers = helperBodies(code);

      for (const em of code.matchAll(/\$effect(?:\.pre)?\s*\(/g)) {
        effects++;
        const effectBody = balanced(code, code.indexOf("(", em.index ?? 0));
        for (const sc of effectBody.matchAll(SYNC_CALLBACK_APIS)) {
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
                file: file.slice(SRC.length + 1),
                line: raw.slice(0, offset + (em.index ?? 0)).split("\n").length,
                state,
                api: sc[1],
                reads,
                writes: total,
              });
            }
          }
        }
      }
    }
  }
  return { violations, files: files.length, runeModules, effects, callbackSites, statesSeen };
}

describe("no $effect reads the state it writes through a synchronous callback", () => {
  const result = scan();

  it("finds no self-invalidating callback", () => {
    const report = result.violations.map(
      (v) =>
        `${v.file}:${v.line} — $effect writes ${v.state} and reads it back inside .${v.api}() (${v.reads} read/${v.writes} write)`,
    );
    expect(report).toEqual([]);
  });

  it("actually examined the panes it claims to cover", () => {
    // A check that could not run must never report the same result as a check
    // that ran and passed.
    expect(result.files).toBeGreaterThan(50);
    expect(result.runeModules).toBeGreaterThan(20);
    expect(result.effects).toBeGreaterThan(40);
    expect(result.callbackSites).toBeGreaterThan(3);
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

  it("does not flag a read that is explicitly untracked", () => {
    const sample = stripUntracked(
      blankNonCode(`rows.subscribe(p, () => { const n = [...untrack(() => rows)]; rows = n; })`),
    );
    expect(countReads(sample, "rows")).toBe(1); // the .subscribe receiver only
  });
});
