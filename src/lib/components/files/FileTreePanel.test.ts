import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./FileTreePanel.svelte", import.meta.url), "utf8");

/**
 * What the explorer does is checked by rendering it: `harness/file-tree.html`
 * (`node scripts/browser-regressions.mjs --harness file-tree`) drives the real
 * panel for the single-select tree keys and the active item a screen reader
 * follows, the glob filter and status scopes, following the open file, the
 * context menu's keys and focus return, the delete and discard confirmations,
 * create, rename and move, which index action each file is offered, the
 * unverified-churn mark, the filter-budget caption, and long-name scrolling.
 * `harness/repo-churn.html` checks that a publish does not re-list the
 * repository. Only what a rendered page cannot see stays here.
 */
describe("FileTreePanel source contracts", () => {
  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  /**
   * Opening and revealing go through `desktop/openInShell`, not the opener
   * plugin. The plugin's commands take an absolute path from the webview,
   * which made the worktree containment check optional; these hand Rust the
   * root and the relative path separately so it can prove containment. The
   * repo-wide version of this assertion lives in `openInShell.test.ts`.
   */
  it("opens and reveals through the validated shell gate, not the opener plugin", () => {
    expect(source).toContain("openInDefaultApp");
    expect(source).toContain("revealInFileManager");
    expect(source).not.toContain("@tauri-apps/plugin-opener");
  });
});
