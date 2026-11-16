import { describe, expect, it, vi } from "vitest";
import { createSession } from "./projection";
import { createStatusPoller, STATUS_UNAVAILABLE_AFTER_FAILURES } from "./statusPoller";
import type { FileStatus, RepoSession } from "./storeTypes";
import { WATCH_ACTIVE, watchFailed } from "./watchState";

function harness(options: {
  session?: RepoSession;
  hidden?: boolean;
  invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
  hydrate?: () => Promise<void>;
} = {}) {
  let current: RepoSession | undefined =
    options.session ?? createSession({ id: "t1", path: "/repo/a", pinned: false }, { watch: WATCH_ACTIVE });
  const sessions = new Map<string, RepoSession>();
  if (current) sessions.set(current.id, current);
  const pollRuns = new Map<string, number>();
  let snapshotRun = 1;
  let clock = 1_000_000;

  const acquireWatch = vi.fn(async () => {});
  const hydrate = vi.fn(options.hydrate ?? (async () => {}));

  const poller = createStatusPoller({
    invoke: (options.invoke ?? (async () => [])) as never,
    activeSession: () => current,
    sessionById: (id) => sessions.get(id),
    applyToSession: (id, generation, patchOrFn) => {
      const live = sessions.get(id);
      if (!live || live.generation !== generation) return false;
      const patch = typeof patchOrFn === "function" ? patchOrFn(live) : patchOrFn;
      const next = { ...live, ...patch };
      sessions.set(id, next);
      if (current?.id === id) current = next;
      return true;
    },
    snapshotRunOf: () => snapshotRun,
    getPollRun: (id) => pollRuns.get(id),
    setPollRun: (id, run) => {
      pollRuns.set(id, run);
    },
    deletePollRun: (id) => {
      pollRuns.delete(id);
    },
    acquireWatch,
    hydrate,
    isBackground: () => options.hidden ?? false,
    now: () => clock,
  });

  return {
    poller,
    acquireWatch,
    hydrate,
    getSession: () => current,
    bumpSnapshotRun: () => {
      snapshotRun += 1;
    },
    advanceClock: (ms: number) => {
      clock += ms;
    },
  };
}

describe("createStatusPoller", () => {
  it("delegates degraded-watch sessions to full hydrate instead of status-only poll", async () => {
    const session = createSession({ id: "t1", path: "/repo/a", pinned: false }, { watch: watchFailed("watch unavailable") });
    const invoke = vi.fn(async () => []);
    const h = harness({ session, invoke });

    await h.poller.run();

    expect(h.hydrate).toHaveBeenCalledTimes(1);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("tracks consecutive status poll failures up to threshold and resets on success", async () => {
    let fail = true;
    const statuses: FileStatus[] = [
      { path: "src/a.ts", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 },
    ];
    const h = harness({
      invoke: async () => {
        if (fail) throw new Error("git status failed");
        return statuses;
      },
    });
    const polledChanges: string[] = [];
    const unsubscribe = h.poller.onPolledChange((repoPath) => {
      polledChanges.push(repoPath);
    });

    for (let i = 1; i < STATUS_UNAVAILABLE_AFTER_FAILURES; i += 1) {
      await h.poller.run();
      expect(h.getSession()?.statusPollFailures).toBe(i);
      expect(h.getSession()?.statusUnavailableSince).toBeNull();
    }

    await h.poller.run();
    expect(h.getSession()?.statusPollFailures).toBe(STATUS_UNAVAILABLE_AFTER_FAILURES);
    expect(h.getSession()?.statusUnavailableSince).not.toBeNull();

    fail = false;
    await h.poller.run();
    expect(h.getSession()?.statusPollFailures).toBe(0);
    expect(h.getSession()?.statusUnavailableSince).toBeNull();
    expect(h.getSession()?.statuses).toEqual(statuses);
    expect(polledChanges).toEqual(["/repo/a"]);
    unsubscribe();
  });

  it("discards a poll whose snapshotRun advanced while cmd_get_status was in flight", async () => {
    let h!: ReturnType<typeof harness>;
    h = harness({
      invoke: async () => {
        h.bumpSnapshotRun();
        return [{ path: "stale.ts", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 }];
      },
    });

    await h.poller.run();
    expect(h.getSession()?.statuses).toEqual([]);
  });

  it("skips polling while hidden or trustRequired", async () => {
    const hiddenInvoke = vi.fn(async () => []);
    const hiddenHarness = harness({ hidden: true, invoke: hiddenInvoke });
    await hiddenHarness.poller.run();
    expect(hiddenInvoke).not.toHaveBeenCalled();

    const untrustedSession = createSession({ id: "t1", path: "/repo/a", pinned: false }, { trustRequired: true });
    const untrustedInvoke = vi.fn(async () => []);
    const untrustedHarness = harness({ session: untrustedSession, invoke: untrustedInvoke });
    await untrustedHarness.poller.run();
    expect(untrustedInvoke).not.toHaveBeenCalled();
  });
});
