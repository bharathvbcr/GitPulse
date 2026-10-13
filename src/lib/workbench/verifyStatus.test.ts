import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "../ipc/invoke";
import { POST_CONFIRM, postAvailability, postedSummary, postVerifyResult, setVerifyStatus, verifyStatusEnabled, type PostedVerifyStatus } from "./verifyStatus";
vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
beforeEach(() => native.mockReset());
describe("verify status", () => {
  it("never comments or posts for real unless asked", async () => {
    native.mockResolvedValue({});
    await postVerifyResult("/repo", "task");
    expect(native).toHaveBeenLastCalledWith("cmd_verify_status_post", { repoPath: "/repo", taskId: "task", comment: false, dryRun: false });
    await postVerifyResult("/repo", "task", { comment: true, dryRun: true });
    expect(native).toHaveBeenLastCalledWith("cmd_verify_status_post", { repoPath: "/repo", taskId: "task", comment: true, dryRun: true });
  });
  it("reads and saves the per-repository opt-in", async () => {
    native.mockResolvedValue(false);
    await expect(verifyStatusEnabled("/repo")).resolves.toBe(false);
    expect(native).toHaveBeenLastCalledWith("cmd_verify_status_enabled", { repoPath: "/repo" });
    await setVerifyStatus("/repo", true);
    expect(native).toHaveBeenLastCalledWith("cmd_verify_status_save", { repoPath: "/repo", enabled: true });
  });
  it("lets a refusal reach the caller instead of reading as success", async () => {
    native.mockRejectedValueOnce({ code: "verify_status_disabled", message: "posting is off for this repository" });
    await expect(postVerifyResult("/repo", "task")).rejects.toMatchObject({ code: "verify_status_disabled" });
  });
});

describe("when the sheet may post", () => {
  it("is enabled only by an opt-in that was read as true", () => {
    expect(postAvailability(true, "", "/repo")).toEqual({ allowed: true, reason: "" });
    for (const [enabled, error, path] of [[false, "", "/repo"], [null, "", "/repo"], [true, "denied", "/repo"], [true, "", "  "]] as const) {
      const out = postAvailability(enabled, error, path);
      expect(out.allowed).toBe(false);
      expect(out.reason.length).toBeGreaterThan(0);
    }
  });
  it("asks before anything is sent and names what it sends", () => {
    expect(POST_CONFIRM.title).toBe("Post verify result?");
    expect(POST_CONFIRM.message).toContain("devcouncil/verify");
    expect(POST_CONFIRM.message).toContain("never as a pass");
  });
  it("says what a post did, and that a dry result sent nothing", () => {
    const base: PostedVerifyStatus = { sha: "0123456789abcdef", state: "success", description: "", dry_run: false, status_posted: true, commented: [4, 9], comment_note: null, would_run: [] };
    expect(postedSummary(base)).toBe("Posted success for 0123456 and commented on #4, #9.");
    expect(postedSummary({ ...base, status_posted: false, commented: [] })).toBe("Nothing was posted for 0123456.");
  });
});
