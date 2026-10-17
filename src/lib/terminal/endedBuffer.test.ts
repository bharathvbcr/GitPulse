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
    write(data: string, callback: () => void): void;
    dispose(): void;
    _core: { _bufferService: { buffer: { lines: { length: number; get(i: number): { _data: { byteLength: number } } | undefined } } } };
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
      releaseEndedScrollback(term);
      expect(term.options.scrollback).toBe(0);
      expect(bytes()).toBeLessThan(200 * 1024);
      // Raising the option, as the live scrollback setting does, must not
      // allocate the rows back. An ended session receives no further output.
      term.options.scrollback = 5000;
      expect(bytes()).toBeLessThan(200 * 1024);
    } finally {
      term.dispose();
    }
  });
});
