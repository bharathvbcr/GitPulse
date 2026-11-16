import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "../ipc/invoke";
import { WorkbenchError } from "./client";
import {
  acquireWorkQueue,
  cancelWorkQueue,
  enqueueWorkQueue,
  transitionQueueMember,
  verifyWorkQueueCombined,
  type WorkQueueEntry,
} from "./workQueue";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);

const sampleQueue = (overrides: Partial<WorkQueueEntry> = {}): WorkQueueEntry => ({
  id: "q-1",
  revision: 1,
  title: "Cross-repo ship",
  state: "leased",
  concurrency_limit: 2,
  active_timeout_seconds: 2700,
  elapsed_timeout_seconds: 7200,
  lease_order: ["repo-a", "repo-b"],
  leased_at: 1700000000,
  finished_at: null,
  publication: "manual_per_repository",
  reason: "",
  verification: null,
  created_at: 1700000000,
  updated_at: 1700000000,
  members: [
    {
      member_id: "m-a",
      repository_id: "repo-a",
      git_dir: "/repos/a/.git",
      base_oid: "a".repeat(40),
      task_id: "task-a",
      run_id: null,
      position: 0,
      state: "leased",
      lease_held: true,
      depends_on: [],
      started_at: 1700000000,
      active_seconds: 0,
      last_active_at: 1700000000,
      verification: null,
    },
    {
      member_id: "m-b",
      repository_id: "repo-b",
      git_dir: "/repos/b/.git",
      base_oid: "b".repeat(40),
      task_id: "task-b",
      run_id: null,
      position: 1,
      state: "leased",
      lease_held: true,
      depends_on: [],
      started_at: 1700000000,
      active_seconds: 0,
      last_active_at: 1700000000,
      verification: null,
    },
  ],
  ...overrides,
});

beforeEach(() => native.mockReset());

describe("workQueue", () => {
  it("enqueues through queue.enqueue_host and acquires leases through queue.acquire", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: sampleQueue({ state: "queued" }) }));
    const entry = await enqueueWorkQueue({
      id: "q-1",
      request_id: "req-1",
      title: "Cross-repo ship",
      concurrency_limit: 2,
      active_timeout_seconds: 2700,
      elapsed_timeout_seconds: 7200,
      members: [
        { member_id: "m-a", repository_id: "repo-a", repo_path: "/repos/a", task_id: "task-a" },
        { member_id: "m-b", repository_id: "repo-b", repo_path: "/repos/b", task_id: "task-b", depends_on: ["m-a"] },
      ],
    });
    expect(entry.state).toBe("queued");
    expect(native).toHaveBeenCalledWith("cmd_workbench_request", {
      method: "queue.enqueue_host",
      input: JSON.stringify({
        id: "q-1",
        request_id: "req-1",
        expected_revision: 0,
        title: "Cross-repo ship",
        members: [
          { member_id: "m-a", repository_id: "repo-a", repo_path: "/repos/a", task_id: "task-a" },
          { member_id: "m-b", repository_id: "repo-b", repo_path: "/repos/b", task_id: "task-b", depends_on: ["m-a"] },
        ],
        concurrency_limit: 2,
        active_timeout_seconds: 2700,
        elapsed_timeout_seconds: 7200,
      }),
    });

    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: sampleQueue({ revision: 2, state: "leased" }) }));
    const acquired = await acquireWorkQueue({ id: "q-1", request_id: "req-acq", expected_revision: 1 });
    expect(acquired.state).toBe("leased");
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", {
      method: "queue.acquire",
      input: JSON.stringify({ id: "q-1", request_id: "req-acq", expected_revision: 1 }),
    });
  });

  it("transitions members via queue.member.update and enforces manual_per_repository on combined verification", async () => {
    native.mockResolvedValueOnce({ ok: true, item: sampleQueue({ revision: 2 }) });
    await transitionQueueMember({
      id: "q-1",
      request_id: "req-t",
      expected_revision: 1,
      member_id: "m-a",
      state: "passed",
      run_id: "run-1",
    });
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", {
      method: "queue.member.update",
      input: JSON.stringify({
        id: "q-1",
        request_id: "req-t",
        expected_revision: 1,
        member_id: "m-a",
        state: "passed",
        run_id: "run-1",
      }),
    });

    const verified = sampleQueue({
      revision: 4,
      state: "passed",
      verification: {
        verified_at: 1700000100,
        ok: true,
        publication: "manual_per_repository",
        results: [],
      },
    });
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: verified }));
    const result = await verifyWorkQueueCombined({
      id: "q-1",
      request_id: "req-v",
      expected_revision: 3,
      checkouts: { "m-a": "/repos/a", "m-b": "/repos/b" },
    });
    expect(result.verification?.publication).toBe("manual_per_repository");
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", {
      method: "queue.verify_combined",
      input: JSON.stringify({
        id: "q-1",
        request_id: "req-v",
        expected_revision: 3,
        publication: "manual_per_repository",
        checkouts: { "m-a": "/repos/a", "m-b": "/repos/b" },
      }),
    });
  });

  it("surfaces typed WorkbenchError failures from host or store", async () => {
    native.mockRejectedValueOnce({ code: "repository_mismatch", message: "wrong repo checkout" });
    await expect(
      cancelWorkQueue({ id: "q-1", request_id: "req-c", expected_revision: 1 }),
    ).rejects.toMatchObject({ code: "repository_mismatch" });

    native.mockResolvedValueOnce(
      JSON.stringify({ ok: false, code: "queue_timed_out", message: "elapsed limit exceeded" }),
    );
    await expect(
      cancelWorkQueue({ id: "q-1", request_id: "req-c2", expected_revision: 1 }),
    ).rejects.toBeInstanceOf(WorkbenchError);
  });
});
