import { describe, expect, it, vi } from "vitest";
import { runBootSequence, type BootStepDeps, type BootStepName } from "./bootSequence";
import { createListenerTracker } from "../dom/listenerTracker";

/**
 * Recording fakes: every dep appends to a shared call log so ordering and
 * continuation can be asserted from one array.
 */
function createHarness(overrides: Partial<BootStepDeps> = {}) {
  const log: string[] = [];
  const errors: Array<{ step: BootStepName; err: unknown }> = [];
  let repoChangedHandler: ((path?: string) => void) | null = null;
  const deps: BootStepDeps = {
    subscribeNativeShell: async () => {
      log.push("subscribe-native-shell");
      return () => log.push("unsub-native-shell");
    },
    takePendingOpen: async () => {
      log.push("take-pending-open");
      return null;
    },
    restoreWorkspace: async () => {
      log.push("restore-workspace");
    },
    openRepo: async (path) => {
      log.push(`open-repo:${path}`);
    },
    syncRecentMenu: async (paths) => {
      log.push(`sync-recent-menu:[${paths.join("|")}]`);
    },
    handleRepoChanged: (path) => {
      log.push(`handle-repo-changed:${path ?? "undefined"}`);
    },
    listenRepoChanged: async (handler) => {
      log.push("listen-repo-changed");
      repoChangedHandler = handler;
      return () => log.push("unlisten-repo-changed");
    },
    track: (unlisten) => {
      log.push("track");
      tracked.push(unlisten);
    },
    onError: (step, err) => {
      errors.push({ step, err });
    },
  };
  const tracked: Array<() => void> = [];
  return {
    deps: Object.assign(deps, overrides),
    log,
    errors,
    tracked,
    get repoChangedHandler() {
      return repoChangedHandler;
    },
  };
}

const RECENTS = ["/repos/alpha", "/repos/beta"];

describe("runBootSequence", () => {
  it("happy path: exact ordering, pending applied after restore, menu fed the passed recents", async () => {
    const h = createHarness({
      takePendingOpen: async () => {
        h.log.push("take-pending-open");
        return "/repos/external";
      },
    });
    await runBootSequence(h.deps, [...RECENTS], {} as never);
    expect(h.log).toEqual([
      "subscribe-native-shell",
      "track",
      "take-pending-open",
      "restore-workspace",
      "open-repo:/repos/external",
      "sync-recent-menu:[/repos/alpha|/repos/beta]",
      "listen-repo-changed",
      "track",
    ]);
    expect(h.errors).toEqual([]);
    expect(h.tracked).toHaveLength(2);
  });

  it("skips open-pending when no external intent is queued", async () => {
    const h = createHarness();
    await runBootSequence(h.deps, [], {} as never);
    expect(h.log).not.toContain("open-repo:/repos/external");
    expect(h.log.filter((entry) => entry.startsWith("open-repo:"))).toEqual([]);
  });

  it("THE REGRESSION: restore failure is reported but later steps — including the repo-changed listener — still run", async () => {
    const boom = new Error("workspace blob corrupt");
    const h = createHarness({
      restoreWorkspace: async () => {
        h.log.push("restore-workspace");
        throw boom;
      },
    });
    await runBootSequence(h.deps, [...RECENTS], {} as never);
    // The failure was reported against its own step…
    expect(h.errors).toEqual([{ step: "workspace-restore", err: boom }]);
    // …the menu was still synced AFTER the failing step…
    expect(h.log.indexOf("sync-recent-menu:[/repos/alpha|/repos/beta]")).toBeGreaterThan(
      h.log.indexOf("restore-workspace"),
    );
    // …and the watcher listener was registered with its unlisten tracked,
    // so watcher-driven refresh survives a bad persisted session.
    expect(h.log).toContain("listen-repo-changed");
    // Two tracked: the successful native-shell unsub plus the listener.
    expect(h.tracked).toHaveLength(2);
    h.tracked[1]?.();
    expect(h.log).toContain("unlisten-repo-changed");
  });

  it("a listen rejection is reported and nothing throws out of the sequence", async () => {
    const boom = new Error("vite preview has no Tauri event bus");
    const h = createHarness({
      listenRepoChanged: async () => {
        h.log.push("listen-repo-changed");
        throw boom;
      },
    });
    await expect(runBootSequence(h.deps, [], {} as never)).resolves.toBeUndefined();
    expect(h.errors).toEqual([{ step: "repo-changed-listen", err: boom }]);
  });

  it("survives every step throwing before the listener registration", async () => {
    const fail = (msg: string) => async () => {
      throw new Error(msg);
    };
    const h = createHarness({
      subscribeNativeShell: fail("no shell"),
      takePendingOpen: async () => {
        h.log.push("take-pending-open");
        return "/repos/external";
      },
      restoreWorkspace: fail("bad workspace"),
      openRepo: fail("gone"),
      syncRecentMenu: fail("no menu"),
      listenRepoChanged: async (handler) => {
        h.log.push("listen-repo-changed");
        handler("/repos/watched");
        return () => h.log.push("unlisten-repo-changed");
      },
      track: (unlisten) => {
        h.log.push("track");
        h.tracked.push(unlisten);
      },
    });
    await expect(runBootSequence(h.deps, [], {} as never)).resolves.toBeUndefined();
    // pending-open succeeded (it queued the intent); the other four
    // pre-listener steps each failed without stopping boot.
    expect(h.errors.map((e) => e.step)).toEqual([
      "native-shell",
      "workspace-restore",
      "open-pending",
      "recent-menu",
    ]);
    expect(h.log).not.toContain("open-repo:/repos/external");
    expect(h.log).toContain("listen-repo-changed");
    expect(h.log).toContain("handle-repo-changed:/repos/watched");
    expect(h.log.filter((entry) => entry === "track")).toHaveLength(1);
  });

  it("routes each failure to onError tagged with its own step name", async () => {
    const nativeErr = new Error("menu bus down");
    const menuErr = new Error("menu rebuild failed");
    const h = createHarness({
      subscribeNativeShell: async () => {
        throw nativeErr;
      },
      syncRecentMenu: async () => {
        throw menuErr;
      },
    });
    await runBootSequence(h.deps, [], {} as never);
    expect(h.errors).toEqual([
      { step: "native-shell", err: nativeErr },
      { step: "recent-menu", err: menuErr },
    ]);
    // Both non-fatal steps' successors still ran.
    expect(h.log).toContain("take-pending-open");
    expect(h.log).toContain("listen-repo-changed");
  });

  it("forwards repo-changed payloads through the injected handler", async () => {
    const h = createHarness();
    await runBootSequence(h.deps, [], {} as never);
    h.repoChangedHandler?.("/repos/live");
    h.repoChangedHandler?.(undefined);
    expect(h.log).toContain("handle-repo-changed:/repos/live");
    expect(h.log).toContain("handle-repo-changed:undefined");
  });

  it("composes with listenerTracker: teardown before resolution still unwinds late registrations", async () => {
    // Simulates the disposed-flag race App.svelte guards: cleanup runs
    // (dispose) while boot awaits, then steps resolve and must not leak.
    const tracker = createListenerTracker();
    const unlisten = vi.fn();
    const h = createHarness({
      track: (fn) => tracker.track(fn),
    });
    tracker.dispose(); // owner torn down first…
    await runBootSequence(h.deps, [], {} as never); // …registrations land after
    expect(tracker.disposed).toBe(true);
    expect(tracker.size).toBe(0);
    expect(unlisten).not.toHaveBeenCalled();

    // A genuinely late arrival self-unregisters instead of leaking.
    tracker.track(unlisten);
    expect(unlisten).toHaveBeenCalledTimes(1);

    // And the live path: tracking before dispose unwinds on teardown.
    const live = createListenerTracker();
    const h2 = createHarness({ track: (fn) => live.track(fn) });
    await runBootSequence(h2.deps, [], {} as never);
    expect(live.size).toBe(2);
    live.dispose();
    expect(h2.log).toContain("unsub-native-shell");
    expect(h2.log).toContain("unlisten-repo-changed");
  });
});
