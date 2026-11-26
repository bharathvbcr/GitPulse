import { describe, expect, it, vi } from "vitest";
import { MAX_VISIBLE_SESSIONS } from "./attendance";
import {
  attachWebglAddon,
  createGpuRenderer,
  createGpuSlots,
  GPU_HIDE_GRACE_MS,
  GPU_LOSS_BUDGET,
  GPU_RECOVER_DELAY_MS,
  gpuPolicy,
  isRecoverableWebglAttachError,
  WebglContextLostAttachError,
  type GpuAddon,
  type GpuSlots,
  type GpuState,
  type WebglHostCanvas,
  type WebglHostContainer,
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
    // Written against the roomy grace; the shared policy starts at the compact one.
    policy: { ...gpuPolicy, hideGraceMs: GPU_HIDE_GRACE_MS },
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

function fakeCanvas(classes: string[] = []): WebglHostCanvas & {
  removed: boolean;
  contextLost: boolean;
  getContextCalls: number;
} {
  const canvas = {
    classList: classes,
    removed: false,
    contextLost: false,
    getContextCalls: 0,
    getContext(kind: "webgl2") {
      canvas.getContextCalls += 1;
      if (kind !== "webgl2" || classes.some((name) => name.endsWith("-layer"))) return null;
      return {
        isContextLost: () => canvas.contextLost,
        getExtension: (name: "WEBGL_lose_context") =>
          name === "WEBGL_lose_context" ? { loseContext: () => { canvas.contextLost = true; } } : null,
      };
    },
    remove() {
      canvas.removed = true;
    },
  };
  return canvas;
}

describe("attachWebglAddon", () => {
  it("returns the non-layer WebGL canvas and forwards addon methods on success", () => {
    const canvases: ReturnType<typeof fakeCanvas>[] = [];
    const linkCanvas = fakeCanvas(["xterm-link-layer"]);
    const glCanvas = fakeCanvas([]);
    const addon = Object.assign(fakeAddon(), {
      cleared: 0,
      clearTextureAtlas() {
        this.cleared += 1;
      },
    });
    const handle = attachWebglAddon(
      {
        element: { querySelectorAll: () => canvases },
        loadAddon() {
          canvases.push(linkCanvas, glCanvas);
        },
      },
      () => addon,
    );

    expect(handle.canvas).toBe(glCanvas);
    handle.clearTextureAtlas?.();
    expect(addon.cleared).toBe(1);
    handle.dispose();
    expect(addon.disposed).toBe(1);
  });

  it("finds the WebGL canvas in the screen, not the atlas scratch canvas parked on the root ahead of it", () => {
    // addon-webgl's shared glyph atlas puts a classless 2D canvas on the root
    // of whichever terminal last rasterized a glyph; here it comes first in
    // document order, so a root search would pick it.
    const atlasScratch = fakeCanvas([]);
    const linkCanvas = fakeCanvas(["xterm-link-layer"]);
    const glCanvas = fakeCanvas([]);
    const root: ReturnType<typeof fakeCanvas>[] = [];
    const screen: ReturnType<typeof fakeCanvas>[] = [];
    const handle = attachWebglAddon(
      {
        element: { querySelectorAll: () => root },
        screenElement: { querySelectorAll: () => screen },
        loadAddon() {
          root.push(atlasScratch, linkCanvas, glCanvas);
          screen.push(linkCanvas, glCanvas);
        },
      },
      () => fakeAddon(),
    );

    expect(handle.canvas).toBe(glCanvas);
  });

  it("cleans up orphaned canvases and disposes the addon when loadAddon throws mid-constructor", () => {
    const existing = fakeCanvas([]);
    const linkCanvas = fakeCanvas(["xterm-link-layer"]);
    const glCanvas = fakeCanvas([]);
    const canvases = [existing];
    const addon = fakeAddon();
    const boom = new Error("value must not be falsy");

    expect(() =>
      attachWebglAddon(
        {
          element: { querySelectorAll: () => canvases },
          loadAddon() {
            // Mirrors @xterm/addon-webgl WebglRenderer appending its link-layer
            // and WebGL canvases before _initializeWebGLState() throws.
            canvases.push(linkCanvas, glCanvas);
            throw boom;
          },
        },
        () => addon,
      ),
    ).toThrow(boom);

    expect(addon.disposed).toBe(1);
    expect(existing.removed).toBe(false);
    expect(existing.contextLost).toBe(false);
    expect(linkCanvas.removed).toBe(true);
    expect(linkCanvas.getContextCalls).toBe(0);
    expect(glCanvas.removed).toBe(true);
    expect(glCanvas.contextLost).toBe(true);
  });

  it("still removes orphaned canvases and rethrows the original error if dispose() throws", () => {
    const glCanvas = fakeCanvas([]);
    const canvases: ReturnType<typeof fakeCanvas>[] = [];
    const originalError = new Error("shader compile failed");

    expect(() =>
      attachWebglAddon(
        {
          element: { querySelectorAll: () => canvases },
          loadAddon() {
            canvases.push(glCanvas);
            throw originalError;
          },
        },
        () => ({
          ...fakeAddon(),
          dispose() {
            throw new Error("dispose failed");
          },
        }),
      ),
    ).toThrow(originalError);

    expect(glCanvas.removed).toBe(true);
    expect(glCanvas.contextLost).toBe(true);
  });

  it("silences orphaned webglcontextlost and webglcontextrestored listeners via capture reparenting", () => {
    const targetCalls: string[] = [];
    const targetListeners = new Map<string, Array<(event: { stopPropagation(): void }) => void>>();
    const graveyards: Array<{
      captureListeners: Map<string, Array<(event: { stopPropagation(): void }) => void>>;
      children: unknown[];
    }> = [];

    let parent: {
      captureListeners: Map<string, Array<(event: { stopPropagation(): void }) => void>>;
      children: unknown[];
    } | null = null;
    const removedFromHost = { value: false };
    const dispatchOnCanvas = (type: "webglcontextlost" | "webglcontextrestored") => {
      let stopped = false;
      const event = {
        stopPropagation() {
          stopped = true;
        },
      };
      // DOM CAPTURING_PHASE on parent container runs before AT_TARGET on canvas.
      for (const listener of parent?.captureListeners.get(type) ?? []) {
        listener(event);
      }
      if (stopped) return;
      for (const listener of targetListeners.get(type) ?? []) {
        listener(event);
      }
    };

    interface SilencedCanvas extends WebglHostCanvas<SilencedCanvas> {
      contextLost: boolean;
    }
    const glCanvas: SilencedCanvas = {
      classList: [],
      contextLost: false,
      ownerDocument: {
        createElement() {
          const container = {
            captureListeners: new Map<string, Array<(event: { stopPropagation(): void }) => void>>(),
            children: [] as unknown[],
          };
          graveyards.push(container);
          const host: WebglHostContainer<SilencedCanvas> = {
            addEventListener(type, listener, capture) {
              if (!capture) return;
              const list = container.captureListeners.get(type) ?? [];
              list.push(listener);
              container.captureListeners.set(type, list);
            },
            appendChild(node) {
              removedFromHost.value = true;
              parent = container;
              container.children.push(node);
              return node;
            },
          };
          return host;
        },
      },
      getContext(kind: "webgl2") {
        if (kind !== "webgl2") return null;
        return {
          isContextLost: () => glCanvas.contextLost,
          getExtension: (name: "WEBGL_lose_context") =>
            name === "WEBGL_lose_context"
              ? {
                  loseContext: () => {
                    glCanvas.contextLost = true;
                    // Browser queues webglcontextlost directly on the canvas.
                    dispatchOnCanvas("webglcontextlost");
                  },
                }
              : null,
        };
      },
      remove() {
        removedFromHost.value = true;
      },
    };

    const canvases: SilencedCanvas[] = [];
    const boom = new Error("non-loss shader error");

    expect(() =>
      attachWebglAddon(
        {
          element: { querySelectorAll: () => canvases },
          loadAddon() {
            // Mirror WebglRenderer binding target listeners on this._canvas
            // before _initializeWebGLState() throws.
            targetListeners.set("webglcontextlost", [() => targetCalls.push("lost")]);
            targetListeners.set("webglcontextrestored", [() => targetCalls.push("restored")]);
            canvases.push(glCanvas);
            throw boom;
          },
        },
        () => fakeAddon(),
      ),
    ).toThrow(boom);

    // Neither the loseContext() event nor a subsequent browser restore event reaches WebglRenderer.
    dispatchOnCanvas("webglcontextlost");
    dispatchOnCanvas("webglcontextrestored");

    expect(removedFromHost.value).toBe(true);
    expect(glCanvas.contextLost).toBe(true);
    expect(graveyards).toHaveLength(1);
    expect(graveyards[0].children).toEqual([glCanvas]);
    expect(targetCalls).toEqual([]);
  });

  it("wraps lost-context attach failures in WebglContextLostAttachError for bounded recovery", () => {
    const linkCanvas = fakeCanvas(["xterm-link-layer"]);
    const glCanvas = fakeCanvas([]);
    const canvases: ReturnType<typeof fakeCanvas>[] = [];
    const addon = fakeAddon();
    const boom = new Error("Could not get value");

    let caught: unknown;
    try {
      attachWebglAddon(
        {
          element: { querySelectorAll: () => canvases },
          loadAddon() {
            canvases.push(linkCanvas, glCanvas);
            // GPU process lost the context before/during shader compilation.
            glCanvas.contextLost = true;
            throw boom;
          },
        },
        () => addon,
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(WebglContextLostAttachError);
    expect(isRecoverableWebglAttachError(caught)).toBe(true);
    expect((caught as WebglContextLostAttachError).cause).toBe(boom);
    expect(addon.disposed).toBe(1);
    expect(linkCanvas.removed).toBe(true);
    expect(glCanvas.removed).toBe(true);
  });

  it("recovers after a transient WebglContextLostAttachError and still finalizes once lossBudget is exceeded", () => {
    vi.useFakeTimers();
    try {
      const slots = createGpuSlots();
      const changes: GpuState[] = [];
      let attempts = 0;
      let failAsLost = true;
      const renderer = createGpuRenderer({
        attach: (): GpuAddon => {
          attempts += 1;
          if (failAsLost) throw new WebglContextLostAttachError(new Error("Could not get value"));
          return fakeAddon();
        },
        slots,
        policy: { ...gpuPolicy, hideGraceMs: GPU_HIDE_GRACE_MS },
        onChange: (state) => changes.push(state),
      });

      // Transient loss during wake/GPU reset moves to reset, not permanently lost.
      expect(renderer.sync(true)).toBe("reset");
      expect(slots.held()).toBe(0);
      expect(attempts).toBe(1);

      // Once the GPU process is back, the scheduled retry attaches WebGL cleanly.
      failAsLost = false;
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
      expect(renderer.state()).toBe("webgl");
      expect(attempts).toBe(2);
      expect(changes).toEqual(["reset", "webgl"]);
      renderer.dispose();

      // Bounded: repeated lost-context attach failures still finalize after GPU_LOSS_BUDGET.
      const brokenChanges: GpuState[] = [];
      let brokenAttempts = 0;
      const broken = createGpuRenderer({
        attach: (): GpuAddon => {
          brokenAttempts += 1;
          throw new WebglContextLostAttachError(new Error("Could not get value"));
        },
        slots,
        policy: { ...gpuPolicy, hideGraceMs: GPU_HIDE_GRACE_MS },
        onChange: (state) => brokenChanges.push(state),
      });

      expect(broken.sync(true)).toBe("reset");
      for (let i = 0; i < GPU_LOSS_BUDGET; i += 1) {
        vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
      }
      expect(broken.state()).toBe("lost");
      expect(brokenAttempts).toBe(GPU_LOSS_BUDGET + 1);
      expect(slots.held()).toBe(0);
      expect(brokenChanges).toEqual(["reset", "lost"]);

      // Does not keep retrying after finalizing.
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS * 5);
      expect(brokenAttempts).toBe(GPU_LOSS_BUDGET + 1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("handles a terminal with no element cleanly", () => {
    const addon = fakeAddon();
    const handle = attachWebglAddon(
      {
        element: null,
        loadAddon() {},
      },
      () => addon,
    );
    expect(handle.canvas).toBeUndefined();
  });
});
