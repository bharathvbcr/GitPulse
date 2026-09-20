import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { MAX_SUSPECT_CONE_DEPTH } from "../codeintel/client";

const here = dirname(fileURLToPath(import.meta.url));
const panel = readFileSync(join(here, "RegressionSuspectsPanel.svelte"), "utf8");
const rust = readFileSync(
  join(here, "..", "..", "..", "src-tauri", "src", "codeintel", "suspects.rs"),
  "utf8",
);

describe("the depth bound the UI offers is the one the backend accepts", () => {
  it("mirrors MAX_CONE_DEPTH from the Rust module that enforces it", () => {
    // The backend REFUSES an out-of-range depth rather than clamping it, so a
    // UI that offers 12 is a control that fails on use. Parsed from the Rust
    // source rather than agreed by convention: the two constants are one
    // contract written twice, and nothing else would notice them diverging.
    const match = rust.match(/pub const MAX_CONE_DEPTH: u32 = (\d+);/);
    expect(match, "MAX_CONE_DEPTH not found in suspects.rs — did it move?").not.toBeNull();
    expect(Number(match![1])).toBe(MAX_SUSPECT_CONE_DEPTH);
  });

  it("bounds the input from the constant, not from a literal", () => {
    expect(panel).toContain("max={MAX_SUSPECT_CONE_DEPTH}");
    expect(panel).not.toContain('max="10"');
  });
});

describe("RegressionSuspectsPanel keeps refused apart from empty", () => {
  it("renders unavailable as its own state, named as not-zero", () => {
    // The whole surface exists to keep "the walk found nothing" apart from
    // "the walk never ran". Collapsing them is the one defect that would make
    // the pane actively misleading rather than merely unhelpful.
    expect(panel).toContain("{#if !available}");
    expect(panel).toContain("not the same as no suspects");
    expect(panel).toContain("{:else if suspects.length === 0}");
    expect(panel).toContain("The walk ran; it found");
  });

  it("does not show either state before the first run", () => {
    // Without this a freshly opened pane reads as "no suspects" for a query
    // nobody has asked yet.
    expect(panel).toContain("{#if ran}");
    expect(panel).toContain("let ran = $state(false)");
    // A thrown error is not a completed run.
    expect(panel).toMatch(/errorMsg = formatError\(err\);[\s\S]{0,200}ran = false;/);
  });

  it("says the window ends at the indexed commit, not at HEAD", () => {
    expect(panel).toContain("scope.indexed_head");
    expect(panel).toContain("The window ends at the indexed commit, not at HEAD.");
    expect(panel).toContain("scope.cone_size");
    expect(panel).toContain("scope.blamed_symbols");
  });

  it("leaves a missing scope out rather than drawing zeroes", () => {
    // `parseSuspectsPayload` keeps scope nullable precisely so a refused run
    // does not claim to have measured an empty cone; the pane has to honour it.
    expect(panel).toContain("{#if scope}");
    expect(panel).not.toContain("scope?.cone_size ?? 0");
  });

  it("surfaces every bound the answer came with", () => {
    expect(panel).toContain("scope.refusals.length > 0");
    expect(panel).toContain("lower bound");
    expect(panel).toContain("{#if truncated}");
    expect(panel).toContain("not ruled out");
    expect(panel).toContain("tooltipWalkIncomplete([walkIncomplete])");
    expect(panel).toContain("line-clamp-3");
    expect(panel).toContain("boundedJoin(scope.refusals");
  });

  it("never claims fewer rows than it lists", () => {
    expect(panel).toContain("Math.max(payload.response.total ?? 0, payload.response.items.length)");
  });
});

describe("the score is rendered as a rank, never as a probability", () => {
  it("scales the bar against the top hit in the same answer", () => {
    // The backend's own doc says the score is comparable only within one
    // answer. A percentage or a raw number invites comparison across runs.
    expect(panel).toContain("topScore > 0 ? Math.round((suspect.score / topScore) * 100) : 0");
    expect(panel).toContain("comparable only against other scores in this answer");
    expect(panel).not.toContain("{suspect.score}");
    expect(panel).not.toContain("suspect.score.toFixed");
  });

  it("guards the denominator so a zero-score answer does not divide by zero", () => {
    expect(panel).toContain("suspects.reduce((max, s) => Math.max(max, s.score), 0)");
  });
});

describe("three-state evidence survives to the screen", () => {
  it("renders 'could not tell' rather than folding it into unchanged", () => {
    // `body_changed: null` means the question could not be asked. Rendering it
    // as "unchanged" makes a reformat start looking like exculpatory evidence.
    expect(panel).toContain('if (changed === true) return "changed"');
    expect(panel).toContain('if (changed === false) return "moved"');
    expect(panel).toContain('return "could not tell"');
    expect(panel).toContain("touch.body_changed === null");
    expect(panel).toContain("not the same as unchanged");
  });

  it("keeps an unrecognised evidence class visible instead of blank", () => {
    // A backend that grows a fourth class must not render as an empty badge.
    expect(panel).toContain("return evidence;");
  });
});

describe("the pane is wired to the rest of History", () => {
  it("opens a suspect in the Diff lens, sharing the selection", () => {
    expect(panel).toContain("repoStore.selectCommitDiff(commit)");
    expect(panel).toContain('repoStore.setViewSection("history", "diff")');
  });

  it("drops the last answer when the repository changes", () => {
    // LazyView caches the pane, so it outlives a repository switch; stale
    // suspects under a new repository's name would be read as that repo's.
    expect(panel).toMatch(/\$effect\(\(\) => \{\s*void repoPath;/);
    expect(panel).toContain("ran = false;");
  });

  it("cancels an in-flight query rather than letting it land late", () => {
    expect(panel).toContain("inflight?.cancel()");
    expect(panel).toContain("createAsyncGuard()");
    expect(panel).toContain("if (!guard.isLive()) return");
  });

  it("refuses to run without a repository, a symptom and a base ref", () => {
    expect(panel).toContain("Boolean(repoPath) && symptom.trim().length > 0");
    expect(panel).toContain("since.trim().length > 0");
    expect(panel).toContain("disabled={!canRun}");
  });
});
