import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { relinkRepository, WorkbenchError, type Repository } from "./client";
import { checkoutFlag, currentLocation, readCheckout, readCheckouts, relinkCheckout, relinkConfirmation, CHECKOUT_READ_CONCURRENCY, type RelinkIO } from "./repositoryRelink";

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

describe("readCheckout", () => {
  const options = { caseInsensitive: false };
  it("is available when the host finds the checkout where the store says it is", async () => {
    const asked: string[] = [];
    const health = await readCheckout(repo, async (path) => { asked.push(path); return path; }, options);
    expect(health).toMatchObject({ state: "available", path: "/old/GitPulse" });
    expect(asked).toEqual(["/old/GitPulse"]);
  });
  it("is missing when the host says the folder holds no repository", async () => {
    const health = await readCheckout(repo, async (path) => { throw `Not a Git repository: ${path}`; }, options);
    expect(health.state).toBe("missing");
    expect(health.detail).toContain("/old/GitPulse");
  });
  it("is missing when the walk up lands in a different, enclosing repository", async () => {
    expect((await readCheckout(repo, async () => "/old", options)).state).toBe("missing");
  });
  it("is unknown — never available or missing — when the check itself fails", async () => {
    const health = await readCheckout(repo, async () => { throw new Error("Operation not permitted"); }, options);
    expect(health.state).toBe("unknown");
    expect(health.detail).toContain("Operation not permitted");
    expect(checkoutFlag(health)).toBeNull();
  });
  it("is remote for a repository registered without a local path, and asks the host nothing", async () => {
    let asked = false;
    const remote = { ...repo, identity_key: "remote:github.com/x/y", remote_url: "https://github.com/x/y.git" };
    const health = await readCheckout(remote, async (path) => { asked = true; return path; }, options);
    expect(health).toMatchObject({ state: "remote", path: null });
    expect(health.detail).toContain("https://github.com/x/y.git");
    expect(asked).toBe(false);
    expect(checkoutFlag(health)?.label).toBe("Remote only");
  });
  it("follows the host's case rule when comparing where it found the checkout", async () => {
    expect((await readCheckout(repo, async () => "/OLD/gitpulse", { caseInsensitive: true })).state).toBe("available");
    expect((await readCheckout(repo, async () => "/OLD/gitpulse", { caseInsensitive: false })).state).toBe("missing");
  });
  it("reads a whole catalog with a bounded number of checks in flight", async () => {
    let inFlight = 0, peak = 0;
    const repos = Array.from({ length: 20 }, (_, i) => ({ ...repo, id: `r${i}`, identity_key: `local:/w/r${i}/.git` }));
    const read = await readCheckouts(repos, async (path) => {
      inFlight++; peak = Math.max(peak, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 2));
      inFlight--;
      return path;
    }, options);
    expect(read.size).toBe(20);
    expect([...read.values()].every((health) => health.state === "available")).toBe(true);
    expect(peak).toBeLessThanOrEqual(CHECKOUT_READ_CONCURRENCY);
  });
  it("says a remote-only repository had no local checkout when a relink gives it one", () => {
    const remote = { ...repo, identity_key: "remote:github.com/x/y", remote_url: "https://github.com/x/y.git" };
    expect(relinkConfirmation(remote, "/new/y").message).toContain("had no local checkout (known by its remote, https://github.com/x/y.git)");
    expect(relinkConfirmation(repo, "/new/GitPulse").message).toContain(`It was registered at ${currentLocation(repo)}.`);
  });
});
