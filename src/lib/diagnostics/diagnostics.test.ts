import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import {
  APP_VERSION,
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
    store.error("test", `START${"x".repeat(5000)}END`);
    const entries = get(store);
    expect(entries[0].message.length).toBeLessThanOrEqual(2000);
    // This used to assert the message ended in "…", which encoded the very
    // defect being fixed: truncating from the head threw the ending away, and
    // for command output the ending is where the reason lives. A clamped
    // message now keeps both ends and says what it dropped in between.
    expect(entries[0].message).toMatch(/… \d+ characters elided …/);
    expect(entries[0].message.startsWith("START")).toBe(true);
    expect(entries[0].message.endsWith("END")).toBe(true);
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
      { id: 1, at: 0, severity: "error", source: "t", message: "line one\nline two", count: 1, version: "1.2.3" },
    ];
    // Recorded by the running build, so the header carries no build note.
    const report = formatDiagnosticReport(entries, generatedAt, "1.2.3");
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

/**
 * The ring is what a user copies to report a problem, so a clamped entry must
 * keep the part that explains the failure.
 *
 * Head-only truncation kept the wrong half: a real coverage failure produced
 * an 18,580-character message whose cause sat at character 18,356, and the
 * 2,000-character head held nothing but the aborting script's own chatter.
 */
describe("message clamping keeps both ends", () => {
  function longMessage(): string {
    const header = 'Coverage command "pytest --cov" failed (exit 3):';
    const noise = Array.from({ length: 900 }, (_, i) => `  ok   check number ${i}`).join("\n");
    const cause = [
      'INTERNALERROR>   File "/repo/bench/stress_test.py", line 944, in <module>',
      "INTERNALERROR>     sys.exit(1 if FAIL else 0)",
      "INTERNALERROR> SystemExit: 0",
      "============================ no tests ran in 17.74s ============================",
    ].join("\n");
    return [header, noise, cause].join("\n");
  }

  function recordAndRead(message: string) {
    const store = createDiagnostics({ storage: null, now: () => 0 });
    store.error("coverage", message);
    let entries: readonly { message: string }[] = [];
    store.subscribe((next) => (entries = next))();
    return entries[0].message;
  }

  it("keeps the ending, where the reason for a failure lives", () => {
    const kept = recordAndRead(longMessage());
    expect(kept).toContain("stress_test.py");
    expect(kept).toContain("line 944");
    expect(kept).toContain("SystemExit: 0");
    expect(kept).toContain("no tests ran");
  });

  it("keeps the beginning, which names the command", () => {
    const kept = recordAndRead(longMessage());
    expect(kept).toContain('Coverage command "pytest --cov" failed (exit 3):');
  });

  it("announces the elision, and the accounting is exact", () => {
    const message = longMessage();
    const kept = recordAndRead(message);
    const notice = kept.match(/\n… (\d+) characters elided …\n/);
    expect(notice, "a clamped message must say what it dropped").not.toBeNull();
    const dropped = Number(notice![1]);
    const retained = kept.length - notice![0].length;
    // Nothing unaccounted for: what was kept plus what was dropped is the
    // whole original message.
    expect(retained + dropped).toBe(message.length);
  });

  it("pays for the elision notice out of the budget, not on top of it", () => {
    const kept = recordAndRead(longMessage());
    expect(kept.length).toBeLessThanOrEqual(2000);
  });

  it("leaves a message inside the budget completely untouched", () => {
    const short = "Coverage command failed (exit 1):\nboom";
    expect(recordAndRead(short)).toBe(short);
    const exact = "x".repeat(2000);
    expect(recordAndRead(exact)).toBe(exact);
    expect(recordAndRead("x".repeat(2001))).toContain("characters elided");
  });

  it("never loses both ends of a pathological single-line payload", () => {
    const kept = recordAndRead(`START${"y".repeat(50_000)}END`);
    expect(kept.startsWith("START")).toBe(true);
    expect(kept.endsWith("END")).toBe(true);
  });
});

/**
 * The ring is persisted, so it outlives the build that wrote it. Without a
 * stamp, a log copied after an upgrade presents entries from an older build
 * as though they described the running one — which is how an already-fixed
 * bug reads as a live one.
 */
describe("build stamping", () => {
  const generatedAt = new Date("2026-08-25T12:00:00Z");

  function entry(overrides: Partial<DiagnosticEntry> = {}): DiagnosticEntry {
    return {
      id: 1,
      at: Date.parse("2026-08-25T11:00:00Z"),
      severity: "error",
      source: "coverage",
      message: "boom",
      count: 1,
      ...overrides,
    };
  }

  it("stamps new entries with the running build", () => {
    const store = createDiagnostics({ storage: null, now: () => 0 });
    store.error("coverage", "boom");
    expect(get(store)[0].version).toBe(APP_VERSION);
  });

  it("injects a real version rather than falling back to unknown", () => {
    // If the build-time define is ever dropped, this catches it before the
    // stamp silently degrades to a constant that says nothing.
    expect(APP_VERSION).not.toBe("unknown");
    expect(APP_VERSION).toMatch(/^\d+\.\d+\.\d+$/);
  });

  it("says nothing when the entry came from the running build", () => {
    const report = formatDiagnosticReport([entry({ version: "0.0.3" })], generatedAt, "0.0.3");
    expect(report).toContain("] ERROR (coverage)\n");
    expect(report).not.toContain("recorded by");
  });

  it("names the recording build when it differs from the running one", () => {
    const report = formatDiagnosticReport([entry({ version: "0.0.2" })], generatedAt, "0.0.3");
    expect(report).toContain("[recorded by 0.0.2, now running 0.0.3]");
  });

  it("marks an entry written before stamping existed", () => {
    const report = formatDiagnosticReport([entry()], generatedAt, "0.0.3");
    expect(report).toContain("[recorded by an earlier build, now running 0.0.3]");
  });

  it("names the running build in the report header", () => {
    const report = formatDiagnosticReport([entry({ version: "0.0.3" })], generatedAt, "0.0.3");
    expect(report).toContain("Generated: 2026-08-25T12:00:00.000Z by GitPulse 0.0.3");
  });

  it("never folds a repeat into an entry from a different build", () => {
    // Coalescing rewrites the timestamp, so merging across builds would claim
    // the older build produced something it never saw.
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        { id: 1, at: 1, severity: "error", source: "coverage", message: "boom", count: 1, version: "0.0.1" },
      ]),
    });
    const store = createDiagnostics({ storage, now: () => 99 });
    store.error("coverage", "boom");
    const entries = get(store);
    expect(entries).toHaveLength(2);
    expect(entries[0].version).toBe(APP_VERSION);
    expect(entries[0].count).toBe(1);
    expect(entries[1].version).toBe("0.0.1");
    expect(entries[1].at).toBe(1);
  });

  it("still coalesces repeats from the same build", () => {
    const store = createDiagnostics({ storage: null, now: () => 7 });
    store.error("coverage", "boom");
    store.error("coverage", "boom");
    const entries = get(store);
    expect(entries).toHaveLength(1);
    expect(entries[0].count).toBe(2);
  });

  it("refuses a malformed version from a hostile persisted blob", () => {
    const storage = memoryStorage({
      [DIAGNOSTIC_STORAGE_KEY]: JSON.stringify([
        { id: 4, at: 4, severity: "error", source: "s", message: "object", count: 1, version: { evil: true } },
        { id: 3, at: 3, severity: "error", source: "s", message: "blank", count: 1, version: "   " },
        { id: 2, at: 2, severity: "error", source: "s", message: "oversized", count: 1, version: "9".repeat(500) },
        { id: 1, at: 1, severity: "error", source: "s", message: "fine", count: 1, version: "0.0.2" },
      ]),
    });
    const entries = get(createDiagnostics({ storage }));
    const byMessage = Object.fromEntries(entries.map((e) => [e.message, e.version]));
    expect(byMessage.object).toBeUndefined();
    expect(byMessage.blank).toBeUndefined();
    expect(byMessage.oversized).toHaveLength(32);
    expect(byMessage.fine).toBe("0.0.2");
  });
});
