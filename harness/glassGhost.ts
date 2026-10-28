/**
 * Measures whether xterm's WebGL renderer leaves stale pixels behind when the
 * terminal background is translucent (xterm.js #5847, Tauri / WKWebView).
 *
 * Two oracles, neither of which trusts the renderer being measured:
 *
 *  - Blank rows. After the workload the page erases a known set of rows. A row
 *    whose cells are all default-background spaces must read back exactly the
 *    pixel an EMPTY terminal of the same options reads back. Any other pixel in
 *    such a row is a stale one.
 *  - A single-frame twin. A second terminal is fed the identical bytes in one
 *    write and rendered once, so nothing in it can be left over from an earlier
 *    frame. Its pixels are compared with the streamed terminal's.
 *
 * The pixels are read with `readPixels` inside xterm's own `onRender`, in the
 * same task that drew them: the context is created without
 * `preserveDrawingBuffer`, so the browser may clear the buffer the moment the
 * task ends.
 */
import { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";

export interface Arm {
  name: string;
  background: string;
  allowTransparency: boolean;
}

export interface Measure {
  arm: string;
  seed: number;
  frames: number;
  cols: number;
  rows: number;
  dpr: number;
  /** Rows the workload left all-blank (default background, no text). */
  blankRows: number;
  /** Pixels in those rows that differ from an empty terminal's pixel. */
  blankPixels: number;
  /** How many of the blank rows hold at least one such pixel. */
  blankRowsDirty: number;
  /** Pixels that differ from the single-frame twin, and the rows they sit in. */
  twinPixels: number;
  twinRows: number;
  twinRowList: string;
  /** Twin against a second twin: how much the yardstick itself wobbles. */
  twinRepeatPixels: number;
  /** The streamed terminal against the twin after a full refresh, and after a fresh atlas. */
  afterRefreshPixels: number;
  afterAtlasPixels: number;
  /** The WebGL canvas was in use for the whole run. */
  webgl: boolean;
  /** Streamed and twin pictures of the first differing rows, as PNG data URLs (debug only). */
  strips?: string[];
  /** Lengths of the loaded Terminal and WebglAddon constructors' source: tells
   * two xterm builds apart by the code that ran, not by a version string. */
  sig: string;
  /** What a blank row reads in the single-frame twin. */
  cleanPixel: string;
  /** The most common pixels in the blank rows, for reading blankPixels. */
  blankTop: string;
}

const SCROLL_ROUNDS = 40;
const COLS = 100;
const ROWS = 32;
const FONT = "Menlo, 'DejaVu Sans Mono', monospace";

/** Deterministic: two runs with one seed are byte-identical. */
function rng(seed: number): () => number {
  let state = (seed * 2654435761) >>> 0 || 1;
  return () => {
    state ^= state << 13; state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5; state >>>= 0;
    return state / 0x100000000;
  };
}

const WORDS = ("the quick brown fox jumps over a lazy dog while streaming tokens into the terminal " +
  "diff hunk render frame cell glyph atlas texture row column buffer scroll alt screen redraw").split(" ");

/** Frames of a Claude-Code-like session: diff blocks with 48;2 backgrounds, Ink
 * boxes that are drawn with a 48;5 background and later erased and redrawn in
 * the default one, a spinner rewritten in place, prose in many foreground and
 * background colours, and the odd alt-screen round trip. */
export function workload(seed: number, frames: number, opsMax = 3, distinct = 0): string[] {
  const rand = rng(seed);
  const pick = <T,>(items: readonly T[]): T => items[Math.floor(rand() * items.length)];
  const int = (lo: number, hi: number) => lo + Math.floor(rand() * (hi - lo + 1));
  // `distinct` caps how many colours the stream draws from: a real session uses
  // a handful, a stress stream uses the whole space.
  const col = (lo: number, hi: number) => (distinct > 0 ? lo + Math.floor((int(0, distinct - 1) * (hi - lo)) / Math.max(1, distinct - 1)) : int(lo, hi));
  const words = (n: number) => Array.from({ length: n }, () => pick(WORDS)).join(" ");
  const out: string[] = [];
  let box = 0;
  for (let f = 0; f < frames; f++) {
    let s = "";
    const ops = int(1, opsMax);
    for (let o = 0; o < ops; o++) {
      const op = rand();
      if (op < 0.3) {
        // A diff hunk: red/green rows, tail painted by erase-to-EOL (BCE).
        const bg = pick(["60;20;25", "20;60;30", "90;30;35", "25;90;45"]);
        for (let i = int(3, 9); i > 0; i--) {
          s += `\x1b[48;2;${bg}m\x1b[38;2;${col(150, 255)};${col(150, 255)};${col(150, 255)}m${words(int(4, 12))}\x1b[K\x1b[0m\r\n`;
        }
      } else if (op < 0.5) {
        for (let i = int(1, 4); i > 0; i--) s += `\x1b[38;5;${col(16, 231)}m${words(int(6, 16))}\x1b[0m\r\n`;
      } else if (op < 0.7) {
        if (box === 0) {
          // Draw an Ink-style box, then (next time) erase it and redraw it plain.
          box = int(3, 6);
          for (let i = 0; i < box; i++) s += `\x1b[48;5;${col(232, 250)}m\x1b[38;5;${col(16, 231)}m ${words(int(3, 8))}\x1b[K\x1b[0m\r\n`;
        } else {
          s += `\x1b[${box}F`;
          for (let i = 0; i < box; i++) s += `\x1b[2K${i === box - 1 ? "" : "\r\n"}`;
          s += `\x1b[${box - 1}F`;
          for (let i = 0; i < box; i++) s += `${words(int(3, 8))}\r\n`;
          box = 0;
        }
      } else if (op < 0.85) {
        // Spinner: absolute row 1, rewritten in place with a fresh colour.
        s += `\x1b7\x1b[1;1H\x1b[48;5;${col(16, 231)}m\x1b[38;5;${col(16, 231)}m ${words(2)} ${f} \x1b[K\x1b[0m\x1b8`;
      } else if (op < 0.97) {
        // Prose with a colour pair per word: many distinct (glyph, fg, bg) triples.
        let line = "";
        for (let w = int(6, 14); w > 0; w--) {
          line += `\x1b[38;2;${col(0, 255)};${col(0, 255)};${col(0, 255)}m\x1b[48;2;${col(0, 63)};${col(0, 63)};${col(0, 63)}m${pick(WORDS)}\x1b[0m `;
        }
        s += `${line}\r\n`;
      } else {
        s += "\x1b[?1049h\x1b[2J";
        for (let i = 0; i < ROWS; i++) s += `\x1b[${i + 1};1H\x1b[48;5;${col(232, 255)}m ${words(4)}\x1b[K`;
        s += "\x1b[0m";
        out.push(s); s = "";
        s += "\x1b[?1049l";
      }
    }
    out.push(s);
  }
  // A tail that leaves a known shape: erased rows (must read blank), two plain
  // rows, two rows with a background. Cursor hidden so it paints nothing.
  let tail = "\x1b[0m";
  for (const r of [4, 5, 6, 12, 13, 20, 21, 22, 27, 28, 29]) tail += `\x1b[${r};1H\x1b[2K`;
  tail += `\x1b[8;1Hplain ${words(5)}\x1b[9;1Hplain ${words(5)}`;
  tail += `\x1b[15;1H\x1b[48;2;60;20;25m${words(5)}\x1b[K\x1b[0m\x1b[16;1H\x1b[48;2;20;60;30m${words(5)}\x1b[K\x1b[0m`;
  tail += "\x1b[?25l";
  out.push(tail);
  return out;
}

const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

interface Live {
  term: Terminal;
  host: HTMLElement;
  canvas: HTMLCanvasElement;
  gl: WebGL2RenderingContext;
  /** The pixels of the most recent render, read in the task that drew them. */
  latest: Uint8Array | null;
  capture: boolean;
  lost: boolean;
  addon: WebglAddon;
  dispose(): void;
}

function open(arm: Arm, label: string, webgl: boolean): Live {
  const host = document.createElement("div");
  host.dataset.glassGhost = label;
  host.style.cssText = "position:absolute;left:0;top:0;";
  document.body.appendChild(host);
  const term = new Terminal({
    cols: COLS, rows: ROWS, fontFamily: FONT, fontSize: 13, lineHeight: 1.15, scrollback: 2000,
    cursorBlink: false, allowTransparency: arm.allowTransparency, allowProposedApi: true,
    theme: { background: arm.background, foreground: "#e9edf8" },
  });
  term.open(host);
  const live = { term, host, latest: null, capture: false, lost: false } as unknown as Live;
  if (webgl) {
    const addon = new WebglAddon();
    term.loadAddon(addon);
    live.addon = addon;
    addon.onContextLoss(() => { live.lost = true; });
    live.canvas = host.querySelector<HTMLCanvasElement>("canvas:not([class])") as HTMLCanvasElement;
    live.gl = live.canvas.getContext("webgl2") as WebGL2RenderingContext;
    term.onRender(() => {
      if (!live.capture) return;
      const { width, height } = live.canvas;
      const px = new Uint8Array(width * height * 4);
      live.gl.readPixels(0, 0, width, height, live.gl.RGBA, live.gl.UNSIGNED_BYTE, px);
      live.latest = px;
    });
  }
  live.dispose = () => { term.dispose(); host.remove(); };
  return live;
}

const write = (live: Live, data: string) => new Promise<void>((resolve) => live.term.write(data, resolve));

/** Let xterm's debounced render land, then read it. The last render after the
 * last write is the picture of the final buffer. */
async function settle(live: Live, force = true): Promise<Uint8Array> {
  if (force) {
    live.capture = true;
    live.latest = null;
    // A forced refresh redraws every row, so it is only for terminals whose
    // history does not matter: the streamed one must keep what its stream drew.
    live.term.refresh(0, ROWS - 1);
  }
  for (let i = 0; i < 4; i++) await frame();
  await sleep(30);
  if (!live.latest) throw new Error("xterm rendered nothing after the last write");
  return live.latest;
}

/** Pixels flipped to top-down rows: `readPixels` starts at the bottom. */
function rowOf(y: number, height: number, rows: number): number {
  return rows - 1 - Math.min(rows - 1, Math.floor(y / (height / rows)));
}

function hex(px: Uint8Array, at: number): string {
  return `${px[at]},${px[at + 1]},${px[at + 2]},${px[at + 3]}`;
}

/** One terminal row of a read-back picture as a PNG over a blue ground, for a
 * person looking at what a differing row holds. Debugging only. */
function strip(px: Uint8Array, width: number, height: number, row: number): string {
  const cellH = Math.round(height / ROWS);
  const layer = document.createElement("canvas");
  layer.width = width;
  layer.height = cellH;
  const image = new ImageData(width, cellH);
  // `readPixels` is bottom-up: top-down row `row` starts (ROWS - 1 - row) bands up.
  const y0 = (ROWS - 1 - row) * cellH;
  for (let y = 0; y < cellH; y++) {
    const from = (y0 + (cellH - 1 - y)) * width * 4;
    image.data.set(px.subarray(from, from + width * 4), y * width * 4);
  }
  (layer.getContext("2d") as CanvasRenderingContext2D).putImageData(image, 0, 0);
  const out = document.createElement("canvas");
  out.width = width;
  out.height = cellH;
  const ctx = out.getContext("2d") as CanvasRenderingContext2D;
  ctx.fillStyle = "#3a6ea5";
  ctx.fillRect(0, 0, width, cellH);
  ctx.drawImage(layer, 0, 0);
  return out.toDataURL("image/png");
}

export async function measure(arm: Arm, seed: number, frames: number, pace = 1, opsMax = 3, debug = false, distinct = 0, clearEvery = 0, scrollBack = false): Promise<Measure> {
  const script = workload(seed, frames, opsMax, distinct);

  const streamed = open(arm, "streamed", true);
  await settle(streamed);
  streamed.capture = false;
  for (let i = 0; i < script.length; i++) {
    if (i === script.length - 1) { streamed.capture = true; streamed.latest = null; }
    await write(streamed, script[i]);
    // Phased by half a period so the last clear is not at the very end of the
    // stream: a terminal cleared one chunk before the picture is trivially clean.
    if (clearEvery > 0 && (i + 1 + Math.floor(clearEvery / 2)) % clearEvery === 0) streamed.addon.clearTextureAtlas();
    // Several frames can land in one render, as a busy PTY's do.
    if ((i + 1) % pace === 0) await frame();
  }
  if (scrollBack) {
    // Read the history back and forth, as xterm.js #5847's later comments do.
    streamed.capture = true;
    streamed.term.scrollToTop();
    await frame();
    for (let k = 0; k < SCROLL_ROUNDS; k++) {
      streamed.term.scrollLines(13); await frame();
      streamed.term.scrollLines(-5); await frame();
    }
  }
  const streamedPx = await settle(streamed, false);

  // One write, one render. Twice, so the yardstick is itself checked: two
  // single-frame renders of one stream must agree pixel for pixel.
  const oneShot = async (label: string) => {
    const twin = open(arm, label, true);
    await settle(twin);
    twin.capture = false;
    await write(twin, script.join(""));
    // The streamed terminal's viewport ends 8 lines per round below the top.
    if (scrollBack) { twin.term.scrollToTop(); twin.term.scrollLines(SCROLL_ROUNDS * 8); }
    const px = await settle(twin);
    const size = { width: twin.canvas.width, height: twin.canvas.height };
    twin.dispose();
    return { px, ...size };
  };
  const { px: twinPx, width, height } = await oneShot("twin");
  const { px: twin2Px } = await oneShot("twin-repeat");

  // Heal tests on the streamed terminal, after its own picture is taken: does a
  // full row refresh bring it to the twin, and does a fresh texture atlas?
  const afterRefresh = await settle(streamed, true);
  streamed.addon.clearTextureAtlas();
  const afterAtlas = await settle(streamed, true);

  // Blank rows, from the buffer rather than from the pixels.
  const buf = streamed.term.buffer.active;
  const cell = buf.getNullCell();
  const blank: boolean[] = [];
  for (let y = 0; y < ROWS; y++) {
    const line = buf.getLine(buf.viewportY + y);
    let isBlank = !!line && line.translateToString(true) === "";
    for (let x = 0; isBlank && x < COLS; x++) {
      line!.getCell(x, cell);
      if (!cell.isBgDefault()) isBlank = false;
    }
    blank.push(isBlank);
  }

  // What a blank row looks like when nothing could have been left over: the
  // commonest pixel in the twin's blank rows (an erased row is not painted the
  // way a never-written one is, so an empty terminal is the wrong yardstick).
  const stride = width * 4;
  const clean = new Map<string, number>();
  for (let y = 0; y < height; y++) {
    if (!blank[rowOf(y, height, ROWS)]) continue;
    for (let x = 0; x < width; x++) {
      const key = hex(twinPx, y * stride + x * 4);
      clean.set(key, (clean.get(key) ?? 0) + 1);
    }
  }
  const reference = [...clean].sort((a, b) => b[1] - a[1])[0]?.[0] ?? "no blank rows";
  const histogram = new Map<string, number>();
  let blankPixels = 0;
  const dirtyRows = new Set<number>();
  for (let y = 0; y < height; y++) {
    const row = rowOf(y, height, ROWS);
    for (let x = 0; x < width; x++) {
      const at = y * stride + x * 4;
      if (blank[row] && hex(streamedPx, at) !== reference) {
        blankPixels++; dirtyRows.add(row);
        const key = hex(streamedPx, at);
        histogram.set(key, (histogram.get(key) ?? 0) + 1);
      }
    }
  }
  const differ = (a: Uint8Array, b: Uint8Array) => {
    const rows = new Set<number>();
    let pixels = 0;
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const at = y * stride + x * 4;
        if (a[at] !== b[at] || a[at + 1] !== b[at + 1] || a[at + 2] !== b[at + 2] || a[at + 3] !== b[at + 3]) {
          pixels++; rows.add(rowOf(y, height, ROWS));
        }
      }
    }
    return { pixels, rows: [...rows].sort((p, q) => p - q) };
  };
  const vsTwin = differ(streamedPx, twinPx);
  const twinRepeat = differ(twinPx, twin2Px);
  const healedRefresh = differ(afterRefresh, twinPx);
  const healedAtlas = differ(afterAtlas, twinPx);
  const result: Measure = {
    arm: arm.name, seed, frames, cols: COLS, rows: ROWS, dpr: devicePixelRatio,
    blankRows: blank.filter(Boolean).length, blankPixels, blankRowsDirty: dirtyRows.size,
    twinPixels: vsTwin.pixels, twinRows: vsTwin.rows.length, twinRowList: vsTwin.rows.join(","),
    twinRepeatPixels: twinRepeat.pixels, afterRefreshPixels: healedRefresh.pixels, afterAtlasPixels: healedAtlas.pixels,
    webgl: !streamed.lost && !!streamed.canvas?.isConnected,
    cleanPixel: reference,
    sig: `${Terminal.toString().length}/${WebglAddon.toString().length}`,
    blankTop: [...histogram].sort((a, b) => b[1] - a[1]).slice(0, 3).map(([k, v]) => `${k}x${v}`).join(" "),
  };
  if (debug && vsTwin.rows.length) {
    result.strips = vsTwin.rows.slice(0, 3).flatMap((row) => [strip(streamedPx, width, height, row), strip(twinPx, width, height, row)]);
  }
  streamed.dispose();
  return result;
}
