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
    expect(source).toContain("return untrack(() => storageMetric.subscribe(path,");
  });

  it("cleans up timers on unmount", () => {
    expect(source).toContain("copyFlash.dispose();"); // the timer itself is covered in desktop/clipboard.test.ts
  });

  it("never renders a retained report as a current measurement", () => {
    // The honesty rule: a refresh that failed, or a repository that changed
    // since the scan, keeps the numbers on screen but must label them.
    expect(source).toContain("describeStaleness(snap, Date.now())");
    expect(source).toContain("Out of date");
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
    expect(source).toContain("Not ignored");
    expect(source).toContain("committed");
  });

  it("provides single-click navigation to branch cleanup for merged branches", () => {
    expect(source).toContain(`openManviFocus("cleanup")`);
    expect(source).toContain(`manviFocusHint("cleanup")`);
    expect(source).toContain("Clean up branches");
  });

  it("honestly flags truncated/budgeted scans instead of presenting as complete", () => {
    expect(source).toContain("report.scan.truncated");
    expect(source).toContain("Partial scan");
    expect(source).toContain("Scan hit a safety budget or skipped unreadable locations; totals are floors.");
    expect(source).toContain("report.scan.unreadable > 0");
    expect(source).toContain("report.scan.unaudited > 0");
    expect(source).toContain("couldn't be read and are not counted");
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

describe("StoragePanel design-language contracts", () => {
  it("headlines a failed scan with describeFailure, never the raw backend text", () => {
    expect(source).toContain('describeFailure({ verb: "scan", object: "disk usage" }, errorMsg)');
    expect(source).toContain("{failure.headline}");
    expect(source).not.toMatch(/>\s*\{errorMsg\}\s*</);
  });

  it("shows a branch summary that could not run as an unknown notice, not as counts", () => {
    // A failed summary is "couldn't check", offered with a Rescan, and must
    // not fall through to the 0 / 0 / 0 row that reads as a clean repository.
    expect(source).toContain('unknownState("branch weight", report.branches.error)');
    expect(source).toContain('data-tone="unknown"');
    expect(source).toContain("{unknown.actionLabel}");
  });

  it("says there is no report rather than inventing numbers when no repository is open", () => {
    const { body } = render(StoragePanel);
    expect(body).toContain("No storage report yet");
    expect(body).not.toContain("0 B");
  });

  it("uses role tokens and the type scale, with no palette colors or pixel sizes", () => {
    expect(source).not.toMatch(/\b(?:text|bg|border)-(?:amber|rose|sky|emerald)-\d/);
    expect(source).not.toMatch(/text-\[\d+px\]/);
    expect(source).not.toMatch(/\bborder-[tblr]\b/);
  });
});
