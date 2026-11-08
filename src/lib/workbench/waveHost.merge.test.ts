import { describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getTaskRun: vi.fn(),
  getRepository: vi.fn(),
  mergeVerifiedAttempt: vi.fn(),
}));

vi.mock("./client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./client")>()),
  getRepository: mocks.getRepository,
  getTaskRun: mocks.getTaskRun,
}));
vi.mock("./attemptWorktree", () => ({ mergeVerifiedAttempt: mocks.mergeVerifiedAttempt }));
vi.mock("./taskHandoff", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./taskHandoff")>()),
  checkoutCandidates: () => [],
  preferredCheckout: () => null,
  normalizeCheckout: () => "/repo",
}));

import { waveDeps } from "./waveHost";

async function deps() {
  mocks.getRepository.mockResolvedValue({});
  return waveDeps("repo-1", async () => {});
}

describe("the wave's merge", () => {
  it("verifies the candidate under the task the run belongs to, read from the run", async () => {
    mocks.getTaskRun.mockResolvedValue({ task_id: "task-from-run" });
    mocks.mergeVerifiedAttempt.mockResolvedValue({ message: "merged", beforeOid: "b0", afterOid: "b1" });
    const wave = await deps();
    // The member's own task id is not what the verification is filed under; the run's is.
    const landed = await wave.merge({ taskId: "member-task", runId: "run-1", cwd: "/wt/a", head: "h1" });
    expect(mocks.getTaskRun).toHaveBeenCalledWith("run-1");
    expect(mocks.mergeVerifiedAttempt).toHaveBeenCalledWith({ id: "run-1", cwd: "/wt/a", state: "exited", expires_at: 0 }, "h1", "task-from-run");
    expect(landed).toEqual({ message: "merged", beforeOid: "b0", afterOid: "b1" });
  });

  it("names no task when the run has none, so the host refuses an unverified candidate instead of guessing one", async () => {
    mocks.getTaskRun.mockResolvedValue({ task_id: "" });
    mocks.mergeVerifiedAttempt.mockResolvedValue({ message: "m", beforeOid: "", afterOid: "" });
    const wave = await deps();
    await wave.merge({ taskId: "member-task", runId: "run-2", cwd: "/wt/b", head: "h2" });
    expect(mocks.mergeVerifiedAttempt).toHaveBeenLastCalledWith(expect.anything(), "h2", undefined);
  });

  it("does not merge when the run cannot be read", async () => {
    mocks.mergeVerifiedAttempt.mockClear();
    mocks.getTaskRun.mockRejectedValue(new Error("run not found"));
    const wave = await deps();
    await expect(wave.merge({ taskId: "t", runId: "run-3", cwd: "/wt/c", head: "h3" })).rejects.toThrow("run not found");
    expect(mocks.mergeVerifiedAttempt).not.toHaveBeenCalled();
  });
});
