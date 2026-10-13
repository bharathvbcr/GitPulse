import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const recordAction = vi.fn();
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("../stores/harnessStore", () => ({ harnessStore: { recordAction: (...a: unknown[]) => recordAction(...a) } }));

import { MANVI_ACTION_GRACE_MS, MANVI_ACTION_TIMEOUT_SECS, runManviAction } from "./manviAction";

const request = {
  repoPath: "/repo",
  argv: ["npm", "test"],
  actionKind: "coverage",
  journalKind: "coverage-step",
  label: "npm test",
};
const result = (over: Record<string, unknown> = {}) => ({
  command: "npm test", gated: false, policy: null, timed_out: false, exit_code: 0,
  stdout_tail: "", stderr_tail: "", truncated: false, truncation_reason: null, duration_ms: 5, ...over,
});

describe("runManviAction", () => {
  beforeEach(() => { invoke.mockReset(); recordAction.mockReset(); });

  it("sends one bounded call with the shared timeout and the caller's action kind", async () => {
    invoke.mockResolvedValue(result());
    await runManviAction(request);
    expect(invoke).toHaveBeenCalledWith(
      "cmd_manvi_run_action",
      { repoPath: "/repo", args: ["npm", "test"], actionKind: "coverage", timeoutSecs: MANVI_ACTION_TIMEOUT_SECS },
      { deadlineMs: MANVI_ACTION_TIMEOUT_SECS * 1000 + MANVI_ACTION_GRACE_MS },
    );
  });

  it("journals the outcome once, with the verdict, and a non-zero exit as not ok", async () => {
    invoke.mockResolvedValue(result({ exit_code: 2, policy: { decision: "allow" } }));
    const res = await runManviAction(request);
    expect(res.exit_code).toBe(2);
    expect(recordAction).toHaveBeenCalledTimes(1);
    expect(recordAction).toHaveBeenCalledWith({
      repoPath: "/repo", kind: "coverage-step", label: "npm test", ok: false, verdict: { decision: "allow" },
    });
  });

  it("a timed-out command is not ok", async () => {
    invoke.mockResolvedValue(result({ timed_out: true, exit_code: null }));
    await runManviAction(request);
    expect(recordAction.mock.calls[0][0].ok).toBe(false);
  });

  it("journals a failure once and rethrows when the call rejects", async () => {
    const boom = new Error("ipc down");
    invoke.mockRejectedValue(boom);
    await expect(runManviAction(request)).rejects.toBe(boom);
    expect(recordAction).toHaveBeenCalledTimes(1);
    expect(recordAction).toHaveBeenCalledWith({ repoPath: "/repo", kind: "coverage-step", label: "npm test", ok: false });
  });
});
