import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { MAX_SUSPECT_CONE_DEPTH } from "../codeintel/client";

const here = dirname(fileURLToPath(import.meta.url));
const list = readFileSync(join(here, "SuspectsList.svelte"), "utf8");
const bar = readFileSync(join(here, "SuspectsQueryBar.svelte"), "utf8");
const details = readFileSync(join(here, "CommitDetails.svelte"), "utf8");
const query = readFileSync(join(here, "..", "codeintel", "suspectsQuery.ts"), "utf8");
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
    expect(bar).toContain("max={MAX_SUSPECT_CONE_DEPTH}");
    expect(bar).not.toContain('max="10"');
  });
});

describe("SuspectsList keeps refused apart from empty", () => {
  it("renders unavailable as its own state, named as not-zero", () => {
    // The whole surface exists to keep "the walk found nothing" apart from
    // "the walk never ran". Collapsing them is the one defect that would make
    // the list actively misleading rather than merely unhelpful.
    expect(list).toContain("{#if !answer.available}");
    expect(list).toContain("not the same as no suspects");
    expect(list).toContain("{:else if suspects.length === 0}");
    expect(list).toContain("The walk ran; it found");
  });

  it("does not show either state before the first run", () => {
    // Without this a freshly opened list reads as "no suspects" for a query
    // nobody has asked yet. `answer` is null until a run completes, and a
    // thrown error leaves it null (held by suspectsQuery.test.ts).
    expect(list).toContain("{#if !answer}");
    expect(list).toContain("Which commit broke it?");
  });

  it("says the window ends at the indexed commit, not at HEAD", () => {
    expect(list).toContain("scope.indexed_head");
    expect(list).toContain("The window ends at the indexed commit, not at HEAD.");
    expect(list).toContain("scope.cone_size");
    expect(list).toContain("scope.blamed_symbols");
  });

  it("leaves a missing scope out rather than drawing zeroes", () => {
    // `parseSuspectsPayload` keeps scope nullable precisely so a refused run
    // does not claim to have measured an empty cone; the list has to honour it.
    expect(list).toContain("{#if scope}");
    expect(list).not.toContain("scope?.cone_size ?? 0");
  });

  it("surfaces every bound the answer came with", () => {
    expect(list).toContain("scope.refusals.length > 0");
    expect(list).toContain("lower bound");
    expect(list).toContain("{#if answer.truncated}");
    expect(list).toContain("not ruled out");
    expect(list).toContain("tooltipWalkIncomplete([answer.walkIncomplete])");
    expect(list).toContain("line-clamp-3");
    expect(list).toContain("boundedJoin(scope.refusals");
  });

  it("never claims fewer rows than it lists", () => {
    expect(query).toContain("Math.max(response.total ?? 0, response.items.length)");
    expect(list).toContain("{suspects.length} of {answer.total.toLocaleString()}");
  });

  it("only draws this repository's answer", () => {
    expect(list).toContain("$suspectsQuery.repoPath === repoPath ? $suspectsQuery.answer : null");
  });
});

describe("the score is rendered as a rank, never as a probability", () => {
  it("scales the bar against the top hit in the same answer", () => {
    // The backend's own doc says the score is comparable only within one
    // answer. A percentage or a raw number invites comparison across runs.
    expect(list).toContain("relativeScoreWidth(suspect.score, topScore)");
    expect(list).toContain("comparable only against other scores in this answer");
    expect(list).not.toContain("{suspect.score}");
    expect(list).not.toContain("suspect.score.toFixed");
  });
});

describe("three-state evidence survives to the screen", () => {
  it("renders 'could not tell' rather than folding it into unchanged", () => {
    // `body_changed: null` means the question could not be asked. Rendering it
    // as "unchanged" makes a reformat start looking like exculpatory evidence.
    expect(list).toContain("bodyChangedLabel(touch.body_changed)");
    expect(list).toContain("touch.body_changed === null");
    expect(list).toContain("not the same as unchanged");
  });
});

describe("the list is one of Graph's lists", () => {
  it("selects a suspect in place, pinned, and highlights it", () => {
    // It used to jump to the Diff section, so the ranking and the commit it
    // named were never on screen together.
    expect(list).toContain("repoStore.selectHistoryCommit(commit, {");
    expect(list).toContain("pin: true");
    expect(list).not.toContain("setViewSection");
    expect(list).toContain("aria-pressed={selected}");
    expect(list).toContain("$graphStore.selectedCommit?.id");
  });

  it("asks its question from the context bar, through the shared store", () => {
    expect(bar).toContain("suspectsQuery.run(repoPath)");
    expect(bar).toContain("disabled={!canRun}");
    expect(bar).toContain("canRunSuspects($suspectsQuery, repoPath)");
    // ⌘F focuses the symptom on this list, where the field is not the commit
    // filter (App routes the chord through findChordTarget).
    expect(bar).toContain("window.addEventListener(FOCUS_SUSPECTS_SYMPTOM_EVENT, focusSymptom)");
  });

  it("puts its verdict and its way in on the details pane", () => {
    // The verdict travels with the commit whichever list selected it, and any
    // commit's details can start a question from itself.
    expect(details).toContain("suspectMarks($suspectsQuery, $currentRepoPath).get(selectedCommit.id)");
    expect(details).toContain('aria-label="Suspect verdict"');
    expect(details).toContain('repoStore.setHistoryList("suspects")');
    expect(details).toContain("suspectsQuery.seedSince($currentRepoPath, commitId)");
  });
});
