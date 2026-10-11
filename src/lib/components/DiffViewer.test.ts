import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DiffViewer.svelte"),
  "utf8"
);

/**
 * What the diff pane does is checked by rendering it: `harness/diff.html`
 * (`node scripts/browser-regressions.mjs --harness diff`) mounts the real
 * DiffViewer against fixture IPC and checks the busy state, staging (which
 * hunk or line each action sends, and what a truncated diff refuses), find,
 * change stepping, split pairing, wrapping, the empty state, accessible names
 * and that a refresh keeps the reader's place. `harness/uncommitted.html`
 * checks the file rail beside it. Only what a rendered page cannot see stays
 * here: invariants about cost.
 */
describe("DiffViewer source contracts", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "DiffViewer.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("parses through one reference-identity cache and composes each line's spans once", () => {
    // A scroll frame must not re-parse the diff or re-tokenize a visible line;
    // a render check sees the same rows either way and cannot tell.
    expect(source).toContain("const parseCache = createParseCache();");
    expect(source).not.toContain("parseUnifiedDiff($repoStore.selectedDiff || \"\")");
    expect(source).toContain("const spanCache = new WeakMap<");
  });

  it("keeps the codeintel query effects off the whole repo store", () => {
    // `$repoStore.x` inside an effect subscribes to the whole store, which
    // publishes a fresh object per keystroke and per status poll, so the
    // impact and head queries would be cancelled and re-sent on every one.
    for (const marker of [
      "getImpactAtRung(repoPath, filePath",
      'invoke<string>("cmd_get_head_id"',
      "getImpactLayeredMany(repo, kept",
    ]) {
      const at = source.indexOf(marker);
      expect(at, `${marker} is no longer in the component`).toBeGreaterThan(-1);
      const start = source.lastIndexOf("$effect(() => {", at);
      const end = source.indexOf("\n  });\n", at);
      expect(start).toBeGreaterThan(-1);
      expect(source.slice(start, end), `the effect around ${marker} reads $repoStore`).not.toContain("$repoStore");
    }
  });
});
