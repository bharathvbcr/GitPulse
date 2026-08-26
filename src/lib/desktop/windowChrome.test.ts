import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { repoWindowTitle } from "./windowChrome";

const tauriWindow = vi.hoisted(() => ({
  setTitle: vi.fn(),
  setBadgeCount: vi.fn(),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => tauriWindow,
}));

function setTauriWindow(present: boolean) {
  if (present) {
    (globalThis as Record<string, unknown>).window = { __TAURI_INTERNALS__: {} };
  } else {
    Reflect.deleteProperty(globalThis, "window");
  }
}

describe("syncWindowChrome", () => {
  beforeEach(() => {
    vi.resetModules();
    setTauriWindow(true);
    tauriWindow.setTitle.mockReset().mockResolvedValue(undefined);
    tauriWindow.setBadgeCount.mockReset().mockResolvedValue(undefined);
  });

  afterEach(() => {
    setTauriWindow(false);
  });

  async function freshSync(): Promise<(title: string, badgeCount: number) => Promise<void>> {
    const { syncWindowChrome } = await import("./windowChrome");
    return syncWindowChrome;
  }

  it("does nothing outside Tauri", async () => {
    setTauriWindow(false);
    const sync = await freshSync();
    await sync("GitPulse", 1);
    expect(tauriWindow.setTitle).not.toHaveBeenCalled();
    expect(tauriWindow.setBadgeCount).not.toHaveBeenCalled();
  });

  it("skips IPC when title and badge are unchanged", async () => {
    const sync = await freshSync();
    await sync("repo — main", 3);
    await sync("repo — main", 3);
    expect(tauriWindow.setTitle).toHaveBeenCalledTimes(1);
    expect(tauriWindow.setTitle).toHaveBeenCalledWith("repo — main");
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledTimes(1);
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledWith(3);
  });

  it("normalizes non-positive badges and re-syncs when they change", async () => {
    const sync = await freshSync();
    await sync("GitPulse", 0);
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledWith(undefined);
    await sync("GitPulse", 2);
    expect(tauriWindow.setBadgeCount).toHaveBeenLastCalledWith(2);
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledTimes(2);
  });

  it("lets the latest call win when an older call resolves last", async () => {
    const sync = await freshSync();
    let releaseStale!: () => void;
    tauriWindow.setTitle.mockImplementationOnce(
      () => new Promise<void>((resolve) => (releaseStale = resolve)),
    );

    const stale = sync("old — main", 7);
    await vi.waitUntil(() => tauriWindow.setTitle.mock.calls.length === 1);

    const fresh = sync("new — main", 0);
    await fresh;

    releaseStale();
    await stale;

    // Stale call invoked first but bailed before touching the badge.
    expect(tauriWindow.setTitle).toHaveBeenNthCalledWith(1, "old — main");
    expect(tauriWindow.setTitle).toHaveBeenNthCalledWith(2, "new — main");
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledTimes(1);
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledWith(undefined);

    // Cache reflects the winner: repeating the latest sync skips IPC.
    await sync("new — main", 0);
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledTimes(1);
    expect(tauriWindow.setTitle).toHaveBeenCalledTimes(2);
  });

  it("retries after a failed sync instead of caching the failure", async () => {
    const sync = await freshSync();
    tauriWindow.setTitle.mockRejectedValueOnce(new Error("no live window"));
    await sync("repo", 1);
    expect(tauriWindow.setTitle).toHaveBeenCalledTimes(1);
    await sync("repo", 1);
    expect(tauriWindow.setTitle).toHaveBeenCalledTimes(2);
    // First attempt died at setTitle; the retry completes both writes.
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledTimes(1);
    expect(tauriWindow.setBadgeCount).toHaveBeenCalledWith(1);
  });
});

describe("repoWindowTitle", () => {
  it("formats repo and branch for Mission Control / the Window menu", () => {
    expect(repoWindowTitle(null, null)).toBe("GitPulse");
    expect(repoWindowTitle("/Users/acme/gitpulse", "main")).toBe("gitpulse — main");
    expect(repoWindowTitle("/Users/acme/gitpulse", null)).toBe("gitpulse");
  });
});
