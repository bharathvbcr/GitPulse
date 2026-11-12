import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { functionBody } from "../stores/sourceScan";
import { render } from "svelte/server";
import BranchTagCleanup from "./BranchTagCleanup.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "BranchTagCleanup.svelte"),
  "utf8",
);

describe("BranchTagCleanup tag retention", () => {
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
    expect(source).toContain("{tagPlan.uncompared_tags} couldn't be compared");
  });

  it("pre-selects no tags, unlike merged branches", () => {
    // A tag is usually kept deliberately, so deletion is a per-row choice.
    const scan = functionBody(source, "scanTags");
    expect(scan).toContain("selectedTags = [];");
    expect(source).toContain("selectedBranches = next.candidates.map((candidate) => candidate.name);");
  });
});

describe("BranchTagCleanup rendering", () => {
  it("renders the tag retention card, not just its source", () => {
    // A source-text assertion cannot tell a card that mounts from one that
    // throws mid-render. This is the render path.
    const { body } = render(BranchTagCleanup, { props: { open: true } });
    expect(body).toContain("Tag retention");
    expect(body).toContain("Build tag plan");
    expect(body).toContain("Scan branches");
    expect(body).toContain("Stale branches…");
  });

  it("is present and collapsed by default, whatever a scan would find", () => {
    // A manual scan that hid itself when nothing was found would read as
    // "checked, clean" with no check made.
    const { body } = render(BranchTagCleanup, { props: {} });
    expect(body).toContain("Branch and tag cleanup");
    expect(body).toContain('aria-expanded="false"');
    expect(body).not.toContain("Tag retention");
  });

  it("shows no count before a scan has run", () => {
    const { body } = render(BranchTagCleanup, { props: { open: true } });
    expect(body).not.toContain('data-kind="count"');
    expect(source).toContain("{#if cleanup}");
    expect(source).toContain("{#if tagPlan}");
  });

  it("opens the existing stale-branch dialog through its event", () => {
    expect(functionBody(source, "openStaleBranches")).toContain('"gitpulse:branch-cleanup"');
  });

  it("anchors the card for deep links and takes focus with the scroll", () => {
    const anchor = 'id={manviSectionId("cleanup")}';
    expect(source).toContain(anchor);
    expect(source.slice(source.indexOf(anchor)).slice(0, 200)).toContain('tabindex="-1"');
  });
});

describe("BranchTagCleanup plans belong to one repository", () => {
  it("drops both plans and every selection when the repository changes", () => {
    // A tag plan kept from repository A would offer deletions by name in B.
    const reset = source.slice(source.indexOf("void $currentRepoPath;"));
    for (const line of [
      "cleanup = null;",
      "selectedBranches = [];",
      "tagPlan = null;",
      "selectedTags = [];",
    ]) {
      expect(reset, line).toContain(line);
    }
    expect(source).toContain("untrack(() => {");
  });
});

describe("BranchTagCleanup cleanup runs against the repository it asked about", () => {
  it.each([
    ["cleanTags", "repoStore.deleteTag(name, target)"],
    ["cleanBranches", "repoStore.deleteBranch(candidate.name, false, target)"],
  ])("%s names its repository before the confirm and stops on a refusal", (name, call) => {
    const body = functionBody(source, name);
    expect(body.indexOf("repoStore.captureTarget()")).toBeGreaterThan(-1);
    expect(body.indexOf("repoStore.captureTarget()")).toBeLessThan(body.indexOf("await askConfirm("));
    expect(body).toContain(call);
    expect(body).toContain("outcome.refused");
    expect(body).not.toMatch(/\$repoStore\.currentPath !== repo\) break/);
    expect(body).not.toMatch(/\$currentRepoPath !== repo\) break/);
  });

  it("deletes tree-merged branches through the gated clean with a backup, never -d or -D by hand", () => {
    const body = functionBody(source, "cleanBranches");
    expect(body).toContain("splitBranchCleanup(selected, selectedBranches)");
    expect(body).toContain("repoStore.deadbranchClean(");
    expect(body).toMatch(/byTree\.map\(\(c\) => c\.name\),\s*false,\s*true,\s*target/);
    expect(body).not.toContain("deleteBranch(candidate.name, true");
  });

  it("offers every Undo through the one owner and never restores by hand", () => {
    expect(functionBody(source, "cleanBranches")).toContain("offerBranchUndo(");
    expect(source).not.toContain("repoStore.createBranch(");
    expect(source).not.toContain("Restored ${restoredTips.length}");
  });

  it("releases busy on every path, and never starts a scan while another operation runs", () => {
    expect(source.match(/busy = null;/g)?.length).toBeGreaterThanOrEqual(4);
    expect(functionBody(source, "scanBranches")).toContain("busy !== null");
    expect(functionBody(source, "scanTags")).toContain("busy !== null");
  });

  it("deletes only after the confirm", () => {
    for (const name of ["cleanTags", "cleanBranches"]) {
      const body = functionBody(source, name);
      expect(body, name).toContain("if (!confirmed)");
      expect(body.indexOf("if (!confirmed)"), name).toBeLessThan(body.indexOf("repoStore.delete"));
    }
  });
});
