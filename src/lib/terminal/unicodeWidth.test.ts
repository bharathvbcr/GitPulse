import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";
import type { Terminal as XTerm } from "@xterm/xterm";
import type { Unicode11Addon as Unicode11AddonType } from "@xterm/addon-unicode11";

const require = createRequire(import.meta.url);
const { Terminal } = require("@xterm/xterm") as { Terminal: new (options: { cols: number; rows: number; allowProposedApi: boolean }) => XTerm };
const { Unicode11Addon } = require("@xterm/addon-unicode11") as { Unicode11Addon: new () => Unicode11AddonType };

/**
 * The widths TerminalSession gets by loading the Unicode 11 addon, measured on
 * the real emulator and the pinned pair. `harness/terminal.html` asserts the
 * same through the session itself; this is the part that runs without a
 * browser, and the one that notices when a bump moves xterm's default table.
 */
async function cellsAfter(text: string, unicode11: boolean): Promise<{ cursorX: number; widths: number[] }> {
  const term = new Terminal({ cols: 40, rows: 4, allowProposedApi: true });
  if (unicode11) {
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
  }
  await new Promise<void>((resolve) => term.write(text, resolve));
  const line = term.buffer.active.getLine(0);
  const widths: number[] = [];
  for (let x = 0; x < term.buffer.active.cursorX; x++) widths.push(line?.getCell(x)?.getWidth() ?? -1);
  const cursorX = term.buffer.active.cursorX;
  term.dispose();
  return { cursorX, widths };
}

describe("terminal cell widths", () => {
  it("is Unicode 6 by default: an emoji takes one cell, which is the bug", async () => {
    expect((await cellsAfter("🚀", false)).cursorX).toBe(1);
    expect((await cellsAfter("a🚀b", false)).cursorX).toBe(3);
  });

  it("gives an emoji two cells once the Unicode 11 addon is active", async () => {
    expect(await cellsAfter("🚀", true)).toEqual({ cursorX: 2, widths: [2, 0] });
    expect(await cellsAfter("a🚀b", true)).toEqual({ cursorX: 4, widths: [1, 2, 0, 1] });
    expect((await cellsAfter("🚀🔥✅", true)).cursorX).toBe(6);
  });

  it("leaves CJK at two cells and plain text at one", async () => {
    for (const unicode11 of [false, true]) {
      expect(await cellsAfter("世界", unicode11), `unicode11=${unicode11}`).toEqual({ cursorX: 4, widths: [2, 0, 2, 0] });
      expect((await cellsAfter("𠀀", unicode11)).cursorX, `astral CJK, unicode11=${unicode11}`).toBe(2);
      expect((await cellsAfter("café", unicode11)).cursorX).toBe(4);
    }
  });

  it("keeps a VS16 selector out of the width of the character it follows", async () => {
    for (const unicode11 of [false, true]) {
      const { cursorX, widths } = await cellsAfter("❤️x", unicode11);
      // The selector is zero width whichever table is active. This pins the
      // measured behaviour, not a claim that the heart is drawn two cells wide.
      expect(widths.at(-1), `unicode11=${unicode11}`).toBe(1);
      expect(cursorX, `unicode11=${unicode11}`).toBe(widths.length);
    }
  });
});
