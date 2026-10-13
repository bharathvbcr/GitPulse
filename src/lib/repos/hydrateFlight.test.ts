import { describe, expect, it } from "vitest";
import { createHydrateFlight, HYDRATE_INFLIGHT_MAX_MS } from "./hydrateFlight";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

const flush = async () => {
  for (let i = 0; i < 8; i += 1) await Promise.resolve();
};

describe("createHydrateFlight", () => {
  it("starts the first request at once and a request for another key beside it", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    const started: string[] = [];
    const a = deferred();
    const b = deferred();
    void flight.run("a", 0, () => (started.push("a"), a.promise));
    void flight.run("b", 0, () => (started.push("b"), b.promise));
    await flush();
    expect(started).toEqual(["a", "b"]);
    a.resolve();
    b.resolve();
    await flush();
    expect(flight.busy()).toEqual([]);
  });

  it("owes one trailing run however many requests arrive while one is running", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    let running = 0;
    let peak = 0;
    let runs = 0;
    const gates: ReturnType<typeof deferred>[] = [];
    const start = (label: string) => async () => {
      runs += 1;
      running += 1;
      peak = Math.max(peak, running);
      const gate = deferred();
      gates.push(gate);
      await gate.promise;
      running -= 1;
      void label;
    };

    const first = flight.run("tab", 0, start("first"));
    const joiners = Array.from({ length: 40 }, (_, i) => flight.run("tab", 0, start(`event-${i}`)));
    await flush();
    expect(runs).toBe(1);

    gates[0].resolve();
    await flush();
    // The one owed run started; the other 39 joined it rather than queuing.
    expect(runs).toBe(2);
    gates[1].resolve();
    await Promise.all([first, ...joiners]);
    await flush();

    expect(runs).toBe(2);
    expect(peak).toBe(1);
    expect(flight.busy()).toEqual([]);
  });

  it("runs the trailing request with the newest start function", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    const gate = deferred();
    const order: string[] = [];
    void flight.run("tab", 0, async () => {
      order.push("first");
      await gate.promise;
    });
    void flight.run("tab", 0, async () => void order.push("middle"));
    const last = flight.run("tab", 0, async () => void order.push("newest"));
    gate.resolve();
    await last;
    expect(order).toEqual(["first", "newest"]);
  });

  it("settles a joined request only after the run that covers it", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    const gate = deferred();
    const second = deferred();
    let covered = false;
    void flight.run("tab", 0, () => gate.promise);
    const joined = flight.run("tab", 0, async () => {
      await second.promise;
      covered = true;
    });
    let settled = false;
    void joined.then(() => (settled = true));
    gate.resolve();
    await flush();
    expect(settled).toBe(false);
    second.resolve();
    await joined;
    expect(covered).toBe(true);
  });

  it("never rejects, and a failed run still lets the owed one start", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    const gate = deferred();
    let trailing = 0;
    const first = flight.run("tab", 0, async () => {
      await gate.promise;
      throw new Error("git declined");
    });
    const second = flight.run("tab", 0, async () => void (trailing += 1));
    gate.resolve();
    await expect(first).resolves.toBeUndefined();
    await second;
    expect(trailing).toBe(1);
  });

  it("stops waiting for a run that has outlived the bound", async () => {
    let now = 0;
    const flight = createHydrateFlight({ now: () => now });
    const hung = deferred();
    let started = 0;
    void flight.run("tab", 0, () => (started += 1, hung.promise));
    now = HYDRATE_INFLIGHT_MAX_MS - 1;
    void flight.run("tab", 0, async () => void (started += 10));
    await flush();
    expect(started).toBe(1);
    now = HYDRATE_INFLIGHT_MAX_MS;
    await flight.run("tab", 0, async () => void (started += 100));
    expect(started).toBe(101);
    // The hung run settling late owes nothing further.
    hung.resolve();
    await flush();
    expect(started).toBe(101);
  });

  it("starts a newer epoch at once instead of queueing it behind the running one", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    const old = deferred();
    const started: string[] = [];
    void flight.run("tab", 1, async () => {
      started.push("epoch 1");
      await old.promise;
    });
    // Owed after epoch 1, then superseded: it must not run.
    void flight.run("tab", 1, async () => void started.push("owed epoch 1"));
    await flight.run("tab", 2, async () => void started.push("epoch 2"));
    expect(started).toEqual(["epoch 1", "epoch 2"]);
    old.resolve();
    await flush();
    expect(started).toEqual(["epoch 1", "epoch 2"]);
  });

  it("answers a request from an older epoch with the run already going", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    const gate = deferred();
    let runs = 0;
    void flight.run("tab", 5, async () => {
      runs += 1;
      await gate.promise;
    });
    const stale = flight.run("tab", 4, async () => void (runs += 10));
    gate.resolve();
    await stale;
    expect(runs).toBe(1);
  });

  it("forget drops what was owed without running it", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    const gate = deferred();
    let trailing = 0;
    void flight.run("tab", 0, () => gate.promise);
    const owed = flight.run("tab", 0, async () => void (trailing += 1));
    flight.forget("tab");
    gate.resolve();
    await owed;
    expect(trailing).toBe(0);
    expect(flight.current("tab")).toBeNull();
  });

  it("current names the run in flight without owing another", async () => {
    const flight = createHydrateFlight({ now: () => 0 });
    const gate = deferred();
    expect(flight.current("tab")).toBeNull();
    const run = flight.run("tab", 0, () => gate.promise);
    expect(flight.current("tab")).not.toBeNull();
    gate.resolve();
    await run;
    expect(flight.current("tab")).toBeNull();
  });
});
