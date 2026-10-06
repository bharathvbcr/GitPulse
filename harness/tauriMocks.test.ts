import { emit, listen } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mockIPCWithEvents } from "./tauriMocks";

const STALE = "Couldn't find callback id";
let warn: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  // A fresh window per case: the mock keeps its listener table in a closure
  // and its callbacks on window.__TAURI_INTERNALS__.
  vi.stubGlobal("window", { crypto: globalThis.crypto });
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const staleWarnings = () => warn.mock.calls.filter((call: unknown[]) => String(call[0]).includes(STALE)).length;

describe("upstream mock", () => {
  // Pins why mockIPCWithEvents exists. When this fails, @tauri-apps/api has
  // fixed its unlisten lookup and the wrapper can be deleted.
  it("still dispatches to a listener after unlisten", async () => {
    mockIPC(() => null, { shouldMockEvents: true });
    const unlisten = await listen("terminal-exit", () => {});
    unlisten();
    await emit("terminal-exit", { id: "s1" });
    expect(staleWarnings()).toBe(1);
  });
});

describe("mockIPCWithEvents", () => {
  it("stops dispatching to a listener once it is unlistened", async () => {
    mockIPCWithEvents(() => null);
    const received: unknown[] = [];
    const unlisten = await listen<{ id: string }>("terminal-exit", event => { received.push(event.payload); });
    await emit("terminal-exit", { id: "before" });
    expect(received).toEqual([{ id: "before" }]);

    unlisten();
    await emit("terminal-exit", { id: "after" });
    expect(received).toEqual([{ id: "before" }]);
    expect(staleWarnings()).toBe(0);
  });

  it("removes only the listener that was released", async () => {
    mockIPCWithEvents(() => null);
    const first: unknown[] = [];
    const second: unknown[] = [];
    const releaseFirst = await listen("terminal-output", event => { first.push(event.payload); });
    await listen("terminal-output", event => { second.push(event.payload); });
    releaseFirst();
    await emit("terminal-output", "chunk");
    expect(first).toEqual([]);
    expect(second).toEqual(["chunk"]);
    expect(staleWarnings()).toBe(0);
  });

  it("passes every other command to the handler unchanged", async () => {
    const calls: Array<{ cmd: string; args: unknown }> = [];
    mockIPCWithEvents((cmd, args) => { calls.push({ cmd, args }); return "ok"; });
    const { invoke } = await import("@tauri-apps/api/core");
    await expect(invoke("cmd_status", { repoPath: "/fixture", eventId: 7 })).resolves.toBe("ok");
    expect(calls).toEqual([{ cmd: "cmd_status", args: { repoPath: "/fixture", eventId: 7 } }]);
  });
});
