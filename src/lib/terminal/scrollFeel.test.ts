import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { SMOOTH_SCROLL_MS, attachScrollGuard, isScrolledBack, scrollFeelOptions, shouldLeaveBottom } from "./scrollFeel";

const require = createRequire(import.meta.url);
const { Terminal } = require("@xterm/xterm") as {
  Terminal: new (options: { cols: number; rows: number; scrollback: number; allowProposedApi: boolean }) => XtermLike;
};

interface XtermLike {
  buffer: { active: { viewportY: number; baseY: number; type: "normal" | "alternate" } };
  modes: { mouseTrackingMode: string };
  write(data: string, callback: () => void): void;
  scrollLines(amount: number): void;
  scrollToBottom(): void;
  input(data: string, wasUserInput?: boolean): void;
  dispose(): void;
}

const lines = (from: number, count: number) => Array.from({ length: count }, (_, i) => `line ${from + i}\r\n`).join("");
const write = (term: XtermLike, data: string) => new Promise<void>((resolve) => term.write(data, resolve));
const here = dirname(fileURLToPath(import.meta.url));
const session = readFileSync(join(here, "..", "components", "TerminalSession.svelte"), "utf8");

describe("isScrolledBack", () => {
  it("is true only while the view is above the newest output", () => {
    expect(isScrolledBack({ viewportY: 90, baseY: 91 })).toBe(true);
    expect(isScrolledBack({ viewportY: 91, baseY: 91 })).toBe(false);
    expect(isScrolledBack({ viewportY: 0, baseY: 0 })).toBe(false);
  });
});

describe("a scrolled-back view while output streams (real xterm buffer)", () => {
  it("is not pulled to the bottom by output, and the Latest-output test keeps saying scrolled back", async () => {
    const term = new Terminal({ cols: 40, rows: 10, scrollback: 500, allowProposedApi: true });
    try {
      await write(term, lines(0, 100));
      expect(isScrolledBack(term.buffer.active)).toBe(false);
      term.scrollLines(-20);
      const parked = term.buffer.active.viewportY;
      for (let burst = 0; burst < 10; burst += 1) await write(term, lines(100 + burst * 5, 5));
      expect(term.buffer.active.viewportY).toBe(parked);
      expect(term.buffer.active.baseY).toBeGreaterThan(parked);
      expect(isScrolledBack(term.buffer.active)).toBe(true);
      // Latest output: scrollToBottom clears it.
      term.scrollToBottom();
      expect(isScrolledBack(term.buffer.active)).toBe(false);
    } finally {
      term.dispose();
    }
  });

  it("keeps scrollOnUserInput: typing returns a scrolled-back view to the bottom", async () => {
    const term = new Terminal({ cols: 40, rows: 10, scrollback: 500, allowProposedApi: true });
    try {
      await write(term, lines(0, 100));
      term.scrollLines(-20);
      expect(isScrolledBack(term.buffer.active)).toBe(true);
      term.input("x", true);
      expect(isScrolledBack(term.buffer.active)).toBe(false);
    } finally {
      term.dispose();
    }
    expect(session).not.toMatch(/scrollOnUserInput\s*:\s*false/);
  });
});

describe("the wheel guard", () => {
  const terminal = (over: Partial<{ viewportY: number; baseY: number; type: "normal" | "alternate"; mouse: string }> = {}) => ({
    buffer: { active: { viewportY: over.viewportY ?? 100, baseY: over.baseY ?? 100, type: over.type ?? "normal" } },
    modes: { mouseTrackingMode: over.mouse ?? "none" },
  });

  it("leaves the bottom for a wheel toward history, and only then", () => {
    expect(shouldLeaveBottom(-100, terminal())).toBe(true);
    expect(shouldLeaveBottom(100, terminal())).toBe(false);
    expect(shouldLeaveBottom(-100, terminal({ viewportY: 40 }))).toBe(false);
    expect(shouldLeaveBottom(-100, terminal({ baseY: 0, viewportY: 0 }))).toBe(false);
  });

  it("leaves the wheel to a program that is reading it, and to the alternate screen", () => {
    expect(shouldLeaveBottom(-100, terminal({ mouse: "vt200" }))).toBe(false);
    expect(shouldLeaveBottom(-100, terminal({ type: "alternate" }))).toBe(false);
  });

  it("scrolls one row up on the first upward wheel event, in the capture phase, and detaches", () => {
    const listeners: Array<{ type: string; fn: (event: WheelEvent) => void; options: AddEventListenerOptions }> = [];
    const element = {
      addEventListener: (type: string, fn: (event: WheelEvent) => void, options: AddEventListenerOptions) => listeners.push({ type, fn, options }),
      removeEventListener: (type: string, fn: (event: WheelEvent) => void) => {
        const at = listeners.findIndex((entry) => entry.type === type && entry.fn === fn);
        if (at >= 0) listeners.splice(at, 1);
      },
    } as unknown as HTMLElement;
    const moved: Array<[number, number | undefined]> = [];
    const options = { smoothScrollDuration: 150 as number | undefined };
    const detach = attachScrollGuard({ ...terminal(), element, options, scrollLines: (amount) => moved.push([amount, options.smoothScrollDuration]) });
    expect(listeners).toHaveLength(1);
    expect(listeners[0].options).toMatchObject({ capture: true, passive: true });
    listeners[0].fn({ deltaY: -100 } as WheelEvent);
    listeners[0].fn({ deltaY: 100 } as WheelEvent);
    // One row, delivered with smoothing off for that call only.
    expect(moved).toEqual([[-1, 0]]);
    expect(options.smoothScrollDuration).toBe(150);
    detach();
    expect(listeners).toHaveLength(0);
  });

  it("does nothing for a terminal that is not open", () => {
    const detach = attachScrollGuard({ ...terminal(), options: {}, scrollLines: () => { throw new Error("must not scroll"); } });
    expect(() => detach()).not.toThrow();
  });
});

describe("TerminalSession wiring", () => {
  it("builds the terminal with the scroll-feel options and uses the shared scrolled-back test", () => {
    expect(session).toContain("...scrollFeelOptions(get(interfaceStore).terminalSmoothWheel)");
    expect(session).toContain("scrolledBack = isScrolledBack(created.buffer.active)");
    expect(session).toContain("createScrollFeel(t,");
    expect(session).toContain("get(interfaceStore).terminalSmoothWheel");
    expect(session).toContain("scrollFeel?.refresh()");
  });

  it("sets a smooth-scroll duration xterm will animate", () => {
    expect(scrollFeelOptions()).toEqual({ smoothScrollDuration: SMOOTH_SCROLL_MS });
    expect(SMOOTH_SCROLL_MS).toBeGreaterThan(0);
  });
});
