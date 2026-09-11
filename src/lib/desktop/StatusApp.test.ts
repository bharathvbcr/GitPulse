import { afterEach, describe, expect, it, vi } from "vitest";
import { mountStatusResize, statusPanelHeight } from "./statusResize";

type FakeEntry = { target: Element };

function fakeObserverClass(deliveries: Array<(entries: FakeEntry[]) => void>) {
  return class FakeResizeObserver {
    callback: (entries: FakeEntry[]) => void;
    constructor(callback: (entries: FakeEntry[]) => void) {
      this.callback = callback;
      deliveries.push((entries) => this.callback(entries));
    }
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

describe("StatusApp resize", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("clamps panel height to the native window bounds", () => {
    expect(statusPanelHeight(12.2)).toBe(100);
    expect(statusPanelHeight(240.4)).toBe(241);
    expect(statusPanelHeight(900)).toBe(640);
  });

  it("never invokes resize synchronously inside ResizeObserver delivery", async () => {
    const queued: FrameRequestCallback[] = [];
    const raf = vi.fn((cb: FrameRequestCallback) => {
      queued.push(cb);
      return queued.length;
    });
    const deliveries: Array<(entries: FakeEntry[]) => void> = [];
    const send = vi.fn().mockResolvedValue(undefined);
    let height = 180;
    const host = {
      getBoundingClientRect: () => ({ height }),
    } as unknown as Element & { getBoundingClientRect(): { height: number } };

    const dispose = mountStatusResize(host, {
      send,
      failed: vi.fn(),
      isDisposed: () => false,
      raf,
      caf: vi.fn(),
      ResizeObserver: fakeObserverClass(deliveries) as unknown as typeof ResizeObserver,
    });

    deliveries[0]([{ target: host }]);
    expect(send).not.toHaveBeenCalled();
    queued[0](16);
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    expect(send).toHaveBeenCalledWith(180);
    dispose();
  });

  it("coalesces many deliveries in one frame into a single invoke", async () => {
    const queued: FrameRequestCallback[] = [];
    const raf = vi.fn((cb: FrameRequestCallback) => {
      queued.push(cb);
      return queued.length;
    });
    const deliveries: Array<(entries: FakeEntry[]) => void> = [];
    const send = vi.fn().mockResolvedValue(undefined);
    let height = 120;
    const host = {
      getBoundingClientRect: () => ({ height }),
    } as unknown as Element & { getBoundingClientRect(): { height: number } };

    const dispose = mountStatusResize(host, {
      send,
      failed: vi.fn(),
      isDisposed: () => false,
      raf,
      caf: vi.fn(),
      ResizeObserver: fakeObserverClass(deliveries) as unknown as typeof ResizeObserver,
    });

    height = 150;
    deliveries[0]([{ target: host }]);
    height = 200;
    deliveries[0]([{ target: host }]);
    height = 260;
    deliveries[0]([{ target: host }]);
    expect(raf).toHaveBeenCalledTimes(1);
    expect(send).not.toHaveBeenCalled();
    queued[0](16);
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    expect(send).toHaveBeenCalledWith(260);
    dispose();
  });

  it("dedupes identical heights through createMenuSync", async () => {
    const queued: FrameRequestCallback[] = [];
    const raf = (cb: FrameRequestCallback) => {
      queued.push(cb);
      return queued.length;
    };
    const deliveries: Array<(entries: FakeEntry[]) => void> = [];
    const send = vi.fn().mockResolvedValue(undefined);
    const host = {
      getBoundingClientRect: () => ({ height: 300 }),
    } as unknown as Element & { getBoundingClientRect(): { height: number } };

    const dispose = mountStatusResize(host, {
      send,
      failed: vi.fn(),
      isDisposed: () => false,
      raf,
      caf: vi.fn(),
      ResizeObserver: fakeObserverClass(deliveries) as unknown as typeof ResizeObserver,
    });

    deliveries[0]([{ target: host }]);
    queued.shift()!(16);
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));

    deliveries[0]([{ target: host }]);
    queued.shift()!(32);
    await Promise.resolve();
    expect(send).toHaveBeenCalledTimes(1);
    dispose();
  });

  it("cancels a pending frame on unmount so invoke never fires after dispose", async () => {
    const queued: FrameRequestCallback[] = [];
    const raf = (cb: FrameRequestCallback) => {
      queued.push(cb);
      return 9;
    };
    const caf = vi.fn();
    const deliveries: Array<(entries: FakeEntry[]) => void> = [];
    const send = vi.fn().mockResolvedValue(undefined);
    let disposed = false;
    const host = {
      getBoundingClientRect: () => ({ height: 220 }),
    } as unknown as Element & { getBoundingClientRect(): { height: number } };

    const dispose = mountStatusResize(host, {
      send,
      failed: vi.fn(),
      isDisposed: () => disposed,
      raf,
      caf,
      ResizeObserver: fakeObserverClass(deliveries) as unknown as typeof ResizeObserver,
    });

    deliveries[0]([{ target: host }]);
    disposed = true;
    dispose();
    expect(caf).toHaveBeenCalledWith(9);
    queued[0](48);
    await Promise.resolve();
    expect(send).not.toHaveBeenCalled();
  });
});
