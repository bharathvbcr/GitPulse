import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mulberry32 } from "../__tests__/prng";
import { MAX_VISIBLE_SESSIONS } from "./attendance";
import {
  createGpuRenderer,
  createGpuSlots,
  GPU_HIDE_GRACE_MS,
  GPU_LOSS_BUDGET,
  GPU_LOSS_WINDOW_MS,
  GPU_RECOVER_DELAY_MS,
  type GpuAddon,
  type GpuRenderer,
  type GpuSlots,
  type GpuState,
} from "./gpuRenderer";

/**
 * A browser's WebGL context pool, small enough to reason about.
 *
 * Both engines cap live contexts per page at 16 (Chromium's
 * `CurrentMaxGLContexts`, WebKit's `maxActiveContexts`) and, when a new context
 * would pass the cap, force-lose the one used least recently: that is how a
 * long-lived on-screen session goes `lost`. They differ in when a context
 * stops counting. Chromium drops it on `loseContext()`; WebKit's
 * `forceLostContext` frees its GL objects but keeps it in the active set until
 * the wrapper is collected or recycled as the oldest (`WebGLRenderingContextBase.cpp`).
 * Nothing here collects: addon-webgl's dispose never frees a context, so a
 * context nobody explicitly lost waits for a GC that never comes in a test.
 */
type Engine = "chromium" | "webkit";

interface FakeCanvas {
  id: number;
  attached: boolean;
  /** Set while the session would hear this canvas's `webglcontextlost`. */
  onLostEvent: (() => void) | null;
  getContext(kind: string): FakeGl | null;
}
interface FakeGl {
  isContextLost(): boolean;
  getExtension(name: string): { loseContext(): void } | null;
}
interface FakeCtx {
  canvas: FakeCanvas;
  lost: boolean;
  lastUse: number;
  byUs: boolean;
}

function fakePage(engine: Engine, cap = 16) {
  const active: FakeCtx[] = [];
  let tick = 0;
  let nextId = 1;
  const stats = { created: 0, evictions: 0, explicitLoses: 0, evicted: [] as number[], peakLive: 0 };
  const live = () => active.filter((ctx) => !ctx.lost).length;

  const markLost = (ctx: FakeCtx, reason: "explicit" | "evicted") => {
    if (ctx.lost) return;
    ctx.lost = true;
    if (reason === "explicit") {
      stats.explicitLoses += 1;
      ctx.byUs = true;
    }
    if (engine === "chromium") active.splice(active.indexOf(ctx), 1);
    // The browser queues the event; our deliberately released canvases are
    // detached by then, so only an attached canvas reaches its session.
    if (ctx.canvas.attached) ctx.canvas.onLostEvent?.();
  };

  return {
    stats,
    live,
    activeSize: () => active.length,
    create(): FakeCanvas {
      while (active.length >= cap) {
        const victim = active.reduce((a, b) => (b.lastUse < a.lastUse ? b : a));
        if (!victim.lost) {
          stats.evictions += 1;
          stats.evicted.push(victim.canvas.id);
          markLost(victim, "evicted");
        }
        const index = active.indexOf(victim);
        if (index >= 0) active.splice(index, 1);
      }
      tick += 1;
      const id = nextId++;
      const ctx: FakeCtx = {
        canvas: undefined as unknown as FakeCanvas,
        lost: false,
        lastUse: tick,
        byUs: false,
      };
      const gl: FakeGl = {
        isContextLost: () => ctx.lost,
        getExtension: (name) => (name === "WEBGL_lose_context" ? { loseContext: () => markLost(ctx, "explicit") } : null),
      };
      ctx.canvas = { id, attached: true, onLostEvent: null, getContext: (kind) => (kind === "webgl2" ? gl : null) };
      active.push(ctx);
      stats.created += 1;
      stats.peakLive = Math.max(stats.peakLive, live());
      return ctx.canvas;
    },
    /** The session drew a frame: the engine's "last used" moves. */
    draw(canvas: FakeCanvas) {
      const ctx = active.find((item) => item.canvas === canvas);
      if (ctx && !ctx.lost) {
        tick += 1;
        ctx.lastUse = tick;
      }
    },
    /** The collector ran: WebKit's lost wrappers leave the active set. */
    gc() {
      for (let index = active.length - 1; index >= 0; index -= 1) {
        if (active[index].lost) active.splice(index, 1);
      }
    },
    /** A GPU reset or a sleep/wake: every live context is lost by the system. */
    resetAll() {
      for (const ctx of [...active]) markLost(ctx, "evicted");
    },
    forceLose(canvas: FakeCanvas) {
      const ctx = active.find((item) => item.canvas === canvas);
      if (ctx) markLost(ctx, "evicted");
    },
  };
}
type FakePage = ReturnType<typeof fakePage>;

interface FakeAddon extends GpuAddon {
  canvas: FakeCanvas;
  disposed: number;
  cleared: number;
}

function session(page: FakePage, slots: GpuSlots, options: { failAttach?: () => boolean } = {}) {
  const made: FakeAddon[] = [];
  const changes: GpuState[] = [];
  const refreshes = { count: 0 };
  const handle: { renderer: GpuRenderer } = { renderer: undefined as unknown as GpuRenderer };
  handle.renderer = createGpuRenderer({
    attach: (): GpuAddon => {
      if (options.failAttach?.()) throw new Error("WebGL2 not supported null");
      const canvas = page.create();
      const addon: FakeAddon = {
        canvas,
        disposed: 0,
        cleared: 0,
        clearTextureAtlas() {
          addon.cleared += 1;
        },
        dispose() {
          addon.disposed += 1;
          canvas.attached = false;
          canvas.onLostEvent = null;
        },
        onContextLoss: () => ({ dispose() {} }),
      };
      // The session's capture listener on the host: `lose(event.target)`.
      canvas.onLostEvent = () => handle.renderer.lose(canvas);
      made.push(addon);
      return addon;
    },
    slots,
    onChange: (state) => changes.push(state),
    refresh: () => {
      refreshes.count += 1;
    },
  });
  return {
    renderer: handle.renderer,
    refreshes,
    made,
    changes,
    /** The addon currently attached, if any. */
    current: () => made.filter((addon) => addon.canvas.attached).at(-1),
  };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("a hide releases the context for real", () => {
  it("frees the WebGL context itself when the grace expires, not only the addon", () => {
    const page = fakePage("chromium");
    const s = session(page, createGpuSlots());
    s.renderer.sync(true);
    expect(page.live()).toBe(1);
    s.renderer.sync(false);
    vi.advanceTimersByTime(GPU_HIDE_GRACE_MS + 1);
    expect(s.made[0].disposed).toBe(1);
    expect(page.stats.explicitLoses).toBe(1);
    expect(page.live()).toBe(0);
    expect(s.renderer.state()).toBe("dom");
  });

  it("200 hide/show rounds on 11 sessions never push a long-lived session past the page's cap", () => {
    const rounds = 200;
    for (const engine of ["chromium", "webkit"] as const) {
      const page = fakePage(engine);
      const slots = createGpuSlots();
      const sessions = Array.from({ length: 11 }, () => session(page, slots));
      // Session 0 stays on screen the whole time and draws every step.
      sessions[0].renderer.sync(true);
      const anchor = sessions[0].current()!.canvas;
      for (let round = 0; round < rounds; round += 1) {
        for (let index = 1; index < sessions.length; index += 1) {
          // At most MAX_VISIBLE_SESSIONS on screen: anchor plus seven others.
          const shown = index <= MAX_VISIBLE_SESSIONS - 1;
          sessions[index].renderer.sync(shown);
          page.draw(anchor);
        }
        // Everything shown now hides, past the grace.
        for (let index = 1; index < sessions.length; index += 1) sessions[index].renderer.sync(false);
        vi.advanceTimersByTime(GPU_HIDE_GRACE_MS + 1);
        page.draw(anchor);
      }
      // 1400 contexts created and each one freed on purpose but the anchor's.
      expect(page.stats.created, engine).toBe(1401);
      expect(page.stats.explicitLoses, engine).toBe(1400);
      expect(sessions[0].renderer.state(), engine).toBe("webgl");
      expect(page.stats.evictions, `${engine} evictions`).toBe(0);
      expect(page.stats.peakLive, `${engine} peak live contexts`).toBeLessThanOrEqual(MAX_VISIBLE_SESSIONS);
      // Every release freed its context: nothing waits for a collector.
      expect(page.live(), engine).toBe(1);
    }
  });
});

describe("hidden is not released at once", () => {
  it("reuses the warm addon when the session returns inside the grace", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots);
    s.renderer.sync(true);
    s.renderer.sync(false);
    expect(s.made[0].disposed).toBe(0);
    expect(slots.held()).toBe(1);
    vi.advanceTimersByTime(GPU_HIDE_GRACE_MS - 1);
    expect(s.renderer.sync(true)).toBe("webgl");
    expect(s.made).toHaveLength(1);
    // The flick restarted nothing: the original grace timer must not fire later.
    vi.advanceTimersByTime(GPU_HIDE_GRACE_MS * 3);
    expect(s.made[0].disposed).toBe(0);
    expect(slots.held()).toBe(1);
  });

  it("gives the slot back when the grace expires, and a later show attaches afresh", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots);
    s.renderer.sync(true);
    s.renderer.sync(false);
    vi.advanceTimersByTime(GPU_HIDE_GRACE_MS);
    expect(slots.held()).toBe(0);
    expect(s.renderer.state()).toBe("dom");
    expect(s.renderer.sync(true)).toBe("webgl");
    expect(s.made).toHaveLength(2);
  });

  it("does not extend the grace when a hidden session is told to hide again", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots);
    s.renderer.sync(true);
    s.renderer.sync(false);
    vi.advanceTimersByTime(GPU_HIDE_GRACE_MS - 10);
    s.renderer.sync(false);
    vi.advanceTimersByTime(10);
    expect(slots.held()).toBe(0);
  });

  it("hands a hidden session's slot to an on-screen session that needs it", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const sessions = Array.from({ length: MAX_VISIBLE_SESSIONS + 1 }, () => session(page, slots));
    for (const item of sessions.slice(0, MAX_VISIBLE_SESSIONS)) item.renderer.sync(true);
    sessions[0].renderer.sync(false);
    expect(slots.held()).toBe(MAX_VISIBLE_SESSIONS);
    // Not waiting out the grace: the slot is needed now.
    expect(sessions[MAX_VISIBLE_SESSIONS].renderer.sync(true)).toBe("webgl");
    expect(sessions[0].made[0].disposed).toBe(1);
    expect(sessions[0].renderer.state()).toBe("dom");
    expect(slots.held()).toBe(MAX_VISIBLE_SESSIONS);
    expect(page.live()).toBe(MAX_VISIBLE_SESSIONS);
  });

  it("never reclaims the slot of a session that is on screen", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const sessions = Array.from({ length: MAX_VISIBLE_SESSIONS + 1 }, () => session(page, slots));
    for (const item of sessions.slice(0, MAX_VISIBLE_SESSIONS)) item.renderer.sync(true);
    expect(sessions[MAX_VISIBLE_SESSIONS].renderer.sync(true)).toBe("dom");
    expect(sessions.slice(0, MAX_VISIBLE_SESSIONS).every((item) => item.renderer.state() === "webgl")).toBe(true);
  });

  it("releases during the grace when the session closes, leaving no timer", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots);
    s.renderer.sync(true);
    s.renderer.sync(false);
    s.renderer.dispose();
    expect(s.made[0].disposed).toBe(1);
    expect(page.stats.explicitLoses).toBe(1);
    expect(slots.held()).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("releases at once, grace or not, when the setting is turned off", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const hiddenSession = session(page, slots);
    hiddenSession.renderer.sync(true);
    hiddenSession.renderer.sync(false);
    hiddenSession.renderer.sync(false, false);
    expect(hiddenSession.made[0].disposed).toBe(1);
    expect(slots.held()).toBe(0);
    expect(vi.getTimerCount()).toBe(0);

    const shownSession = session(page, slots);
    shownSession.renderer.sync(true);
    expect(shownSession.renderer.sync(false, false)).toBe("dom");
    expect(shownSession.made[0].disposed).toBe(1);
    expect(slots.held()).toBe(0);
    expect(page.live()).toBe(0);
  });
});

describe("the hold is LRU under slot pressure, and the atlas is the caller's to clear", () => {
  it("reclaims the session that has been hidden longest first", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots(3);
    const sessions = Array.from({ length: 4 }, () => session(page, slots));
    for (const item of sessions.slice(0, 3)) item.renderer.sync(true);
    sessions[1].renderer.sync(false);
    vi.advanceTimersByTime(1000);
    sessions[0].renderer.sync(false);
    expect(sessions[3].renderer.sync(true)).toBe("webgl");
    expect(sessions[1].made[0].disposed).toBe(1);
    expect(sessions[0].made[0].disposed).toBe(0);
    expect(sessions[2].made[0].disposed).toBe(0);
  });

  it("keeps a hidden session's context well past a tab flick", () => {
    expect(GPU_HIDE_GRACE_MS).toBeGreaterThanOrEqual(60_000);
  });

  it("repaints the viewport after attaching and after resuming from the hold", () => {
    const page = fakePage("chromium");
    const s = session(page, createGpuSlots());
    s.renderer.sync(true);
    expect(s.refreshes.count).toBe(1);
    s.renderer.sync(false);
    expect(s.refreshes.count).toBe(1);
    s.renderer.sync(true);
    expect(s.refreshes.count).toBe(2);
    s.renderer.sync(true);
    expect(s.refreshes.count).toBe(2);
  });

  it("clearAtlas clears a held addon's texture atlas, held through the hold too, and is a no-op otherwise", () => {
    const page = fakePage("chromium");
    const s = session(page, createGpuSlots());
    s.renderer.clearAtlas();
    s.renderer.sync(true);
    s.renderer.clearAtlas();
    expect(s.made[0].cleared).toBe(1);
    s.renderer.sync(false);
    s.renderer.clearAtlas();
    expect(s.made[0].cleared).toBe(2);
    s.renderer.sync(false, false);
    s.renderer.clearAtlas();
    expect(s.made[0].cleared).toBe(2);
  });
});

describe("a refused session is told when a slot frees", () => {
  it("attaches the waiting on-screen session the moment another gives its slot back", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const sessions = Array.from({ length: MAX_VISIBLE_SESSIONS + 1 }, () => session(page, slots));
    for (const item of sessions.slice(0, MAX_VISIBLE_SESSIONS)) item.renderer.sync(true);
    const waiting = sessions[MAX_VISIBLE_SESSIONS];
    expect(waiting.renderer.sync(true)).toBe("dom");
    // A session leaves the screen and its grace runs out; nobody calls sync on `waiting`.
    sessions[0].renderer.dispose();
    expect(waiting.renderer.state()).toBe("webgl");
    expect(slots.held()).toBe(MAX_VISIBLE_SESSIONS);
  });

  it("takes the slot of a session that merely hides, without waiting out its grace", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const sessions = Array.from({ length: MAX_VISIBLE_SESSIONS + 1 }, () => session(page, slots));
    for (const item of sessions.slice(0, MAX_VISIBLE_SESSIONS)) item.renderer.sync(true);
    const waiting = sessions[MAX_VISIBLE_SESSIONS];
    waiting.renderer.sync(true);
    sessions[0].renderer.sync(false);
    expect(waiting.renderer.state()).toBe("webgl");
    expect(sessions[0].renderer.state()).toBe("dom");
  });

  it("stops waiting when the session hides or closes", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots(1);
    const holder = session(page, slots);
    const hidingWaiter = session(page, slots);
    const closedWaiter = session(page, slots);
    holder.renderer.sync(true);
    hidingWaiter.renderer.sync(true);
    closedWaiter.renderer.sync(true);
    hidingWaiter.renderer.sync(false);
    closedWaiter.renderer.dispose();
    holder.renderer.dispose();
    expect(hidingWaiter.made).toHaveLength(0);
    expect(closedWaiter.made).toHaveLength(0);
    expect(slots.held()).toBe(0);
  });

  it("two sessions racing for the last slot: one wins, the other waits and takes the next", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots(1);
    const a = session(page, slots);
    const b = session(page, slots);
    expect(a.renderer.sync(true)).toBe("webgl");
    expect(b.renderer.sync(true)).toBe("dom");
    expect(slots.held()).toBe(1);
    a.renderer.dispose();
    expect(b.renderer.state()).toBe("webgl");
    expect(slots.held()).toBe(1);
    expect(page.live()).toBe(1);
    b.renderer.dispose();
    expect(slots.held()).toBe(0);
    expect(page.live()).toBe(0);
  });
});

describe("a loss is bounded, not final", () => {
  it("falls back to the DOM synchronously, then re-attaches once the delay has passed", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots);
    s.renderer.sync(true);
    page.forceLose(s.made[0].canvas);
    expect(s.renderer.state()).toBe("reset");
    expect(s.made[0].disposed).toBe(1);
    expect(slots.held()).toBe(0);
    // Not at once: a faulty driver must not turn this into a tight loop.
    expect(s.made).toHaveLength(1);
    vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
    expect(s.renderer.state()).toBe("webgl");
    expect(s.made).toHaveLength(2);
    expect(slots.held()).toBe(1);
  });

  it("re-attaches on the next show when the delay has already passed", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots);
    s.renderer.sync(true);
    // Lost on screen (a counted loss), then hidden before the delay passes.
    page.forceLose(s.made[0].canvas);
    expect(s.renderer.state()).toBe("reset");
    s.renderer.sync(false);
    expect(vi.getTimerCount()).toBe(0);
    vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
    expect(s.made).toHaveLength(1);
    expect(s.renderer.sync(true)).toBe("webgl");
    expect(s.made).toHaveLength(2);
  });

  it("does not re-attach a session that is hidden when the delay passes", () => {
    const page = fakePage("chromium");
    const s = session(page, createGpuSlots());
    s.renderer.sync(true);
    page.forceLose(s.made[0].canvas);
    s.renderer.sync(false);
    vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS * 10);
    expect(s.made).toHaveLength(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("a loss during the grace period frees the slot and cancels the grace", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots);
    s.renderer.sync(true);
    s.renderer.sync(false);
    page.forceLose(s.made[0].canvas);
    // A held context is only warmth: losing it is the hold ending early, not
    // a fault on a session anyone is looking at.
    expect(s.renderer.state()).toBe("dom");
    expect(slots.held()).toBe(0);
    expect(s.made[0].disposed).toBe(1);
    vi.advanceTimersByTime(GPU_HIDE_GRACE_MS * 2);
    expect(s.made[0].disposed).toBe(1);
    expect(slots.held()).toBe(0);
    expect(s.renderer.sync(true)).toBe("webgl");
  });

  it("evictions of a held hidden context never count against the loss budget", () => {
    // Both engines evict the context drawn least recently, which is a hidden
    // session's held one; that must not strand the session on the DOM.
    for (const engine of ["chromium", "webkit"] as const) {
      const page = fakePage(engine);
      const slots = createGpuSlots();
      const s = session(page, slots);
      for (let round = 0; round < GPU_LOSS_BUDGET * 4; round += 1) {
        expect(s.renderer.sync(true), `${engine} show ${round}`).toBe("webgl");
        s.renderer.sync(false);
        page.forceLose(s.current()!.canvas);
        expect(s.renderer.state(), `${engine} held loss ${round}`).toBe("dom");
        expect(slots.held()).toBe(0);
      }
      expect(s.renderer.sync(true)).toBe("webgl");
      // An on-screen loss still counts: the budget is untouched by the holds.
      for (let loss = 1; loss <= GPU_LOSS_BUDGET; loss += 1) {
        page.forceLose(s.current()!.canvas);
        expect(s.renderer.state(), `${engine} on-screen loss ${loss}`).toBe("reset");
        vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
      }
      page.forceLose(s.current()!.canvas);
      expect(s.renderer.state()).toBe("lost");
      s.renderer.dispose();
    }
  });

  it("a GPU reset that takes every context back leaves every on-screen session drawing and recovering", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const sessions = Array.from({ length: 4 }, () => session(page, slots));
    for (const item of sessions) item.renderer.sync(true);
    page.resetAll();
    expect(sessions.map((item) => item.renderer.state())).toEqual(["reset", "reset", "reset", "reset"]);
    vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
    expect(sessions.map((item) => item.renderer.state())).toEqual(["webgl", "webgl", "webgl", "webgl"]);
    expect(slots.held()).toBe(4);
    expect(page.live()).toBe(4);
  });

  it(`goes final after more than ${GPU_LOSS_BUDGET} losses in the window`, () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots);
    s.renderer.sync(true);
    for (let loss = 1; loss <= GPU_LOSS_BUDGET; loss += 1) {
      page.forceLose(s.current()!.canvas);
      expect(s.renderer.state(), `loss ${loss}`).toBe("reset");
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
      expect(s.renderer.state(), `recovered after loss ${loss}`).toBe("webgl");
    }
    page.forceLose(s.current()!.canvas);
    expect(s.renderer.state()).toBe("lost");
    const made = s.made.length;
    for (let round = 0; round < 5; round += 1) {
      expect(s.renderer.sync(false)).toBe("lost");
      expect(s.renderer.sync(true)).toBe("lost");
      vi.advanceTimersByTime(GPU_LOSS_WINDOW_MS);
    }
    expect(s.made).toHaveLength(made);
    expect(slots.held()).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("forgets losses that are older than the window", () => {
    const page = fakePage("chromium");
    const s = session(page, createGpuSlots());
    s.renderer.sync(true);
    for (let loss = 0; loss < GPU_LOSS_BUDGET * 4; loss += 1) {
      page.forceLose(s.current()!.canvas);
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
      expect(s.renderer.state(), `loss ${loss}`).toBe("webgl");
      vi.advanceTimersByTime(GPU_LOSS_WINDOW_MS);
    }
  });

  it("a create that fails is final, as before, and does not retry", () => {
    const page = fakePage("chromium");
    const slots = createGpuSlots();
    const s = session(page, slots, { failAttach: () => true });
    expect(s.renderer.sync(true)).toBe("lost");
    expect(slots.held()).toBe(0);
    vi.advanceTimersByTime(GPU_LOSS_WINDOW_MS);
    expect(s.renderer.sync(false)).toBe("lost");
    expect(s.renderer.sync(true)).toBe("lost");
    expect(s.made).toHaveLength(0);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("a re-attach that fails is final too", () => {
    const page = fakePage("chromium");
    let fail = false;
    const s = session(page, createGpuSlots(), { failAttach: () => fail });
    s.renderer.sync(true);
    page.forceLose(s.made[0].canvas);
    fail = true;
    vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
    expect(s.renderer.state()).toBe("lost");
    fail = false;
    s.renderer.sync(false);
    expect(s.renderer.sync(true)).toBe("lost");
  });
});

describe("a deliberate release is not a fault", () => {
  it("ignores the loss event of a canvas that was released on purpose", () => {
    const page = fakePage("webkit");
    const s = session(page, createGpuSlots());
    s.renderer.sync(true);
    const old = s.made[0].canvas;
    s.renderer.sync(false);
    vi.advanceTimersByTime(GPU_HIDE_GRACE_MS);
    // The queued `webglcontextlost` from our own loseContext() arrives late.
    s.renderer.lose(old);
    expect(s.renderer.state()).toBe("dom");
    expect(s.renderer.sync(true)).toBe("webgl");
  });

  it("ignores a stale canvas's loss while a newer context is attached", () => {
    const page = fakePage("webkit");
    const s = session(page, createGpuSlots());
    s.renderer.sync(true);
    const old = s.made[0].canvas;
    s.renderer.sync(false);
    vi.advanceTimersByTime(GPU_HIDE_GRACE_MS);
    s.renderer.sync(true);
    s.renderer.lose(old);
    expect(s.renderer.state()).toBe("webgl");
    expect(s.current()).toBe(s.made[1]);
    expect(s.made[1].disposed).toBe(0);
  });

  it("does not count a deliberate release against the loss budget", () => {
    const page = fakePage("chromium");
    const s = session(page, createGpuSlots());
    for (let round = 0; round < GPU_LOSS_BUDGET * 5; round += 1) {
      s.renderer.sync(true);
      s.renderer.sync(false);
      vi.advanceTimersByTime(GPU_HIDE_GRACE_MS);
    }
    expect(s.renderer.sync(true)).toBe("webgl");
  });
});

describe("seeded fuzz of show / hide / loss / setting / close", () => {
  /**
   * Invariants, every step:
   *  - held slots never pass MAX_VISIBLE_SESSIONS, and equal the sessions holding an addon;
   *  - the page's live contexts equal the held slots: nothing leaks, nothing is evicted;
   *  - a session reports webgl exactly when it holds an addon;
   *  - a closed session holds nothing;
   *  - a hidden session holds nothing once its grace has run out;
   *  - an on-screen session with a free slot, no loss pending and no failure is on webgl.
   */
  for (const seed of [1, 2, 3, 7, 42, 20261028]) {
    for (const engine of ["chromium", "webkit"] as const) {
      it(`holds the invariants (seed ${seed}, ${engine})`, () => {
        const rng = mulberry32(seed);
        const page = fakePage(engine);
        const slots = createGpuSlots();
        const count = 11;
        const sessions = Array.from({ length: count }, () => session(page, slots));
        const wants = new Array<boolean>(count).fill(false);
        const enabled = new Array<boolean>(count).fill(true);
        const closed = new Array<boolean>(count).fill(false);
        const hiddenAt = new Array<number>(count).fill(-1);
        let clock = 0;
        const advance = (ms: number) => {
          vi.advanceTimersByTime(ms);
          clock += ms;
        };

        const check = (step: number, op: string) => {
          const label = `step ${step} (${op})`;
          const holders = sessions.filter((item) => item.current() !== undefined);
          expect(slots.held(), label).toBeLessThanOrEqual(MAX_VISIBLE_SESSIONS);
          expect(slots.held(), `${label}: held slots equal holders`).toBe(holders.length);
          expect(page.live(), `${label}: live contexts equal held slots`).toBe(slots.held());
          expect(page.stats.evictions, `${label}: no eviction`).toBe(0);
          sessions.forEach((item, index) => {
            const holds = item.current() !== undefined;
            expect(item.renderer.state() === "webgl", `${label}: session ${index} state vs holding`).toBe(holds);
            if (closed[index]) expect(holds, `${label}: closed session ${index} holds a context`).toBe(false);
            if (holds && !wants[index] && !closed[index]) {
              expect(clock - hiddenAt[index], `${label}: session ${index} kept its context past the grace`).toBeLessThan(
                GPU_HIDE_GRACE_MS,
              );
            }
            if (holds && !enabled[index]) throw new Error(`${label}: session ${index} holds a context with the setting off`);
          });
        };

        for (let step = 0; step < 1500; step += 1) {
          const index = Math.floor(rng() * count);
          const roll = rng();
          let op: string;
          if (closed[index]) {
            op = "advance";
            advance(Math.floor(rng() * 800));
          } else if (roll < 0.3) {
            op = `show ${index}`;
            wants[index] = true;
            sessions[index].renderer.sync(enabled[index], enabled[index]);
          } else if (roll < 0.55) {
            op = `hide ${index}`;
            if (wants[index]) hiddenAt[index] = clock;
            wants[index] = false;
            sessions[index].renderer.sync(false, enabled[index]);
          } else if (roll < 0.65) {
            op = `setting ${index}`;
            enabled[index] = !enabled[index];
            sessions[index].renderer.sync(wants[index] && enabled[index], enabled[index]);
            if (!enabled[index]) hiddenAt[index] = Number.NEGATIVE_INFINITY;
          } else if (roll < 0.72) {
            op = `lose ${index}`;
            const addon = sessions[index].current();
            if (addon) page.forceLose(addon.canvas);
          } else if (roll < 0.74) {
            op = `close ${index}`;
            sessions[index].renderer.dispose();
            closed[index] = true;
            wants[index] = false;
          } else if (roll < 0.76) {
            op = "reset all";
            page.resetAll();
          } else {
            op = "advance";
            advance(Math.floor(rng() * (GPU_HIDE_GRACE_MS / 2)));
          }
          // An on-screen draw keeps the engine's "last used" moving for the shown.
          sessions.forEach((item, at) => {
            const addon = item.current();
            if (addon && wants[at]) page.draw(addon.canvas);
          });
          // The collector keeps pace here; the 200-round test above runs without one.
          page.gc();
          check(step, op);
        }

        // Everything closes: nothing is left holding a context, a slot or a timer.
        sessions.forEach((item) => item.renderer.dispose());
        expect(slots.held()).toBe(0);
        expect(page.live()).toBe(0);
        expect(vi.getTimerCount()).toBe(0);
      });
    }
  }
});
