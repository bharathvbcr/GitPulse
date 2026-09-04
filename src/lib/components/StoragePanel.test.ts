import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import StoragePanel from "./StoragePanel.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "StoragePanel.svelte"), "utf8");
/** The panel's scan now lives behind the shared metric; assert it there. */
const metricSource = readFileSync(join(here, "..", "metrics", "repoMetrics.ts"), "utf8");

describe("StoragePanel source contracts & async hygiene", () => {
  it("sources its report from the shared storage metric rather than its own invoke", () => {
    // The scan itself moved to repoMetrics so that one measurement serves
    // every panel and the watcher can revalidate it. The panel keeping a
    // private invoke would reintroduce exactly the duplication that had
    // PulseView and CoverageViewer scanning coverage twice.
    expect(source).toContain("storageMetric.subscribe(path,");
    expect(source).not.toContain('invoke<StorageReport>("cmd_storage_scan"');
    expect(metricSource).toContain('invoke<StorageReport>("cmd_storage_scan", { repoPath })');
  });

  it("Rescan forces a measurement so an explicit user action always does something", () => {
    expect(source).toContain("storageMetric.refresh(repoPath, { force: true })");
  });

  it("guards against races by returning the unsubscribe from its effect", () => {
    // The old per-fetch AsyncGuard is replaced by the metric's generation
    // guard plus this teardown: a snapshot for a repository the panel has
    // moved off can no longer reach component state at all.
    expect(source).toContain("return storageMetric.subscribe(path,");
  });

  it("cleans up timers on unmount", () => {
    expect(source).toContain("if (copyTimer !== null) window.clearTimeout(copyTimer);");
  });

  it("never renders a retained report as a current measurement", () => {
    // The honesty rule: a refresh that failed, or a repository that changed
    // since the scan, keeps the numbers on screen but must label them.
    expect(source).toContain("describeStaleness(snap, Date.now())");
    expect(source).toContain("out of date");
  });

  it("coalesces scans into history and supports clearing per repo", () => {
    expect(source).toContain("recordSnapshot(");
    expect(source).toContain("clearRepoHistory(");
    expect(source).toContain("saveHistory(");
  });

  it("reports the reclaim audit with its three figures kept apart", () => {
    // The headline must not absorb estimates or items needing a human: both
    // would promise space the repository cannot actually hand back.
    expect(source).toContain("reclaim_summary.reclaimable_bytes");
    expect(source).toContain("safely recoverable");
    expect(source).toContain("reclaim_summary.estimated_bytes");
    expect(source).toContain("estimated");
    expect(source).toContain("reclaim_summary.needs_review_bytes");
    expect(source).toContain("needs review");
  });

  it("shows every audited item's action and what blocks it", () => {
    expect(source).toContain("{item.action}");
    expect(source).toContain("{item.detail}");
    expect(source).toContain("{item.blocked_reason}");
  });

  it("says so when the audit is a floor rather than a total", () => {
    expect(source).toContain("reclaim_summary.partial");
    expect(source).toContain("this is a floor, not a total");
  });

  it("formats hygiene gaps and surfaces unignored / tracked issues", () => {
    expect(source).toContain("artifact.unignored");
    expect(source).toContain("artifact.tracked_files");
    expect(source).toContain("not ignored");
    expect(source).toContain("committed");
  });

  it("provides single-click navigation to MANVI for merged-branch cleanup", () => {
    expect(source).toContain('repoStore.setActiveTab("work", "policy")');
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

describe("StoragePanel flicker contracts", () => {
  it("renders the last report instantly on revisit, now via the shared metric", () => {
    // The panel-local LRU is gone; the metric is a module-level singleton, so
    // its cell outlives the per-tab remount and `subscribe` delivers the
    // current snapshot synchronously before any refetch. Same guarantee, one
    // cache instead of two.
    expect(metricSource).toContain("export const storageMetric");
    expect(source).not.toContain("createRepoPanelCache");
    expect(source).toContain("storageMetric.subscribe(path,");
  });

  it("gates its loading placeholder on having no data yet", () => {
    expect(source).toContain("{#if loading && !report}");
  });
});
