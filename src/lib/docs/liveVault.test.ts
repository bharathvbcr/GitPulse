import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { onDocsRepoChanged, resetDocsVaultRefresh, setDocsVaultRefreshScope } from "./liveVault";

const refresh = vi.hoisted(() => vi.fn<(path: string) => Promise<void>>());
const warn = vi.hoisted(() => vi.fn());
vi.mock("./client", () => ({ docsRefresh: refresh }));
vi.mock("../diagnostics/diagnostics", () => ({ diagnostics: { warn } }));

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}

describe("background document refresh", () => {
  it("production scope defers inactive docs and resumes after visibility changes", async () => {
    try {
      setDocsVaultRefreshScope({ activeKey: "/a", retainedKeys: ["/a", "/b"], visible: false });
      onDocsRepoChanged("/a");
      onDocsRepoChanged("/b");
      await vi.advanceTimersByTimeAsync(30_000);
      expect(refresh).not.toHaveBeenCalled();
      setDocsVaultRefreshScope({ activeKey: "/a", retainedKeys: ["/a", "/b"], visible: true });
      await vi.advanceTimersByTimeAsync(200);
      expect(refresh).toHaveBeenCalledExactlyOnceWith("/a", { background: true });
      setDocsVaultRefreshScope({ activeKey: "/b", retainedKeys: ["/b"], visible: true });
      onDocsRepoChanged("/a");
      await vi.advanceTimersByTimeAsync(1_000);
      expect(refresh.mock.calls.map(([path]) => path)).toEqual(["/a", "/b"]);
    } finally {
      resetDocsVaultRefresh();
      setDocsVaultRefreshScope(null);
    }
  });
  beforeEach(() => {
    vi.useFakeTimers();
    resetDocsVaultRefresh();
    refresh.mockReset().mockResolvedValue(undefined);
    warn.mockReset();
  });
  afterEach(() => {
    resetDocsVaultRefresh();
    vi.useRealTimers();
  });

  it("coalesces a burst into one refresh", async () => {
    for (let i = 0; i < 100; i++) onDocsRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(199);
    expect(refresh).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(refresh).toHaveBeenCalledExactlyOnceWith("/repo", { background: true });
  });

  it("retains changes arriving during a rebuild as one follow-up", async () => {
    const first = deferred();
    refresh.mockReturnValueOnce(first.promise);
    onDocsRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(200);
    for (let i = 0; i < 100; i++) onDocsRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(500);
    expect(refresh).toHaveBeenCalledTimes(1);
    first.resolve();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("admits only one background vault build across repositories", async () => {
    const first = deferred();
    refresh.mockReturnValueOnce(first.promise);
    for (let i = 0; i < 32; i++) onDocsRepoChanged(`/repo/${i}`);
    await vi.advanceTimersByTimeAsync(200);
    expect(refresh).toHaveBeenCalledTimes(1);
    first.resolve();
    await vi.advanceTimersByTimeAsync(40_000);
    expect(refresh).toHaveBeenCalledTimes(32);
    expect(new Set(refresh.mock.calls.map(([path]) => path)).size).toBe(32);
  });

  it("paces repeated completed rebuilds instead of continuously scanning", async () => {
    for (let i = 0; i < 20; i++) {
      onDocsRepoChanged("/repo");
      await vi.advanceTimersByTimeAsync(250);
    }
    expect(refresh.mock.calls.length).toBeLessThanOrEqual(5);
    expect(refresh.mock.calls.length).toBeGreaterThan(0);
  });

  it("reports a failed refresh and still drains other repositories", async () => {
    refresh.mockRejectedValueOnce(new Error("vault unavailable"));
    onDocsRepoChanged("/a");
    onDocsRepoChanged("/b");
    await vi.advanceTimersByTimeAsync(2_000);
    expect(warn).toHaveBeenCalledWith("docs-refresh", expect.stringContaining("vault unavailable"));
    expect(refresh).toHaveBeenLastCalledWith("/b", { background: true });
  });

  it("reset cancels queued work without freeing a running native operation", async () => {
    const first = deferred();
    refresh.mockReturnValueOnce(first.promise);
    onDocsRepoChanged("/old");
    await vi.advanceTimersByTimeAsync(200);
    resetDocsVaultRefresh();
    onDocsRepoChanged("/new");
    await vi.advanceTimersByTimeAsync(200);
    expect(refresh).toHaveBeenCalledTimes(1);
    first.resolve();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(refresh).toHaveBeenLastCalledWith("/new", { background: true });
  });

  it("continuous events cannot starve a rebuild indefinitely", async () => {
    for (let i = 0; i < 30; i++) {
      onDocsRepoChanged("/busy");
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(refresh.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(refresh.mock.calls.length).toBeLessThanOrEqual(3);
  });

  it("bounds the queue, reports overflow once, and retains already queued repos", async () => {
    for (let i = 0; i < 1_000; i++) onDocsRepoChanged(`/repo/${i}`);
    expect(vi.getTimerCount()).toBe(1);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(warn.mock.calls[0][1]).toContain("not refreshed");
    await vi.advanceTimersByTimeAsync(65_000);
    expect(refresh).toHaveBeenCalledTimes(64);
    expect(vi.getTimerCount()).toBe(0);
    onDocsRepoChanged("/repo/999");
    await vi.advanceTimersByTimeAsync(1_200);
    expect(refresh).toHaveBeenLastCalledWith("/repo/999", { background: true });
  });

  it("empty paths and reset-before-start do no work", async () => {
    onDocsRepoChanged("");
    expect(vi.getTimerCount()).toBe(0);
    onDocsRepoChanged("/old");
    resetDocsVaultRefresh();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(refresh).not.toHaveBeenCalled();
  });
});
