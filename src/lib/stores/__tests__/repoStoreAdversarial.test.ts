import { afterEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";
import { cancelPrompt, completePrompt, isPromptOpen, askText, promptError, promptState } from "../modalStore";
import type { FilterState } from "../filterStore";
import { gitActionCount } from "../../desktop/exitGuard";

/**
 * Adversarial review of the repoStore mutation contract (ft-1629347b).
 * Tests named "BREAK" assert the behaviour the contract should have and fail
 * against the reviewed code; tests named "HOLDS" / "CHARACTERIZATION" pass and
 * pin what was verified.
 */

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function makeFilter() {
  const store = writable<FilterState>({ searchQuery: "", selectedBranch: null });
  return {
    subscribe: store.subscribe,
    setSearch: (query: string) => store.update((s) => ({ ...s, searchQuery: query })),
    selectBranch: (branch: string | null) => store.update((s) => ({ ...s, selectedBranch: branch })),
    clear: () => store.set({ searchQuery: "", selectedBranch: null }),
  };
}

const graph = {
  showRepo: () => {},
  loadGraph: async () => {},
  evict: () => {},
};

function makeInvoke(overrides: Partial<Record<string, InvokeFn>> = {}): InvokeFn {
  return async (cmd, args) => {
    const override = overrides[cmd];
    if (override) return override(cmd, args);
    switch (cmd) {
      case "cmd_resolve_repo": {
        const path = String(args?.repoPath ?? "");
        return { path, name: path.split("/").pop(), is_bare: false } as never;
      }
      case "cmd_list_branches":
      case "cmd_get_status":
        return [] as never;
      case "cmd_list_tags":
        return { tags: [], truncated: false } as never;
      case "cmd_repo_operation":
        return null as never;
      case "cmd_stash_list":
        return { entries: [], truncated: false } as never;
      case "cmd_watch_repo":
        return String(args?.repoPath) as never;
      case "cmd_branch_stats":
        return { compared_to: "", updates: [], computed: 0, cached: 0, capped: false } as never;
      case "cmd_unwatch_repo":
      case "cmd_note_tab_activated":
      case "cmd_set_recent_menu":
        return undefined as never;
      default:
        throw new Error(`unexpected command ${cmd}`);
    }
  };
}

function makeStore(invoke: InvokeFn) {
  return createRepoStore({ invoke, storage: memoryStorage(), caseInsensitive: true, graph, filter: makeFilter() });
}

afterEach(() => {
  vi.useRealTimers();
  cancelPrompt();
});

describe("attack 1: target identity", () => {
  it("PINNED: a tab closed and reopened at the same path shares its tabId, so the old target runs against the new session — the same path is the same repository", async () => {
    const calls: string[] = [];
    const store = makeStore(makeInvoke({ cmd_discard_changes: async (_c, a) => void calls.push(String(a?.repoPath)) as never }));
    await store.openRepo("/r/a");
    const a = store.captureTarget()!;
    await store.openRepo("/r/b");
    await store.closeTab(a.tabId);
    await store.openRepo("/r/a");
    expect(store.captureTarget()!.tabId).toBe(a.tabId);
    // Ruled correct: a closed target is refused only while the repository is
    // not open; reopening the path restores it, and the user's intent holds.
    const outcome = await store.discardChanges("f", a);
    expect(outcome.ok).toBe(true);
    expect(calls).toEqual(["/r/a"]);
  });

  it("HOLDS: the same repository spelled three ways opens one tab, so two tabs can never share a path", async () => {
    const store = makeStore(makeInvoke());
    await store.openRepo("/r/a");
    await store.openRepo("/r/A");
    await store.openRepo("/r/a/");
    expect(get(store).openTabs).toHaveLength(1);
  });

  it("HOLDS: repo-gone is refused, and the same target works again once the checkout is back", async () => {
    const calls: string[] = [];
    const store = makeStore(makeInvoke({ cmd_delete_branch: async (_c, a) => void calls.push(String(a?.repoPath)) as never }));
    await store.openRepo("/r/a");
    const a = store.captureTarget()!;
    store.markRepoGone("/r/a");
    expect(await store.deleteBranch("x", false, a)).toMatchObject({ ok: false, refused: true });
    await store.refresh("/r/a");
    expect((await store.deleteBranch("x", false, a)).ok).toBe(true);
    expect(calls).toEqual(["/r/a"]);
  });

  it("HOLDS: an explicit undefined target falls back to the active tab (a caller that passes a possibly-undefined capture is not protected)", async () => {
    const calls: string[] = [];
    const store = makeStore(makeInvoke({ cmd_delete_branch: async (_c, a) => void calls.push(String(a?.repoPath)) as never }));
    await store.openRepo("/r/a");
    await store.openRepo("/r/b");
    await store.deleteBranch("x", false, undefined);
    expect(calls).toEqual(["/r/b"]);
  });
});

describe("attack 6: bulk fetch/pull", () => {
  it("a repo that went missing while queued is skipped, not pulled (the bulk skip check is live)", async () => {
    const pulled: string[] = [];
    const holdA = deferred<unknown>();
    const store = makeStore(
      makeInvoke({
        cmd_pull: async (_c, a) => {
          pulled.push(String(a?.repoPath));
          if (String(a?.repoPath) === "/r/a") await holdA.promise;
          return { policy: null, output: "ok" } as never;
        },
      }),
    );
    await store.openRepo("/r/a");
    await store.openRepo("/r/b");
    const run = store.runAcrossOpenRepos("pull", { concurrency: 1 });
    await tick();
    expect(pulled).toEqual(["/r/a"]);
    // /r/b is still queued when its checkout disappears.
    store.markRepoGone("/r/b");
    holdA.resolve({ policy: null, output: "ok" });
    const report = await run;
    expect(pulled).toEqual(["/r/a"]);
    expect(report.results.find((r) => r.path === "/r/b")?.status).toBe("skipped");
  });

  it("a bulk pull and a single pull on the same repository run one at a time", async () => {
    let inFlight = 0;
    let peak = 0;
    const gate = deferred<unknown>();
    const store = makeStore(
      makeInvoke({
        cmd_pull: async () => {
          inFlight += 1;
          peak = Math.max(peak, inFlight);
          await gate.promise;
          inFlight -= 1;
          return { policy: null, output: "ok" } as never;
        },
      }),
    );
    await store.openRepo("/r/a");
    const first = store.runAcrossOpenRepos("pull");
    const second = store.pull();
    await tick();
    gate.resolve(undefined);
    await Promise.all([first, second]);
    expect(peak).toBe(1);
  });

  it("HOLDS: the sweep token and the quit guard survive a hung repository, and the token is released after a failure and after cancellation", async () => {
    const hang = deferred<unknown>();
    const controller = new AbortController();
    const store = makeStore(
      makeInvoke({
        cmd_pull: async (_c, a) => {
          if (String(a?.repoPath) === "/r/a") await hang.promise;
          throw new Error("boom");
        },
      }),
    );
    await store.openRepo("/r/a");
    await store.openRepo("/r/b");
    const run = store.runAcrossOpenRepos("pull", { concurrency: 1, signal: controller.signal });
    await tick();
    expect(gitActionCount(get(store.mutationActivity))).toBeGreaterThan(0);
    expect(Object.keys(get(store.mutationActivity))).toContain("");
    controller.abort();
    hang.resolve(undefined);
    const report = await run;
    expect(report.failed).toBeGreaterThanOrEqual(1);
    expect(gitActionCount(get(store.mutationActivity))).toBe(0);
  });

  it("HOLDS: a bulk failure filed on a background repo does not resurface as an error when that tab is next opened", async () => {
    const store = makeStore(
      makeInvoke({
        cmd_pull: async (_c, a) => {
          if (String(a?.repoPath) === "/r/a") throw new Error("diverged");
          return { policy: null, output: "ok" } as never;
        },
      }),
    );
    await store.openRepo("/r/a");
    await store.openRepo("/r/b");
    const report = await store.runAcrossOpenRepos("pull");
    expect(report.failed).toBe(1);
    // The sweep already reports "failed=1" to its caller; the same error must not wait on the session as well.
    await store.activateTab(get(store).openTabs.find((t) => t.path === "/r/a")!.id);
    expect(get(store).error).toBeNull();
  });
});

describe("attack 6: echo window", () => {
  it("a pull that outlasts the 2.5s window keeps suppressing its own watcher echoes while it runs", async () => {
    vi.useFakeTimers();
    const loads = { n: 0 };
    const hold = deferred<unknown>();
    const store = makeStore(
      makeInvoke({
        cmd_pull: async () => hold.promise as never,
        cmd_list_branches: async () => {
          loads.n += 1;
          return [] as never;
        },
      }),
    );
    await store.openRepo("/r/a");
    loads.n = 0;
    const running = store.pull();
    await vi.advanceTimersByTimeAsync(3000);
    await store.handleRepoChanged("/r/a");
    await vi.advanceTimersByTimeAsync(600);
    // Mid-pull, our own .git writes: git is still working, the final refresh happens when it ends.
    expect(loads.n).toBe(0);
    hold.resolve({ policy: null, output: "ok" });
    await running;
  });

  it("a mutation that failed leaves no echo window, and a change made during its hold is replayed, so nothing is dropped unseen", async () => {
    vi.useFakeTimers();
    const loads = { n: 0 };
    const hold = deferred<unknown>();
    const store = makeStore(
      makeInvoke({
        cmd_pull: async () => {
          await hold.promise;
          throw new Error("CONFLICT: merge stopped half way");
        },
        cmd_list_branches: async () => {
          loads.n += 1;
          return [] as never;
        },
      }),
    );
    await store.openRepo("/r/a");
    loads.n = 0;
    // A real change lands while the pull is held, then the pull fails: the
    // failure never refreshes, so the dropped event is replayed.
    const running = store.pull();
    await vi.advanceTimersByTimeAsync(0);
    await store.handleRepoChanged("/r/a");
    await vi.advanceTimersByTimeAsync(600);
    expect(loads.n).toBe(0);
    hold.resolve(undefined);
    await running;
    await vi.advanceTimersByTimeAsync(600);
    expect(loads.n).toBeGreaterThan(0);
    // ...and a change right after the failure is not suppressed either.
    loads.n = 0;
    await store.handleRepoChanged("/r/a");
    await vi.advanceTimersByTimeAsync(600);
    // The failure path never refreshes; the watcher is the only thing that could show what the failed pull left behind.
    expect(loads.n).toBeGreaterThan(0);
  });
});

describe("attack 7: surfaced", () => {
  it("concurrent failures with the same message on the active tab all report surfaced:true, so reportFailure adds no extra toast", async () => {
    const store = makeStore(
      makeInvoke({
        cmd_create_branch: async () => {
          throw new Error("already exists");
        },
      }),
    );
    await store.openRepo("/r/a");
    const a = store.captureTarget()!;
    const outcomes = await Promise.all([store.createBranch("x", "s", a), store.createBranch("y", "s", a), store.createBranch("z", "s", a)]);
    // One error banner is shown for the active tab; every outcome that matches it is "surfaced".
    expect(outcomes.filter((o) => !o.ok && !o.surfaced)).toEqual([]);
  });
});

describe("attack 4: prompts", () => {
  it("RULED: a validate() that throws is shown in the prompt rather than thrown; the prompt stays open until answered or cancelled", async () => {
    const asked = askText({
      title: "t",
      message: "m",
      validate: () => {
        throw new Error("validator bug");
      },
    } as never);
    expect(isPromptOpen()).toBe(true);
    expect(() => completePrompt("typed")).not.toThrow();
    expect(get(promptError)).toContain("validator bug");
    expect(isPromptOpen()).toBe(true);
    cancelPrompt();
    expect(await asked).toBeNull();
    expect(isPromptOpen()).toBe(false);
    expect(get(promptState)).toBeNull();
  });
});
