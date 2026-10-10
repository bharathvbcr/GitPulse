import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * No `$effect` reads `$repoStore`.
 *
 * repoStore publishes a fresh object for every change it makes — a status
 * poll, a loading flag, an error — and Svelte treats every object-valued store
 * set as a change. An effect that reads `$repoStore.x` therefore re-runs on
 * all of them, whatever `x` is. That is how a file tree re-listed the
 * repository per poll, an armed "click again to remove" disarmed itself, a
 * GitHub spinner stayed on after its answer landed, and "Show more" snapped
 * back to the first page. Each was patched with a hand-copied `prev*` guard
 * until the next effect forgot one.
 *
 * Effects read `repoStore.currentPath` (the canonical repository key, which
 * notifies only on a switch) or a `$derived` of the field they need, which
 * compares by value; the read of `$repoStore` then lives outside the effect.
 */

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function svelteFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return svelteFiles(path);
    return entry.name.endsWith(".svelte") ? [path] : [];
  });
}

/**
 * The script with comments and string contents blanked to spaces, so neither
 * a comment that names `$repoStore.x` nor a `)` inside a string is read as
 * code. Template-literal `${...}` expressions are code and are kept. Offsets
 * are preserved, so a match maps back to its line.
 */
function codeOnly(script: string): string {
  const out = script.split("");
  const blank = (from: number, to: number) => {
    for (let i = from; i < to; i += 1) if (out[i] !== "\n") out[i] = " ";
  };
  // Braces opened inside template expressions, innermost last; a `}` that
  // closes one of these resumes the template literal around it.
  const templateDepths: number[] = [];
  let depth = 0;
  let lastSignificant = "";
  let i = 0;
  const scanTemplate = (start: number): number => {
    let j = start;
    while (j < script.length) {
      if (script[j] === "\\") { j += 2; continue; }
      if (script[j] === "`") { blank(start, j); return j + 1; }
      if (script[j] === "$" && script[j + 1] === "{") {
        blank(start, j);
        templateDepths.push(depth);
        depth += 1;
        return j + 2;
      }
      j += 1;
    }
    blank(start, j);
    return j;
  };
  while (i < script.length) {
    const c = script[i];
    const next = script[i + 1];
    if (c === "/" && next === "/") {
      const end = script.indexOf("\n", i);
      const stop = end === -1 ? script.length : end;
      blank(i, stop);
      i = stop;
      continue;
    }
    if (c === "/" && next === "*") {
      const end = script.indexOf("*/", i + 2);
      const stop = end === -1 ? script.length : end + 2;
      blank(i, stop);
      i = stop;
      continue;
    }
    if (c === '"' || c === "'") {
      let j = i + 1;
      while (j < script.length && script[j] !== c && script[j] !== "\n") j += script[j] === "\\" ? 2 : 1;
      blank(i + 1, j);
      i = j + 1;
      lastSignificant = c;
      continue;
    }
    if (c === "`") {
      i = scanTemplate(i + 1);
      lastSignificant = "`";
      continue;
    }
    if (c === "/" && (lastSignificant === "" || "(,=:[!&|?{};+-*%<>~^".includes(lastSignificant))) {
      // A regular expression literal: skip to its closing slash.
      let j = i + 1;
      let inClass = false;
      while (j < script.length && script[j] !== "\n") {
        if (script[j] === "\\") { j += 2; continue; }
        if (script[j] === "[") inClass = true;
        else if (script[j] === "]") inClass = false;
        else if (script[j] === "/" && !inClass) break;
        j += 1;
      }
      blank(i + 1, j);
      i = j + 1;
      lastSignificant = "/";
      continue;
    }
    if (c === "{") depth += 1;
    if (c === "}") {
      depth -= 1;
      if (templateDepths.length > 0 && templateDepths[templateDepths.length - 1] === depth) {
        templateDepths.pop();
        i = scanTemplate(i + 1);
        lastSignificant = "`";
        continue;
      }
    }
    if (!/\s/.test(c)) lastSignificant = /[\w$]/.test(c) ? "a" : c;
    i += 1;
  }
  return out.join("");
}

interface EffectRead {
  line: number;
  read: string;
}

/** Every `$repoStore` read inside an `$effect` / `$effect.pre` body. */
function effectStoreReads(source: string): EffectRead[] {
  const reads: EffectRead[] = [];
  const scripts = /<script\b[^>]*>([\s\S]*?)<\/script>/g;
  let block: RegExpExecArray | null;
  while ((block = scripts.exec(source)) !== null) {
    const offset = block.index + block[0].indexOf(">") + 1;
    const code = codeOnly(block[1]);
    const effects = /\$effect(?:\.pre)?\s*\(/g;
    let effect: RegExpExecArray | null;
    while ((effect = effects.exec(code)) !== null) {
      let end = effect.index + effect[0].length;
      for (let parens = 1; end < code.length && parens > 0; end += 1) {
        if (code[end] === "(") parens += 1;
        else if (code[end] === ")") parens -= 1;
      }
      const body = code.slice(effect.index, end);
      for (const read of body.matchAll(/\$repoStore\b[\w.?]*/g)) {
        const at = offset + effect.index + (read.index ?? 0);
        reads.push({ line: source.slice(0, at).split("\n").length, read: read[0] });
      }
    }
  }
  return reads;
}

describe("effects never read $repoStore", () => {
  it("finds a read in an effect, and nothing in comments, strings or outside effects", () => {
    const fixture = [
      "<script>",
      "  const path = $derived($repoStore.currentPath);",
      "  // $effect(() => $repoStore.currentPath) in a comment",
      "  $effect(() => {",
      "    /* $repoStore.branches in a block comment */",
      "    const label = \"$repoStore.x) in a string\";",
      "    const re = /\\)$/;",
      "    const tpl = `${$repoStore.generation} and $repoStore.notCode`;",
      "    if (path) void $repoStore.currentPath;",
      "  });",
      "  $effect.pre(() => { const s = $repoStore; void s; });",
      "  $effect(() => { void $currentRepoPath; });",
      "</script>",
      "<p>{$repoStore.currentPath} — markup is not an effect</p>",
    ].join("\n");

    expect(effectStoreReads(fixture)).toEqual([
      { line: 8, read: "$repoStore.generation" },
      { line: 9, read: "$repoStore.currentPath" },
      { line: 11, read: "$repoStore" },
    ]);
  });

  it("keeps one repository key: no component derives its own from the store", () => {
    // `repoStore.currentPath` is the canonical key. A private
    // `$derived($repoStore.currentPath)` behaves the same today and drifts
    // tomorrow — each copy grew its own guard beside it.
    const copies = svelteFiles(SRC).filter((file) =>
      /\$derived\(\s*\$repoStore\.currentPath\s*\)/.test(readFileSync(file, "utf8")),
    );
    expect(copies.map((file) => relative(SRC, file))).toEqual([]);
  });

  it("holds for every component", () => {
    const offenders = svelteFiles(SRC).flatMap((file) =>
      effectStoreReads(readFileSync(file, "utf8")).map(
        ({ line, read }) => `${relative(SRC, file)}:${line} ${read}`,
      ),
    );
    expect(offenders).toEqual([]);
  });
});
