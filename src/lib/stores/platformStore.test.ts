import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The store caches per session, so each case needs a fresh module. `vi.resetModules`
 * plus a dynamic import gives one; reusing the singleton would let an earlier
 * case's loaded answer satisfy a later one and hide a regression.
 */
async function freshStore() {
  vi.resetModules();
  return import("./platformStore");
}

const ORIGINAL_WINDOW = globalThis.window;

function setTauriHost(present: boolean): void {
  if (present) {
    Object.defineProperty(globalThis, "window", {
      value: { __TAURI_INTERNALS__: {} },
      configurable: true,
      writable: true,
    });
  } else {
    Object.defineProperty(globalThis, "window", {
      value: undefined,
      configurable: true,
      writable: true,
    });
  }
}

afterEach(() => {
  Object.defineProperty(globalThis, "window", {
    value: ORIGINAL_WINDOW,
    configurable: true,
    writable: true,
  });
  vi.doUnmock("@tauri-apps/api/core");
  vi.resetModules();
});

describe("host platform store outside Tauri", () => {
  beforeEach(() => setTauriHost(false));

  it("resolves to the conservative fallback without invoking anything", async () => {
    const { loadHostPlatform, hostPlatformNow } = await freshStore();
    const resolved = await loadHostPlatform();
    expect(resolved.dock_hiding).toBe(false);
    expect(hostPlatformNow()).toEqual(resolved);
  });
});

describe("host platform store inside Tauri", () => {
  beforeEach(() => setTauriHost(true));

  it("does not invoke anything merely because something subscribed", async () => {
    const invoke = vi.fn().mockResolvedValue({ os: "windows", dock_hiding: false });
    vi.doMock("@tauri-apps/api/core", () => ({ invoke }));
    const { hostPlatform } = await freshStore();

    // Seventeen components read this store. If subscribing loaded the profile,
    // every one of them would make a backend call on mount -- which is both a
    // second owner for a load `main.ts` already performs, and an undeclared
    // dependency that breaks the browser harnesses' "every IPC call has an
    // explicit fixture" contract. Startup owns the load; this is a pure reader.
    const stop = hostPlatform.subscribe(() => {});
    // The load reaches `invoke` only after `await import(...)` settles, which a
    // single microtask turn does not cover. Flushing macrotasks is what makes
    // this case fail when the subscribe-time load comes back; asserting after
    // `await Promise.resolve()` passes either way, which is no assertion at all.
    for (let turn = 0; turn < 10; turn += 1) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    stop();

    expect(invoke).not.toHaveBeenCalled();
  });

  it("publishes the backend's answer to subscribers", async () => {
    const invoke = vi.fn().mockResolvedValue({ os: "windows", dock_hiding: false });
    vi.doMock("@tauri-apps/api/core", () => ({ invoke }));
    const { loadHostPlatform, hostPlatform } = await freshStore();

    const seen: string[] = [];
    const stop = hostPlatform.subscribe((value) => seen.push(value.os));
    await loadHostPlatform();
    stop();

    expect(invoke).toHaveBeenCalledWith("cmd_host_platform");
    expect(seen.at(-1)).toBe("windows");
  });

  it("reports a macOS host's dock capability", async () => {
    vi.doMock("@tauri-apps/api/core", () => ({
      invoke: vi.fn().mockResolvedValue({ os: "macos", dock_hiding: true }),
    }));
    const { loadHostPlatform } = await freshStore();
    expect(await loadHostPlatform()).toEqual({ os: "macos", dock_hiding: true });
  });

  /**
   * A failed probe must not read like a passed one. Denying the capability is
   * the only safe direction: the worst case is a macOS reader briefly missing an
   * optional toggle, versus every Windows reader being offered a dead one.
   */
  it("denies capabilities when the command fails", async () => {
    vi.doMock("@tauri-apps/api/core", () => ({
      invoke: vi.fn().mockRejectedValue(new Error("command not found")),
    }));
    const { loadHostPlatform } = await freshStore();
    expect((await loadHostPlatform()).dock_hiding).toBe(false);
  });

  it("denies capabilities when the module itself cannot load", async () => {
    vi.doMock("@tauri-apps/api/core", () => {
      throw new Error("module missing");
    });
    const { loadHostPlatform } = await freshStore();
    expect((await loadHostPlatform()).dock_hiding).toBe(false);
  });

  it("asks the backend once and shares one in-flight request", async () => {
    const invoke = vi.fn().mockResolvedValue({ os: "linux", dock_hiding: false });
    vi.doMock("@tauri-apps/api/core", () => ({ invoke }));
    const { loadHostPlatform } = await freshStore();

    // Concurrent callers, then a later one: the platform cannot change while the
    // app is open, so re-invoking per component would be pure overhead.
    const [a, b] = await Promise.all([loadHostPlatform(), loadHostPlatform()]);
    const c = await loadHostPlatform();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(a).toEqual(b);
    expect(c).toEqual(a);
  });

  it("keeps the denied fallback cached after a failure rather than retrying forever", async () => {
    const invoke = vi.fn().mockRejectedValue(new Error("nope"));
    vi.doMock("@tauri-apps/api/core", () => ({ invoke }));
    const { loadHostPlatform } = await freshStore();
    await loadHostPlatform();
    await loadHostPlatform();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("restores pre-load state through the test seam", async () => {
    vi.doMock("@tauri-apps/api/core", () => ({
      invoke: vi.fn().mockResolvedValue({ os: "macos", dock_hiding: true }),
    }));
    const { loadHostPlatform, resetHostPlatformForTests, hostPlatformNow } = await freshStore();
    await loadHostPlatform();
    expect(hostPlatformNow().dock_hiding).toBe(true);

    resetHostPlatformForTests({ os: "windows", dock_hiding: false });
    expect(hostPlatformNow()).toEqual({ os: "windows", dock_hiding: false });
  });
});
