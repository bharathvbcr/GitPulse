/**
 * The parts of the scroll bench that touch no DOM: the arms, the scripted
 * gestures, the synthetic output and the round order. They live apart from
 * `scrollBench.ts` so a unit test can import them without a browser.
 */
export type Renderer = "webgl" | "dom";
export type Input = "wheel" | "wheel-fast" | "pixel" | "pixel-int";

export interface Arm {
  renderer: Renderer;
  input: Input;
  stream: boolean;
  /** xterm's `smoothScrollDuration` in ms; 0 is its default. */
  smooth: number;
  /** xterm's `scrollSensitivity`; 1 is its default. */
  sens: number;
  /** `attachScrollGuard` from `src/lib/terminal/scrollFeel.ts` on the terminal. */
  guard: boolean;
}

export const COLS = 120;
export const ROWS = 40;
export const FILL_ROWS = 5000;
/** More than `FILL_ROWS` so streaming for a few seconds never trims the buffer, which would move the viewport by itself. */
export const SCROLLBACK = 6000;
export const SETTLE_MS = 600;
export const STREAM_INTERVAL_MS = 16;
export const STREAM_LINES = 2;

/** Deterministic: two runs are byte-identical. */
export function rng(seed: number): () => number {
  let state = (seed * 2654435761) >>> 0 || 1;
  return () => {
    state ^= state << 13; state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5; state >>>= 0;
    return state / 0x100000000;
  };
}

const WORDS = "the terminal keeps scrolling through history while an agent prints diffs and test output and the viewport moves a row at a time".split(" ");

/** One row of agent-CLI-shaped output: a few colour runs, some box drawing, an occasional emoji. */
export function line(next: () => number, index: number): string {
  const runs: string[] = [];
  let width = 0;
  while (width < COLS - 14) {
    const word = WORDS[Math.floor(next() * WORDS.length)];
    const colour = next() < 0.5 ? `\x1b[38;5;${16 + Math.floor(next() * 216)}m` : `\x1b[38;2;${Math.floor(next() * 255)};${Math.floor(next() * 255)};${Math.floor(next() * 255)}m`;
    runs.push(`${colour}${word}\x1b[0m `);
    width += word.length + 1;
  }
  const lead = index % 7 === 0 ? "│ " : index % 11 === 0 ? "● " : "  ";
  return `${lead}${String(index).padStart(5, "0")} ${runs.join("")}${index % 13 === 0 ? "🚀" : ""}\r\n`;
}

/** A scripted gesture: `[ms from start, deltaY in pixels]`, negative scrolls toward history. */
export function gesture(input: Input): Array<[number, number]> {
  const events: Array<[number, number]> = [];
  if (input === "wheel" || input === "wheel-fast") {
    // 100 px is one notch (`wheelDeltaY` 120); `wheel-fast` is three notches at once, a spun or accelerated wheel.
    const delta = input === "wheel" ? -100 : -300;
    for (let notch = 0; notch < 24; notch += 1) events.push([notch * 70, delta]);
    return events;
  }
  const period = 1000 / 120;
  const finger = 48;
  let at = 0;
  for (let i = 0; i < finger; i += 1, at += period) events.push([at, -(8 + 40 * Math.sin((Math.PI * i) / finger))]);
  let momentum = 24;
  while (momentum > 0.5) { events.push([at, -momentum]); momentum *= 0.96; at += period; }
  return input === "pixel-int" ? events.map(([t, dy]) => [t, Math.round(dy)]) : events.map(([t, dy]) => [t, Math.round(dy * 100) / 100]);
}

/** The stream an agent produces while the person reads history. */
export const STREAM_CHUNK = (next: () => number, from: number) => Array.from({ length: STREAM_LINES }, (_, i) => line(next, from + i)).join("");

/** `arms` in the order round `round` runs them: rotated, so each arm leads, and trails, in turn. */
export function roundOrder<T>(arms: readonly T[], round: number): T[] {
  const shift = (round * 7) % arms.length;
  return [...arms.slice(shift), ...arms.slice(0, shift)];
}

/**
 * Every combination of the dimensions, in a fixed order. The guard only acts
 * on a terminal that smooths, so it is not run on its own: three arms per
 * renderer, input and output, not four. That also keeps a WebKit page to
 * `3 * rounds` WebGL contexts, under the 16 it allows live (`gpuRenderer.ts`).
 */
export function armsFor(options: { renderers: Renderer[]; inputs: Input[]; streams: boolean[]; smooths: number[]; sens: number[]; guards: boolean[] }): Arm[] {
  const arms: Arm[] = [];
  for (const renderer of options.renderers) for (const input of options.inputs) for (const stream of options.streams)
    for (const smooth of options.smooths) for (const sens of options.sens) for (const guard of options.guards) if (smooth !== 0 || !guard) arms.push({ renderer, input, stream, smooth, sens, guard });
  return arms;
}

