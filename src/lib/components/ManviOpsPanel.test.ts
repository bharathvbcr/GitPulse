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

describe("ManviOpsPanel tag retention", () => {
  it("checks every tag lands in exactly one bucket before allowing deletion", () => {
    // Mirrors cleanupInvariant. A split that does not add up is not describing
    // the repository, and deleting from it would be guesswork.
    expect(source).toContain(
      "tagPlan.uncompared_tags + tagPlan.retained_count + tagPlan.deletable_count ===",
    );
    expect(source).toContain("disabled={!tagInvariant || selectedTags.length === 0");
  });

  it("says when a count describes a sample rather than the whole repository", () => {
    // Three separate caps, three separate admissions: the tag listing itself,
    // the retained rows, and the deletable rows.
    expect(source).toContain(
      "The tag listing was capped, so these counts describe a sample of this repository's tags",
    );
    expect(source).toContain("{#if tagPlan.retained.length < tagPlan.retained_count}");
    expect(source).toContain("{#if tagPlan.candidates.length < tagPlan.deletable_count}");
  });

  it("reports tags it could not compare instead of hiding them in a bucket", () => {
    // An unmeasured tag is neither deletable nor evidence of retained work.
    expect(source).toContain("{tagPlan.uncompared_tags} could not be compared");
  });

  it("pre-selects no tags, unlike merged branches", () => {
    // A tag is usually kept deliberately, so deletion is a per-row choice.
    const start = source.indexOf("async function scanTags()");
    const scan = source.slice(start, source.indexOf("function toggleTag(", start));
    expect(scan).toContain("selectedTags = [];");
    expect(source).toContain("selectedBranches = next.candidates.map((candidate) => candidate.name);");
  });
});

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
  it("renders the tag retention card, not just its source", () => {
    // A source-text assertion cannot tell a card that mounts from one that
    // throws mid-render. This is the render path.
    const { body } = render(ManviOpsPanel);
    expect(body).toContain("Tag retention");
    expect(body).toContain("Build tag plan");
  });

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

describe("ManviOpsPanel deep-link landing", () => {
  it("consumes the pending focus request instead of replaying it", () => {
    // The view mounts lazily, after the click that requested the section, so
    // the effect covers both orders; taking the request is what stops an
    // unrelated later visit from jumping to the same card.
    expect(source).toContain("const requested = $manviFocusRequest;");
    expect(source).toContain("takeManviFocus();");
    expect(source).toContain("void revealFocus(requested);");
  });

  it("switches to the pane the catalog names before looking for the section", () => {
    const reveal = source.slice(source.indexOf("async function revealFocus"));
    expect(reveal.indexOf("pane = target.pane;")).toBeLessThan(
      reveal.indexOf("document.getElementById(manviSectionId(id))"),
    );
    expect(reveal).toContain("await tick();");
  });

  it("lands instantly and moves focus with the scroll", () => {
    // Smooth scrolling never advances in a webview that is not compositing,
    // which turns the whole deep link into a no-op; every other
    // scroll-into-view in the app is instant too.
    expect(source).not.toContain('behavior: "smooth"');
    expect(source).toContain('section.scrollIntoView({ block: "start" });');
    expect(source).toContain("section.focus({ preventScroll: true });");
  });

  it("re-aligns over the pane's settle, and yields to a scroll gesture", () => {
    // Cards above the target grow as their data arrives; without the second
    // pass the reader lands just under the heading they asked for.
    expect(source).toContain("const REALIGN_DELAYS_MS = [0, 250];");
    expect(source).toContain('window.addEventListener(event, clearRealign, { once: true, passive: true });');
  });

  it("drops the landing cue, its timers and its listeners on teardown", () => {
    expect(source).toContain("flashed?.classList.remove(\"gp-focus-flash\");");
    const teardown = source.slice(source.indexOf("return () => {\n      clearFlash();"));
    expect(teardown).toContain("clearRealign();");
    expect(teardown).toContain("window.removeEventListener(event, clearRealign);");
  });
});
