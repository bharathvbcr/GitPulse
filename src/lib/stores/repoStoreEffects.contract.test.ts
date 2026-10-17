import { readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { codeOnly, svelteFiles } from "./sourceScan";

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
