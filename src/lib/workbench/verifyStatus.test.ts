import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "../ipc/invoke";
import { postVerifyResult, setVerifyStatus, verifyStatusEnabled } from "./verifyStatus";
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
