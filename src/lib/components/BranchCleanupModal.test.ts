import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "BranchCleanupModal.svelte"),
  "utf8",
);

describe("BranchCleanupModal scan trigger", () => {
  it("rescans only when the modal opens or the repository changes", () => {
    // repoStore publishes a fresh object per keystroke and per status poll;
    // reading `$repoStore.currentPath` in the effect rescanned on each one
    // and reset the reader's selection to the defaults.
    expect(source).toContain("const scanRepoPath = $derived($repoStore.currentPath);");
    const call = source.indexOf("void loadBackups();");
    expect(call).toBeGreaterThan(-1);
    const effect = source.slice(source.lastIndexOf("$effect(() => {", call), call);
    expect(effect).toContain("if (isOpen && scanRepoPath)");
    expect(effect).not.toContain("$repoStore");
    // runScan reads the filters before its first await; untracked, so a
    // filter edit rescans through its own onchange and not a second time here.
    expect(effect).toContain("untrack(() => {");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("void runScan();"));
  });
});
