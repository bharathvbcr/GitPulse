import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import {
  DIAGNOSTIC_STORAGE_KEY,
  MAX_DIAGNOSTIC_ENTRIES,
  createDiagnostics,
  formatDiagnosticReport,
  formatDiagnosticTime,
  installGlobalDiagnostics,
  isHostRuntimeNoise,
  type DiagnosticEntry,
  type DiagnosticSeverity,
} from "./diagnostics";
import { memoryStorage } from "../repos/persist";

function makeStore(now: number[] = [], storage = memoryStorage()) {
  let tick = 0;
  return {
    store: createDiagnostics({
      storage,
      now: () => now[tick++] ?? now[now.length - 1] ?? 0,
    }),
    storage,
  };
}

describe("createDiagnostics", () => {
  it("records errors and warnings newest-first through formatError", () => {
    const { store } = makeStore([10, 20]);
    store.error("console", new TypeError("nope"));
    store.warn("console", "watch out");
    const entries = get(store);
    expect(entries.map((entry) => entry.severity)).toEqual(["warning", "error"]);
    expect(entries[1].message).toBe("nope");
    expect(entries.map((entry) => entry.id)).toEqual([2, 1]);
  });

  it("coalesces identical consecutive repeats into a counted entry", () => {
    const { store } = makeStore([1, 2, 3]);
    store.error("repo", "clone failed");
    store.error("repo", "clone failed");
    store.error("repo", "clone failed");
    const entries = get(store);
    expect(entries).toHaveLength(1);
    expect(entries[0].count).toBe(3);
    expect(entries[0].at).toBe(3);
  });

  it("keeps non-consecutive repeats as separate entries", () => {
    const { store } = makeStore();
    store.error("a", "same");
    store.warn("b", "other");
    store.error("a", "same");
    const entries = get(store);
    expect(entries).toHaveLength(3);
    expect(entries.every((entry) => entry.count === 1)).toBe(true);
  });

  it("drops the oldest entries past the ring-buffer bound", () => {
    const { store } = makeStore();
    for (let i = 0; i < MAX_DIAGNOSTIC_ENTRIES + 5; i += 1) {
      store.error("test", `failure ${i}`);
    }
    const entries = get(store);
    expect(entries).toHaveLength(MAX_DIAGNOSTIC_ENTRIES);
    expect(entries[0].message).toBe(`failure ${MAX_DIAGNOSTIC_ENTRIES + 4}`);
    expect(entries.at(-1)?.message).toBe("failure 5");
  });

  it("clamps oversized messages instead of storing them whole", () => {
    const { store } = makeStore();
    store.error("test", "x".repeat(5000));
    const entries = get(store);
    expect(entries[0].message.length).toBeLessThanOrEqual(2001);
    expect(entries[0].message.endsWith("…")).toBe(true);
  });

  it("persists entries and restores them into a fresh instance", () => {
    const storage = memoryStorage();
    const first = createDiagnostics({ storage, now: () => 5 });
    first.error("pane-crash", "graph blew up");
    expect(JSON.parse(storage.getItem(DIAGNOSTIC_STORAGE_KEY) ?? "[]")).toHaveLength(1);

    const second = createDiagnostics({ storage, now: () => 6 });
    const entries = get(second);
    expect(entries.map((entry) => entry.source)).toEqual(["pane-crash"]);
    // New entries continue the restored id sequence instead of colliding.
    second.error("console", "fresh failure");
    const after = get(second);
    expect(after[0].id).toBeGreaterThan(entries[0].id);
  });

  it("survives corrupt or hostile persisted blobs", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        null,
        42,
        { id: "x", at: 1, severity: "error", source: "s", message: "m" },
        { id: 7, at: Number.NaN, severity: "error", source: "s", message: "bad time" },
        { id: 9, at: 3, severity: "catastrophic", source: "s", message: "bad severity" },
        { id: 11, at: 4, severity: "warning", source: "ok", message: "kept" },
        { id: 5, at: 2, severity: "error", source: "reordered", message: "sorted by id", count: 0 },
      ]),
    });
    const store = createDiagnostics({ storage });
    const entries = get(store);
    expect(entries.map((entry) => entry.id)).toEqual([11, 5]);
    expect(entries[1].count).toBe(1);
  });

  it("starts empty when the blob is not valid JSON", () => {
    const storage = memoryStorage({ [DIAGNOSTIC_STORAGE_KEY]: "{not json" });
    const store = createDiagnostics({ storage });
    expect(get(store)).toEqual([]);
  });

  it("clear() empties memory and removes the persisted blob", () => {
    const { store, storage } = makeStore();
    store.error("test", "gone soon");
    expect(storage.getItem(DIAGNOSTIC_STORAGE_KEY)).not.toBeNull();
    store.clear();
    expect(get(store)).toEqual([]);
    expect(storage.getItem(DIAGNOSTIC_STORAGE_KEY)).toBeNull();
  });

  it("works with storage disabled entirely", () => {
    const store = createDiagnostics({ storage: null });
    store.error("test", "memory only");
    expect(get(store)).toHaveLength(1);
  });

  it("does not record host-runtime noise that is not a GitPulse failure", () => {
    const { store } = makeStore();
    store.warn(
      "console",
      "[TAURI] Couldn't find callback id 2063278846. This might happen when the app is reloaded while Rust is running an asynchronous operation.",
    );
    store.warn(
      "console",
      "IPC custom protocol failed, Tauri will now use the postMessage interface instead Load failed",
    );
    store.error(
      "console",
      "[hmr] Failed to reload /src/app.css. This could be due to syntax errors or importing non-existent modules. (see errors above)",
    );
    store.error("console", "Importing a module script failed.");
    store.error("unhandled-rejection", "undefined is not an object (evaluating 'module.default')");
    store.error("repo", "clone failed");
    expect(get(store).map((entry) => entry.message)).toEqual(["clone failed"]);
  });

  it("drops host-runtime noise when restoring a persisted blob", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        {
          id: 2,
          at: 2,
          severity: "warning",
          source: "console",
          message:
            "[TAURI] Couldn't find callback id 1. This might happen when the app is reloaded while Rust is running an asynchronous operation.",
          count: 12,
        },
        {
          id: 1,
          at: 1,
          severity: "error",
          source: "pane-crash",
          message: "graph blew up",
          count: 1,
        },
      ]),
    });
    const store = createDiagnostics({ storage });
    expect(get(store).map((entry) => entry.message)).toEqual(["graph blew up"]);
    expect(JSON.parse(storage.getItem(DIAGNOSTIC_STORAGE_KEY) ?? "[]")).toEqual([
      { id: 1, at: 1, severity: "error", source: "pane-crash", message: "graph blew up", count: 1 },
    ]);
  });
});

describe("isHostRuntimeNoise", () => {
  it("matches the Tauri reload, IPC fallback, and Vite HMR messages from a WKWebView session", () => {
    const fromDump = [
      "[TAURI] Couldn't find callback id 3802601472. This might happen when the app is reloaded while Rust is running an asynchronous operation.",
      "IPC custom protocol failed, Tauri will now use the postMessage interface instead Load failed",
      "[hmr] Failed to reload /src/lib/components/SettingsModal.svelte. This could be due to syntax errors or importing non-existent modules. (see errors above)",
      "Importing a module script failed.",
      "undefined is not an object (evaluating 'module.default')",
    ];
    for (const message of fromDump) {
      expect(isHostRuntimeNoise(message), message).toBe(true);
    }
  });

  it("does not swallow product failures that merely mention reload or modules", () => {
    const keep = [
      "clone failed",
      "Couldn't find callback in the rebase plan",
      "Failed to reload the repository graph",
      "Importing a patch failed.",
      "undefined is not an object (evaluating 'commit.defaultBranch')",
    ];
    for (const message of keep) {
      expect(isHostRuntimeNoise(message), message).toBe(false);
    }
  });
});

describe("formatDiagnosticReport", () => {
  const generatedAt = new Date("2026-08-25T12:00:00Z");

  it("reports an empty log explicitly", () => {
    expect(formatDiagnosticReport([], generatedAt)).toContain("nothing recorded");
  });

  it("sums coalesced occurrences and lists distinct entries", () => {
    const entries: DiagnosticEntry[] = [
      { id: 2, at: Date.parse("2026-08-25T11:00:00Z"), severity: "warning", source: "console", message: "careful", count: 2 },
      { id: 1, at: Date.parse("2026-08-25T10:00:00Z"), severity: "error", source: "repo", message: "boom", count: 3 },
    ];
    const report = formatDiagnosticReport(entries, generatedAt);
    expect(report).toContain("3 error(s), 2 warning(s), 2 distinct");
    expect(report).toContain("[2026-08-25T11:00:00.000Z] WARNING x2 (console)");
    expect(report).toContain("[2026-08-25T10:00:00.000Z] ERROR x3 (repo)");
    expect(report).toContain("Generated: 2026-08-25T12:00:00.000Z");
  });

  it("indents multi-line messages so they stay inside one block", () => {
    const entries: DiagnosticEntry[] = [
      { id: 1, at: 0, severity: "error", source: "t", message: "line one\nline two", count: 1 },
    ];
    const report = formatDiagnosticReport(entries, generatedAt);
    expect(report).toContain("(t)\n  line one\n  line two");
  });

  it("marks single occurrences without an x-count suffix", () => {
    const entries: DiagnosticEntry[] = [
      { id: 1, at: 0, severity: "error", source: "t", message: "once", count: 1 },
    ];
    expect(formatDiagnosticReport(entries, generatedAt)).toContain("] ERROR (t)");
  });
});

describe("formatDiagnosticTime", () => {
  it("shows only the clock for today and prepends the date otherwise", () => {
    const now = new Date(2026, 7, 25, 12, 0, 0).getTime();
    const today = new Date(2026, 7, 25, 9, 30, 5).getTime();
    const earlier = new Date(2026, 6, 1, 9, 30, 5).getTime();

    const todayText = formatDiagnosticTime(today, now);
    const earlierText = formatDiagnosticTime(earlier, now);
    expect(todayText).toMatch(/^\d{2}:\d{2}:\d{2}$/);
    expect(earlierText).toContain("2026");
    expect(earlierText).not.toBe(todayText);
  });
});

describe("installGlobalDiagnostics", () => {
  type FailureEvent = { reason?: unknown; error?: unknown; message?: unknown };
  interface FakeTarget {
    addEventListener(type: string, listener: (event: FailureEvent) => void): void;
    removeEventListener(type: string, listener: (event: FailureEvent) => void): void;
    emit(type: string, event: FailureEvent): void;
  }

  function makeTarget(): FakeTarget {
    const listeners = new Map<string, Array<(event: FailureEvent) => void>>();
    return {
      addEventListener(type, listener) {
        listeners.set(type, [...(listeners.get(type) ?? []), listener]);
      },
      removeEventListener(type, listener) {
        listeners.set(
          type,
          (listeners.get(type) ?? []).filter((candidate) => candidate !== listener),
        );
      },
      emit(type, event) {
        for (const listener of listeners.get(type) ?? []) listener(event);
      },
    };
  }

  function setup() {
    const recorded: Array<{ severity: DiagnosticSeverity; source: string; message: string }> = [];
    const sink = {
      error: (source: string, detail: unknown) =>
        recorded.push({ severity: "error" as const, source, message: String(detail) }),
      warn: (source: string, detail: unknown) =>
        recorded.push({ severity: "warning" as const, source, message: String(detail) }),
    };
    // The installer replaces con.error/con.warn with wrappers, so tests keep
    // references to the original spies to observe forwarded calls.
    const originalError = vi.fn();
    const originalWarn = vi.fn();
    const con = { error: originalError, warn: originalWarn };
    const target = makeTarget();
    const uninstall = installGlobalDiagnostics(sink, { target, console: con });
    return { recorded, originalError, originalWarn, con, target, uninstall };
  }

  it("records console.error/warn calls and still forwards them untouched", () => {
    const { recorded, originalError, originalWarn, con, uninstall } = setup();
    con.error("disk full", { code: 28 });
    con.warn("getting tight");
    expect(recorded).toEqual([
      { severity: "error", source: "console", message: 'disk full {"code":28}' },
      { severity: "warning", source: "console", message: "getting tight" },
    ]);
    expect(originalError).toHaveBeenCalledWith("disk full", { code: 28 });
    expect(originalWarn).toHaveBeenCalledWith("getting tight");
    uninstall();
  });

  it("records unhandled rejections and uncaught errors from window events", () => {
    const { recorded, target, uninstall } = setup();
    target.emit("unhandledrejection", { reason: new Error("promise died") });
    target.emit("error", { error: undefined, message: "syntax goop" });
    target.emit("error", { error: new TypeError("typed badly") });
    expect(recorded.map((entry) => entry.source)).toEqual([
      "unhandled-rejection",
      "uncaught-error",
      "uncaught-error",
    ]);
    expect(recorded[0].message).toContain("promise died");
    expect(recorded[1].message).toBe("syntax goop");
    expect(recorded[2].message).toContain("typed badly");
    uninstall();
  });

  it("forwards host-runtime noise to the original console without recording it", () => {
    const { recorded, originalWarn, originalError, con, target, uninstall } = setup();
    con.warn(
      "[TAURI] Couldn't find callback id 1. This might happen when the app is reloaded while Rust is running an asynchronous operation.",
    );
    con.error("Importing a module script failed.");
    target.emit("unhandledrejection", {
      reason: "undefined is not an object (evaluating 'module.default')",
    });
    expect(recorded).toEqual([]);
    expect(originalWarn).toHaveBeenCalledTimes(1);
    expect(originalError).toHaveBeenCalled();
    uninstall();
  });

  it("keeps the legacy console prefixes on global failures", () => {
    const { originalError, target, uninstall } = setup();
    target.emit("unhandledrejection", { reason: "late" });
    target.emit("error", { message: "sync death" });
    expect(originalError).toHaveBeenNthCalledWith(
      1,
      "[gitpulse] unhandled promise rejection: late",
    );
    expect(originalError).toHaveBeenNthCalledWith(2, "[gitpulse] uncaught error: sync death");
    uninstall();
  });

  it("does not recurse when the sink itself logs via console", () => {
    const originalError = vi.fn();
    const con = { error: originalError, warn: vi.fn() };
    const sink = {
      error: (_source: string, _detail: unknown) => {
        con.error("sink exploded");
      },
      warn: () => {},
    };
    installGlobalDiagnostics(sink, { target: makeTarget(), console: con });
    expect(() => con.error("original")).not.toThrow();
    // Inner call first (during recording), then the forwarded original.
    expect(originalError).toHaveBeenNthCalledWith(1, "sink exploded");
    expect(originalError).toHaveBeenNthCalledWith(2, "original");
  });

  it("restores the original console functions and detaches listeners", () => {
    const { recorded, originalError, con, target, uninstall } = setup();
    const wrapper = con.error;
    uninstall();
    uninstall(); // double-uninstall stays safe
    expect(con.error).not.toBe(wrapper);
    con.error("after restore");
    target.emit("unhandledrejection", { reason: "late again" });
    expect(recorded).toEqual([]);
    expect(originalError).toHaveBeenCalledTimes(1);
  });
});
