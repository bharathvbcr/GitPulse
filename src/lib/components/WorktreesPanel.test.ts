import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import WorktreesPanel from "./WorktreesPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "WorktreesPanel.svelte"),
  "utf8"
);

describe("WorktreesPanel", () => {
  it("labels the add-worktree button for screen readers", () => {
    const { body } = render(WorktreesPanel);
    expect(body).toContain('aria-label="Create worktree"');
  });

  it("gives the two-step remove button a spoken label, including the arm state", () => {
    // Rows render from backend data (absent in SSR), so the remove control is
    // asserted at source level like DiffViewer.test.ts does.
    expect(source).toContain("aria-label={removeArmTitle(wt)}");
    expect(source).toContain("Click again to remove");
    expect(source).toContain("Remove this worktree");
  });

  it("drops stale cmd_list_worktrees responses via async guard", () => {
    // Overlapping loads after rapid create/remove must not land out of order:
    // every apply path re-checks the guard captured at trigger time.
    expect(source).toContain("createAsyncGuard()");
    expect(source).toContain("if (!guard.isLive()) return;");
    // A superseded load's finally must not clear the newer load's spinner.
    expect(source).toContain("if (guard.isLive()) isLoading = false;");
  });
});

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

describe("WorktreesPanel removal safety", () => {
  it("passes --force only when the worktree scan found no changes", () => {
    expect(source).toContain("const force = (wt.dirty_files ?? 0) === 0;");
    // The invoke must carry the computed flag, never a literal true.
    expect(source).not.toMatch(/cmd_remove_worktree[\s\S]{0,120}?force:\s*true/);
    expect(source).toMatch(/cmd_remove_worktree[\s\S]{0,120}?force\s*\}/);
  });

  it("names the discard cost in the armed confirm when files would be lost", () => {
    expect(source).toContain("`Discard ${dirty} changed files? Click again to remove`");
    expect(source).toContain("`Discard ${wt.dirty_files} changed files?`");
  });

  it("closes the stranded tab instead of leaving it on the removed directory (T-F09)", () => {
    const guardIdx = source.indexOf("$repoStore.currentPath === wt.path");
    expect(guardIdx).toBeGreaterThan(-1);
    const closeIdx = source.indexOf("repoStore.closeTab(stranded.id)");
    expect(closeIdx).toBeGreaterThan(guardIdx);
  });

  it("preserves the concurrent-session currentPath guards after the await", () => {
    const fn = source.slice(source.indexOf("async function remove"), source.indexOf("function open"));
    expect(fn.match(/\$repoStore\.currentPath !== repo/g)?.length).toBe(2);
    expect(fn).toContain("await load()");
  });
});

describe("WorktreesPanel store-emission churn guards", () => {
  it("keeps exactly one mount trigger for the worktree list load", () => {
    // The load effect runs on mount; a second onMount(load) double-fetched.
    expect(source).not.toContain("onMount");
    const effect = source.slice(source.indexOf("let prevRepoPath"), source.indexOf("async function load"));
    expect(effect).toContain("void load();");
  });

  it("memo-guards the load effect so unrelated store emissions are no-ops", () => {
    const effect = source.slice(source.indexOf("let prevRepoPath"), source.indexOf("async function load"));
    expect(effect).toContain("if (repo === prevRepoPath && generation === prevGeneration) return;");
  });

  it("resets the armed remove confirm only on real repo/generation change or unmount", () => {
    const effect = source.slice(source.indexOf("let prevRepoPath"), source.indexOf("async function load"));
    expect(effect).toContain("clearTimeout(confirmTimer)");
    // Clearing the timer without disarming would strand a permanently armed confirm.
    expect(effect).toContain("removingPath = null;");
  });
});
