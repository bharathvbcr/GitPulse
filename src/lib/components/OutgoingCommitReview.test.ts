import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import OutgoingCommitReview, { outgoingState } from "./OutgoingCommitReview.svelte";
import type { BranchInfo } from "../branches/types";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "OutgoingCommitReview.svelte"), "utf8");
const panel = readFileSync(join(here, "GitHubPanel.svelte"), "utf8");

function branch(overrides: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name: "feature",
    is_current: true,
    is_remote: false,
    tip_commit_id: "abc",
    ahead_count: 0,
    behind_count: 0,
    upstream: "origin/feature",
    is_default: false,
    is_gone: false,
    last_commit_timestamp: 0,
    last_author: "a",
    last_summary: "s",
    commits_ahead_of_base: 0,
    commits_behind_base: 0,
    additions: 0,
    deletions: 0,
    files_changed: 0,
    ...overrides,
  };
}
const settled = { loading: false, error: null };

describe("outgoingState: when the card shows", () => {
  it("is 'ahead' with the count and upstream when the branch has unpushed commits", () => {
    expect(outgoingState(branch({ ahead_count: 3 }), settled)).toEqual({ kind: "ahead", count: 3, upstream: "origin/feature" });
  });

  it("is 'none' only when an upstream exists and nothing is ahead of it", () => {
    expect(outgoingState(branch(), settled)).toEqual({ kind: "none" });
  });

  it.each([
    ["no upstream", branch({ upstream: null })],
    ["a blank upstream", branch({ upstream: "  " })],
    ["a gone upstream", branch({ is_gone: true })],
  ])("is unknown, never zero, with %s", (_name, current) => {
    const state = outgoingState(current, settled);
    expect(state.kind).toBe("unknown");
  });

  it("is unknown without a branch row, and says whether the list is loading, failed or just empty", () => {
    expect(outgoingState(null, { loading: true, error: null })).toMatchObject({ kind: "unknown", reason: expect.stringContaining("Still reading") });
    expect(outgoingState(null, { loading: false, error: "boom" })).toMatchObject({ kind: "unknown", reason: expect.stringContaining("could not be read") });
    expect(outgoingState(null, settled)).toMatchObject({ kind: "unknown", reason: expect.stringContaining("No branch is checked out") });
  });

  it("trusts a branch row already read over a listing that is reloading", () => {
    expect(outgoingState(branch({ ahead_count: 2 }), { loading: true, error: "late" }).kind).toBe("ahead");
  });
});

describe("OutgoingCommitReview", () => {
  it("renders titled and described, with the Review messages action, when the count is unknown", () => {
    const { body } = render(OutgoingCommitReview);
    expect(body).toContain("Outgoing commits");
    expect(body).toContain("Conventional format, duplicate subjects, issue links and length, checked before you push.");
    expect(body).toContain("Review messages");
    expect(body).toContain("check for unpushed commits");
  });

  it("reads the ahead count from the current local branch's row, as the sidebar does", () => {
    expect(source).toContain("const branchesStore = repoStore.branches;");
    expect(source).toContain("$branchesStore.find((b) => b.is_current && !b.is_remote)");
    // The whole-store array reads are budgeted (repoStoreEffects.contract); this adds none.
    expect(source).not.toMatch(/\$repoStore\.branches\s*\.\s*find/);
  });

  it("reviews through cmd_review_outgoing_commits and discards a review on a repository switch", () => {
    expect(source).toContain('invoke<CommitReviewReport>("cmd_review_outgoing_commits", { repoPath: repo })');
    expect(source).toContain("if ($repoStore.currentPath === repo) review = next;");
    expect(source).toContain("if (repo !== reviewedRepo) {");
    expect(source).toContain("summarizeCommitReview(review)");
  });

  it("is mounted by GitHubPanel before, and outside, the GitHub availability branch", () => {
    const at = panel.indexOf("<OutgoingCommitReview />");
    const chain = panel.indexOf("{#if (loading && !ctx) || (workflowsLoading && !workflows)}");
    expect(at).toBeGreaterThan(0);
    expect(at).toBeLessThan(chain);
    // Nothing between the mount and the availability chain opens a branch that could hide it.
    expect(panel.slice(at, chain)).not.toMatch(/\{#if|\{:else/);
  });
});
