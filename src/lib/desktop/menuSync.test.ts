import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createRepoStore } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { buildMenuState } from "./menuState";
import { createMenuSync } from "./menuSync";

const state = () => buildMenuState(get(createRepoStore({ storage: null })), get(interfaceStore), "system", {}, false);
afterEach(() => vi.useRealTimers());
describe("serialized native menu updates", () => {
  it("coalesces scalar presentation values without dropping zero", async () => {
    let release: () => void = () => {};
    const send = vi.fn<(value: number) => Promise<void>>()
      .mockImplementationOnce(() => new Promise<void>((resolve) => { release = resolve; }))
      .mockResolvedValue(undefined);
    const sync = createMenuSync(send, vi.fn());
    sync.update(400);
    sync.update(560);
    sync.update(0);
    release();
    await vi.waitFor(() => expect(send.mock.calls).toEqual([[400], [0]]));
    sync.dispose();
  });
  it("coalesces a burst to its latest state and does not apply unchanged snapshots", async () => {
    let release: () => void = () => {};
    const send = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => { release = resolve; })).mockResolvedValue(undefined);
    const sync = createMenuSync(send, vi.fn());
    const first = state();
    sync.update(first);
    sync.update({ ...first, trayDetail: "old" });
    sync.update({ ...first, trayDetail: "latest" });
    expect(send).toHaveBeenCalledTimes(1);
    release();
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(2));
    expect(send.mock.calls[1][0].trayDetail).toBe("latest");
    sync.update({ ...first, trayDetail: "latest" });
    expect(send).toHaveBeenCalledTimes(2);
    sync.dispose();
  });
  it("reports and retries failures with a bound, then allows a later update to recover", async () => {
    vi.useFakeTimers();
    const send = vi.fn().mockRejectedValue(new Error("bridge unavailable"));
    const failed = vi.fn();
    const sync = createMenuSync(send, failed);
    sync.update(state());
    await vi.runAllTimersAsync();
    expect(send).toHaveBeenCalledTimes(3);
    expect(failed).toHaveBeenCalledTimes(3);
    send.mockResolvedValue(undefined);
    sync.update(state());
    await vi.runAllTimersAsync();
    expect(send).toHaveBeenCalledTimes(4);
    sync.dispose();
  });
  it("cancels queued work and retries on disposal", async () => {
    vi.useFakeTimers();
    const send = vi.fn().mockRejectedValue(new Error("offline"));
    const sync = createMenuSync(send, vi.fn());
    sync.update(state());
    await Promise.resolve();
    sync.dispose();
    await vi.runAllTimersAsync();
    expect(send).toHaveBeenCalledTimes(1);
  });
  it("delivers a newer snapshot queued while the last retry fails", async () => {
    vi.useFakeTimers();
    let rejectLast: (error: Error) => void = () => {};
    const send = vi.fn().mockRejectedValueOnce(new Error("offline"))
      .mockRejectedValueOnce(new Error("offline"))
      .mockImplementationOnce(() => new Promise<void>((_, reject) => { rejectLast = reject; }))
      .mockResolvedValue(undefined);
    const sync = createMenuSync(send, vi.fn());
    sync.update(state());
    await vi.advanceTimersByTimeAsync(1500);
    expect(send).toHaveBeenCalledTimes(3);
    const latest = { ...state(), trayDetail: "Latest status" };
    sync.update(latest);
    rejectLast(new Error("last attempt failed"));
    await vi.runAllTimersAsync();
    expect(send).toHaveBeenCalledTimes(4);
    expect(send).toHaveBeenLastCalledWith(latest);
    sync.dispose();
  });
});
