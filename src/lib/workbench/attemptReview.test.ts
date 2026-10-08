import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "../ipc/invoke";
import { readAttemptReview, recordAttemptReview, reviewSummary, setReviewGate, type AttemptReview } from "./attemptReview";
vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const view: AttemptReview = { run_id: "run", branch: "gitpulse/fix-1234abcd", base_oid: "a".repeat(40), head_oid: "b".repeat(40), files_changed: 2, ended: true, review: { status: "unreviewed" }, gate_on: true, gate_error: null };
beforeEach(() => native.mockReset());
describe("attempt review", () => {
  it("names only the run and the decision, and never sends an empty note", async () => {
    native.mockResolvedValue(view);
    await readAttemptReview("run");
    expect(native).toHaveBeenLastCalledWith("cmd_attempt_review", { runId: "run" });
    await recordAttemptReview("run", "approve", "  ");
    expect(native).toHaveBeenLastCalledWith("cmd_attempt_review_record", { runId: "run", decision: "approve", note: null });
    await recordAttemptReview("run", "merge_unreviewed", " hotfix ");
    expect(native).toHaveBeenLastCalledWith("cmd_attempt_review_record", { runId: "run", decision: "merge_unreviewed", note: "hotfix" });
    await setReviewGate("/repo", true);
    expect(native).toHaveBeenLastCalledWith("cmd_review_gate_save", { repoPath: "/repo", enabled: true });
  });
  it("says which range a review covers and when it went stale", () => {
    expect(reviewSummary(view)).toBe(`Not reviewed: gitpulse/fix-1234abcd @ ${"b".repeat(12)} (2 files since ${"a".repeat(12)}).`);
    expect(reviewSummary({ ...view, ended: false })).toMatch(/^Reviewable once the attempt ends/);
    expect(reviewSummary({ ...view, review: { status: "stale", reviewed_head: "c".repeat(40) } })).toMatch(/Approved at c{12}, but the branch has moved/);
    expect(reviewSummary({ ...view, review: { status: "changes_requested", note: "add a test" } })).toMatch(/Note: add a test$/);
  });
});
