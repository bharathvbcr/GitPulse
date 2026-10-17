import { createRequire } from "node:module";
import { afterEach, describe, expect, it } from "vitest";
import {
  ENDED_TAIL_CHARS,
  MAX_ENDED_TAILS,
  clipEndedTail,
  endedTail,
  endedTailCount,
  forgetEndedTail,
  releaseEndedScrollback,
  rememberEndedTail,
  resetEndedTails,
  shouldReleaseEndedBuffer,
} from "./endedBuffer";

const require = createRequire(import.meta.url);
const { Terminal } = require("@xterm/xterm") as {
  Terminal: new (options: { cols: number; rows: number; scrollback: number; allowProposedApi: boolean }) => {
    options: { scrollback?: number };
    rows: number;
    write(data: string, callback: () => void): void;
    resize(cols: number, rows: number): void;
    dispose(): void;
    _core: { _bufferService: { buffer: { ydisp: number; ybase: number; lines: { length: number; get(i: number): { _data: { byteLength: number } } | undefined } } } };
  };
};

afterEach(() => resetEndedTails());

describe("ended terminal scrollback", () => {
  it("releases only an ended session that is off screen", () => {
    expect(shouldReleaseEndedBuffer("exited", false)).toBe(true);
    expect(shouldReleaseEndedBuffer("exited", true)).toBe(false);
    expect(shouldReleaseEndedBuffer("running", false)).toBe(false);
    expect(shouldReleaseEndedBuffer("starting", false)).toBe(false);
    expect(shouldReleaseEndedBuffer("error", false)).toBe(false);
    expect(shouldReleaseEndedBuffer(undefined, false)).toBe(false);
  });

  it("keeps a bounded tail and forgets the oldest past the global cap", () => {
    expect(clipEndedTail("a".repeat(ENDED_TAIL_CHARS + 10))).toHaveLength(ENDED_TAIL_CHARS);
    expect(clipEndedTail("a".repeat(ENDED_TAIL_CHARS + 10)).startsWith("a")).toBe(true);
    expect(clipEndedTail("short")).toBe("short");
    rememberEndedTail("keep", "hello");
    rememberEndedTail("keep", "");
    expect(endedTail("keep")).toBe("hello");
    for (let i = 0; i < MAX_ENDED_TAILS + 5; i += 1) rememberEndedTail(`tab-${i}`, `tail-${i}`);
    expect(endedTailCount()).toBe(MAX_ENDED_TAILS);
    expect(endedTail("tab-0")).toBeUndefined();
    expect(endedTail(`tab-${MAX_ENDED_TAILS + 4}`)).toBe(`tail-${MAX_ENDED_TAILS + 4}`);
    forgetEndedTail(`tab-${MAX_ENDED_TAILS + 4}`);
    expect(endedTail(`tab-${MAX_ENDED_TAILS + 4}`)).toBeUndefined();
  });

  it("drops a filled scrollback's cells when the session has ended off screen", async () => {
    const term = new Terminal({ cols: 200, rows: 40, scrollback: 5000, allowProposedApi: true });
    try {
      const line = `${"x".repeat(196)}\r\n`;
      await new Promise<void>((resolve) => term.write(line.repeat(5200), () => resolve()));
      const bytes = () => {
        const lines = term._core._bufferService.buffer.lines;
        let total = 0;
        for (let i = 0; i < lines.length; i += 1) total += lines.get(i)?._data.byteLength ?? 0;
        return total;
      };
      const full = bytes();
      expect(full).toBeGreaterThan(8 * 1024 * 1024);
      // What the view used to do: leave the filled buffer in place.
      expect(shouldReleaseEndedBuffer("running", false)).toBe(false);
      expect(bytes()).toBe(full);
      expect(shouldReleaseEndedBuffer("exited", false)).toBe(true);
      releaseEndedScrollback(term, 5000);
      // The limit is back, and putting it back did not allocate the rows
      // again. An ended session receives no further output.
      expect(term.options.scrollback).toBe(5000);
      expect(bytes()).toBeLessThan(200 * 1024);
    } finally {
      term.dispose();
    }
  });

  // A hidden dock is still fitted, to a sliver. The sequence is the one the
  // WKWebView coverage harness recorded: released at 190x7 with the cursor
  // on row 3, fitted to 13x5 while hidden, then back to 180x7. With
  // scrollback left at 0 the narrowing reflow has no room above the viewport
  // and leaves ybase at 1 over a 5-line buffer; growing back then scrolls
  // into that phantom row instead of adding one, so 7 rows sit on 6 lines.
  // The DOM renderer skips a missing row; the WebGL renderer reads it
  // (`loadCell` of undefined).
  it("leaves a released terminal that is refitted with a buffer as tall as its screen", async () => {
    const term = new Terminal({ cols: 190, rows: 7, scrollback: 1000, allowProposedApi: true });
    try {
      const line = "Simulated agent session ready. No commands execute.\r\n";
      await new Promise<void>((resolve) => term.write(line.repeat(3), () => resolve()));
      releaseEndedScrollback(term, 1000);
      const buffer = () => term._core._bufferService.buffer;
      for (const [cols, rows] of [[13, 5], [180, 7], [190, 7], [11, 5], [178, 7]]) {
        term.resize(cols, rows);
        // `lines.get` wraps around instead of failing, so the bounds are
        // asserted on the indices, not on what `get` returns. 13x5 at 0 was
        // already broken (ybase 1 over 5 lines) with as many lines as rows.
        const at = `${cols}x${rows}`;
        expect(buffer().ybase + term.rows, at).toBeLessThanOrEqual(buffer().lines.length);
        expect(buffer().ydisp + term.rows, at).toBeLessThanOrEqual(buffer().lines.length);
      }
    } finally {
      term.dispose();
    }
  });
});
