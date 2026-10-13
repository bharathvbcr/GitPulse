import { describe, expect, it } from "vitest";
import { createWorkbenchChanges, ECHO_WINDOW_MS, type WorkbenchChange } from "./changes";
import { sameAutomaticUpdate } from "./client";

/** A host that emits on demand and counts listens and unlistens. */
function host(options: { fail?: string; slow?: boolean } = {}) {
  let handler: ((event: { payload: unknown }) => void) | null = null;
  let release: (() => void) | null = null;
  const log = { listens: 0, unlistens: 0 };
  let clock = 1_000;
  const hub = createWorkbenchChanges({
    now: () => clock,
    listen: (_event, next) => {
      log.listens++;
      if (options.fail) return Promise.reject(new Error(options.fail));
      handler = next;
      const stop = () => { log.unlistens++; if (handler === next) handler = null; };
      return options.slow ? new Promise<() => void>((resolve) => { release = () => resolve(stop); }) : Promise.resolve(stop);
    },
  });
  return {
    hub, log,
    emit: (payload: unknown) => handler?.({ payload }),
    advance: (ms: number) => { clock += ms; },
    release: () => release?.(),
    listening: () => handler !== null,
  };
}
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
function surface() {
  const changes: WorkbenchChange[] = [], notices: string[] = [];
  return { changes, notices, subscriber: { changed: (c: WorkbenchChange) => changes.push(c), unavailable: (m: string) => notices.push(m) } };
}

describe("workbench change hub", () => {
  it("listens once for any number of surfaces, and each write reaches each surface once", async () => {
    const h = host();
    const surfaces = Array.from({ length: 4 }, surface);
    const stops = surfaces.map((s) => h.hub.subscribe(s.subscriber));
    await settle();
    expect(h.log.listens).toBe(1);
    h.emit({ sequence: 7 });
    for (const s of surfaces) expect(s.changes).toEqual([{ sequence: 7, source: "local" }]);
    stops.forEach((stop) => stop());
    await settle();
    expect(h.log.unlistens).toBe(1);
    expect(h.listening()).toBe(false);
  });

  it("drops a repeated sequence but not a new one, and tolerates sequences out of order", async () => {
    const h = host(); const s = surface();
    h.hub.subscribe(s.subscriber); await settle();
    h.emit({ sequence: 5 }); h.emit({ sequence: 5 }); h.emit({ sequence: 4 }); h.emit({ sequence: 6 });
    expect(s.changes.map((c) => c.sequence)).toEqual([5, 4, 6]);
  });

  it("drops the external echo of an in-process write, once", async () => {
    const h = host(); const s = surface();
    h.hub.subscribe(s.subscriber); await settle();
    h.emit({ sequence: 9 });
    h.advance(1_000);
    h.emit({ sequence: null, source: "external" });
    expect(s.changes).toHaveLength(1);
    // The watcher coalesces a burst into one echo, so the next external event is a real one.
    h.advance(100);
    h.emit({ sequence: null, source: "external" });
    expect(s.changes).toHaveLength(2);
    expect(s.changes[1]).toEqual({ sequence: null, source: "external" });
  });

  it("delivers an external change with no local write before it, or long after one", async () => {
    const h = host(); const s = surface();
    h.hub.subscribe(s.subscriber); await settle();
    h.emit({ sequence: null, source: "external" });
    h.emit({ sequence: 1 });
    h.advance(ECHO_WINDOW_MS + 1);
    h.emit({ sequence: null, source: "external" });
    expect(s.changes.map((c) => c.source)).toEqual(["external", "local", "external"]);
  });

  it("treats a payload it cannot read as a change rather than losing it", async () => {
    const h = host(); const s = surface();
    h.hub.subscribe(s.subscriber); await settle();
    for (const payload of [null, undefined, "x", 3, {}, { sequence: "4" }, { sequence: -1 }, { sequence: 1.5 }]) h.emit(payload);
    expect(s.changes).toHaveLength(8);
    expect(s.changes.every((c) => c.source === "external" && c.sequence === null)).toBe(true);
  });

  it("tells every surface when the listener cannot be made, including one that subscribes later", async () => {
    const h = host({ fail: "no event bus" });
    const a = surface(), b = surface();
    h.hub.subscribe(a.subscriber); h.hub.subscribe(b.subscriber);
    await settle();
    expect(a.notices).toEqual(["no event bus"]);
    expect(b.notices).toEqual(["no event bus"]);
    const late = surface();
    h.hub.subscribe(late.subscriber);
    expect(late.notices).toEqual(["no event bus"]);
    expect(h.log.listens).toBe(1);
  });

  it("tries again once every surface has left", async () => {
    const h = host({ fail: "boom" });
    const a = surface();
    const stop = h.hub.subscribe(a.subscriber); await settle();
    stop();
    h.hub.subscribe(surface().subscriber); await settle();
    expect(h.log.listens).toBe(2);
  });

  it("releases a listener that answers after the last surface left", async () => {
    const h = host({ slow: true });
    const stop = h.hub.subscribe(surface().subscriber);
    stop();
    h.release(); await settle();
    expect(h.log.unlistens).toBe(1);
  });

  it("unsubscribing twice, or from a surface that left mid-event, is harmless", async () => {
    const h = host();
    const first = surface();
    let stopFirst = () => {};
    const second = surface();
    stopFirst = h.hub.subscribe({ changed: (c) => { first.changes.push(c); stopFirst(); }, unavailable: () => {} });
    h.hub.subscribe(second.subscriber); await settle();
    h.emit({ sequence: 1 }); h.emit({ sequence: 2 });
    expect(first.changes).toHaveLength(1);
    expect(second.changes).toHaveLength(2);
    stopFirst();
  });

  it("bounds the sequences it remembers", async () => {
    const h = host(); const s = surface();
    h.hub.subscribe(s.subscriber); await settle();
    for (let i = 0; i < 200; i++) h.emit({ sequence: i });
    h.emit({ sequence: 0 });
    expect(s.changes).toHaveLength(201);
    h.emit({ sequence: 199 });
    expect(s.changes).toHaveLength(201);
  });
});

describe("sameAutomaticUpdate", () => {
  const status = (over: Record<string, unknown> = {}) => ({ state: "waiting" as const, reason: "", task_id: "t1", proposal_id: "", next_check_at: 100, ...over });
  it("ignores a countdown or a reworded reason, and sees a real change", () => {
    const base = { status: status(), error: "" };
    expect(sameAutomaticUpdate(base, { status: status({ next_check_at: 200, reason: "later" }), error: "" })).toBe(true);
    expect(sameAutomaticUpdate(base, { status: status({ state: "generating" }), error: "" })).toBe(false);
    expect(sameAutomaticUpdate(base, { status: status({ proposal_id: "p1" }), error: "" })).toBe(false);
    expect(sameAutomaticUpdate(base, { status: null, error: "down" })).toBe(false);
    expect(sameAutomaticUpdate({ status: null, error: "" }, { status: null, error: "" })).toBe(true);
  });
});
