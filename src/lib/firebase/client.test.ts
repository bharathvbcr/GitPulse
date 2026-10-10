import { describe, expect, it, vi } from "vitest";
import {
  createFirebaseRollout,
  getFirebaseStatus,
  listFirebaseBackends,
  listFirebaseRollouts,
  type InvokeFn,
} from "./client";

/** Records every call so each wrapper's command name and argument names are pinned. */
function recorder(answer: unknown = { ok: true }) {
  const calls: Array<{ cmd: string; args: Record<string, unknown> | undefined }> = [];
  const invokeFn = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    return answer;
  }) as unknown as InvokeFn;
  return { calls, invokeFn };
}

describe("the Firebase IPC seam", () => {
  it("reads status with only the repository path", async () => {
    const { calls, invokeFn } = recorder({ configured: false });
    await expect(getFirebaseStatus("/r", invokeFn)).resolves.toEqual({ configured: false });
    expect(calls).toEqual([{ cmd: "cmd_firebase_status", args: { repoPath: "/r" } }]);
  });

  it("lists backends for the project, never a location", async () => {
    const { calls, invokeFn } = recorder();
    await listFirebaseBackends("/r", "proj", invokeFn);
    expect(calls).toEqual([{ cmd: "cmd_firebase_backends", args: { repoPath: "/r", projectId: "proj" } }]);
  });

  it("lists rollouts for one backend", async () => {
    const { calls, invokeFn } = recorder();
    await listFirebaseRollouts("/r", "proj", "web", invokeFn);
    expect(calls).toEqual([
      { cmd: "cmd_firebase_rollouts", args: { repoPath: "/r", projectId: "proj", backendId: "web" } },
    ]);
  });

  it("creates a rollout pinned to the commit it was given, byte for byte", async () => {
    const { calls, invokeFn } = recorder();
    const sha = "0123456789abcdef0123456789abcdef01234567";
    await createFirebaseRollout("/r", "proj", "web", sha, invokeFn);
    expect(calls).toEqual([
      {
        cmd: "cmd_firebase_create_rollout",
        args: { repoPath: "/r", projectId: "proj", backendId: "web", gitCommit: sha },
      },
    ]);
  });

  it("lets a failed call reach the caller instead of swallowing it", async () => {
    const invokeFn = (async () => {
      throw new Error("firebase unavailable");
    }) as unknown as InvokeFn;
    await expect(getFirebaseStatus("/r", invokeFn)).rejects.toThrow("firebase unavailable");
    await expect(createFirebaseRollout("/r", "p", "b", "sha", invokeFn)).rejects.toThrow("firebase unavailable");
  });
});
