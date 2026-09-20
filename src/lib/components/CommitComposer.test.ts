import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CommitComposer from "./CommitComposer.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CommitComposer.svelte"),
  "utf8",
);

describe("CommitComposer", () => {
  it("renders the include-unstaged option and staged-only commit by default", () => {
    const { body } = render(CommitComposer);
    expect(body).toContain("Include unstaged");
    expect(body).toContain('aria-label="Include unstaged files in this commit"');
    expect(body).toContain("Amend");
    expect(body).toContain("Commit (0)");
    expect(body).not.toContain("Commit all");
  });

  it("offers include-unstaged as the quick-commit option", () => {
    expect(source).toContain("includeUnstaged");
    expect(source).toContain("repoStore.quickCommit");
    expect(source).toContain("Commit all");
    expect(source).toContain("onclick={() => void handleCommit()}");
  });

  it("keeps staged-only commit as the default path", () => {
    expect(source).toContain("repoStore.commit(message, isAmending)");
    expect(source).toContain("Amend");
  });

  it("routes Cmd/Ctrl+Enter through the composer and Shift to include unstaged", () => {
    expect(source).toContain("onMessageKeydown");
    expect(source).toContain("isImeComposition");
    expect(source).toContain('event.key !== "Enter"');
    expect(source).toContain("event.shiftKey");
    expect(source).toContain("handleCommit(true)");
    expect(source).toContain("forceQuick");
  });

  it("disables commit when conflicts are present", () => {
    expect(source).toContain("conflictedCount > 0");
  });

  it("previews staged edits via the shared codeintel preview store", () => {
    expect(source).toContain("previewStore.refresh");
    expect(source).toContain("What this commit breaks");
    expect(source).toContain("parse=");
    expect(source).toContain("bodies_not_compared=");
    expect(source).toContain("walk incomplete:");
    expect(source).toContain("tooltipWalkIncomplete");
    expect(source).toContain('data-testid="commit-preview-breaks"');
  });

  it("depends on the staged-path key, not the statuses array identity", () => {
    expect(source).toMatch(/import\s*\{[^}]*\buntrack\b[^}]*\}\s*from\s*"svelte"/);
    const start = source.indexOf("let previewPathsKey");
    expect(start).toBeGreaterThan(-1);
    const effect = source.slice(start, source.indexOf("A3: blast radius"));
    expect(effect).toContain("void previewPathsKey");
    expect(effect).toContain("untrack(() => {");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(
      effect.indexOf("previewStore.refresh"),
    );
  });

  it("composes staged blast radius with layered impact (no min_rung)", () => {
    expect(source).toContain("getImpactLayeredMany");
    expect(source).toContain("cancelCodeintelQuery");
    expect(source).toContain("newCodeintelCancelToken");
    expect(source).toContain("BlastRadiusPanel");
    expect(source).not.toContain("minRung");
  });

  it("caps staged blast fan-out and reports omitted seeds", () => {
    expect(source).toContain("capFanout");
    expect(source).toContain("omittedLayeredImpact");
  });

  it("summarizes each previewed file instead of printing the report struct", () => {
    // The rail already derived a verdict per file from markerForHonesty; this
    // panel re-rendered the raw fields beside it, so the two surfaces could
    // describe the same preview differently. One owner, both callers.
    expect(source).toContain("markerForHonesty(file)");
    expect(source).toContain("fileGlance(file)");
    // The telemetry is kept, but behind a disclosure rather than above the
    // finding it was meant to support.
    const summaryRegion = source.slice(0, source.indexOf("Preview details"));
    expect(summaryRegion).not.toContain("bodies_not_compared=");
    expect(source).toContain("bodies_not_compared=");
    expect(source).toContain("<details");
  });

  it("keeps each commit toggle's label on one line", () => {
    // "Include unstaged" broke across two lines inside its own label at
    // sidebar width and took the commit button's row height with it. Each
    // control refuses to wrap; the ROW wraps when it genuinely cannot fit.
    const footer = source.slice(source.indexOf("<!-- The two toggles"));
    expect(footer.length).toBeGreaterThan(0);
    expect(footer).toContain("flex-wrap");
    expect((footer.match(/whitespace-nowrap/g) ?? []).length).toBeGreaterThanOrEqual(3);
  });

  it("keeps the impact harness's stand-in commit footer in step with this one", () => {
    // harness/impact.html measures that footer's geometry against a copy.
    const host = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../../../harness/ImpactHost.svelte"),
      "utf8",
    );
    const standIn = host.slice(host.indexOf("data-impact-commit-footer"));
    expect(standIn).toContain("flex-wrap");
    expect(standIn).toContain("whitespace-nowrap");
    for (const label of ["Amend", "Include unstaged"]) {
      expect(standIn, `stand-in footer is missing ${label}`).toContain(label);
    }
  });
});
