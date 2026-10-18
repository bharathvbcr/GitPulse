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
    landed_on_base: null,
    on_base: null,
    base_oid: null,
    merge_base: null,
    ahead: null,
    behind: null,
    base_relation_reason: "not measured",
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
    const branches = store.consumer("branches");

    return branches.load("/repo", ["main", "feature/x"], "main").then(() => {
      const state = get(store);
      expect(Object.keys(state.byRevision)).toEqual(["main", "feature/x"]);
      expect(state.byRevision["main"].commit_sha).toBe("aaa");
      expect(state.byRevision["feature/x"].commit_sha).toBe("bbb");
      expect(state.consumers.branches.error).toBe("");
      expect(state.consumers.branches.loading).toBe(false);
    });
  });

  it("passes the base branch through and collapses duplicates", async () => {
    const invoke = vi.fn().mockResolvedValue([row("aaa"), row("bbb")]);
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    await branches.load("/repo", ["main", "topic", "main", ""], "develop");

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
    const branches = store.consumer("branches");

    await branches.load("/repo", ["main", "feature/x"]);

    const state = get(store);
    expect(state.byRevision).toEqual({});
    expect(state.consumers.branches.error).toContain("asked for 2");
    expect(state.consumers.branches.loading).toBe(false);
  });

  it("keeps what it had when a load fails", async () => {
    // Clearing on failure would render every row as "nothing recorded", which
    // is a claim we cannot make when the call did not complete.
    const invoke = vi
      .fn()
      .mockResolvedValueOnce([row("aaa")])
      .mockRejectedValueOnce(new Error("harness is down"));
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    await branches.load("/repo", ["main"]);
    await branches.load("/repo", ["main"]);

    const state = get(store);
    expect(state.byRevision["main"].commit_sha).toBe("aaa");
    expect(state.consumers.branches.error).toContain("harness is down");
  });

  it("lets the newer load win when two overlap", async () => {
    const slow = deferred<ProvenanceFreshness[]>();
    const fast = deferred<ProvenanceFreshness[]>();
    const invoke = vi
      .fn()
      .mockReturnValueOnce(slow.promise)
      .mockReturnValueOnce(fast.promise);
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    const first = branches.load("/repo-a", ["main"]);
    const second = branches.load("/repo-b", ["main"]);

    fast.resolve([row("newer")]);
    await second;
    slow.resolve([row("older")]);
    await first;

    expect(get(store).byRevision["main"].commit_sha).toBe("newer");
  });

  it("drops an in-flight answer after a repository switch", async () => {
    // Switching repositories mid-load must not paint the old repository's
    // verifications onto the new one's branches.
    const pending = deferred<ProvenanceFreshness[]>();
    const invoke = vi.fn().mockReturnValue(pending.promise);
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    const inFlight = branches.load("/repo-a", ["main"]);
    store.forRepo("/repo-b");
    pending.resolve([row("stale")]);
    await inFlight;

    expect(get(store).byRevision).toEqual({});
  });

  it("keeps answers and loads in flight when told the same repository again", async () => {
    // The branch list says which repository it shows from an effect. It used
    // to reset unconditionally, on every store emission, dropping every load
    // in flight; the next emission asked again, ~54 batches in 30 s.
    const pending = deferred<ProvenanceFreshness[]>();
    const invoke = vi
      .fn()
      .mockResolvedValueOnce([row("aaa")])
      .mockReturnValueOnce(pending.promise);
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    await branches.load("/repo", ["main"]);
    const inFlight = branches.load("/repo", ["topic"]);
    for (let i = 0; i < 5; i += 1) store.forRepo("/repo");
    pending.resolve([row("bbb")]);
    await inFlight;

    const state = get(store);
    expect(state.byRevision["main"].commit_sha).toBe("aaa");
    expect(state.byRevision["topic"].commit_sha).toBe("bbb");
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("asks once for a request identical to one still in flight", async () => {
    const pending = deferred<ProvenanceFreshness[]>();
    const invoke = vi.fn().mockReturnValue(pending.promise);
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    // Order and duplicates do not make a different request.
    const loads = [
      branches.load("/repo", ["main", "topic"], "main"),
      branches.load("/repo", ["topic", "main", "main"], "main"),
      branches.load("/repo", ["main", "topic"], "main"),
    ];
    pending.resolve([row("aaa"), row("bbb")]);
    await Promise.all(loads);

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(get(store).byRevision["topic"].commit_sha).toBe("bbb");
    expect(get(store).consumers.branches.loading).toBe(false);

    // Settled, the same request is a new one: a refresh may have moved notes.
    invoke.mockResolvedValueOnce([row("aaa"), row("ccc")]);
    await branches.load("/repo", ["main", "topic"], "main");
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(get(store).byRevision["topic"].commit_sha).toBe("ccc");
  });

  it("keeps both lists' answers when their loads overlap in one repository", async () => {
    // The branch list asks for tips, the pull-request list for head refs. The
    // older answer used to be thrown away whole, so whichever list asked
    // first drew no badges until something asked again.
    const branchAnswer = deferred<ProvenanceFreshness[]>();
    const pullAnswer = deferred<ProvenanceFreshness[]>();
    const invoke = vi
      .fn()
      .mockReturnValueOnce(branchAnswer.promise)
      .mockReturnValueOnce(pullAnswer.promise);
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");
    const pulls = store.consumer("pull-requests");

    const first = branches.load("/repo", ["tip-a", "shared"], "main");
    const second = pulls.load("/repo", ["origin/pr", "shared"], "main");
    expect(get(store).consumers.branches.loading).toBe(true);
    pullAnswer.resolve([row("pr"), row("shared-newer")]);
    await second;
    expect(get(store).consumers["pull-requests"].loading).toBe(false);
    expect(get(store).consumers.branches.loading).toBe(true);
    branchAnswer.resolve([row("tip-a"), row("shared-older")]);
    await first;

    const state = get(store);
    expect(state.byRevision["tip-a"].commit_sha).toBe("tip-a");
    expect(state.byRevision["origin/pr"].commit_sha).toBe("pr");
    // Asked by both: the newer request's answer stands.
    expect(state.byRevision["shared"].commit_sha).toBe("shared-newer");
    expect(state.consumers.branches.loading).toBe(false);
  });

  it("keeps each list's status its own", async () => {
    // One token per consumer. With one shared token, whichever list answered
    // last decided the error: a successful branch batch after a failed
    // pull-request batch cleared the pull-request list's error, and a short
    // pull-request batch reset the branch list's truncation.
    const invoke = vi
      .fn()
      .mockRejectedValueOnce(new Error("gh notes unreadable"))
      .mockResolvedValueOnce([row("tip-a")]);
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");
    const pulls = store.consumer("pull-requests");

    await pulls.load("/repo", ["origin/pr"], "main");
    await branches.load("/repo", ["tip-a"], "main");

    const state = get(store);
    expect(state.consumers["pull-requests"].error).toContain("gh notes unreadable");
    expect(state.consumers.branches.error).toBe("");
    expect(state.byRevision["tip-a"].commit_sha).toBe("tip-a");
  });

  it("lets only a list's newest request set its status", async () => {
    // A refresh asks again while the first batch is out. The first answer
    // landing late must neither clear the spinner of the request that
    // replaced it nor overwrite that request's outcome.
    const older = deferred<ProvenanceFreshness[]>();
    const newer = deferred<ProvenanceFreshness[]>();
    const invoke = vi.fn().mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    const first = branches.load("/repo", ["tip-a"], "main");
    const second = branches.load("/repo", ["tip-a", "tip-b"], "main");
    older.resolve([row("tip-a-old")]);
    await first;
    expect(get(store).consumers.branches.loading).toBe(true);
    newer.resolve([row("tip-a-new"), row("tip-b")]);
    await second;

    const state = get(store);
    expect(state.consumers.branches.loading).toBe(false);
    expect(state.byRevision["tip-a"].commit_sha).toBe("tip-a-new");
  });

  it("says when it trimmed the request rather than silently dropping rows", async () => {
    const revisions = Array.from({ length: MAX_REVISIONS + 5 }, (_, i) => `r${i}`);
    const invoke = vi.fn().mockResolvedValue(revisions.slice(0, MAX_REVISIONS).map((r) => row(r)));
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    await branches.load("/repo", revisions);

    expect(invoke.mock.calls[0][1].revisions).toHaveLength(MAX_REVISIONS);
    expect(get(store).consumers.branches.truncated).toBe(true);
    // A row past the cap has no entry, and no entry draws no badge — which is
    // exactly what a commit with nothing recorded looks like. `truncated` is
    // the only thing that tells those apart.
    expect(get(store).byRevision[`r${MAX_REVISIONS}`]).toBeUndefined();
  });

  it("does not call out for nothing", async () => {
    const invoke = vi.fn();
    const store = createFreshnessStore({ invoke: invoke as never });
    const branches = store.consumer("branches");

    await branches.load("", ["main"]);
    await branches.load("/repo", []);
    await branches.load("/repo", ["", ""]);

    expect(invoke).not.toHaveBeenCalled();
  });
});
