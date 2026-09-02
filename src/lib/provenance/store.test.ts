import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";
import { createFreshnessStore, MAX_REVISIONS } from "./store";
import type { ProvenanceFreshness } from "./types";

function row(sha: string, over: Partial<ProvenanceFreshness> = {}): ProvenanceFreshness {
  return {
    commit_sha: sha,
    distance: 0,
    confidence: 1,
    is_fresh: true,
    unmeasured_reason: "",
    notes_readable: true,
    verification: null,
    session: null,
    ...over,
  };
}

/** A resolvable promise, so overlapping loads can be ordered deliberately. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

describe("freshness store", () => {
  it("keys answers by the revision that was asked for, not by the sha", () => {
    // The branch list holds names; the PR list holds head refs. Keying by the
    // resolved sha would make every lookup a second resolution step.
    const invoke = vi.fn().mockResolvedValue([row("aaa"), row("bbb")]);
    const store = createFreshnessStore({ invoke: invoke as never });

    return store.load("/repo", ["main", "feature/x"], "main").then(() => {
      const state = get(store);
      expect(Object.keys(state.byRevision)).toEqual(["main", "feature/x"]);
      expect(state.byRevision["main"].commit_sha).toBe("aaa");
      expect(state.byRevision["feature/x"].commit_sha).toBe("bbb");
      expect(state.error).toBe("");
      expect(state.loading).toBe(false);
    });
  });

  it("passes the base branch through and collapses duplicates", async () => {
    const invoke = vi.fn().mockResolvedValue([row("aaa"), row("bbb")]);
    const store = createFreshnessStore({ invoke: invoke as never });

    await store.load("/repo", ["main", "topic", "main", ""], "develop");

    expect(invoke).toHaveBeenCalledWith("cmd_provenance_freshness_batch", {
      repoPath: "/repo",
      revisions: ["main", "topic"],
      baseBranch: "develop",
    });
  });

  /**
   * The backend's contract is one row per input in input order. If it ever
   * answered short, zipping would silently attach every row to the wrong
   * revision — a verified badge landing on an unverified branch.
   */
  it("refuses a short answer rather than mis-keying the rows", async () => {
    const invoke = vi.fn().mockResolvedValue([row("aaa")]);
    const store = createFreshnessStore({ invoke: invoke as never });

    await store.load("/repo", ["main", "feature/x"]);

    const state = get(store);
    expect(state.byRevision).toEqual({});
    expect(state.error).toContain("asked for 2");
    expect(state.loading).toBe(false);
  });

  it("keeps what it had when a load fails", async () => {
    // Clearing on failure would render every row as "nothing recorded", which
    // is a claim we cannot make when the call did not complete.
    const invoke = vi
      .fn()
      .mockResolvedValueOnce([row("aaa")])
      .mockRejectedValueOnce(new Error("harness is down"));
    const store = createFreshnessStore({ invoke: invoke as never });

    await store.load("/repo", ["main"]);
    await store.load("/repo", ["main"]);

    const state = get(store);
    expect(state.byRevision["main"].commit_sha).toBe("aaa");
    expect(state.error).toContain("harness is down");
  });

  it("lets the newer load win when two overlap", async () => {
    const slow = deferred<ProvenanceFreshness[]>();
    const fast = deferred<ProvenanceFreshness[]>();
    const invoke = vi
      .fn()
      .mockReturnValueOnce(slow.promise)
      .mockReturnValueOnce(fast.promise);
    const store = createFreshnessStore({ invoke: invoke as never });

    const first = store.load("/repo-a", ["main"]);
    const second = store.load("/repo-b", ["main"]);

    fast.resolve([row("newer")]);
    await second;
    slow.resolve([row("older")]);
    await first;

    expect(get(store).byRevision["main"].commit_sha).toBe("newer");
  });

  it("drops an in-flight answer after a reset", async () => {
    // Switching repositories mid-load must not paint the old repository's
    // verifications onto the new one's branches.
    const pending = deferred<ProvenanceFreshness[]>();
    const invoke = vi.fn().mockReturnValue(pending.promise);
    const store = createFreshnessStore({ invoke: invoke as never });

    const inFlight = store.load("/repo-a", ["main"]);
    store.reset();
    pending.resolve([row("stale")]);
    await inFlight;

    expect(get(store).byRevision).toEqual({});
  });

  it("says when it trimmed the request rather than silently dropping rows", async () => {
    const revisions = Array.from({ length: MAX_REVISIONS + 5 }, (_, i) => `r${i}`);
    const invoke = vi.fn().mockResolvedValue(revisions.slice(0, MAX_REVISIONS).map((r) => row(r)));
    const store = createFreshnessStore({ invoke: invoke as never });

    await store.load("/repo", revisions);

    expect(invoke.mock.calls[0][1].revisions).toHaveLength(MAX_REVISIONS);
    expect(get(store).truncated).toBe(true);
    // A row past the cap has no entry, and no entry draws no badge — which is
    // exactly what a commit with nothing recorded looks like. `truncated` is
    // the only thing that tells those apart.
    expect(get(store).byRevision[`r${MAX_REVISIONS}`]).toBeUndefined();
  });

  it("does not call out for nothing", async () => {
    const invoke = vi.fn();
    const store = createFreshnessStore({ invoke: invoke as never });

    await store.load("", ["main"]);
    await store.load("/repo", []);
    await store.load("/repo", ["", ""]);

    expect(invoke).not.toHaveBeenCalled();
  });
});
