import { readdirSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A pane that crashes with `effect_update_depth_exceeded` takes its whole
 * surface down, and the shape that causes it is invisible on inspection.
 *
 * `Metric.subscribe` (src/lib/metrics/freshness.ts) delivers the CURRENT
 * snapshot synchronously, from inside `subscribe()` itself. So when an
 * `$effect` subscribes, the first callback runs while that effect is still
 * being tracked. If the callback reads a `$state` the effect also writes, the
 * read registers as a dependency of the effect that writes it, every write
 * re-invalidates the effect, and Svelte aborts the pane after ~1000 passes.
 *
 * Two panes shipped that shape — PulseView's workspace LOC strip
 * (`const next = [...workspaceLoc]`) and StoragePanel's usage history
 * (`historyVersion += 1`, where the compound assignment IS the read).
 *
 * This roster is derived by walking src/ rather than listing the panes known
 * to subscribe today: a hand-written list would keep passing while the next
 * panel added the same bug. The counters at the end exist for the same
 * reason — a regex that silently stops matching would otherwise report a
 * clean scan of nothing at all.
 */

const SRC = fileURLToPath(new URL("../src", import.meta.url));

function svelteFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = `${dir}/${entry}`;
    if (statSync(full).isDirectory()) out.push(...svelteFiles(full));
    else if (entry.endsWith(".svelte")) out.push(full);
  }
  return out.sort();
}

/**
 * Blank out comments and string/template literals, preserving length and line
 * structure so offsets stay valid.
 *
 * Without this the scan matches identifiers inside prose and string values:
 * StoragePanel writes `loading = snap.state === "loading"`, and the word in
 * that string reads exactly like a variable read. Every such false positive is
 * a check that eventually gets deleted for crying wolf.
 */
function blankNonCode(src: string): string {
  const out = src.split("");
  let i = 0;
  const blank = (from: number, to: number) => {
    for (let k = from; k < to && k < out.length; k++) {
      if (out[k] !== "\n") out[k] = " ";
    }
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
      // Template holes carry real code; keep them, blank the literal text.
      blank(i + 1, j);
      if (quote === "`") {
        for (const hole of src.slice(i, j).matchAll(/\$\{[^}]*\}/g)) {
          const start = i + (hole.index ?? 0);
          for (let k = 0; k < hole[0].length; k++) out[start + k] = hole[0][k];
        }
      }
      i = j + 1;
    } else {
      i++;
    }
  }
  return out.join("");
}

/** Text of the balanced `(...)` starting at `open`. */
function balanced(src: string, open: number): string {
  let i = open;
  let depth = 0;
  do {
    if (src[i] === "(") depth++;
    else if (src[i] === ")") depth--;
    i++;
  } while (i < src.length && depth > 0);
  return src.slice(open, i);
}

/** Text of the balanced `{...}` starting at `open`. */
function balancedBraces(src: string, open: number): string {
  let i = open;
  let depth = 0;
  do {
    if (src[i] === "{") depth++;
    else if (src[i] === "}") depth--;
    i++;
  } while (i < src.length && depth > 0);
  return src.slice(open, i);
}

const word = (name: string) => name.replace(/\$/g, "\\$");

function countWrites(region: string, name: string): { plain: number; compound: number } {
  const n = word(name);
  const plain = [...region.matchAll(new RegExp(`(?<![=!<>+\\-*/%&|^])\\b${n}\\s*=(?!=)`, "g"))].length;
  const compound = [
    ...region.matchAll(new RegExp(`\\b${n}\\s*(?:\\+\\+|--|(?:\\+|-|\\*|/|%|\\||&|\\^|\\?\\?|\\|\\||&&)=)`, "g")),
    ...region.matchAll(new RegExp(`(?:\\+\\+|--)\\s*\\b${n}\\b`, "g")),
  ].length;
  return { plain, compound };
}

/** Occurrences that are not a plain assignment target — i.e. tracked reads. */
function countReads(region: string, name: string): number {
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
    out = out.slice(0, at) + " ".repeat("untrack".length + call.length) + out.slice(at + "untrack".length + call.length);
  }
}

interface Violation {
  file: string;
  line: number;
  state: string;
  reads: number;
  writes: number;
}

function scan(): { violations: Violation[]; effects: number; subscriptions: number; files: number } {
  const violations: Violation[] = [];
  let effects = 0;
  let subscriptions = 0;
  const files = svelteFiles(SRC);

  for (const file of files) {
    const raw = readFileSync(file, "utf8");
    const scriptOpen = raw.indexOf("<script");
    if (scriptOpen === -1) continue;
    const bodyStart = raw.indexOf(">", scriptOpen) + 1;
    const bodyEnd = raw.indexOf("</script>", bodyStart);
    if (bodyEnd === -1) continue;
    const code = blankNonCode(raw.slice(bodyStart, bodyEnd));

    const stateNames = [
      ...code.matchAll(/(?:let|const|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]*)?=\s*\$state\b/g),
    ].map((m) => m[1]);
    if (stateNames.length === 0) continue;

    // Component-local function bodies, so a callback that delegates to a
    // helper (StoragePanel's `applySnapshot`) is analysed through it.
    const helpers = new Map<string, string>();
    for (const m of code.matchAll(/function\s+([A-Za-z_$][\w$]*)\s*\([^)]*\)[^{]*\{/g)) {
      helpers.set(m[1], balancedBraces(code, code.indexOf("{", m.index! + m[0].length - 1)));
    }

    for (const em of code.matchAll(/\$effect(?:\.pre)?\s*\(/g)) {
      effects++;
      const effectBody = balanced(code, code.indexOf("(", em.index!));
      for (const sc of effectBody.matchAll(/\.subscribe\s*\(/g)) {
        subscriptions++;
        const call = balanced(effectBody, effectBody.indexOf("(", sc.index!));
        let region = call;
        for (const [name, body] of helpers) {
          if (new RegExp(`\\b${word(name)}\\s*\\(`).test(call)) region += body;
        }
        region = stripUntracked(region);
        for (const state of stateNames) {
          const writes = countWrites(region, state);
          const totalWrites = writes.plain + writes.compound;
          const reads = countReads(region, state);
          if (totalWrites > 0 && reads > 0) {
            violations.push({
              file: file.slice(SRC.length + 1),
              line: raw.slice(0, bodyStart + em.index!).split("\n").length,
              state,
              reads,
              writes: totalWrites,
            });
          }
        }
      }
    }
  }
  return { violations, effects, subscriptions, files: files.length };
}

describe("no $effect subscribes to a metric and reads the state it writes", () => {
  const result = scan();

  it("finds no self-invalidating subscription callback", () => {
    const report = result.violations.map(
      (v) => `${v.file}:${v.line} — $effect writes ${v.state} and reads it back inside .subscribe() (${v.reads} read/${v.writes} write)`,
    );
    expect(report).toEqual([]);
  });

  it("actually examined the panes it claims to cover", () => {
    // A check that could not run must never report the same result as a check
    // that ran and passed. If these regexes stop matching, the scan above
    // returns "no violations" while inspecting nothing.
    expect(result.files).toBeGreaterThan(50);
    expect(result.effects).toBeGreaterThan(40);
    expect(result.subscriptions).toBeGreaterThan(3);
  });

  it("recognises the shape it is meant to catch", () => {
    // The scanner's own regression guard, against the two real defects this
    // contract was written for.
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

  it("does not count identifiers inside comments or string literals", () => {
    // StoragePanel writes `loading = snap.state === "loading"` and comments
    // about `report`; neither is a read.
    const sample = blankNonCode(`
      // a previous report survives: report the failure
      loading = snap.state === "loading";
      report = snap.value;`);
    expect(countReads(sample, "loading")).toBe(0);
    expect(countReads(sample, "report")).toBe(0);
  });
});
