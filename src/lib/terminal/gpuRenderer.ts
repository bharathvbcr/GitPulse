/**
 * When a terminal draws through WebGL instead of the DOM.
 *
 * The DOM renderer rebuilds rows of spans on the WebContent main thread for
 * every redraw, and an agent CLI redraws most of its screen continuously. The
 * WebGL renderer moves that to the GPU, but a WebGL context is a scarce,
 * losable resource, so the pool of them is managed here.
 *
 * ## Why a context is freed explicitly
 *
 * `@xterm/addon-webgl`'s `dispose()` removes the canvas and the GL objects but
 * never calls `WEBGL_lose_context.loseContext()` (xterm.js #6068, open; the fix
 * in #6069 is unmerged), so a disposed context lives until the garbage
 * collector finds the canvas. Browsers cap live contexts per page at 16
 * (Chromium `CurrentMaxGLContexts`; WebKit `maxActiveContexts` in
 * `WebGLRenderingContextBase.cpp`) and, past the cap, force-lose the one used
 * least recently, which is a long-lived on-screen terminal that is idle
 * between bursts of output. After about 16 attach/dispose cycles that session
 * went `lost` for good: a blank pane, then the DOM renderer forever.
 * `release` therefore disposes the addon and then loses its context itself.
 *
 * Chromium stops counting a context when it is lost. WebKit's `loseContext()`
 * frees its GL objects and marks it lost but keeps it in the active set until
 * the wrapper is collected or recycled, so there a released context still
 * counts toward 16 for a while; as a victim it is chosen before a context that
 * has drawn since, which is why the explicit loss is still the right fix.
 * (Residual, inferred rather than measured: an on-screen session that has
 * drawn nothing for longer than a burst of released contexts could be chosen
 * first; the loss policy below is what absorbs that.)
 *
 * ## Invariants this module keeps
 *
 *  - **At most `MAX_VISIBLE_SESSIONS` contexts per window, and a terminal on
 *    screen is never refused one before that.** Slots are counted exactly: a
 *    slot is held from `attach` until the context has been freed, a session in
 *    its hold counts, and a caller that claims more sessions are on screen than
 *    can be is refused here, where the mistake is, and keeps drawing through
 *    the DOM. The resource profile's cap budgets only what is held beyond the
 *    terminals on screen (`max(cap, on screen)` contexts in all), so a small
 *    machine holds fewer hidden contexts, never a slower on-screen pane.
 *  - **A loss never blanks the pane.** The DOM renderer is back synchronously,
 *    on the canvas's own `webglcontextlost`, without waiting out the addon's
 *    three-second restore window.
 *  - **Hidden sessions hold a context only briefly and only while nobody needs
 *    it.** A hidden session keeps its warm context (and the glyph atlas rasterised
 *    into it, so showing it again is not a hitch, and the canvas element is the
 *    same one) for `GPU_HIDE_GRACE_MS`, as VS Code keeps its addon through a
 *    hide. The hold is a least-recently-hidden pool bounded by the same slot
 *    cap: when an on-screen session needs a slot and none is free, the session
 *    hidden longest gives its context up at once. The hold ends on the setting
 *    being turned off and on `dispose`, immediately.
 *  - **No loss loop, now with an explicit bound.** A loss is recoverable: the
 *    session draws through the DOM (`reset`) and, if it is still on screen,
 *    attaches again after `GPU_RECOVER_DELAY_MS`, so a GPU reset or a wake from
 *    sleep does not strand every pane on the DOM for the rest of its life. More
 *    than `GPU_LOSS_BUDGET` losses within `GPU_LOSS_WINDOW_MS`, or a context
 *    that cannot be created at all, is final (`lost`): the session never asks
 *    again.
 *  - **A deliberate release is not a fault.** The `webglcontextlost` that our
 *    own `loseContext()` queues arrives after the canvas is detached, and a
 *    loss reported for a canvas the renderer does not hold (or while it holds
 *    none) is ignored.
 *  - **A refused session is not forgotten.** It waits, and is attached as soon
 *    as a slot frees or a hidden session's hold can be taken over.
 *
 * Nothing here imports xterm: `attach` is the caller's, so the decisions are
 * testable without a WebGL context.
 */
import { COMPACT_BUDGETS } from "../resourceBudgets";
import { MAX_VISIBLE_SESSIONS } from "./attendance";

/**
 * How a session is drawing: through the DOM (`dom`), through WebGL (`webgl`),
 * through the DOM after a lost context that it will try to replace (`reset`),
 * or through the DOM for good (`lost`).
 */
export type GpuState = "dom" | "webgl" | "reset" | "lost";

/**
 * How long a hidden session keeps its context before giving it back, on a
 * machine with room (the `roomy` resource profile). `gpuPolicy.hideGraceMs`
 * follows the profile (`resourceBudgets.ts`) and starts at the compact value.
 */
export const GPU_HIDE_GRACE_MS = 5 * 60_000;
/** After a loss, how long before the session attaches again. */
export const GPU_RECOVER_DELAY_MS = 1_500;
/** Losses a session may have within the window before it stops asking. */
export const GPU_LOSS_BUDGET = 3;
/** The span a session's losses are counted over. */
export const GPU_LOSS_WINDOW_MS = 10 * 60_000;

/** The numbers above, as one object the browser harness can shorten. */
export interface GpuPolicy {
  hideGraceMs: number;
  recoverDelayMs: number;
  lossBudget: number;
  lossWindowMs: number;
}

/**
 * Read at each decision; the harness narrows it and the resource profile sets
 * the grace (`applyResourceBudgets`). Until a profile arrives the grace is the
 * compact one, so a small machine never starts out holding hidden contexts for
 * the roomy five minutes.
 */
export const gpuPolicy: GpuPolicy = {
  hideGraceMs: COMPACT_BUDGETS.gpu_hide_grace_ms,
  recoverDelayMs: GPU_RECOVER_DELAY_MS,
  lossBudget: GPU_LOSS_BUDGET,
  lossWindowMs: GPU_LOSS_WINDOW_MS,
};

/** The part of a WebGL2 context the release needs. */
export interface GpuContext {
  isContextLost(): boolean;
  getExtension(name: "WEBGL_lose_context"): { loseContext(): void } | null;
}

/** The part of a canvas the release needs: the context it was given. */
export interface GpuCanvas {
  getContext(kind: "webgl2"): GpuContext | null;
}

/** The part of `WebglAddon` the lifecycle uses. */
export interface GpuAddon {
  dispose(): void;
  onContextLoss(listener: () => void): { dispose(): void };
  /** The canvas the addon draws to; lets a release free its context. */
  canvas?: GpuCanvas | null;
  /** Rebuilds the glyph atlas (a display-metrics change). */
  clearTextureAtlas?(): void;
}

/**
 * Loses a canvas's WebGL context on purpose, so the browser stops counting it.
 * Returns whether it did: false for a canvas with no live context, no
 * `WEBGL_lose_context` extension, or a context that throws.
 */
export function loseWebglContext(canvas: GpuCanvas | null | undefined): boolean {
  if (!canvas) return false;
  try {
    const gl = canvas.getContext("webgl2");
    if (!gl || gl.isContextLost()) return false;
    const extension = gl.getExtension("WEBGL_lose_context");
    if (!extension) return false;
    extension.loseContext();
    return true;
  } catch {
    // A context the GPU process already tore down: it is gone, which is the goal.
    return false;
  }
}

/** The shared count of live WebGL contexts. */
export interface GpuSlots {
  /**
   * Claims a slot. When every slot is held, takes over the one a hidden
   * session has held longest; false only when every slot is in use by an
   * on-screen session.
   */
  take(): boolean;
  /** Returns a slot taken earlier, then tells a waiting session. */
  give(): void;
  /** Slots held now, including those a hidden session is holding. */
  held(): number;
  /**
   * The budget for contexts held beyond what is on screen. The pool holds
   * at most `max(cap, terminals on screen)` contexts, never more than its
   * window maximum; a terminal on screen always gets one up to that maximum.
   */
  cap(): number;
  /**
   * Changes the cap (floored, at least one; a value that is not a finite
   * number is ignored). A smaller cap gives up hidden holders beyond it,
   * longest-hidden first, at once; an on-screen or attaching session is never
   * taken from.
   */
  setCap(next: number): void;
  /**
   * Offers a held slot to whoever needs it: `release` must free the context
   * and `give()` its slot. Returns a function that withdraws the offer. When
   * the pool is over its budget the longest-hidden offer is taken up at once,
   * so a terminal hidden past the budget does not wait out its grace.
   */
  idle(release: () => void): () => void;
  /**
   * Registers a session that was refused. `retry` runs when a slot frees or
   * can be taken over; returns a function that stops the wait.
   */
  wait(retry: () => void): () => void;
}

export function createGpuSlots(
  initialCap: number = MAX_VISIBLE_SESSIONS,
  /** The most contexts that may be live at once, whatever the budget. */
  hardCap: number = initialCap,
): GpuSlots {
  let cap = initialCap;
  let held = 0;
  /** Hidden holders, longest-hidden first (a Set keeps insertion order). */
  const idles = new Set<() => void>();
  const waiters = new Set<() => void>();
  /** Above zero while a take is freeing a slot for itself: nobody else may have it. */
  let reclaiming = 0;
  let notifying = false;

  /** Holders that are drawing or attaching: everything held that is not on offer. */
  const active = () => held - idles.size;
  /**
   * How many contexts may be held while `onScreen` terminals are drawing: the
   * budget, or one per terminal on screen when that is more, never past the
   * window's maximum. The budget is for the hidden; it never takes a context
   * from a terminal that can be seen.
   */
  const allowed = (onScreen: number) => Math.min(hardCap, Math.max(cap, onScreen));

  const reclaimOldest = () => {
    const [oldest] = idles;
    if (!oldest) return;
    idles.delete(oldest);
    reclaiming += 1;
    try {
      oldest();
    } finally {
      reclaiming -= 1;
    }
  };

  /** Gives up hidden holders the pool can no longer afford, longest-hidden first. */
  const trim = () => {
    const limit = allowed(active());
    while (held > limit && idles.size > 0) reclaimOldest();
  };

  const notify = () => {
    if (notifying) return;
    notifying = true;
    try {
      // Each retry either takes the slot, which ends the loop's condition, or
      // is final; a waiter that is refused again re-registers behind the loop.
      while (waiters.size > 0 && active() < hardCap) {
        const [next] = waiters;
        waiters.delete(next);
        next();
      }
    } finally {
      notifying = false;
    }
  };

  return {
    take() {
      if (active() >= hardCap) return false;
      // A terminal on screen always gets its context: the budget is spent on
      // hidden holders first, longest-hidden first.
      const limit = allowed(active() + 1);
      while (held + 1 > limit && idles.size > 0) reclaimOldest();
      held += 1;
      return true;
    },
    give() {
      if (held > 0) held -= 1;
      if (reclaiming === 0) notify();
    },
    held: () => held,
    cap: () => cap,
    setCap(next) {
      if (!Number.isFinite(next)) return;
      cap = Math.max(1, Math.floor(next));
      // An on-screen or attaching session is not in `idles`, so it is never taken from.
      trim();
      notify();
    },
    idle(release) {
      idles.add(release);
      // Hidden while the pool is over its budget: the longest-hidden holder
      // goes now, which is this one when it is the only one on offer.
      trim();
      notify();
      return () => {
        idles.delete(release);
      };
    },
    wait(retry) {
      waiters.add(retry);
      return () => {
        waiters.delete(retry);
      };
    },
  };
}

/**
 * Every terminal in the window draws from this one pool. Its budget for
 * hidden contexts starts at the compact one and follows the resource profile
 * (`applyResourceBudgets`); a terminal on screen is never held back by it, up
 * to `MAX_VISIBLE_SESSIONS`.
 */
export const gpuSlots = createGpuSlots(COMPACT_BUDGETS.gpu_contexts, MAX_VISIBLE_SESSIONS);

export interface GpuRenderer {
  /**
   * Brings the renderer in line with whether the session wants WebGL now —
   * on screen and opened — and whether the setting allows it at all. A session
   * that stops wanting WebGL keeps its context through the hold; one whose
   * setting is off releases it at once. Returns the resulting state.
   */
  sync(want: boolean, enabled?: boolean): GpuState;
  /**
   * The context was lost: dispose now. The session draws through the DOM and
   * attaches again unless it has lost too many. `source`, the canvas that
   * reported it, lets a late event from a context released on purpose be
   * ignored.
   */
  lose(source?: unknown): void;
  /** Rebuilds the glyph atlas for a display-metrics change; a no-op without a context. */
  clearAtlas(): void;
  state(): GpuState;
  /** The session is closing: release the context without marking it lost. */
  dispose(): void;
}

export function createGpuRenderer(deps: {
  /** Creates the addon and loads it into the terminal; may throw (no WebGL2). */
  attach: () => GpuAddon;
  slots?: GpuSlots;
  policy?: GpuPolicy;
  /** Every state change, after it happened. */
  onChange?: (state: GpuState) => void;
  /**
   * Repaints the viewport. Called once a context is attached or taken out of
   * the hold: a renderer set while xterm's render service was paused draws a
   * blank first frame (xterm.js #6239).
   */
  refresh?: () => void;
}): GpuRenderer {
  const slots = deps.slots ?? gpuSlots;
  const policy = () => deps.policy ?? gpuPolicy;
  let state: GpuState = "dom";
  let addon: GpuAddon | null = null;
  let lossListener: { dispose(): void } | null = null;
  let closed = false;
  /** The session can never hold a context again (`lost`). */
  let final = false;
  let wanted = false;
  /** In the hold: hidden, but still holding its context and slot. */
  let holding = false;
  let holdTimer: ReturnType<typeof setTimeout> | null = null;
  let withdrawOffer: (() => void) | null = null;
  let stopWaiting: (() => void) | null = null;
  let recoverTimer: ReturnType<typeof setTimeout> | null = null;
  let losses: number[] = [];

  const set = (next: GpuState) => {
    if (next === state) return;
    state = next;
    deps.onChange?.(next);
  };

  const clearHold = () => {
    if (holdTimer) clearTimeout(holdTimer);
    holdTimer = null;
    withdrawOffer?.();
    withdrawOffer = null;
    holding = false;
  };

  const clearRecover = () => {
    if (recoverTimer) clearTimeout(recoverTimer);
    recoverTimer = null;
  };

  const clearWait = () => {
    stopWaiting?.();
    stopWaiting = null;
  };

  /**
   * Drops the addon, frees its context and gives the slot back, in that order:
   * a session waiting for the slot attaches only once the context is gone.
   * xterm falls back to its DOM renderer.
   */
  const release = () => {
    const held = addon;
    if (!held) return;
    addon = null;
    clearHold();
    lossListener?.dispose();
    lossListener = null;
    try {
      // First, while the GL objects can still be deleted and the canvas is
      // attached; dispose also detaches the canvas, so the `webglcontextlost`
      // our own loseContext() queues next reaches no listener.
      held.dispose();
    } catch {
      // The context is already gone; the slot and the state are what matter.
    } finally {
      loseWebglContext(held.canvas);
      slots.give();
    }
  };

  /** A context released on purpose, or never needed: the session is on the DOM. */
  const toDom = () => {
    release();
    if (state === "webgl") set("dom");
  };

  const finalize = () => {
    final = true;
    clearRecover();
    clearWait();
    set("lost");
  };

  const retry = () => {
    stopWaiting = null;
    attempt();
  };

  const attempt = () => {
    if (closed || final || addon || !wanted) return;
    if (!slots.take()) {
      stopWaiting ??= slots.wait(retry);
      return;
    }
    clearWait();
    let created: GpuAddon;
    try {
      created = deps.attach();
    } catch {
      // No WebGL2, or a context the GPU process refused: as final as a loss.
      slots.give();
      finalize();
      return;
    }
    addon = created;
    lossListener = created.onContextLoss(() => lose());
    set("webgl");
    repaint();
  };

  const repaint = () => {
    try {
      deps.refresh?.();
    } catch {
      // A repaint that cannot be requested (the terminal is going away) must
      // not unwind the lifecycle; the next output repaints.
    }
  };

  const scheduleRecover = () => {
    clearRecover();
    if (closed || final || !wanted) return;
    const wait = Math.max(0, (losses.at(-1) ?? 0) + policy().recoverDelayMs - Date.now());
    recoverTimer = setTimeout(() => {
      recoverTimer = null;
      attempt();
    }, wait);
  };

  const lose = (source?: unknown) => {
    // Nothing held: the late event of a context released on purpose.
    if (!addon) return;
    if (source && addon.canvas && source !== addon.canvas) return;
    // A context held only for warmth is the one both engines evict first (it
    // drew least recently). Losing it ends the hold early; it is not a fault
    // on a session anyone is looking at, so it neither counts against the
    // budget nor schedules a recovery: the next show attaches afresh.
    const wasHeld = holding;
    release();
    if (wasHeld) {
      set("dom");
      return;
    }
    const now = Date.now();
    losses = losses.filter((at) => now - at < policy().lossWindowMs);
    losses.push(now);
    if (losses.length > policy().lossBudget) {
      finalize();
      return;
    }
    set("reset");
    scheduleRecover();
  };

  const beginHold = () => {
    holding = true;
    holdTimer = setTimeout(() => {
      holdTimer = null;
      toDom();
    }, policy().hideGraceMs);
    // Offering the slot can hand it to a waiting session on the spot, which
    // reclaims this very hold: only keep the withdrawal if it is still ours.
    const withdraw = slots.idle(() => {
      withdrawOffer = null;
      toDom();
    });
    if (addon && holding) withdrawOffer = withdraw;
    else withdraw();
  };

  return {
    sync(want, enabled = true) {
      if (closed) return state;
      wanted = want && enabled;
      if (!enabled) {
        clearRecover();
        clearWait();
        toDom();
        if (state === "reset") set("dom");
        return state;
      }
      if (final) return state;
      if (!want) {
        clearRecover();
        clearWait();
        if (addon && !holding) beginHold();
        return state;
      }
      if (addon) {
        if (holding) {
          clearHold();
          repaint();
        }
        return state;
      }
      if (state === "reset" && (losses.at(-1) ?? 0) + policy().recoverDelayMs > Date.now()) {
        if (!recoverTimer) scheduleRecover();
        return state;
      }
      attempt();
      return state;
    },
    lose,
    clearAtlas() {
      try {
        addon?.clearTextureAtlas?.();
      } catch {
        // The atlas is rebuilt on the next draw anyway.
      }
    },
    state: () => state,
    dispose() {
      closed = true;
      wanted = false;
      clearRecover();
      clearWait();
      release();
      if (state === "webgl" || state === "reset") set("dom");
    },
  };
}
