// ft-0ff882a6: synchronous persistence per console.warn, and per unchanged patch.
// The first two cases are the second audit's reproductions, kept verbatim in
// what they assert; against the unmodified code they reported 100 writes and
// 50 writes + 50 notifications.
import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import {
  createDiagnostics,
  DIAGNOSTIC_PERSIST_DELAY_MS,
  DIAGNOSTIC_STORAGE_KEY,
  installGlobalDiagnostics,
} from "../diagnostics/diagnostics";

function countingStorage() {
  const data = new Map<string, string>();
  const setItem = vi.fn((k: string, v: string) => void data.set(k, v));
  return {
    setItem,
    getItem: (k: string) => data.get(k) ?? null,
    removeItem: (k: string) => void data.delete(k),
  };
}

function pageWindow(storage: ReturnType<typeof countingStorage>) {
  const listeners = new Map<string, Set<() => void>>();
  return {
    localStorage: storage,
    addEventListener: (type: string, listener: () => void) => {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type)!.add(listener);
    },
    removeEventListener: (type: string, listener: () => void) => {
      listeners.get(type)?.delete(listener);
    },
    emit: (type: string) => {
      for (const listener of listeners.get(type) ?? []) listener();
    },
  };
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe("ft-0ff882a6 diagnostics persistence", () => {
  it("does not stringify+setItem synchronously on every console.warn", () => {
    const storage = countingStorage();
    const diags = createDiagnostics({ storage, development: false });
    const con = { error: () => {}, warn: () => {} } as unknown as Console;
    const uninstall = installGlobalDiagnostics(diags, { target: null, console: con });
    const before = storage.setItem.mock.calls.length;
    for (let i = 0; i < 100; i += 1) con.warn(`distinct warning number ${i} ${"x".repeat(i)}`);
    const writes = storage.setItem.mock.calls.length - before;
    uninstall();
    expect(writes).toBeLessThanOrEqual(1);
  });

  it("saves the whole burst once, one window later", () => {
    vi.useFakeTimers();
    const storage = countingStorage();
    const diags = createDiagnostics({ storage, development: false });
    for (let i = 0; i < 100; i += 1) diags.warn("console", `distinct warning number ${i}`);
    expect(storage.setItem).not.toHaveBeenCalled();
    vi.advanceTimersByTime(DIAGNOSTIC_PERSIST_DELAY_MS);
    expect(storage.setItem).toHaveBeenCalledTimes(1);
    expect(JSON.parse(storage.getItem(DIAGNOSTIC_STORAGE_KEY) ?? "[]")).toHaveLength(100);
  });

  it("flushes the pending save when the page is hidden", async () => {
    const storage = countingStorage();
    const win = pageWindow(storage);
    vi.stubGlobal("window", win);
    // The singleton reads the global `localStorage`, not `window.localStorage`.
    vi.stubGlobal("localStorage", storage);
    vi.resetModules();
    const { diagnostics } = await import("../diagnostics/diagnostics");
    diagnostics.warn("console", "recorded just before quitting");
    expect(storage.setItem).not.toHaveBeenCalled();
    win.emit("pagehide");
    expect(storage.setItem).toHaveBeenCalledTimes(1);
    expect(storage.getItem(DIAGNOSTIC_STORAGE_KEY)).toContain("recorded just before quitting");
  });
});

describe("ft-0ff882a6 interfaceStore.patch no-op check", () => {
  it("does not write localStorage or notify subscribers when the value is unchanged", async () => {
    const storage = countingStorage();
    vi.stubGlobal("window", { localStorage: storage });
    vi.resetModules();
    const { interfaceStore } = await import("../stores/interfaceStore");
    interfaceStore.setTerminalDockHeight(300);
    let notified = 0;
    const unsub = interfaceStore.subscribe(() => (notified += 1));
    notified = 0;
    const before = storage.setItem.mock.calls.length;
    for (let i = 0; i < 50; i += 1) interfaceStore.setTerminalDockHeight(300);
    const writes = storage.setItem.mock.calls.length - before;
    unsub();
    expect(writes).toBe(0);
    expect(notified).toBe(0);
  });

  it("saves a drag once instead of on every pointer move", async () => {
    vi.useFakeTimers();
    const storage = countingStorage();
    vi.stubGlobal("window", { localStorage: storage });
    vi.resetModules();
    const { interfaceStore, INTERFACE_PERSIST_DELAY_MS } = await import("../stores/interfaceStore");
    let notified = 0;
    const unsub = interfaceStore.subscribe(() => (notified += 1));
    notified = 0;
    for (let px = 200; px < 250; px += 1) interfaceStore.setTerminalDockHeight(px);
    // Every move is shown at once; none is written yet.
    expect(notified).toBe(50);
    expect(get(interfaceStore).terminalDockHeight).toBe(249);
    expect(storage.setItem).not.toHaveBeenCalled();
    vi.advanceTimersByTime(INTERFACE_PERSIST_DELAY_MS);
    expect(storage.setItem).toHaveBeenCalledTimes(1);
    expect(JSON.parse(storage.getItem("gitpulse_interface_prefs") ?? "{}").terminalDockHeight).toBe(249);
    unsub();
  });

  it("flushes a pending preference when the page is hidden", async () => {
    const storage = countingStorage();
    const win = pageWindow(storage);
    vi.stubGlobal("window", win);
    vi.resetModules();
    const { interfaceStore } = await import("../stores/interfaceStore");
    interfaceStore.setTerminalDockHeight(321);
    expect(storage.setItem).not.toHaveBeenCalled();
    win.emit("pagehide");
    expect(storage.setItem).toHaveBeenCalledTimes(1);
    expect(JSON.parse(storage.getItem("gitpulse_interface_prefs") ?? "{}").terminalDockHeight).toBe(321);
  });
});
