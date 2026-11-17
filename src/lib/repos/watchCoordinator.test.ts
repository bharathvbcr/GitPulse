import { describe, expect, it, vi } from "vitest";
import { createSessionBook } from "./sessionBook";
import type { RepoSession } from "./storeTypes";
import { createWatchCoordinator, type WatchCoordinatorDeps } from "./watchCoordinator";

function harness(scope?: () => "narrow" | "whole") {
  const calls: [string, Record<string, unknown>][] = [];
  const invoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
    calls.push([command, args ?? {}]);
    return undefined;
  });
  const sessions: Record<string, RepoSession> = {};
  const patches: Record<string, unknown>[] = [];
  const deps = {
    invoke,
    options: { caseInsensitive: false },
    sessionBook: createSessionBook((a, b) => a === b),
    sessionById: (id: string) => sessions[id],
    activeSession: () => undefined,
    findSessionByPath: () => undefined,
    applyToSession: (_id: string, _g: number, patch: unknown) => {
      patches.push(patch as Record<string, unknown>);
      return true;
    },
    refresh: async () => {},
    holdsPath: () => false,
    isHidden: () => false,
    watchScope: scope,
  } as unknown as WatchCoordinatorDeps;
  const coordinator = createWatchCoordinator(deps);
  const open = (id: string, path: string) => {
    sessions[id] = { id, path, generation: 1 } as RepoSession;
  };
  const watchCalls = () => calls.filter(([command]) => command === "cmd_watch_repo").map(([, args]) => args);
  return { coordinator, open, watchCalls, calls };
}

describe("watch scope on the watch invoke", () => {
  it("sends exactly { repoPath } when the dep is absent or narrow", async () => {
    for (const scope of [undefined, () => "narrow" as const]) {
      const h = harness(scope);
      await h.coordinator.watch("/r/a");
      expect(h.watchCalls()).toEqual([{ repoPath: "/r/a" }]);
      expect(Object.keys(h.watchCalls()[0])).toEqual(["repoPath"]);
    }
  });

  it("adds scope: whole when the dep says whole", async () => {
    const h = harness(() => "whole");
    await h.coordinator.watch("/r/a");
    expect(h.watchCalls()).toEqual([{ repoPath: "/r/a", scope: "whole" }]);
  });
});

describe("reassertWatches", () => {
  it("re-invokes the watch for every admitted repo with the scope now in force, and only those", async () => {
    let scope: "narrow" | "whole" = "narrow";
    const h = harness(() => scope);
    h.open("a", "/r/a");
    h.open("b", "/r/b");
    h.open("c", "/r/c");
    await h.coordinator.acquireWatch("a", "/r/a", true);
    await h.coordinator.acquireWatch("b", "/r/b", false);
    // "c" never held a slot, so it is not re-registered.
    expect(h.watchCalls()).toHaveLength(2);

    scope = "whole";
    await h.coordinator.reassertWatches();
    const after = h.watchCalls().slice(2);
    expect(after).toHaveLength(2);
    expect(after).toEqual(
      expect.arrayContaining([
        { repoPath: "/r/a", scope: "whole" },
        { repoPath: "/r/b", scope: "whole" },
      ]),
    );
  });

  it("does nothing when no watch is admitted", async () => {
    const h = harness(() => "whole");
    await h.coordinator.reassertWatches();
    expect(h.watchCalls()).toEqual([]);
  });
});
