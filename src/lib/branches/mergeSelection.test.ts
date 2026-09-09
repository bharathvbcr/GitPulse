import { describe, expect, it } from "vitest";
import { mergeBlockedReason, mergeCandidates, mergeRef, type MergeRequest } from "./mergeSelection";
import type { BranchInfo } from "./types";
import { IDLE_OPERATION } from "../repos/operation";

const branch = (name: string, remote = false): BranchInfo => ({
  name, is_remote: remote, is_current: false, tip_commit_id: "abc1234", ahead_count: 0,
  behind_count: 0, is_default: false, is_gone: false, last_commit_timestamp: 0,
  last_author: "", last_summary: "", commits_ahead_of_base: 0, commits_behind_base: 0,
  additions: 0, deletions: 0, files_changed: 0,
});
const request: MergeRequest = { repoPath: "/repo", targetBranch: "main", sourceRef: "refs/heads/topic", ffOnly: false };
const ready = { currentPath: "/repo", currentBranch: "main", isBare: false, isLoading: false, operation: IDLE_OPERATION };

describe("merge selection", () => {
  it("places the exact branch before fuzzy matches so a selected branch can be revealed", () => {
    const branches = [...Array.from({ length: 105 }, (_, i) => branch(`a${i}/z`)), branch("z")];
    expect(mergeCandidates(branches, "main", " z ")[0].name).toBe("z");
    expect(mergeCandidates([branch("Z"), branch("z")], "main", "z")[0].name).toBe("z");
  });

  it("preserves the selected remote and disambiguates local branches from tags", () => {
    expect(mergeRef(branch("topic"))).toBe("refs/heads/topic");
    expect(mergeRef({ ...branch("upstream/feature/topic", true), remote_name: "upstream" })).toBe("refs/remotes/upstream/feature/topic");
  });
  it("excludes the destination and symbolic remote HEAD, retaining remote counterparts", () => {
    const branches = [branch("main"), branch("origin/main", true), branch("origin/HEAD", true), { ...branch("topic"), is_current: true }];
    expect(mergeCandidates(branches, "main").map(b => b.name)).toEqual(["origin/main"]);
  });
  it("deduplicates by ref, keeping a local and remote branch with the same display name", () => {
    const branches = [branch("origin/topic", true), branch("origin/topic"), branch("origin/topic"), branch("")];
    expect(mergeCandidates(branches, "main").map(mergeRef)).toEqual(["refs/heads/origin/topic", "refs/remotes/origin/topic"]);
  });
  it("searches full paths case-insensitively without mutating the input", () => {
    const branches = [branch("zeta"), branch("upstream/feature/topic", true), branch("alpha")];
    expect(mergeCandidates(branches, "main", "  UPSTREAM/TOPIC ").map(b => b.name)).toEqual(["upstream/feature/topic"]);
    expect(branches[0].name).toBe("zeta");
    expect(mergeCandidates(branches, "main", "missing")).toEqual([]);
    expect(mergeCandidates([], "main")).toEqual([]);
  });
  it("only allows a ready working copy with the reviewed destination", () => {
    expect(mergeBlockedReason(ready, request)).toBeNull();
    for (const patch of [
      { currentPath: null }, { currentPath: "/other" }, { currentBranch: null },
      { currentBranch: "other" }, { isBare: true }, { isLoading: true },
      { operation: { operation: null, probeFailed: true } },
      { operation: { probeFailed: false, operation: { kind: "Merge" as const, current_step: null, total_steps: null, head_ref: "main", incoming_ref: "topic", conflicted_paths: [], conflicted_total: 0, available: [] } } },
    ]) expect(mergeBlockedReason({ ...ready, ...patch }, request)).toBeTruthy();
  });
});
