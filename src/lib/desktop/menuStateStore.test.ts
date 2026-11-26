import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createRepoStore, type OpenRepoTab, type RepoState } from "../stores/repoStore";
import type { AgentInboxThread, InboxSnapshot } from "../agents/inboxTypes";
import type { MenuState } from "./menuState";

const mocks = vi.hoisted(() => {
  function miniStore<T>(initial: T) {
    let value = initial;
    const subs = new Set<(v: T) => void>();
    return {
      subscribe(fn: (v: T) => void) {
        subs.add(fn);
        fn(value);
        return () => subs.delete(fn);
      },
      set(next: T) {
        value = next;
        for (const fn of [...subs]) fn(value);
      },
    };
  }
  return {
    repoWritable: miniStore<RepoState | null>(null),
    mutationActivityWritable: miniStore<Record<string, string[]>>({}),
    invokeMock: vi.fn(),
    boardRefreshMock: vi.fn(),
    boardWakeMock: vi.fn(),
    workbenchSubscriber: null as {
      changed: (c: { sequence: number | null; source: "local" | "external" }) => void;
      unavailable: (m: string) => void;
    } | null,
  };
});

const baseEmptyRepo = (): RepoState => get(createRepoStore({ storage: null }));

function makeTab(path: string, opts: Partial<OpenRepoTab> = {}): OpenRepoTab {
  const name = path.split("/").pop() || "repo";
  return {
    id: path,
    path,
    name,
    label: name,
    pinned: false,
    isActive: false,
    isBare: false,
    isDirty: false,
    isLoading: false,
    error: null,
    currentBranch: "main",
    conflictedCount: 0,
    changedCount: 0,
    ...opts,
  };
}

function makeOpenAsk(threadKey: string, title: string): AgentInboxThread {
  const now = Date.now();
  return {
    thread_key: threadKey,
    title,
    status: "open",
    ask_type: "decide",
    from_agent: "claude-worker",
    to_agent: "developer",
    updated_at_ms: now,
    latest_msg: {
      seq: 1,
      from_agent: "claude-worker",
      to_agent: "developer",
      kind: "ask",
      body: JSON.stringify({ ask_type: "decide", question: title, options: ["Yes", "No"] }),
      branch: "main",
      created_at_ms: now,
      thread_key: threadKey,
    },
    messages: [
      {
        seq: 1,
        from_agent: "claude-worker",
        to_agent: "developer",
        kind: "ask",
        body: JSON.stringify({ ask_type: "decide", question: title, options: ["Yes", "No"] }),
        branch: "main",
        created_at_ms: now,
        thread_key: threadKey,
      },
    ],
  };
}

vi.mock("../stores/repoStore", async () => {
  const actual = await vi.importActual<typeof import("../stores/repoStore")>("../stores/repoStore");
  const { get: svelteGet } = await vi.importActual<typeof import("svelte/store")>("svelte/store");
  if (!mocks.repoWritable) {
    throw new Error("missing repoWritable");
  }
  const fallback = svelteGet(actual.createRepoStore({ storage: null }));
  mocks.repoWritable.set(fallback);
  return {
    ...actual,
    repoStore: {
      subscribe: (fn: (v: RepoState) => void) =>
        mocks.repoWritable.subscribe((v) => fn(v ?? fallback)),
      mutationActivity: { subscribe: mocks.mutationActivityWritable.subscribe },
      jumpStopCount: () => 0,
    },
  };
});

vi.mock("../ipc/invoke", () => ({
  invoke: (...args: unknown[]) => mocks.invokeMock(...args),
}));

vi.mock("../workbench/boardAgents", () => ({
  createBoardAgents: () => {
    const subs = new Set<(v: unknown) => void>();
    const snap = { runs: [], complete: true, pending: new Map(), readAt: null, error: null };
    return {
      subscribe: (fn: (v: unknown) => void) => {
        subs.add(fn);
        fn(snap);
        return () => subs.delete(fn);
      },
      start: vi.fn(),
      stop: vi.fn(),
      refresh: mocks.boardRefreshMock,
      wake: mocks.boardWakeMock,
    };
  },
}));

vi.mock("../workbench/client", () => ({
  subscribeWorkbenchChanges: (sub: typeof mocks.workbenchSubscriber) => {
    mocks.workbenchSubscriber = sub;
    return () => {
      if (mocks.workbenchSubscriber === sub) mocks.workbenchSubscriber = null;
    };
  },
}));

async function flushAsync(turns = 5): Promise<void> {
  for (let i = 0; i < turns; i += 1) {
    await Promise.resolve();
  }
}

describe("menuStateStore", () => {
  const originalWindow = globalThis.window;
  const originalDocument = globalThis.document;
  let windowTarget: EventTarget;

  beforeEach(() => {
    mocks.invokeMock.mockReset();
    mocks.boardRefreshMock.mockReset();
    mocks.boardWakeMock.mockReset();
    mocks.workbenchSubscriber = null;
    mocks.repoWritable.set(baseEmptyRepo());
    mocks.mutationActivityWritable.set({});

    windowTarget = new EventTarget();
    const docTarget = new EventTarget();
    Object.assign(docTarget, {
      hidden: false,
      visibilityState: "visible",
    });

    const mockWindow = Object.assign(windowTarget, {
      __TAURI_INTERNALS__: {},
      addEventListener: windowTarget.addEventListener.bind(windowTarget),
      removeEventListener: windowTarget.removeEventListener.bind(windowTarget),
      dispatchEvent: windowTarget.dispatchEvent.bind(windowTarget),
    });

    (globalThis as unknown as { window: unknown }).window = mockWindow;
    (globalThis as unknown as { document: unknown }).document = docTarget;
  });

  afterEach(() => {
    vi.useRealTimers();
    (globalThis as unknown as { window: unknown }).window = originalWindow;
    (globalThis as unknown as { document: unknown }).document = originalDocument;
  });

  it("trustedInboxRepoPaths skips trustRequired tabs and deduplicates currentPath", async () => {
    const { trustedInboxRepoPaths } = await import("./menuStateStore");
    const repo: RepoState = {
      ...baseEmptyRepo(),
      currentPath: "/repo/A",
      openTabs: [
        makeTab("/repo/A", { isActive: true }),
        makeTab("/repo/untrusted", { trustRequired: true }),
        makeTab("/repo/B"),
        makeTab("/repo/A"),
      ],
    };
    expect(trustedInboxRepoPaths(repo)).toEqual(["/repo/A", "/repo/B"]);

    const untrustedCurrent: RepoState = {
      ...baseEmptyRepo(),
      currentPath: "/repo/untrusted",
      openTabs: [makeTab("/repo/untrusted", { trustRequired: true, isActive: true }), makeTab("/repo/B")],
    };
    expect(trustedInboxRepoPaths(untrustedCurrent)).toEqual(["/repo/B"]);

    const untrackedCurrent: RepoState = {
      ...baseEmptyRepo(),
      currentPath: "/repo/C",
      openTabs: [makeTab("/repo/B")],
    };
    expect(trustedInboxRepoPaths(untrackedCurrent)).toEqual(["/repo/B", "/repo/C"]);
  });

  it("tags multi-repo inbox threads with originating repo_path and ignores stale responses when openTabs change mid-flight", async () => {
    let resolveRepoA!: (snap: InboxSnapshot) => void;
    const deferredA = new Promise<InboxSnapshot>((resolve) => {
      resolveRepoA = resolve;
    });

    mocks.invokeMock.mockImplementation((cmd: string, args: { repoPath?: string }) => {
      if (cmd !== "cmd_inbox_read") return Promise.resolve(null);
      if (args.repoPath === "/repo/A") {
        // First read is held on deferredA when /repo/A is active alone.
        if (mocks.invokeMock.mock.calls.filter((c) => c[1]?.repoPath === "/repo/A").length === 1) {
          return deferredA;
        }
        return Promise.resolve({
          ok: true,
          repo_path: "",
          threads: [makeOpenAsk("ask-A-fresh", "Fresh A")],
          lanes: [],
          total: 1,
        });
      }
      if (args.repoPath === "/repo/B") {
        return Promise.resolve({
          ok: true,
          repo_path: "/repo/B-canonical",
          threads: [makeOpenAsk("ask-B", "Ask B")],
          lanes: [],
          total: 1,
        });
      }
      return Promise.resolve({ ok: true, repo_path: args.repoPath ?? "", threads: [], lanes: [], total: 0 });
    });

    mocks.repoWritable.set({
      ...baseEmptyRepo(),
      currentPath: "/repo/A",
      isLoading: false,
      openTabs: [makeTab("/repo/A", { isActive: true })],
    });

    const { nativeMenuState } = await import("./menuStateStore");
    let latest!: MenuState;
    const unsub = nativeMenuState.subscribe((value) => {
      latest = value;
    });

    try {
      await flushAsync();
      expect(mocks.invokeMock).toHaveBeenCalledTimes(1);
      expect(mocks.invokeMock).toHaveBeenLastCalledWith("cmd_inbox_read", {
        repoPath: "/repo/A",
        status: "open",
        limit: 20,
      });

      // Switch openTabs while /repo/A read is still in flight.
      mocks.repoWritable.set({
        ...baseEmptyRepo(),
        currentPath: "/repo/B",
        isLoading: false,
        openTabs: [makeTab("/repo/A"), makeTab("/repo/B", { isActive: true })],
      });

      // Resolve the stale /repo/A read with a stale ask that must be discarded.
      resolveRepoA({
        ok: true,
        repo_path: "/repo/A",
        threads: [makeOpenAsk("ask-A-stale", "Stale A")],
        lanes: [],
        total: 1,
      });

      await flushAsync(10);

      // Stale A must not appear; queued forced re-run fetched both A (fresh) and B with repo_path tags.
      const requestIds = latest.agentRequests.map((r) => r.id);
      expect(requestIds).not.toContain("inbox:ask-A-stale");
      expect(requestIds).toContain("inbox:ask-A-fresh");
      expect(requestIds).toContain("inbox:ask-B");
      const reqA = latest.agentRequests.find((r) => r.id === "inbox:ask-A-fresh");
      const reqB = latest.agentRequests.find((r) => r.id === "inbox:ask-B");
      expect(reqA?.repoPath).toBe("/repo/A");
      expect(reqB?.repoPath).toBe("/repo/B-canonical");
      expect(latest.agentsWaiting).toBe(2);
    } finally {
      unsub();
    }
  });

  it("refreshes immediately on gitpulse:inbox-changed bypassing the 5s debounce and clears answered asks", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-11-19T12:00:00Z"));

    let askOpen = true;
    mocks.invokeMock.mockImplementation((cmd: string, args: { repoPath?: string }) => {
      if (cmd !== "cmd_inbox_read") return Promise.resolve(null);
      return Promise.resolve({
        ok: true,
        repo_path: args.repoPath ?? "/repo/A",
        threads: askOpen ? [makeOpenAsk("ask-A", "Deploy Repo A")] : [],
        lanes: [],
        total: askOpen ? 1 : 0,
      });
    });

    mocks.repoWritable.set({
      ...baseEmptyRepo(),
      currentPath: "/repo/A",
      isLoading: false,
      openTabs: [makeTab("/repo/A", { isActive: true })],
    });

    const { nativeMenuState } = await import("./menuStateStore");
    let latest!: MenuState;
    const unsub = nativeMenuState.subscribe((value) => {
      latest = value;
    });

    try {
      await flushAsync();
      expect(latest.agentsWaiting).toBe(1);
      expect(latest.agentRequests.map((r) => r.id)).toEqual(["inbox:ask-A"]);
      expect(mocks.invokeMock).toHaveBeenCalledTimes(1);

      // Advance 1s (< 5s MIN_INBOX_REFRESH_MS): workbench-changed is debounced for inbox.
      vi.advanceTimersByTime(1000);
      askOpen = false;
      mocks.workbenchSubscriber?.changed({ sequence: 1, source: "local" });
      await flushAsync();
      expect(mocks.boardRefreshMock).toHaveBeenCalledTimes(1);
      expect(mocks.invokeMock).toHaveBeenCalledTimes(1);
      expect(latest.agentsWaiting).toBe(1);

      // Dispatch gitpulse:inbox-changed within the same 5s window -> forces immediate refresh.
      windowTarget.dispatchEvent(new Event("gitpulse:inbox-changed"));
      await flushAsync();
      expect(mocks.invokeMock).toHaveBeenCalledTimes(2);
      expect(latest.agentsWaiting).toBe(0);
      expect(latest.agentRequests).toEqual([]);
    } finally {
      unsub();
    }
  });
});
