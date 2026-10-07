import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { relinkRepository, WorkbenchError, type Repository } from "./client";
import { currentLocation, relinkCheckout, relinkConfirmation, type RelinkIO } from "./repositoryRelink";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const repo: Repository = { id: "repo_1", revision: 3, updated_at: 100, name: "GitPulse", identity_key: "local:/old/GitPulse/.git", remote_url: null };

function io(overrides: Partial<RelinkIO> = {}): RelinkIO & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    pick: async () => "/new/GitPulse",
    confirm: async () => true,
    relink: async (target, path, requestId) => { calls.push(requestId); return { ...target, revision: target.revision + 1, identity_key: `local:${path}/.git` }; },
    ...overrides,
  };
}

beforeEach(() => native.mockReset());

describe("relinkRepository", () => {
  it("sends the record's revision and the folder, and accepts only the next revision of that record", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ repository: { ...repo, revision: 4, identity_key: "local:/new/GitPulse/.git" } }));
    const saved = await relinkRepository(repo, "/new/GitPulse", "req-1");
    expect(saved.identity_key).toBe("local:/new/GitPulse/.git");
    expect(native).toHaveBeenCalledWith("cmd_workbench_relink_repository", { repositoryId: "repo_1", expectedRevision: 3, repoPath: "/new/GitPulse", requestId: "req-1" });
    // A reply naming another record, or a revision that did not advance by
    // one, is not this relink — it must not be reported as one.
    native.mockResolvedValueOnce(JSON.stringify({ repository: { ...repo, id: "other", revision: 4 } }));
    await expect(relinkRepository(repo, "/new/GitPulse", "req-2")).rejects.toThrow();
    native.mockResolvedValueOnce(JSON.stringify({ repository: { ...repo, revision: 3 } }));
    await expect(relinkRepository(repo, "/new/GitPulse", "req-3")).rejects.toThrow();
  });
});

describe("relinkCheckout", () => {
  it("names where the repository was registered and where its tasks will go", () => {
    expect(currentLocation(repo)).toBe("/old/GitPulse");
    const prompt = relinkConfirmation(repo, "/new/GitPulse");
    expect(prompt.title).toBe("Relink GitPulse?");
    expect(prompt.message).toContain("/new/GitPulse");
    expect(prompt.message).toContain("/old/GitPulse");
    expect(prompt.message).toContain("never merged");
  });

  it("writes nothing when the picker or the confirmation is cancelled", async () => {
    const noFolder = io({ pick: async () => null });
    expect(await relinkCheckout(repo, noFolder)).toEqual({ kind: "cancelled" });
    expect(noFolder.calls).toEqual([]);
    const declined = io({ confirm: async () => false });
    expect(await relinkCheckout(repo, declined)).toEqual({ kind: "cancelled" });
    expect(declined.calls).toEqual([]);
  });

  it("reports the relinked record", async () => {
    const outcome = await relinkCheckout(repo, io());
    expect(outcome.kind).toBe("relinked");
    if (outcome.kind !== "relinked") return;
    expect(outcome.repository.revision).toBe(4);
    expect(outcome.message).toBe("Relinked GitPulse to /new/GitPulse");
  });

  it("passes the store's refusal through as a failure that offers no retry", async () => {
    const refused = io({ relink: async () => { throw new WorkbenchError("repository_not_empty", "the repository registered at that checkout holds 2 task link(s)"); } });
    const outcome = await relinkCheckout(repo, refused);
    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") expect(outcome.message).toContain("2 task link(s)");
  });

  it("retries an uncertain reply with the same request id, so it cannot relink twice", async () => {
    let attempts = 0;
    const flaky = io();
    const base = flaky.relink;
    flaky.relink = async (target, path, requestId) => {
      attempts += 1;
      if (attempts === 1) { flaky.calls.push(requestId); throw new WorkbenchError("transport_error", "reply lost"); }
      return base(target, path, requestId);
    };
    const first = await relinkCheckout(repo, flaky);
    expect(first.kind).toBe("uncertain");
    if (first.kind !== "uncertain") return;
    expect(first.message).toContain("may not have finished");
    const second = await first.retry();
    expect(second.kind).toBe("relinked");
    expect(flaky.calls).toHaveLength(2);
    expect(flaky.calls[0]).toBe(flaky.calls[1]);
  });
});
