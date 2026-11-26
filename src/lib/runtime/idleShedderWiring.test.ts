import { beforeEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import { resetForegroundFocus } from "./foreground";
import { IDLE_MEMORY_SCHEDULES, type IdleShedderTimers, type IdleStage } from "./idleShedder";
import {
  idleMemoryMode,
  loadIdleMemoryMode,
  loadIdleMemoryStatus,
  resetIdleMemoryStateForTests,
  saveIdleMemoryMode,
  startIdleShedder,
  stopIdleShedder,
  type InvokeLike,
} from "./idleShedderWiring";
import type { PressureInput } from "../stores/hostLoadStore";

function fakeClock() {
  let now = 10_000;
  let nextId = 1;
  const tasks = new Map<number, { at: number; cb: () => void }>();
  const timers: IdleShedderTimers = {
    now: () => now,
    setTimeout(cb, ms) {
      const id = nextId++;
      tasks.set(id, { at: now + ms, cb });
      return id;
    },
    clearTimeout(handle) {
      tasks.delete(handle as number);
    },
  };
  const advance = (ms: number) => {
    const target = now + ms;
    while (true) {
      let earliestId: number | null = null;
      let earliestAt = Infinity;
      for (const [id, task] of tasks) {
        if (task.at <= target && task.at < earliestAt) {
          earliestAt = task.at;
          earliestId = id;
        }
      }
      if (earliestId === null) break;
      const task = tasks.get(earliestId)!;
      tasks.delete(earliestId);
      now = task.at;
      task.cb();
    }
    now = target;
  };
  return { timers, advance };
}

function fakeHost() {
  const docListeners = new Map<string, Set<() => void>>();
  const winListeners = new Map<string, Set<() => void>>();
  let hidden = false;
  let focused = true;

  const doc = {
    get hidden() {
      return hidden;
    },
    get visibilityState() {
      return hidden ? "hidden" : "visible";
    },
    hasFocus: () => focused,
    addEventListener(type: string, fn: () => void) {
      if (!docListeners.has(type)) docListeners.set(type, new Set());
      docListeners.get(type)!.add(fn);
    },
    removeEventListener(type: string, fn: () => void) {
      docListeners.get(type)?.delete(fn);
    },
  };

  const win = {
    addEventListener(type: string, fn: () => void) {
      if (!winListeners.has(type)) winListeners.set(type, new Set());
      winListeners.get(type)!.add(fn);
    },
    removeEventListener(type: string, fn: () => void) {
      winListeners.get(type)?.delete(fn);
    },
  };

  const fire = (target: Map<string, Set<() => void>>, type: string) => {
    for (const fn of [...(target.get(type) ?? [])]) fn();
  };

  return {
    doc: doc as unknown as EventTarget & { hidden: boolean; visibilityState: string; hasFocus: () => boolean },
    win: win as unknown as EventTarget,
    blur() {
      focused = false;
      fire(winListeners, "blur");
    },
    focus() {
      focused = true;
      hidden = false;
      fire(winListeners, "focus");
    },
    hide() {
      hidden = true;
      focused = false;
      fire(docListeners, "visibilitychange");
    },
    show() {
      hidden = false;
      focused = true;
      fire(docListeners, "visibilitychange");
      fire(winListeners, "focus");
    },
  };
}

describe("idleShedderWiring", () => {
  beforeEach(() => {
    resetForegroundFocus();
    resetIdleMemoryStateForTests();
  });

  it("loads, normalizes, and saves idleMemoryMode via IPC and updates active shedder", async () => {
    const invokeSpy = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "cmd_idle_memory_settings") return { mode: "aggressive" };
      if (cmd === "cmd_idle_memory_settings_save") {
        return { mode: (args?.settings as { mode: string })?.mode ?? "balanced" };
      }
      if (cmd === "cmd_idle_memory_status") {
        return { lifecycle: "visible", hibernate_count: 2, last_wake_ms: 48 };
      }
      return undefined;
    });
    const invoke = invokeSpy as unknown as InvokeLike;

    expect(get(idleMemoryMode)).toBe("balanced");
    const loaded = await loadIdleMemoryMode({ isTauri: () => true, invoke });
    expect(loaded).toBe("aggressive");
    expect(get(idleMemoryMode)).toBe("aggressive");

    const saved = await saveIdleMemoryMode("off", { isTauri: () => true, invoke });
    expect(saved).toBe("off");
    expect(get(idleMemoryMode)).toBe("off");
    expect(invokeSpy).toHaveBeenCalledWith("cmd_idle_memory_settings_save", {
      settings: { mode: "off" },
    });

    const status = await loadIdleMemoryStatus({ isTauri: () => true, invoke });
    expect(status).toEqual({ lifecycle: "visible", hibernate_count: 2, last_wake_ms: 48 });
  });

  it("wires Stage 1A, Stage 1B, Resume (with graph reload), and Stage 2 hibernation", async () => {
    const { timers, advance } = fakeClock();
    const host = fakeHost();
    const pressure = writable<PressureInput>("normal");
    const profile = writable({
      budgets: {
        gpu_contexts: 6,
        gpu_hide_grace_ms: 120_000,
        graph_cache_repositories: 4,
      },
    });
    const stages: IdleStage[] = [];
    const invalidated: Array<string | undefined> = [];
    const wakeHints: unknown[] = [];

    const gpu = {
      policy: { hideGraceMs: 120_000 },
      slots: {
        setCap: vi.fn(),
        releaseIdle: vi.fn(() => 2),
        setSuspended: vi.fn(),
      },
    };
    const graph = {
      setCacheLimit: vi.fn(),
      shedAll: vi.fn(() => 2),
      showRepo: vi.fn(),
      loadGraph: vi.fn(async () => undefined),
    };
    const evictHarness = vi.fn(() => 1);
    const flush = vi.fn();
    const invokeSpy = vi.fn(async (cmd: string) => {
      if (cmd === "cmd_main_hibernate") return true;
      return undefined;
    });
    const invoke = invokeSpy as unknown as InvokeLike;

    const shedder = startIdleShedder({
      initialMode: "balanced",
      skipInitialLoad: true,
      isTauri: () => true,
      invoke,
      doc: host.doc,
      win: host.win,
      timers,
      pressureSource: pressure,
      profileSource: profile,
      activeRepo: () => "/work/repo-a",
      openRepoPaths: () => ["/work/repo-a", "/work/repo-b"],
      repoTrustRequired: () => false,
      graphFilter: () => ({ searchQuery: "fix", selectedBranch: "main", refScope: "all" }),
      gitMutationCount: () => 0,
      pendingFileSaves: () => 0,
      hasUnsavedDrafts: () => false,
      hasOpenModal: () => false,
      onFlushBeforeHibernate: flush,
      snapshotMeta: () => ({
        surface: "repository",
        sidebarWidth: 280,
        hasTerminalDock: true,
        parkedBrowserUrls: ["http://localhost:5173"],
      }),
      gpu,
      graph,
      invalidateListing: (path) => invalidated.push(path),
      evictHarness,
      publishStage: (s) => stages.push(s),
      writeWakeHint: (h) => wakeHints.push(h),
    });

    // Stage 1A on blur
    host.blur();
    advance(IDLE_MEMORY_SCHEDULES.balanced.softShedMs);
    expect(gpu.policy.hideGraceMs).toBe(0);
    expect(gpu.slots.releaseIdle).toHaveBeenCalledTimes(1);
    expect(graph.setCacheLimit).toHaveBeenCalledWith(1);
    expect(invalidated).toEqual(["/work/repo-b"]);
    expect(evictHarness).toHaveBeenCalledTimes(1);
    expect(stages).toEqual(["soft_shed"]);

    // Stage 1B on hide
    host.hide();
    advance(IDLE_MEMORY_SCHEDULES.balanced.deepShedMs);
    expect(gpu.slots.setSuspended).toHaveBeenCalledWith(true);
    expect(graph.shedAll).toHaveBeenCalledTimes(1);
    expect(invalidated).toEqual(["/work/repo-b", undefined]);
    expect(invokeSpy).toHaveBeenCalledWith("cmd_webview_idle_hint", { lowMemory: true });
    expect(stages).toEqual(["soft_shed", "deep_shed"]);

    // Resume from deep_shed restores profile budgets and reloads active graph
    host.show();
    expect(gpu.slots.setCap).toHaveBeenCalledWith(6);
    expect(gpu.policy.hideGraceMs).toBe(120_000);
    expect(graph.setCacheLimit).toHaveBeenCalledWith(4);
    expect(gpu.slots.setSuspended).toHaveBeenCalledWith(false);
    expect(invokeSpy).toHaveBeenCalledWith("cmd_webview_idle_hint", { lowMemory: false });
    expect(graph.showRepo).toHaveBeenCalledWith("/work/repo-a");
    expect(graph.loadGraph).toHaveBeenCalledWith("/work/repo-a", "fix", "main", {
      refScope: "all",
    });
    expect(stages).toEqual(["soft_shed", "deep_shed", "active"]);

    // Stage 2 hibernation after full hibernate timeout
    host.hide();
    advance(IDLE_MEMORY_SCHEDULES.balanced.hibernateMs);
    await Promise.resolve();
    expect(flush).toHaveBeenCalledTimes(1);
    expect(wakeHints).toHaveLength(1);
    expect(wakeHints[0]).toMatchObject({
      surface: "repository",
      sidebarWidth: 280,
      hasTerminalDock: true,
    });
    expect(invokeSpy).toHaveBeenCalledWith("cmd_main_hibernate", {
      snapshot: expect.objectContaining({
        surface: "repository",
        active_repo: "/work/repo-a",
        sidebar_width: 280,
        has_terminal_dock: true,
        parked_browser_urls: ["http://localhost:5173"],
      }),
    });

    expect(get(shedder.stage)).toBe("hibernating");
    stopIdleShedder();
  });

  it("defers hibernation whenever mutations, pending saves, unsaved drafts, or open modals exist", async () => {
    const { timers } = fakeClock();
    const host = fakeHost();
    let gitCount = 1;
    let saves = 0;
    let drafts = false;
    let modal = false;
    const invokeSpy = vi.fn(async () => true);
    const invoke = invokeSpy as unknown as InvokeLike;

    const shedder = startIdleShedder({
      initialMode: "balanced",
      skipInitialLoad: true,
      isTauri: () => true,
      invoke,
      doc: host.doc,
      win: host.win,
      timers,
      gitMutationCount: () => gitCount,
      pendingFileSaves: () => saves,
      hasUnsavedDrafts: () => drafts,
      hasOpenModal: () => modal,
      gpu: {
        policy: { hideGraceMs: 15_000 },
        slots: { setCap: vi.fn(), releaseIdle: vi.fn(() => 0), setSuspended: vi.fn() },
      },
      graph: {
        setCacheLimit: vi.fn(),
        shedAll: vi.fn(() => 0),
        showRepo: vi.fn(),
        loadGraph: vi.fn(async () => undefined),
      },
      invalidateListing: vi.fn(),
      evictHarness: vi.fn(() => 0),
      writeWakeHint: vi.fn(),
    });

    host.hide();
    expect(await shedder.tryHibernateNow()).toBe(false);
    expect(get(shedder.metrics).lastDeferReason).toBe("Git action in progress");

    gitCount = 0;
    saves = 2;
    expect(await shedder.tryHibernateNow()).toBe(false);
    expect(get(shedder.metrics).lastDeferReason).toBe("Editor file save in progress");

    saves = 0;
    drafts = true;
    expect(await shedder.tryHibernateNow()).toBe(false);
    expect(get(shedder.metrics).lastDeferReason).toBe("Unsaved editor drafts");

    drafts = false;
    modal = true;
    expect(await shedder.tryHibernateNow()).toBe(false);
    expect(get(shedder.metrics).lastDeferReason).toBe("Interactive modal is open");

    modal = false;
    expect(await shedder.tryHibernateNow()).toBe(true);
    expect(invokeSpy).toHaveBeenCalledWith("cmd_main_hibernate", expect.any(Object));
    stopIdleShedder();
  });
});
