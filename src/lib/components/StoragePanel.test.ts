import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import StoragePanel from "./StoragePanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "StoragePanel.svelte"),
  "utf8"
);

describe("StoragePanel source contracts & async hygiene", () => {
  it("invokes cmd_storage_scan with the current repo path", () => {
    expect(source).toContain('invoke<StorageReport>("cmd_storage_scan", { repoPath }');
  });

  it("guards against race conditions using createAsyncGuard", () => {
    expect(source).toContain("const guard = createAsyncGuard();");
    expect(source).toContain("if (!guard.isLive()) return;");
    expect(source).toContain("inflight?.cancel();");
  });

  it("cleans up timers and pending requests on unmount", () => {
    expect(source).toContain("inflight?.cancel();");
    expect(source).toContain("if (copyTimer !== null) window.clearTimeout(copyTimer);");
  });

  it("coalesces scans into history and supports clearing per repo", () => {
    expect(source).toContain("recordSnapshot(");
    expect(source).toContain("clearRepoHistory(");
    expect(source).toContain("saveHistory(");
  });

  it("formats hygiene gaps and surfaces unignored / tracked issues", () => {
    expect(source).toContain("artifact.unignored");
    expect(source).toContain("artifact.tracked_files");
    expect(source).toContain("not ignored");
    expect(source).toContain("committed");
  });

  it("provides single-click navigation to MANVI for merged-branch cleanup", () => {
    expect(source).toContain('repoStore.setActiveTab("manvi")');
    expect(source).toContain("Clean up in MANVI");
  });

  it("honestly flags truncated/budgeted scans instead of presenting as complete", () => {
    expect(source).toContain("report.scan.truncated");
    expect(source).toContain("partial scan");
    expect(source).toContain("Scan hit a safety budget; totals are floors.");
  });
});

describe("StoragePanel rendering", () => {
  it("renders the empty state when no repo is active", () => {
    const { body } = render(StoragePanel);
    expect(body).toContain("Storage");
    expect(body).toContain("Rescan");
    expect(body).toContain("Open a repository to measure its disk usage.");
  });
});
