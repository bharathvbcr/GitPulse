import { describe, expect, it, vi } from "vitest";
import { loadOnce } from "./loadOnce";

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("loadOnce", () => {
  it("does not call a rejecting loader again however often the effect re-runs", async () => {
    const load = vi.fn(async () => {
      throw new Error("worker down");
    });
    // The panel's loader catches and records the error itself; model the
    // `finally { busy = false }` re-run by requesting after every settle.
    const loader = loadOnce(() => load().catch(() => undefined));
    loader.request();
    await settle();
    loader.request();
    await settle();
    loader.request();
    await settle();
    expect(load).toHaveBeenCalledTimes(1);
    expect(loader.attempted).toBe(true);
  });

  it("retry() calls the loader once more, and request() still stays quiet after it", async () => {
    const load = vi.fn(async () => {
      throw new Error("still down");
    });
    const loader = loadOnce(() => load().catch(() => undefined));
    loader.request();
    await settle();
    loader.retry();
    await settle();
    loader.request();
    await settle();
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("runs a resolving loader once", async () => {
    const load = vi.fn(async () => undefined);
    const loader = loadOnce(load);
    expect(loader.attempted).toBe(false);
    loader.request();
    loader.request();
    await settle();
    expect(load).toHaveBeenCalledTimes(1);
  });
});
