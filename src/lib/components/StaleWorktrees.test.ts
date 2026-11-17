import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "StaleWorktrees.svelte"), "utf8");
const panel = readFileSync(join(here, "StoragePanel.svelte"), "utf8");

describe("StaleWorktrees source contracts", () => {
  it("is mounted in the Storage panel with the report's repository and storage report", () => {
    expect(panel).toContain("<StaleWorktrees repoPath={report.repo_path} storage={report} />");
  });

  it("finds stale worktrees through cmd_storage_stale_worktrees", () => {
    expect(source).toContain('invoke<StaleWorktreeReport>("cmd_storage_stale_worktrees", { repoPath })');
    expect(source).toContain("Find stale worktrees");
  });

  it("never offers removal for a row that is not stale, and asks first", () => {
    expect(source).toContain("{#if row.stale}");
    expect(source).toContain("Remove…");
    expect(source).toContain("Confirm remove");
    expect(source).toContain("confirming !== path");
  });

  it("removes without force, through the store's guarded path", () => {
    expect(source).toContain("repoStore.removeWorktree(path, false, target, true)");
    expect(source).not.toMatch(/removeWorktree\([^)]*true\s*,\s*target/);
    expect(source.match(/removeWorktree\(/g)?.length).toBe(1);
    expect(source).not.toContain("cmd_remove_worktree");
  });

  it("states how many were examined of those listed and never calls a short list complete", () => {
    expect(source).toContain("Examined {staleReport.examined} of {staleReport.listed}");
    expect(source).toContain("so this list is not complete");
    expect(source).toContain("staleReport.issues");
  });

  it("compares sized worktrees' private bytes to the advisory limit and says when the sum is partial", () => {
    expect(source).toContain("privateBytesTotal(storage.worktrees)");
    expect(source).toContain("maxPrivateBytes > 0 && sums.total > maxPrivateBytes");
    expect(source).toContain("covers only the {sums.sized} sized worktrees");
  });
});
