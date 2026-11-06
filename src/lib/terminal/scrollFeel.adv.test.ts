// Adversarial checks for the wheel guard, run against a real xterm buffer.
// Tests marked KNOWN-GAP encode the behaviour the review says is missing and fail on the current code.
import { createRequire } from "node:module";
import { afterEach, describe, expect, it, vi } from "vitest";
import { attachScrollGuard, scrollFeelOptions, type ScrollGuardTerminal } from "./scrollFeel";

const require = createRequire(import.meta.url);
const { Terminal } = require("@xterm/xterm") as { Terminal: new (options: Record<string, unknown>) => XtermLike };

interface XtermLike extends Omit<ScrollGuardTerminal, "element"> {
  write(data: string, callback: () => void): void;
  dispose(): void;
  _core: { optionsService: { onOptionChange(listener: (key: string) => void): { dispose(): void } } };
}

const lines = (count: number) => Array.from({ length: count }, (_, i) => `line ${i}\r\n`).join("");
const write = (term: XtermLike, data: string) => new Promise<void>((resolve) => term.write(data, resolve));

/** The guard's view of a real terminal, with a fake element standing in for the DOM node xterm has not opened. */
const asGuarded = (term: XtermLike, element: HTMLElement): ScrollGuardTerminal => ({
  element,
  options: term.options,
  buffer: term.buffer,
  get modes() { return term.modes; },
  scrollLines: (amount) => term.scrollLines(amount),
});

function wheel(init: { deltaY: number; deltaX?: number; ctrlKey?: boolean; metaKey?: boolean }): WheelEvent {
  return Object.assign(new Event("wheel"), { deltaX: 0, ctrlKey: false, metaKey: false, ...init }) as unknown as WheelEvent;
}

async function guarded(scrollback = 500, rows = 10) {
  const term = new Terminal({ cols: 40, rows, scrollback, allowProposedApi: true, smoothScrollDuration: 150 });
  const element = new EventTarget() as unknown as HTMLElement;
  await write(term, lines(100));
  const detach = attachScrollGuard(asGuarded(term, element));
  const send = (event: WheelEvent) => (element as unknown as EventTarget).dispatchEvent(event);
  return { term, send, detach, element };
}

afterEach(() => vi.unstubAllGlobals());

describe("guard edge cases (real xterm)", () => {
  it("ignores horizontal-only and downward wheels at the bottom", async () => {
    const { term, send } = await guarded();
    const y = term.buffer.active.viewportY;
    send(wheel({ deltaX: -80, deltaY: 0 }));
    send(wheel({ deltaY: 100 }));
    expect(term.buffer.active.viewportY).toBe(y);
    term.dispose();
  });

  it("moves exactly one row for an upward wheel at the bottom, then stands down", async () => {
    const { term, send } = await guarded();
    const y = term.buffer.active.viewportY;
    send(wheel({ deltaY: -100 }));
    send(wheel({ deltaY: -100 }));
    expect(term.buffer.active.viewportY).toBe(y - 1);
    expect(term.options.smoothScrollDuration).toBe(150);
    term.dispose();
  });

  it("does nothing with scrollback 0 or an empty buffer", async () => {
    const none = await guarded(0);
    none.send(wheel({ deltaY: -100 }));
    expect(none.term.buffer.active.viewportY).toBe(none.term.buffer.active.baseY);
    none.term.dispose();
    const fresh = new Terminal({ cols: 40, rows: 10, scrollback: 100, allowProposedApi: true });
    const element = new EventTarget() as unknown as HTMLElement;
    attachScrollGuard(asGuarded(fresh, element));
    element.dispatchEvent(wheel({ deltaY: -100 }));
    expect(fresh.buffer.active.baseY).toBe(0);
    fresh.dispose();
  });

  it("leaves the wheel alone on the alternate screen and under mouse tracking", async () => {
    const alt = await guarded();
    await write(alt.term, "\x1b[?1049h" + lines(30));
    expect(alt.term.buffer.active.type).toBe("alternate");
    const altY = alt.term.buffer.active.viewportY;
    alt.send(wheel({ deltaY: -100 }));
    expect(alt.term.buffer.active.viewportY).toBe(altY);
    alt.term.dispose();

    const mouse = await guarded();
    await write(mouse.term, "\x1b[?1000h");
    expect(mouse.term.modes.mouseTrackingMode).not.toBe("none");
    const y = mouse.term.buffer.active.viewportY;
    mouse.send(wheel({ deltaY: -100 }));
    expect(mouse.term.buffer.active.viewportY).toBe(y);
    mouse.term.dispose();
  });

  it("a wheel event after dispose does not throw", async () => {
    const { term, send } = await guarded();
    term.dispose();
    expect(() => send(wheel({ deltaY: -100 }))).not.toThrow();
  });

  it("detach stops the guard, and attaching twice would double-fire (no dedupe: callers must attach once)", async () => {
    const { term, send, detach, element } = await guarded();
    detach();
    const y = term.buffer.active.viewportY;
    send(wheel({ deltaY: -100 }));
    expect(term.buffer.active.viewportY).toBe(y);
    attachScrollGuard(asGuarded(term, element));
    attachScrollGuard(asGuarded(term, element));
    send(wheel({ deltaY: -100 }));
    // The second listener sees "scrolled back" already, so a double attach still moves one row.
    expect(term.buffer.active.viewportY).toBe(y - 1);
    term.dispose();
  });

  it("restores the duration when scrollLines throws", async () => {
    const options: { smoothScrollDuration?: number } = { smoothScrollDuration: 150 };
    const element = new EventTarget() as unknown as HTMLElement;
    attachScrollGuard({
      element, options, modes: { mouseTrackingMode: "none" }, buffer: { active: { viewportY: 5, baseY: 5, type: "normal" } },
      scrollLines: () => { throw new Error("boom"); },
    });
    // dispatchEvent reports listener exceptions asynchronously; call through a listener spy instead.
    const seen: unknown[] = [];
    process.once("uncaughtException", (error) => seen.push(error));
    element.dispatchEvent(wheel({ deltaY: -100 }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(options.smoothScrollDuration).toBe(150);
  });

  it("KNOWN-GAP: sub-pixel trackpad noise (deltaY -0.1) must not jump a whole row", async () => {
    const { term, send } = await guarded();
    const y = term.buffer.active.viewportY;
    send(wheel({ deltaY: -0.1 }));
    expect(term.buffer.active.viewportY).toBe(y);
    term.dispose();
  });

  it("KNOWN-GAP: the guard must not fire renderer-wide option-change refreshes (each write refreshes every row; DomRenderer also rewrites its CSS)", async () => {
    const { term, send } = await guarded();
    const fired: string[] = [];
    const sub = term._core.optionsService.onOptionChange((key) => fired.push(key));
    send(wheel({ deltaY: -100 }));
    sub.dispose();
    expect(fired).toEqual([]);
    term.dispose();
  });

  it("KNOWN-GAP: prefers-reduced-motion must turn the glide off", () => {
    vi.stubGlobal("matchMedia", (query: string) => ({ matches: query.includes("prefers-reduced-motion"), media: query }));
    expect(scrollFeelOptions().smoothScrollDuration).toBe(0);
  });
});
