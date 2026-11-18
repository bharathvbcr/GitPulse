import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getRepository: vi.fn(),
  getTask: vi.fn(),
  getTaskRun: vi.fn(),
  listTasks: vi.fn(),
  prepareTaskRun: vi.fn(),
  putTask: vi.fn(),
  request: vi.fn(),
  suggestTasks: vi.fn(),
  getVerification: vi.fn(),
  runVerification: vi.fn(),
  startPreparedAttempt: vi.fn(),
  getCollisionRisk: vi.fn(),
  invoke: vi.fn(),
  checkTasksReadiness: vi.fn(),
  loadTaskScopes: vi.fn(),
  findActiveWave: vi.fn(),
  createWavePersister: vi.fn(),
  startWave: vi.fn(),
  resumeWave: vi.fn(),
  mergeVerifiedAttempt: vi.fn(),
  normalizeCheckout: vi.fn(),
}));

vi.mock("./client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./client")>()),
  getRepository: mocks.getRepository,
  getTask: mocks.getTask,
  getTaskRun: mocks.getTaskRun,
  listTasks: mocks.listTasks,
  prepareTaskRun: mocks.prepareTaskRun,
  putTask: mocks.putTask,
  request: mocks.request,
  suggestTasks: mocks.suggestTasks,
}));
vi.mock("./verification", () => ({ getVerification: mocks.getVerification, runVerification: mocks.runVerification }));
vi.mock("./taskTerminal", () => ({ startPreparedAttempt: mocks.startPreparedAttempt }));
vi.mock("../insights/client", () => ({ getCollisionRisk: mocks.getCollisionRisk }));
vi.mock("../ipc/invoke", () => ({ invoke: mocks.invoke }));
vi.mock("./readiness", () => ({ checkTasksReadiness: mocks.checkTasksReadiness }));
vi.mock("../tasks/scopes", () => ({ loadTaskScopes: mocks.loadTaskScopes }));
vi.mock("./waveStore", () => ({ findActiveWave: mocks.findActiveWave, createWavePersister: mocks.createWavePersister }));
vi.mock("./waveLaunch", () => ({ startWave: mocks.startWave, resumeWave: mocks.resumeWave }));
vi.mock("./attemptWorktree", () => ({ mergeVerifiedAttempt: mocks.mergeVerifiedAttempt }));
vi.mock("./taskHandoff", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./taskHandoff")>()),
  checkoutCandidates: () => [],
  preferredCheckout: () => null,
  normalizeCheckout: mocks.normalizeCheckout,
}));

import { WorkbenchError } from "./client";
import { beginWave, currentWave, resumeActiveWave, taskApi, waveDeps } from "./waveHost";
import type { WaveDeps, WaveRunner } from "./waveLaunch";

const live = () => new AbortController().signal;

async function build(maxPriority?: number): Promise<WaveDeps> {
  return waveDeps("repo-1", async () => {}, maxPriority);
}

const runner = (state: "running" | "done"): WaveRunner =>
  ({ record: () => ({ state }) }) as unknown as WaveRunner;

beforeEach(() => {
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.getRepository.mockResolvedValue({ id: "repo-1", revision: 4 });
  mocks.normalizeCheckout.mockReturnValue("/repo");
});

describe("waveDeps when no checkout is open", () => {
  it("refuses to build, naming why, rather than launching from nowhere", async () => {
    mocks.normalizeCheckout.mockReturnValue("");
    await expect(waveDeps("repo-1", async () => {})).rejects.toThrow(/nowhere to start from/);
  });
});

describe("taskApi", () => {
  it("saves a card at the revision it was read at, as a task write", async () => {
    mocks.putTask.mockResolvedValue({ id: "t1" });
    const saved = await taskApi.saveTask("t1", 7, { title: "T" } as never);
    expect(saved).toEqual({ id: "t1" });
    expect(mocks.putTask).toHaveBeenCalledWith(expect.objectContaining({ id: "t1", expected_revision: 7, title: "T" }));
  });

  it("lists the global board by status and cursor, a hundred at a time", async () => {
    mocks.listTasks.mockResolvedValue({ items: [], has_more: false, next_cursor: null });
    await taskApi.listCards("backlog", "c1");
    expect(mocks.listTasks).toHaveBeenCalledWith({ kind: "global" }, "backlog", "", "c1", 100);
  });
});

describe("the wave's reads", () => {
  it("asks Manvi for the board's repository, under the person's priority filter, and drops blocked tasks", async () => {
    mocks.suggestTasks.mockResolvedValue({
      suggestions: [{ id: "a", title: "A" }, { id: "b", title: "B" }],
      blocked: [{ id: "a" }],
      board: { scanned: 2, complete: true, unhydrated: 0 },
      excluded: { unread: 0 },
      verification: { unchecked: 3 },
    });
    const deps = await build(1);
    const read = await deps.suggest(live());
    expect(mocks.suggestTasks).toHaveBeenCalledWith(expect.objectContaining({ repository_id: "repo-1", max_priority: 1 }));
    expect(read).toEqual({ candidates: [{ id: "b", title: "B" }], unread: "", unchecked: 3 });
  });

  it("flattens each task's planned and agent-appended files into one list keyed by id", async () => {
    mocks.loadTaskScopes.mockResolvedValue({
      a: { id: "a", planned_files: ["x.ts"], agent_appended_files: ["y.ts"] },
      b: { id: "b", planned_files: [], agent_appended_files: [] },
    });
    const deps = await build();
    const scopes = await deps.scopes(["a", "b"], live());
    expect(mocks.loadTaskScopes).toHaveBeenCalledWith(mocks.invoke, "/repo", ["a", "b"]);
    expect(scopes).toEqual(new Map([["a", ["x.ts", "y.ts"]], ["b", []]]));
  });

  it("turns the collision risk into held paths", async () => {
    mocks.getCollisionRisk.mockResolvedValue({
      file_claims: [{ source: "declared", run_id: "R", path: "a.rs", worktree: "/o", ts_ms: 1 }],
    });
    const deps = await build();
    expect(await deps.heldPaths!(live())).toEqual({ held: [{ path: "a.rs", holder: "run R" }] });
    expect(mocks.getCollisionRisk).toHaveBeenCalledWith("/repo");
  });

  it("says claims were unread when the read fails, and rethrows when the wave was stopped", async () => {
    mocks.getCollisionRisk.mockRejectedValue(new Error("claims offline"));
    const deps = await build();
    const read = await deps.heldPaths!(live());
    expect(read.held).toEqual([]);
    expect(read.unreadable).toContain("claims offline");

    const stopped = new AbortController();
    stopped.abort();
    await expect(deps.heldPaths!(stopped.signal)).rejects.toBeDefined();
  });

  it("passes readiness through, marks every task unready when the read fails, and rethrows on a stop", async () => {
    const answer = new Map([["a", { ok: true as const, readiness: {} as never }]]);
    mocks.checkTasksReadiness.mockResolvedValueOnce(answer);
    const deps = await build();
    expect(await deps.readiness!(["a"], live())).toBe(answer);

    mocks.checkTasksReadiness.mockRejectedValueOnce(new Error("readiness offline"));
    const failed = await deps.readiness!(["a", "b"], live());
    expect([...failed.keys()]).toEqual(["a", "b"]);
    for (const check of failed.values()) {
      expect(check.ok).toBe(false);
      expect(check.ok === false && check.error).toContain("readiness offline");
    }

    mocks.checkTasksReadiness.mockRejectedValueOnce(new Error("x"));
    const stopped = new AbortController();
    stopped.abort();
    await expect(deps.readiness!(["a"], stopped.signal)).rejects.toBeDefined();
  });
});

describe("launching a member", () => {
  const run = (over = {}) => ({ state: "running", exit_code: null, cwd: "/wt/a", reason: "", ...over });

  it("prepares a managed worktree run from the task as it stands now and reports the started run", async () => {
    mocks.getTask.mockResolvedValue({ id: "t1", revision: 9 });
    mocks.prepareTaskRun.mockResolvedValue({ id: "prepared" });
    mocks.startPreparedAttempt.mockResolvedValue({ ok: true, run: run() });
    const deps = await build();
    const read = await deps.launch("t1", { runId: "run-1", requestId: "req-1" }, live());
    expect(mocks.prepareTaskRun).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "managed", id: "run-1", request_id: "req-1", task_id: "t1", source_revision: 9,
        repository_id: "repo-1", repository_revision: 4, repo_path: "/repo", worktree: true,
        acknowledge_bypass: false,
      }),
    );
    // A bypass is never carried into a wave: it is acknowledged afresh on every launch.
    expect(mocks.prepareTaskRun.mock.calls[0][0].permission_mode).not.toBe("bypass");
    expect(mocks.startPreparedAttempt).toHaveBeenCalledWith({ id: "prepared" });
    expect(read).toEqual({ state: "running", exitCode: null, cwd: "/wt/a", reason: "" });
  });

  it("throws the host's reason when the prepared run would not start", async () => {
    mocks.getTask.mockResolvedValue({ id: "t1", revision: 1 });
    mocks.prepareTaskRun.mockResolvedValue({ id: "prepared" });
    mocks.startPreparedAttempt.mockResolvedValue({ ok: false, run: run(), error: "no terminal" });
    const deps = await build();
    await expect(deps.launch("t1", { runId: "r", requestId: "q" }, live())).rejects.toThrow("no terminal");
  });

  it("reads a run, treating one the store no longer holds as absent and any other failure as a failure", async () => {
    mocks.getTaskRun.mockResolvedValueOnce(run({ state: "exited", exit_code: 0 }));
    const deps = await build();
    expect(await deps.getRun("r1", live())).toEqual({ state: "exited", exitCode: 0, cwd: "/wt/a", reason: "" });
    mocks.getTaskRun.mockRejectedValueOnce(new WorkbenchError("not_found", "gone"));
    expect(await deps.getRun("r1", live())).toBeNull();
    mocks.getTaskRun.mockRejectedValueOnce(new WorkbenchError("transport_error", "down"));
    await expect(deps.getRun("r1", live())).rejects.toThrow("down");
  });
});

describe("verifying a member", () => {
  it("starts a plain verification and reports the first settled verdict with its head", async () => {
    mocks.runVerification.mockResolvedValue(undefined);
    mocks.getVerification.mockResolvedValue({ state: "passed", stale: false, record: { head: "h1" } });
    const deps = await build();
    expect(await deps.verify("t1", live())).toEqual({ state: "passed", stale: false, head: "h1" });
    expect(mocks.runVerification).toHaveBeenCalledWith("t1");
    expect(mocks.request).not.toHaveBeenCalled();
  });

  it("reports no head when the verdict carries no record", async () => {
    mocks.getVerification.mockResolvedValue({ state: "could_not_run", stale: true, record: null });
    const deps = await build();
    expect(await deps.verify("t1", live())).toEqual({ state: "could_not_run", stale: true, head: null });
  });

  it("judges a re-verify after a merge over the given base, and refuses a non-object answer", async () => {
    mocks.request.mockResolvedValueOnce({ ok: true });
    mocks.getVerification.mockResolvedValue({ state: "passed", stale: false, record: { head: "tip" } });
    const deps = await build();
    await deps.verify("t1", live(), { base: "b0", head: "tip" });
    expect(mocks.request).toHaveBeenCalledWith("verifications.run", { id: "t1", base: "b0" });
    expect(mocks.runVerification).not.toHaveBeenCalled();

    mocks.request.mockResolvedValueOnce(null);
    await expect(deps.verify("t1", live(), { base: "b0", head: "tip" })).rejects.toThrow(/invalid verification/);
  });

  it("waits through a running verdict and one for an earlier head, and gives up waiting once the wave is stopped", async () => {
    vi.useFakeTimers();
    try {
      mocks.request.mockResolvedValue({ ok: true });
      mocks.getVerification
        .mockResolvedValueOnce({ state: "running", stale: false, record: null })
        .mockResolvedValueOnce({ state: "passed", stale: false, record: { head: "old" } })
        .mockResolvedValueOnce({ state: "passed", stale: false, record: { head: "tip" } });
      const deps = await build();
      const pending = deps.verify("t1", live(), { base: "b0", head: "tip" });
      await vi.advanceTimersByTimeAsync(3_000);
      await vi.advanceTimersByTimeAsync(3_000);
      expect(await pending).toEqual({ state: "passed", stale: false, head: "tip" });
      expect(mocks.getVerification).toHaveBeenCalledTimes(3);

      mocks.getVerification.mockReset();
      mocks.getVerification.mockResolvedValue({ state: "running", stale: false, record: null });
      const stop = new AbortController();
      const stopped = deps.verify("t2", stop.signal);
      const assertion = expect(stopped).rejects.toThrow(/stopped while waiting for verification/);
      await vi.advanceTimersByTimeAsync(0);
      stop.abort();
      await vi.advanceTimersByTimeAsync(0);
      await assertion;
    } finally {
      vi.useRealTimers();
    }
  });

  it("reports a verdict still running once the wait is spent, never trusting it", async () => {
    vi.useFakeTimers();
    try {
      mocks.getVerification.mockResolvedValue({ state: "running", stale: false, record: null });
      const deps = await build();
      const pending = deps.verify("t1", live());
      await vi.advanceTimersByTimeAsync(20 * 60_000 + 10_000);
      expect(await pending).toEqual({ state: "running", stale: false, head: null });
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("the wave's view of worktrees", () => {
  const listing = [
    { path: "/repo", is_main: true, head: "m", dirty_files: 2 },
    { path: "/wt/a", is_main: false, head: "ha", dirty_files: 0 },
  ];

  it("reads a member's head from the listing, and refuses a worktree that is gone", async () => {
    mocks.invoke.mockResolvedValue(listing);
    const deps = await build();
    expect(await deps.headOf("/wt/a", live())).toBe("ha");
    expect(mocks.invoke).toHaveBeenCalledWith("cmd_list_worktrees", { repoPath: "/repo", depth: "listing" });
    await expect(deps.headOf("/wt/missing", live())).rejects.toThrow(/no longer a worktree/);
  });

  it("reports the main checkout's dirty count and the operation parked on it", async () => {
    mocks.invoke.mockResolvedValueOnce(listing).mockResolvedValueOnce({ kind: "Merge" });
    const deps = await build();
    expect(await deps.mainState!(live())).toEqual({ dirty: 2, operation: "merge" });
    expect(mocks.invoke).toHaveBeenLastCalledWith("cmd_repo_operation", { repoPath: "/repo" });

    mocks.invoke.mockResolvedValueOnce(listing).mockResolvedValueOnce(null);
    expect(await deps.mainState!(live())).toEqual({ dirty: 2, operation: null });
  });

  it("refuses a repository that lists no main worktree", async () => {
    mocks.invoke.mockResolvedValue([listing[1]]);
    const deps = await build();
    await expect(deps.mainState!(live())).rejects.toThrow(/no main worktree/);
  });
});

describe("resuming after a restart", () => {
  it("finds nothing to resume when the board holds no running wave, and says whether the look was complete", async () => {
    mocks.findActiveWave.mockResolvedValue({ record: null, complete: false });
    expect(currentWave()).toBeNull();
    expect(await resumeActiveWave()).toEqual({ runner: null, complete: false });
  });

  it("continues the wave a restart left running, holding the members it already has", async () => {
    const resumed = runner("done");
    const record = { id: "w", repositoryId: "repo-1", maxPriority: 2, members: [{ taskId: "a" }, { taskId: "b" }] };
    mocks.findActiveWave.mockResolvedValue({ record, complete: true });
    mocks.createWavePersister.mockReturnValue(async () => {});
    mocks.resumeWave.mockResolvedValue(resumed);
    expect(await resumeActiveWave()).toEqual({ runner: resumed, complete: true });
    expect(mocks.createWavePersister).toHaveBeenCalledWith(expect.anything(), ["a", "b"]);
    expect(mocks.resumeWave).toHaveBeenCalledWith(record, expect.anything());
    expect(currentWave()).toBe(resumed);
  });
});

describe("starting and resuming the one wave", () => {
  it("refuses to start beside another wave on the board, or when the board was not read far enough", async () => {
    mocks.findActiveWave.mockResolvedValueOnce({ record: { id: "w" }, complete: true });
    await expect(beginWave({ repositoryId: "repo-1", n: 1, waves: 1 })).rejects.toThrow(/before a restart is still marked running/);
    mocks.findActiveWave.mockResolvedValueOnce({ record: null, complete: false });
    await expect(beginWave({ repositoryId: "repo-1", n: 1, waves: 1 })).rejects.toThrow(/couldn't be read far enough/);
    expect(mocks.startWave).not.toHaveBeenCalled();
  });

  it("adopts a started wave as the window's current one, refuses a second while it runs, then resumes it as is", async () => {
    const started = runner("running");
    mocks.findActiveWave.mockResolvedValue({ record: null, complete: true });
    mocks.createWavePersister.mockReturnValue(async () => {});
    mocks.startWave.mockResolvedValue(started);
    const input = { repositoryId: "repo-1", n: 2, waves: 1, maxPriority: 1 };
    expect(await beginWave(input)).toBe(started);
    expect(currentWave()).toBe(started);
    expect(mocks.startWave).toHaveBeenCalledWith(input, expect.objectContaining({ verifyLimit: 2 }));

    await expect(beginWave(input)).rejects.toThrow(/already running/);
    expect(await resumeActiveWave()).toEqual({ runner: started, complete: true });
  });
});
