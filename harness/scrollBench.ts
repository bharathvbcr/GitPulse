/**
 * Measures how a terminal scrolls through its history: per-frame times and how
 * far the viewport moves each frame, while scripted wheel input runs over a
 * 5,000-row scrollback.
 *
 * One trial is a fresh `Terminal` (WebGL through the real `createGpuRenderer`
 * with the attach `TerminalSession` uses, or the DOM renderer), filled with
 * 5,000 rows, then one scripted gesture, with output streaming in or not. The
 * arms (renderer x input x streaming x scroll options) run interleaved within
 * each round, in an order that rotates per round, so no arm always runs first.
 *
 * Input is dispatched as `WheelEvent`s on xterm's screen element, once per
 * animation frame, as a browser coalesces them: every event whose scripted
 * time has come is summed into one event. Three inputs:
 *
 *  - `wheel`:     discrete notches, `deltaY` 100 pixels, one every 70 ms.
 *  - `pixel`:     trackpad-like, fractional pixel deltas at 120 Hz: a finger
 *                 phase that rises and falls, then momentum decaying by 4% per
 *                 event.
 *  - `pixel-int`: the same, rounded to whole pixels. xterm's wheel classifier
 *                 (`scrollableElement.ts`, `MouseWheelClassifier`) calls a
 *                 stream of whole-number deltas a physical wheel, so this arm
 *                 is what shows whether smoothing would animate a trackpad.
 */
import { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import "@xterm/xterm/css/xterm.css";
import {
  armsFor, COLS, FILL_ROWS, gesture, line, rng, roundOrder, ROWS, SCROLLBACK, SETTLE_MS, STREAM_CHUNK, STREAM_INTERVAL_MS, STREAM_LINES,
  type Arm, type Input, type Renderer,
} from "./scrollScript";
import { attachScrollGuard } from "../src/lib/terminal/scrollFeel";
import { createGpuRenderer, createGpuSlots, gpuPolicy } from "../src/lib/terminal/gpuRenderer";

export type { Arm, Input, Renderer };
export { armsFor };

export interface Trial extends Arm {
  kind: "trial";
  round: number;
  /** Time between consecutive animation frames over the gesture and the settle, ms. */
  frameMs: number[];
  /** Time `dispatchEvent` took at each frame that dispatched wheel input: xterm's handler and the guard, synchronously. */
  dispatchMs: number[];
  /** Rows the viewport moved at each of those frames (absolute). */
  steps: number[];
  startY: number;
  finalY: number;
  /** `buffer.active.baseY` at the end: the bottom of the scrollback. */
  endBaseY: number;
  /** `onScroll` events the terminal fired, and the time the component's handler body took in all of them. */
  scrollEvents: number;
  handlerMs: number;
  linesStreamed: number;
  /** What the engine reports for a plain constructed 100 px event (before `wheelEvent` sets `wheelDeltaY`). */
  wheelSample: { deltaY: number; wheelDeltaY: number | null };
  /** The renderer state through the trial. */
  state: string;
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const frame = () => new Promise<number>((resolve) => requestAnimationFrame(resolve));

/**
 * A wheel event as the browsers' platform path builds it. A constructed
 * `WheelEvent` reports `wheelDeltaY` equal to `deltaY` (Chrome 100 for 100,
 * measured here, see `wheelSample`), but xterm reads `wheelDeltaY` before
 * `deltaY` (`mouseEvent.ts`, `StandardWheelEvent`) and a real event carries the
 * opposite sign: 40 pixels are one tick of 120 units (xterm's own fallback for
 * an event without it is `-deltaY / 40`, same file). Set it so the constructed
 * event takes the path a real one does.
 */
export function wheelEvent(deltaY: number): WheelEvent {
  const event = new WheelEvent("wheel", { deltaY, deltaMode: WheelEvent.DOM_DELTA_PIXEL, bubbles: true, cancelable: true, clientX: 400, clientY: 300 });
  Object.defineProperty(event, "wheelDeltaY", { value: -deltaY * 3 });
  Object.defineProperty(event, "wheelDeltaX", { value: 0 });
  return event;
}

function sampleWheel(): Trial["wheelSample"] {
  const event = new WheelEvent("wheel", { deltaY: 100, deltaMode: WheelEvent.DOM_DELTA_PIXEL });
  const legacy = Reflect.get(event, "wheelDeltaY");
  return { deltaY: event.deltaY, wheelDeltaY: typeof legacy === "number" ? legacy : null };
}

export async function runTrial(host: HTMLElement, arm: Arm, round: number): Promise<Trial> {
  const term = new Terminal({
    cols: COLS,
    rows: ROWS,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace",
    fontSize: 13,
    lineHeight: 1.15,
    allowProposedApi: true,
    allowTransparency: true,
    scrollback: SCROLLBACK,
    smoothScrollDuration: arm.smooth,
    scrollSensitivity: arm.sens,
    theme: { background: "#141a29", foreground: "#e9edf8" },
  });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  term.open(host);
  let canvas: HTMLCanvasElement | undefined;
  const gpu = createGpuRenderer({
    attach: () => {
      const created = new WebglAddon();
      term.loadAddon(created);
      canvas = [...(term.element?.querySelectorAll("canvas") ?? [])].find(
        (node) => ![...node.classList].some((name) => name.endsWith("-layer")),
      );
      return {
        canvas,
        dispose: () => created.dispose(),
        onContextLoss: (listener) => created.onContextLoss(listener),
        clearTextureAtlas: () => created.clearTextureAtlas(),
      };
    },
    slots: createGpuSlots(8),
    policy: { ...gpuPolicy },
    refresh: () => term.refresh(0, term.rows - 1),
  });
  const detachGuard = arm.guard ? attachScrollGuard(term) : () => {};
  let streamTimer: ReturnType<typeof setInterval> | undefined;
  try {
    if (arm.renderer === "webgl" && gpu.sync(true) !== "webgl") throw new Error(`no WebGL context (${gpu.state()})`);
    // The bench is the same terminal the app builds, with the same handler on the scroll event.
    let scrollEvents = 0;
    let handlerMs = 0;
    term.onScroll(() => {
      const start = performance.now();
      void (term.buffer.active.viewportY < term.buffer.active.baseY);
      handlerMs += performance.now() - start;
      scrollEvents += 1;
    });

    const next = rng(11);
    for (let from = 0; from < FILL_ROWS; from += 500) {
      let chunk = "";
      for (let i = from; i < from + 500; i += 1) chunk += line(next, i);
      await new Promise<void>((resolve) => term.write(chunk, () => resolve()));
    }
    await frame(); await frame();
    await sleep(100);
    const startY = term.buffer.active.viewportY;
    scrollEvents = 0; handlerMs = 0;

    let linesStreamed = 0;
    if (arm.stream) {
      const streamNext = rng(23);
      streamTimer = setInterval(() => {
        term.write(STREAM_CHUNK(streamNext, FILL_ROWS + linesStreamed));
        linesStreamed += STREAM_LINES;
      }, STREAM_INTERVAL_MS);
    }

    const target = term.element?.querySelector(".xterm-screen");
    if (!target) throw new Error("no .xterm-screen to dispatch wheel input on");
    const script = gesture(arm.input);
    const frameMs: number[] = [];
    const dispatchMs: number[] = [];
    const steps: number[] = [];
    const lastAt = script[script.length - 1][0];
    let begin = 0;
    let previousTs = 0;
    let previousY = startY;
    let cursor = 0;
    await new Promise<void>((resolve) => {
      const tick = (ts: number) => {
        if (begin === 0) begin = ts;
        else {
          frameMs.push(ts - previousTs);
          const y = term.buffer.active.viewportY;
          steps.push(Math.abs(y - previousY));
          previousY = y;
        }
        previousTs = ts;
        const elapsed = ts - begin;
        let due = 0;
        let any = false;
        while (cursor < script.length && script[cursor][0] <= elapsed) { due += script[cursor][1]; cursor += 1; any = true; }
        if (any) {
          const began = performance.now();
          target.dispatchEvent(wheelEvent(due));
          dispatchMs.push(performance.now() - began);
        }
        if (elapsed > lastAt + SETTLE_MS) resolve(); else requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
    return {
      kind: "trial", ...arm, round, frameMs, dispatchMs, steps, startY,
      finalY: term.buffer.active.viewportY, endBaseY: term.buffer.active.baseY,
      scrollEvents, handlerMs, linesStreamed, state: gpu.state(),
      wheelSample: sampleWheel(),
    };
  } finally {
    if (streamTimer !== undefined) clearInterval(streamTimer);
    detachGuard();
    gpu.dispose();
    term.dispose();
    host.replaceChildren();
  }
}

export async function run(host: HTMLElement, arms: Arm[], rounds: number): Promise<Trial[]> {
  const rows: Trial[] = [];
  for (let round = 0; round < rounds; round += 1) {
    for (const arm of roundOrder(arms, round)) {
      rows.push(await runTrial(host, arm, round));
      await sleep(50);
    }
  }
  return rows;
}
