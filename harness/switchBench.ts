/**
 * Measures what the WebGL hold buys on a show (`gpuRenderer.ts`): the time
 * from a hidden terminal becoming visible to its first draw, with the context
 * kept through the hide ("held") against one released on hide and created
 * again on show ("fresh"), and what one held context occupies.
 *
 * It drives the real `createGpuRenderer` with the attach `TerminalSession`
 * uses, on one terminal, alternating the arms A B B A so neither one always
 * runs warm. The fresh arm is the hold set to 0 ms, which is exactly the
 * build before the hold. With one terminal on the page, a released context
 * also drops xterm's shared glyph atlas, so the fresh arm rasterizes every
 * glyph again: the case a tab switch hits when no other session draws the
 * same font. A page with other sessions holding the atlas would pay less.
 */
import { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import "@xterm/xterm/css/xterm.css";
import { createGpuRenderer, createGpuSlots, GPU_HIDE_GRACE_MS, gpuPolicy, type GpuPolicy } from "../src/lib/terminal/gpuRenderer";

export type Arm = "held" | "fresh";

export interface Trial {
  kind: "trial";
  arm: Arm;
  index: number;
  /** The synchronous part of the show: `sync(true)`, which creates the context in the fresh arm. */
  syncMs: number;
  /** Show to the end of the first WebGL draw call. */
  firstDrawMs: number;
  /** Show to the animation frame after that draw: the frame the person sees. */
  frameMs: number;
  /** The renderer state while hidden, which is what tells the arms apart. */
  hiddenState: string;
  /** The same canvas element came back (held) or a new one (fresh). */
  sameCanvas: boolean;
}

export interface Footprint {
  kind: "memory";
  dpr: number;
  cols: number;
  rows: number;
  /** The canvas's drawing buffer, bytes at 4 per pixel; the browser keeps at least one such buffer per context. */
  drawingBufferBytes: number;
  atlasPages: number;
  atlasPageSizes: string;
  /** The atlas pages, bytes at 4 per pixel: the textures the context uploads them to. */
  atlasBytes: number;
  maxTextureSize: number;
  /** The most pages the addon will create on this device (`min(32, MAX_TEXTURE_IMAGE_UNITS)`). */
  maxAtlasPages: number;
}

export interface Painted {
  kind: "painted";
  arm: Arm;
  painted: boolean;
}

export type Row = Trial | Footprint | Painted;

const COLS = 120;
const ROWS = 40;

const frame = () => new Promise<number>((resolve) => requestAnimationFrame(resolve));
const frames = async (count: number) => { for (let i = 0; i < count; i += 1) await frame(); };
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** Deterministic: two runs are byte-identical. */
function rng(seed: number): () => number {
  let state = (seed * 2654435761) >>> 0 || 1;
  return () => {
    state ^= state << 13; state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5; state >>>= 0;
    return state / 0x100000000;
  };
}

/**
 * A screen shaped like an agent CLI's: a box-drawn frame, prose in a few
 * styles, a diff with truecolor backgrounds, a spinner line and an emoji. The
 * glyph and colour mix is what decides how much atlas a fresh context rebuilds.
 */
export function workload(seed = 7): string {
  const next = rng(seed);
  const words = "the terminal keeps its context through a hide so switching back reuses the glyph atlas and draws at once while a fresh one compiles shaders and rasterizes every glyph again".split(" ");
  const line = (width: number) => {
    let text = "";
    while (text.length < width - 10) text += `${words[Math.floor(next() * words.length)]} `;
    return text.slice(0, width - 4);
  };
  const out: string[] = ["\x1b[2J\x1b[H"];
  out.push(`\x1b[38;2;215;119;87m╭${"─".repeat(COLS - 2)}╮\x1b[0m\r\n`);
  out.push(`\x1b[38;2;215;119;87m│\x1b[0m \x1b[1m✻ Welcome\x1b[22m ${" ".repeat(COLS - 14)}\x1b[38;2;215;119;87m│\x1b[0m\r\n`);
  out.push(`\x1b[38;2;215;119;87m╰${"─".repeat(COLS - 2)}╯\x1b[0m\r\n`);
  for (let row = 0; row < ROWS - 12; row += 1) {
    const style = row % 6;
    if (style === 0) out.push(`\x1b[1m● ${line(COLS)}\x1b[0m\r\n`);
    else if (style === 1) out.push(`\x1b[48;2;34;92;43m\x1b[38;2;220;255;220m+ ${line(COLS)}\x1b[0m\r\n`);
    else if (style === 2) out.push(`\x1b[48;2;110;32;40m\x1b[38;2;255;220;220m- ${line(COLS)}\x1b[0m\r\n`);
    else if (style === 3) out.push(`\x1b[2m  ⎿ ${line(COLS)}\x1b[0m\r\n`);
    else if (style === 4) out.push(`\x1b[38;5;${16 + Math.floor(next() * 216)}m${line(COLS)}\x1b[0m\r\n`);
    else out.push(`\x1b[3m${line(COLS)}\x1b[23m 🚀\r\n`);
  }
  out.push("\x1b[38;2;215;119;87m✶ Thinking…\x1b[0m \x1b[2m(esc to interrupt)\x1b[0m\r\n");
  out.push(`\x1b[2m${"─".repeat(COLS)}\x1b[0m\r\n> `);
  return out.join("");
}

/** `value[key]` when `value` is an object, else undefined. */
const field = (value: unknown, key: string): unknown =>
  typeof value === "object" && value !== null ? Reflect.get(value, key) : undefined;

function atlasPages(addon: WebglAddon | null): HTMLCanvasElement[] {
  // Private: the addon exposes only the first page. Read field by field, not
  // through a cast that trusts the shape, so a build that moves them reports 0.
  const pages = field(field(field(addon, "_renderer"), "_charAtlas"), "pages");
  if (!Array.isArray(pages)) return [];
  return pages.map((page) => field(page, "canvas")).filter((canvas) => canvas instanceof HTMLCanvasElement);
}

export async function run(host: HTMLElement, trialsPerArm: number): Promise<Row[]> {
  const rows: Row[] = [];
  const term = new Terminal({
    cols: COLS,
    rows: ROWS,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace",
    fontSize: 13,
    lineHeight: 1.15,
    allowProposedApi: true,
    allowTransparency: true,
    theme: { background: "#141a29", foreground: "#e9edf8" },
  });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  term.open(host);

  const policy: GpuPolicy = { ...gpuPolicy, hideGraceMs: GPU_HIDE_GRACE_MS };
  let addon: WebglAddon | null = null;
  let canvas: HTMLCanvasElement | undefined;
  const gpu = createGpuRenderer({
    // As TerminalSession.svelte attaches.
    attach: () => {
      const created = new WebglAddon();
      term.loadAddon(created);
      addon = created;
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
    policy,
    refresh: () => term.refresh(0, term.rows - 1),
  });

  // The first WebGL draw after a show. `drawElementsInstanced` is the call the
  // addon's glyph and rectangle renderers both end in.
  const probe = { armed: false, at: 0, readback: false, painted: false };
  const draw = WebGL2RenderingContext.prototype.drawElementsInstanced;
  WebGL2RenderingContext.prototype.drawElementsInstanced = function (...args) {
    const result = draw.apply(this, args);
    if (probe.armed && probe.at === 0) {
      probe.at = performance.now();
      if (probe.readback) {
        const { drawingBufferWidth: w, drawingBufferHeight: h } = this;
        const pixels = new Uint8Array(w * h * 4);
        this.readPixels(0, 0, w, h, this.RGBA, this.UNSIGNED_BYTE, pixels);
        for (let at = 3; at < pixels.length && !probe.painted; at += 4) if (pixels[at] !== 0) probe.painted = true;
      }
    }
    return result;
  };

  try {
    await new Promise<void>((resolve) => term.write(workload(), () => resolve()));
    if (gpu.sync(true) !== "webgl") throw new Error(`no WebGL context at start (${gpu.state()})`);
    await frames(10);

    const hide = async (arm: Arm) => {
      policy.hideGraceMs = arm === "held" ? GPU_HIDE_GRACE_MS : 0;
      host.style.display = "none";
      gpu.sync(false);
      // Lets a 0 ms hold expire, and the page settle.
      await sleep(30);
      await frames(3);
      return gpu.state();
    };
    const show = async (readback: boolean) => {
      const before = canvas;
      probe.armed = true; probe.at = 0; probe.readback = readback; probe.painted = false;
      const start = performance.now();
      host.style.display = "";
      gpu.sync(true);
      const syncMs = performance.now() - start;
      const deadline = start + 2000;
      while (probe.at === 0 && performance.now() < deadline) await frame();
      const firstDrawMs = probe.at ? probe.at - start : Number.NaN;
      await frame();
      const frameMs = performance.now() - start;
      probe.armed = false;
      return { syncMs, firstDrawMs, frameMs, painted: probe.painted, sameCanvas: canvas === before };
    };

    // One untimed round per arm with a pixel read: the first frame is not blank.
    for (const arm of ["held", "fresh"] as const) {
      await hide(arm);
      const { painted } = await show(true);
      rows.push({ kind: "painted", arm, painted });
      await frames(5);
    }

    const order: Arm[] = [];
    for (let pair = 0; pair < trialsPerArm; pair += 1) order.push(...(pair % 2 === 0 ? ["held", "fresh"] as const : ["fresh", "held"] as const));
    for (const [index, arm] of order.entries()) {
      const hiddenState = await hide(arm);
      const { syncMs, firstDrawMs, frameMs, sameCanvas } = await show(false);
      rows.push({ kind: "trial", arm, index, syncMs, firstDrawMs, frameMs, hiddenState, sameCanvas });
      await sleep(50);
      await frames(5);
    }

    const gl = canvas?.getContext("webgl2") ?? null;
    const pages = atlasPages(addon);
    rows.push({
      kind: "memory",
      dpr: devicePixelRatio,
      cols: term.cols,
      rows: term.rows,
      drawingBufferBytes: gl ? gl.drawingBufferWidth * gl.drawingBufferHeight * 4 : 0,
      atlasPages: pages.length,
      atlasPageSizes: pages.map((page) => `${page.width}x${page.height}`).join(","),
      atlasBytes: pages.reduce((sum, page) => sum + page.width * page.height * 4, 0),
      maxTextureSize: gl ? gl.getParameter(gl.MAX_TEXTURE_SIZE) : 0,
      maxAtlasPages: gl ? Math.min(32, gl.getParameter(gl.MAX_TEXTURE_IMAGE_UNITS)) : 0,
    });
  } finally {
    WebGL2RenderingContext.prototype.drawElementsInstanced = draw;
    gpu.dispose();
    term.dispose();
  }
  return rows;
}
