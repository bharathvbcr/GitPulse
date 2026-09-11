import { describe, expect, it, vi } from "vitest";
import { createResizeObservation, observeResize } from "./observeResize";

type FakeEntry = { target: Element };

function fakeObserverClass(deliveries: Array<(entries: FakeEntry[]) => void>) {
  return class FakeResizeObserver {
    callback: (entries: FakeEntry[]) => void;
    observed = new Set<Element>();
    constructor(callback: (entries: FakeEntry[]) => void) {
      this.callback = callback;
      deliveries.push((entries) => this.callback(entries));
    }
    observe(target: Element) {
      this.observed.add(target);
    }
    unobserve(target: Element) {
      this.observed.delete(target);
    }
    disconnect() {
      this.observed.clear();
    }
  };
}

describe("observeResize", () => {
  it("coalesces multiple observer deliveries into one frame callback", () => {
    const queued: FrameRequestCallback[] = [];
    const raf = vi.fn((cb: FrameRequestCallback) => {
      queued.push(cb);
      return queued.length;
    });
    const caf = vi.fn();
    const deliveries: Array<(entries: FakeEntry[]) => void> = [];
    const onResize = vi.fn();
    const a = { id: "a" } as unknown as Element;
    const b = { id: "b" } as unknown as Element;

    const stop = observeResize(a, onResize, {
      raf,
      caf,
      ResizeObserver: fakeObserverClass(deliveries) as unknown as typeof ResizeObserver,
    });

    expect(deliveries).toHaveLength(1);
    deliveries[0]([{ target: a }]);
    deliveries[0]([{ target: a }, { target: b }]);
    expect(raf).toHaveBeenCalledTimes(1);
    expect(onResize).not.toHaveBeenCalled();

    queued[0](16);
    expect(onResize).toHaveBeenCalledTimes(1);
    const batch = onResize.mock.calls[0][0] as FakeEntry[];
    expect(batch.map((entry) => entry.target)).toEqual([a, b]);
    stop();
  });

  it("disconnect cancels a pending frame before it can flush", () => {
    const queued: FrameRequestCallback[] = [];
    const raf = (cb: FrameRequestCallback) => {
      queued.push(cb);
      return 7;
    };
    const caf = vi.fn();
    const deliveries: Array<(entries: FakeEntry[]) => void> = [];
    const onResize = vi.fn();
    const el = { id: "el" } as unknown as Element;

    const stop = observeResize(el, onResize, {
      raf,
      caf,
      ResizeObserver: fakeObserverClass(deliveries) as unknown as typeof ResizeObserver,
    });
    deliveries[0]([{ target: el }]);
    stop();

    expect(caf).toHaveBeenCalledWith(7);
    queued[0](32);
    expect(onResize).not.toHaveBeenCalled();
  });

  it("observes every element in a list and supports dynamic observes", () => {
    const queued: FrameRequestCallback[] = [];
    const raf = (cb: FrameRequestCallback) => {
      queued.push(cb);
      return queued.length;
    };
    const deliveries: Array<(entries: FakeEntry[]) => void> = [];
    const onResize = vi.fn();
    const first = { id: "1" } as unknown as Element;
    const third = { id: "3" } as unknown as Element;

    const observation = createResizeObservation(onResize, {
      raf,
      caf: vi.fn(),
      ResizeObserver: fakeObserverClass(deliveries) as unknown as typeof ResizeObserver,
    });
    observation.observe(first);
    expect(deliveries).toHaveLength(1);
    observation.observe(third);
    deliveries[0]([{ target: third }]);
    queued[0](1);
    expect(onResize.mock.calls[0][0][0].target).toBe(third);
    observation.disconnect();
  });
});
