/**
 * When a terminal draws through WebGL instead of the DOM.
 *
 * The DOM renderer rebuilds rows of spans on the WebContent main thread for
 * every redraw, and an agent CLI redraws most of its screen continuously. The
 * WebGL renderer moves that to the GPU, but a WebGL context is a scarce,
 * losable resource, so it is held only while someone can see the session:
 *
 *  - **Attended only.** A session takes a context when it is on screen (the
 *    same signal `attendance.ts` reports) and gives it back the moment it is
 *    hidden. The slots are capped at `MAX_VISIBLE_SESSIONS`, so a caller that
 *    claims more sessions are on screen than can be is refused here, where the
 *    mistake is, and that session keeps drawing through the DOM.
 *  - **Lost is final.** A context that is lost, or that could not be created
 *    at all, is disposed at once — xterm then draws through its DOM renderer
 *    again — and the session never asks for another. Re-attaching after a loss
 *    is how a driver fault becomes a loss loop.
 *
 * Nothing here imports xterm: `attach` is the caller's, so the decisions are
 * testable without a WebGL context.
 */
import { MAX_VISIBLE_SESSIONS } from "./attendance";

/** How a session is drawing: through the DOM, through WebGL, or through the DOM for good. */
export type GpuState = "dom" | "webgl" | "lost";

/** The part of `WebglAddon` the lifecycle uses. */
export interface GpuAddon {
  dispose(): void;
  onContextLoss(listener: () => void): { dispose(): void };
}

/** The shared count of live WebGL contexts. */
export interface GpuSlots {
  /** Claims a slot; false when every slot is held. */
  take(): boolean;
  /** Returns a slot taken earlier. */
  give(): void;
  /** Slots held now. */
  held(): number;
}

export function createGpuSlots(cap: number = MAX_VISIBLE_SESSIONS): GpuSlots {
  let held = 0;
  return {
    take() {
      if (held >= cap) return false;
      held += 1;
      return true;
    },
    give() {
      if (held > 0) held -= 1;
    },
    held: () => held,
  };
}

/** Every terminal in the window draws from this one pool. */
export const gpuSlots = createGpuSlots();

export interface GpuRenderer {
  /**
   * Brings the renderer in line with whether the session wants WebGL now —
   * on screen, opened, and the setting on. Returns the resulting state.
   */
  sync(want: boolean): GpuState;
  /** The context was lost: dispose now and never attach again. */
  lose(): void;
  state(): GpuState;
  /** The session is closing: release the context without marking it lost. */
  dispose(): void;
}

export function createGpuRenderer(deps: {
  /** Creates the addon and loads it into the terminal; may throw (no WebGL2). */
  attach: () => GpuAddon;
  slots?: GpuSlots;
  /** Every state change, after it happened. */
  onChange?: (state: GpuState) => void;
}): GpuRenderer {
  const slots = deps.slots ?? gpuSlots;
  let state: GpuState = "dom";
  let addon: GpuAddon | null = null;
  let lossListener: { dispose(): void } | null = null;
  let closed = false;

  const set = (next: GpuState) => {
    if (next === state) return;
    state = next;
    deps.onChange?.(next);
  };

  /** Drops the addon and its slot; xterm falls back to its DOM renderer. */
  const release = () => {
    const held = addon;
    if (!held) return;
    addon = null;
    lossListener?.dispose();
    lossListener = null;
    slots.give();
    try {
      held.dispose();
    } catch {
      // The context is already gone; the slot and the state are what matter.
    }
  };

  const lose = () => {
    if (state === "lost") return;
    release();
    set("lost");
  };

  return {
    sync(want) {
      if (closed || state === "lost") return state;
      if (!want) {
        release();
        set("dom");
        return state;
      }
      if (addon) return state;
      if (!slots.take()) return state;
      let created: GpuAddon;
      try {
        created = deps.attach();
      } catch {
        // No WebGL2, or a context the GPU process refused: as final as a loss.
        slots.give();
        set("lost");
        return state;
      }
      addon = created;
      lossListener = created.onContextLoss(lose);
      set("webgl");
      return state;
    },
    lose,
    state: () => state,
    dispose() {
      closed = true;
      release();
      if (state === "webgl") set("dom");
    },
  };
}
