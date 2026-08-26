import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ManviOpsPanel from "./ManviOpsPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ManviOpsPanel.svelte"),
  "utf8",
);

describe("ManviOpsPanel deferred issue-load drain", () => {
  it("queues an issue load skipped because an operation holds busy", () => {
    // Switching repos while an op runs must not drop the new repo's issues
    // until the next interval tick; the request is recorded and drained.
    const busyCheckIdx = source.indexOf("if (busy !== null) {");
    expect(busyCheckIdx).toBeGreaterThan(-1);
    expect(source.indexOf("pendingIssueRepo = repo;")).toBeGreaterThan(busyCheckIdx);
  });

  it("drains the queued load every time busy settles", () => {
    // One settle helper owns the transition; every op's finally routes
    // through it (issues, branches, clean, review, sync, report, release).
    expect(source).toContain("function settleBusy()");
    expect(source.match(/settleBusy\(\);/g)?.length).toBeGreaterThanOrEqual(7);
    expect(source.match(/finally \{\s*busy = null;\s*\}/g)).toBeNull();
  });

  it("drops a stale queued repo when a genuine repo change lands", () => {
    // The effect memoizes on lastRepo (store emissions carry fresh objects
    // per ~6s poll and must not re-run the resets); a real change first
    // discards any request queued for an older repo.
    const lastRepoIdx = source.indexOf("lastRepo = repo;");
    expect(lastRepoIdx).toBeGreaterThan(-1);
    expect(source.indexOf("pendingIssueRepo = null;", lastRepoIdx)).toBeGreaterThan(lastRepoIdx);
  });
});

describe("ManviOpsPanel rendering", () => {
  it("renders the MANVI header with both panes", () => {
    const { body } = render(ManviOpsPanel);
    expect(body).toContain("MANVI");
    expect(body).toContain("Ops");
    expect(body).toContain("Harness");
  });
});

describe("ManviOpsPanel silent background refresh", () => {
  it("routes the once-a-minute poll through a dedicated background flag, not busy", () => {
    // The interval must pass { background: true }; a background refresh sets
    // its own flag and never claims `busy`.
    const intervalIdx = source.indexOf("ISSUE_REFRESH_MS)");
    expect(intervalIdx).toBeGreaterThan(-1);
    expect(source.indexOf("{ background: true }")).toBeGreaterThan(intervalIdx - 200);
    expect(source).toContain("let backgroundRefreshing = $state(false);");
    expect(source).toContain("if (opts.background) {\n      if (backgroundRefreshing) return;\n      backgroundRefreshing = true;");
  });

  it("keeps user-initiated issue loads on busy with settle-drain semantics", () => {
    expect(source).toContain('busy = "issues";');
    expect(source.match(/settleBusy\(\);/g)?.length).toBeGreaterThanOrEqual(7);
  });
});
