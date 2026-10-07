import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import type { InsightsSnapshot } from "../insights/types";
import { MAX_AGENT_REPOS } from "./plane";
import { AGENT_SWEEP_CONCURRENCY, AGENT_SWEEP_DEADLINE_MS, createAgentPlaneStore, sweepTargets } from "./store";

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (reason: unknown) => void = () => {};
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const snap = { repo_path: "/repo" } as InsightsSnapshot;

describe("sweepTargets", () => {
  it("reads one repository once, from its own checkout", () => {
    expect(sweepTargets([
      { path: "/repo/.claude/worktrees/alpha", label: "alpha", family: "git", familyRoot: "/repo" },
      { path: "/repo", label: "Repo", family: "git", familyRoot: "/repo" },
      { path: "/repo/.claude/worktrees/beta", label: "beta", family: "git", familyRoot: "/repo" },
      { path: "/other", label: "Other", family: "other-git", familyRoot: "/other" },
      { path: "", label: "blank", family: "git", familyRoot: "/repo" },
    ])).toEqual([
      { path: "/repo", label: "Repo" },
      { path: "/other", label: "Other" },
    ]);
  });

  it("does not merge checkouts whose family was not read", () => {
    expect(sweepTargets([
      { path: "/repo", label: "Repo", family: null, familyRoot: null },
      { path: "/repo-wt", label: "wt", family: null, familyRoot: null },
    ])).toEqual([
      { path: "/repo", label: "Repo" },
      { path: "/repo-wt", label: "wt" },
    ]);
  });
});

describe("createAgentPlaneStore", () => {
  it("keeps the previous probes on screen until the next sweep finishes", async () => {
    const reads: ReturnType<typeof deferred<InsightsSnapshot>>[] = [];
    const store = createAgentPlaneStore({
      snapshot: () => {
        const next = deferred<InsightsSnapshot>();
        reads.push(next);
        return next.promise;
      },
      paths: { caseInsensitive: false },
    });
    const pending = store.refresh([{ path: "/repo", label: "Repo" }]);
    expect(get(store).scanning).toBe(true);
    expect(get(store).probes).toEqual([]);
    reads[0].resolve(snap);
    await pending;
    expect(get(store).scanning).toBe(false);
    expect(get(store).probes[0].snapshot).toBe(snap);

    const again = store.refresh([{ path: "/repo", label: "Repo" }]);
    expect(get(store).scanning).toBe(true);
    expect(get(store).probes[0].snapshot).toBe(snap);
    reads[1].resolve({ ...snap, repo_path: "/repo-2" });
    await again;
    expect(get(store).probes[0].snapshot?.repo_path).toBe("/repo-2");
    expect(get(store).scanning).toBe(false);
  });

  it("records one repository's failure and still reads the others", async () => {
    const store = createAgentPlaneStore({
      snapshot: async (path) => {
        if (path === "/bad") throw new Error("unreachable");
        return { ...snap, repo_path: path };
      },
      paths: { caseInsensitive: false },
    });
    await store.refresh([
      { path: "/ok", label: "Ok" },
      { path: "/bad", label: "Bad" },
    ]);
    const probes = get(store).probes;
    expect(probes.find((probe) => probe.path === "/ok")?.error).toBe("");
    expect(probes.find((probe) => probe.path === "/bad")?.error).toBe("unreachable");
    expect(probes.find((probe) => probe.path === "/bad")?.skipped).toBe(false);
  });

  it("skips repositories past the cap without reading them", async () => {
    const seen: string[] = [];
    const store = createAgentPlaneStore({
      snapshot: async (path) => {
        seen.push(path);
        return snap;
      },
      paths: { caseInsensitive: false },
    });
    const targets = Array.from({ length: MAX_AGENT_REPOS + 3 }, (_, index) => ({
      path: `/repo/${index}`,
      label: String(index),
    }));
    await store.refresh(targets);
    expect(seen).toHaveLength(MAX_AGENT_REPOS);
    const probes = get(store).probes;
    expect(probes.filter((probe) => probe.skipReason === "cap")).toHaveLength(3);
    expect(probes.filter((probe) => probe.skipReason === "cap").every((probe) => probe.snapshot === null)).toBe(true);
  });

  it("does not start a snapshot once the deadline has passed, and lets one already started finish", async () => {
    let time = 0;
    const gate = deferred<void>();
    let started = 0;
    const store = createAgentPlaneStore({
      now: () => time,
      snapshot: async () => {
        started += 1;
        // The pool starts its width synchronously. Move the clock only once
        // that width is inside a snapshot, so the ones already started may
        // finish and the ones not yet started are the ones that skip.
        if (started === AGENT_SWEEP_CONCURRENCY) time = AGENT_SWEEP_DEADLINE_MS;
        await gate.promise;
        return snap;
      },
      paths: { caseInsensitive: false },
    });
    const targets = Array.from({ length: AGENT_SWEEP_CONCURRENCY + 2 }, (_, index) => ({
      path: `/repo/${index}`,
      label: String(index),
    }));
    const pending = store.refresh(targets);
    expect(started).toBe(AGENT_SWEEP_CONCURRENCY);
    gate.resolve();
    await pending;
    const probes = get(store).probes;
    expect(probes.filter((probe) => probe.skipReason === "deadline").length).toBeGreaterThan(0);
    expect(probes.filter((probe) => probe.error === "" && !probe.skipped).length).toBe(AGENT_SWEEP_CONCURRENCY);
    expect(get(store).scanning).toBe(false);
  });

  it("drops a sweep that was cancelled or superseded, and leaves the last finished probes", async () => {
    const reads: ReturnType<typeof deferred<InsightsSnapshot>>[] = [];
    const store = createAgentPlaneStore({
      snapshot: () => {
        const next = deferred<InsightsSnapshot>();
        reads.push(next);
        return next.promise;
      },
      paths: { caseInsensitive: false },
    });
    const opening = store.refresh([{ path: "/repo", label: "Repo" }]);
    store.cancel();
    reads[0].resolve(snap);
    await opening;
    expect(get(store).probes).toEqual([]);
    expect(get(store).scanning).toBe(false);

    const kept = store.refresh([{ path: "/kept", label: "Kept" }]);
    reads[1].resolve({ ...snap, repo_path: "/kept" });
    await kept;
    expect(get(store).probes.map((probe) => probe.path)).toEqual(["/kept"]);

    const replaced = store.refresh([{ path: "/next", label: "Next" }]);
    const newer = store.refresh([{ path: "/newer", label: "Newer" }]);
    reads[2].resolve({ ...snap, repo_path: "/next" });
    reads[3].resolve({ ...snap, repo_path: "/newer" });
    await replaced;
    await newer;
    expect(get(store).probes.map((probe) => probe.path)).toEqual(["/newer"]);
    expect(get(store).probes[0].snapshot?.repo_path).toBe("/newer");
    expect(get(store).scanning).toBe(false);
  });

  it("collapses two tabs of one repository into one read", async () => {
    const seen: string[] = [];
    const store = createAgentPlaneStore({
      snapshot: async (path) => {
        seen.push(path);
        return snap;
      },
      paths: { caseInsensitive: true },
    });
    await store.refresh([
      { path: "/Repo", label: "A" },
      { path: "/repo", label: "B" },
      { path: "", label: "empty" },
    ]);
    expect(seen).toEqual(["/Repo"]);
    expect(get(store).probes).toHaveLength(1);
  });

  it("clears probes when nothing is open", async () => {
    const store = createAgentPlaneStore({
      snapshot: async () => snap,
      paths: { caseInsensitive: false },
    });
    await store.refresh([{ path: "/repo", label: "Repo" }]);
    await store.refresh([]);
    expect(get(store).probes).toEqual([]);
    expect(get(store).scanning).toBe(false);
  });
});
