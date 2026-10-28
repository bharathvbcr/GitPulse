import { describe, expect, it, vi } from "vitest";
import { MAX_VISIBLE_SESSIONS } from "./attendance";
import {
  createGpuRenderer,
  createGpuSlots,
  GPU_HIDE_GRACE_MS,
  GPU_RECOVER_DELAY_MS,
  type GpuAddon,
  type GpuSlots,
  type GpuState,
} from "./gpuRenderer";

/** A fake addon that records its disposal and can fire its context loss. */
function fakeAddon() {
  let loss: (() => void) | null = null;
  const addon = {
    disposed: 0,
    listening: () => loss !== null,
    fireLoss: () => loss?.(),
    dispose() {
      addon.disposed += 1;
    },
    onContextLoss(listener: () => void) {
      loss = listener;
      return { dispose: () => { loss = null; } };
    },
  };
  return addon;
}

function harness(slots: GpuSlots = createGpuSlots()) {
  const made: ReturnType<typeof fakeAddon>[] = [];
  const changes: GpuState[] = [];
  let failNext = false;
  const renderer = createGpuRenderer({
    attach: (): GpuAddon => {
      if (failNext) throw new Error("WebGL2 not supported null");
      const addon = fakeAddon();
      made.push(addon);
      return addon;
    },
    slots,
    onChange: (state) => changes.push(state),
  });
  return { renderer, made, changes, slots, failAttach: () => { failNext = true; } };
}

describe("terminal GPU renderer lifecycle", () => {
  it("draws through the DOM until the session is on screen", () => {
    const { renderer, made } = harness();
    expect(renderer.state()).toBe("dom");
    expect(renderer.sync(false)).toBe("dom");
    expect(made).toHaveLength(0);
  });

  it("attaches once while on screen, keeps the context through a hide, and disposes when the hold ends", () => {
    vi.useFakeTimers();
    try {
      const { renderer, made, slots } = harness();
      expect(renderer.sync(true)).toBe("webgl");
      expect(renderer.sync(true)).toBe("webgl");
      expect(made).toHaveLength(1);
      expect(slots.held()).toBe(1);

      // Hidden: still held, so a tab flick reuses it (see the lifecycle suite).
      renderer.sync(false);
      expect(made[0].disposed).toBe(0);
      vi.advanceTimersByTime(GPU_HIDE_GRACE_MS);
      expect(renderer.state()).toBe("dom");
      expect(made[0].disposed).toBe(1);
      expect(slots.held()).toBe(0);

      // Shown again: a fresh context, not the disposed one revived.
      expect(renderer.sync(true)).toBe("webgl");
      expect(made).toHaveLength(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("disposes synchronously on context loss, and re-attaches only after the recovery delay", () => {
    vi.useFakeTimers();
    try {
      const { renderer, made, slots, changes } = harness();
      renderer.sync(true);
      made[0].fireLoss();
      expect(renderer.state()).toBe("reset");
      expect(made[0].disposed).toBe(1);
      expect(made[0].listening()).toBe(false);
      expect(slots.held()).toBe(0);

      // Not before the delay, however often the session is shown and hidden.
      for (let round = 0; round < 5; round += 1) {
        renderer.sync(false);
        renderer.sync(true);
      }
      expect(made).toHaveLength(1);
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
      expect(renderer.state()).toBe("webgl");
      expect(made).toHaveLength(2);
      expect(changes).toEqual(["webgl", "reset", "webgl"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("treats a loss reported by the canvas the same as the addon's own event", () => {
    // The addon waits three seconds for a restore before it fires
    // onContextLoss, and its canvas is blank meanwhile; the session reacts to
    // the canvas's `webglcontextlost` itself through `lose()`.
    const { renderer, made } = harness();
    renderer.sync(true);
    renderer.lose();
    made[0].fireLoss();
    expect(made[0].disposed).toBe(1);
    expect(renderer.state()).toBe("reset");
    expect(made).toHaveLength(1);
  });

  it("marks a session that cannot create a context lost, and does not retry it", () => {
    const { renderer, made, slots, failAttach } = harness();
    failAttach();
    expect(renderer.sync(true)).toBe("lost");
    expect(slots.held()).toBe(0);
    expect(renderer.sync(true)).toBe("lost");
    expect(made).toHaveLength(0);
  });

  it("holds at most MAX_VISIBLE_SESSIONS contexts across sessions", () => {
    const slots = createGpuSlots();
    const sessions = Array.from({ length: MAX_VISIBLE_SESSIONS + 3 }, () => harness(slots));
    const states = sessions.map((session) => session.renderer.sync(true));
    expect(states.filter((state) => state === "webgl")).toHaveLength(MAX_VISIBLE_SESSIONS);
    expect(states.slice(MAX_VISIBLE_SESSIONS)).toEqual(["dom", "dom", "dom"]);
    expect(slots.held()).toBe(MAX_VISIBLE_SESSIONS);

    // A refused session is not lost: once a slot frees, its next sync takes it.
    sessions[0].renderer.sync(false);
    expect(sessions[MAX_VISIBLE_SESSIONS].renderer.sync(true)).toBe("webgl");
    expect(slots.held()).toBe(MAX_VISIBLE_SESSIONS);
  });

  it("releases its slot when the session closes, and stays closed", () => {
    const { renderer, made, slots } = harness();
    renderer.sync(true);
    renderer.dispose();
    expect(made[0].disposed).toBe(1);
    expect(slots.held()).toBe(0);
    expect(renderer.state()).toBe("dom");
    expect(renderer.sync(true)).toBe("dom");
    expect(made).toHaveLength(1);
    expect(slots.held()).toBe(0);
  });

  it("gives back no slot it does not hold", () => {
    const slots = createGpuSlots(1);
    slots.give();
    expect(slots.held()).toBe(0);
    expect(slots.take()).toBe(true);
    expect(slots.take()).toBe(false);
  });
});
