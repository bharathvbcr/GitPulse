import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "WorktreesPanel.svelte"),
  "utf8"
);

describe("WorktreesPanel agent worktree affordances", () => {
  it("exposes lock, unlock, and prune commands", () => {
    expect(source).toContain("cmd_lock_worktree");
    expect(source).toContain("cmd_unlock_worktree");
    expect(source).toContain("cmd_prune_worktree");
    expect(source).not.toContain("expire:");
  });

  it("reloads when repository status generation changes", () => {
    expect(source).toContain("$repoStore.generation");
    expect(source).toContain("cmd_list_worktrees");
  });
});
