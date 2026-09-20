import { afterEach, describe, expect, it, vi } from "vitest";
import { popover, restoreFocusTo, type PopoverOptions } from "./popover";

/**
 * A window that remembers exactly which listeners are attached, on which
 * phase. The phase is the point: every call site that moved onto this action
 * used to spell its own `addEventListener`/`removeEventListener` pair, and
 * the capture flag was the detail that silently differed between them.
 */
function fakeWindow(viewport = { width: 1000, height: 800 }) {
  const listeners: Array<{ type: string; handler: EventListener; capture: boolean }> = [];
  const win = {
    innerWidth: viewport.width,
    innerHeight: viewport.height,
    addEventListener(type: string, handler: EventListener, capture?: boolean) {
      listeners.push({ type, handler, capture: capture === true });
    },
    removeEventListener(type: string, handler: EventListener, capture?: boolean) {
      const index = listeners.findIndex(
        (entry) =>
          entry.type === type && entry.handler === handler && entry.capture === (capture === true),
      );
      if (index >= 0) listeners.splice(index, 1);
    },
    setTimeout: (callback: () => void, _ms?: number) => {
      callback();
      return 0;
    },
  };
  vi.stubGlobal("window", win);
  return {
    listeners,
    /** Deliver an event the way the browser would, to matching listeners only. */
    dispatch(type: string, event: Record<string, unknown>, capture = false) {
      for (const entry of [...listeners]) {
        if (entry.type === type && entry.capture === capture) {
          entry.handler(event as unknown as Event);
        }
      }
    },
    shape: () => listeners.map((entry) => `${entry.type}${entry.capture ? ":capture" : ""}`).sort(),
  };
}

/**
 * A popover node with a measurable box and a real style bag. `offsetWidth` is
 * writable here, unlike the real read-only DOM property, so a test can make
 * content change the panel's size the way a submenu page does.
 */
type MeasurableNode = HTMLElement & {
  offsetWidth: number;
  offsetHeight: number;
  style: Record<string, string>;
};

function fakeNode(size = { width: 100, height: 50 }, contained: unknown[] = []): MeasurableNode {
  const style: Record<string, string> = {};
  return {
    nodeType: 1,
    offsetWidth: size.width,
    offsetHeight: size.height,
    style,
    contains: (other: unknown) => contained.includes(other),
    closest: () => null,
  } as unknown as MeasurableNode;
}

/** An element target whose `closest` answers a selector, as `dismiss.ts` probes it. */
function target(matches: string | null) {
  return { nodeType: 1, closest: (selector: string) => (matches === selector ? {} : null) };
}

const anchorElement = (rect: Partial<DOMRect>) =>
  ({ getBoundingClientRect: () => rect as DOMRect }) as unknown as HTMLElement;

afterEach(() => vi.unstubAllGlobals());

describe("popover listener registration", () => {
  it("registers only what the call site asks for", () => {
    const win = fakeWindow();
    const handle = popover(fakeNode(), { dismiss: { pointer: "none" } });
    expect(win.shape()).toEqual([]);
    handle.destroy();
  });

  it("puts each listener on the phase its job requires", () => {
    const win = fakeWindow();
    const handle = popover(fakeNode(), {
      dismiss: { pointer: "pointerdown", contextmenu: true, scroll: true, resize: true, escape: "capture" },
    });
    // pointerdown and Escape capture so a stopPropagation below cannot
    // suppress dismissal; scroll captures because scroll does not bubble at
    // all, so a bubble listener would never see an inner scroller move.
    expect(win.shape()).toEqual([
      "contextmenu",
      "keydown:capture",
      "pointerdown:capture",
      "resize",
      "scroll:capture",
    ]);
    handle.destroy();
  });

  it("keeps the click spelling on the bubble phase", () => {
    // Two context menus stop propagation on their own container and rely on
    // bubbling to stay open while a row runs. Capture would silently take
    // that away.
    const win = fakeWindow();
    const handle = popover(fakeNode(), { dismiss: { pointer: "click" } });
    expect(win.shape()).toEqual(["click"]);
    handle.destroy();
  });

  it("removes every listener it added when the popover closes", () => {
    // The whole reason the action lives on the popover node: destroy is the
    // close, so a leaked listener cannot outlive the panel.
    const win = fakeWindow();
    const handle = popover(fakeNode(), {
      dismiss: { pointer: "pointerdown", contextmenu: true, scroll: true, resize: true, escape: "bubble" },
    });
    expect(win.listeners).toHaveLength(5);
    handle.destroy();
    expect(win.listeners).toEqual([]);
  });

  it("re-registers when an update moves a listener to another phase", () => {
    const win = fakeWindow();
    const handle = popover(fakeNode(), { dismiss: { pointer: "none", escape: "bubble" } });
    expect(win.shape()).toEqual(["keydown"]);
    handle.update({ dismiss: { pointer: "none", escape: "capture" } });
    expect(win.shape()).toEqual(["keydown:capture"]);
    handle.destroy();
    expect(win.listeners).toEqual([]);
  });

  it("does not churn listeners when only the anchor moves", () => {
    const win = fakeWindow();
    const options = (x: number): PopoverOptions => ({
      anchor: { kind: "point", x, y: 0 },
      dismiss: { resize: true },
    });
    const handle = popover(fakeNode(), options(10));
    const first = win.listeners[0];
    handle.update(options(20));
    expect(win.listeners[0]).toBe(first);
    handle.destroy();
  });
});

describe("popover dismissal", () => {
  const dismissals = (options: PopoverOptions) => {
    const seen: string[] = [];
    return { seen, options: { ...options, onDismiss: (reason: string) => seen.push(reason) } };
  };

  it("treats the call site's selector as inside, trigger included", () => {
    const win = fakeWindow();
    const { seen, options } = dismissals({ dismiss: { inside: "[data-x]" } });
    const handle = popover(fakeNode(), options as PopoverOptions);
    win.dispatch("pointerdown", { target: target("[data-x]") }, true);
    expect(seen).toEqual([]);
    win.dispatch("pointerdown", { target: target(null) }, true);
    expect(seen).toEqual(["pointer"]);
    handle.destroy();
  });

  it("falls back to its own subtree when no selector is given", () => {
    const win = fakeWindow();
    const inside = { nodeType: 1 };
    const node = fakeNode({ width: 100, height: 50 }, [inside]);
    const { seen, options } = dismissals({});
    const handle = popover(node, options as PopoverOptions);
    win.dispatch("pointerdown", { target: inside }, true);
    expect(seen).toEqual([]);
    win.dispatch("pointerdown", { target: { nodeType: 1 } }, true);
    expect(seen).toEqual(["pointer"]);
    handle.destroy();
  });

  it("judges scroll by containment even when a selector is set", () => {
    // A scroll inside the panel is the reader reading the list, not leaving
    // it. Judging it by the `inside` selector would count a scroll of the
    // page behind the trigger as inside and never dismiss at all.
    const win = fakeWindow();
    const inner = { nodeType: 1 };
    const node = fakeNode({ width: 100, height: 50 }, [inner]);
    const { seen, options } = dismissals({ dismiss: { inside: "[data-x]", scroll: true } });
    const handle = popover(node, options as PopoverOptions);
    win.dispatch("scroll", { target: inner }, true);
    expect(seen).toEqual([]);
    win.dispatch("scroll", { target: target("[data-x]") }, true);
    expect(seen).toEqual(["scroll"]);
    handle.destroy();
  });

  it("reports which reason closed it", () => {
    const win = fakeWindow();
    const { seen, options } = dismissals({
      dismiss: { inside: "[data-x]", contextmenu: true, resize: true, escape: "bubble" },
    });
    const handle = popover(fakeNode(), options as PopoverOptions);
    win.dispatch("contextmenu", { target: target(null) });
    win.dispatch("resize", {});
    win.dispatch("keydown", { key: "Escape", preventDefault() {}, stopPropagation() {} });
    expect(seen).toEqual(["contextmenu", "resize", "escape"]);
    handle.destroy();
  });

  it("stops one Escape from dismissing two things, but only when asked", () => {
    const win = fakeWindow();
    const stopped = vi.fn();
    const key = () => ({ key: "Escape", preventDefault() {}, stopPropagation: stopped });

    const bubble = popover(fakeNode(), { dismiss: { escape: "bubble" }, onDismiss: () => {} });
    win.dispatch("keydown", key());
    expect(stopped).not.toHaveBeenCalled();
    bubble.destroy();

    const capture = popover(fakeNode(), { dismiss: { escape: "capture" }, onDismiss: () => {} });
    win.dispatch("keydown", key(), true);
    expect(stopped).toHaveBeenCalledTimes(1);
    capture.destroy();
  });

  it("ignores keys that are not Escape", () => {
    const win = fakeWindow();
    const { seen, options } = dismissals({ dismiss: { escape: "capture" } });
    const handle = popover(fakeNode(), options as PopoverOptions);
    const prevented = vi.fn();
    win.dispatch("keydown", { key: "Tab", preventDefault: prevented, stopPropagation() {} }, true);
    expect(seen).toEqual([]);
    // A menu that owns Tab itself must still receive it.
    expect(prevented).not.toHaveBeenCalled();
    handle.destroy();
  });
});

describe("popover placement", () => {
  it("leaves position alone when the panel has no anchor", () => {
    // CSS already places these — an `absolute` dropdown under its trigger, a
    // fixed corner panel. Writing left/top would fight it.
    fakeWindow();
    const node = fakeNode();
    const handle = popover(node, { dismiss: { inside: "[data-x]" } });
    expect(node.style).toEqual({});
    handle.destroy();
  });

  it("places a point anchor where it was asked", () => {
    fakeWindow();
    const node = fakeNode();
    const handle = popover(node, { anchor: { kind: "point", x: 10, y: 20 } });
    expect(node.style).toEqual({ left: "10px", top: "20px" });
    handle.destroy();
  });

  it("pulls a panel back inside rather than off the right or bottom edge", () => {
    fakeWindow({ width: 1000, height: 800 });
    const node = fakeNode({ width: 100, height: 50 });
    const handle = popover(node, { anchor: { kind: "point", x: 980, y: 790 } });
    expect(node.style).toEqual({ left: "900px", top: "750px" });
    handle.destroy();
  });

  it("keeps a left-rail menu inside the window instead of growing past the trigger", () => {
    // The Tasks navigator is 188px on the left edge. A 320px panel opened
    // flush with a plus at ~160px would paint past a 400px window unless
    // clamp pulls it back. The old add-repo menu did that growth in CSS
    // (`right:0` + 260px inside 188px); the owner has to refuse it.
    fakeWindow({ width: 400, height: 300 });
    const node = fakeNode({ width: 320, height: 240 });
    const handle = popover(node, {
      anchor: { kind: "element", element: anchorElement({ left: 160, bottom: 40, top: 14 }), gap: 6 },
      inset: 8,
    });
    expect(Number.parseFloat(node.style.left)).toBeGreaterThanOrEqual(8);
    expect(Number.parseFloat(node.style.left) + 320).toBeLessThanOrEqual(400);
    expect(Number.parseFloat(node.style.top)).toBeGreaterThanOrEqual(8);
    expect(Number.parseFloat(node.style.top) + 240).toBeLessThanOrEqual(300);
    handle.destroy();
  });

  it("opens an element anchor below its trigger, across the gap", () => {
    fakeWindow();
    const node = fakeNode();
    const handle = popover(node, {
      anchor: { kind: "element", element: anchorElement({ left: 40, bottom: 30, top: 8 }), gap: 6 },
    });
    expect(node.style).toEqual({ left: "40px", top: "36px" });
    handle.destroy();
  });

  it("grows a status-bar panel upward by its measured height", () => {
    // "below" is off the bottom of the window for a trigger on the last row,
    // so the panel is placed by its own height above the trigger.
    fakeWindow();
    const node = fakeNode({ width: 288, height: 220 });
    const handle = popover(node, {
      anchor: { kind: "element", element: anchorElement({ left: 40, top: 300 }), gap: 6, place: "above" },
    });
    expect(node.style).toEqual({ left: "40px", top: "74px" });
    handle.destroy();
  });

  it("clamps an upward panel that is taller than the room above it", () => {
    // A raw `bottom:` would run off the top of the window and put the head of
    // the list out of reach.
    fakeWindow();
    const node = fakeNode({ width: 288, height: 400 });
    const handle = popover(node, {
      anchor: { kind: "element", element: anchorElement({ left: 40, top: 100 }), gap: 6, place: "above" },
    });
    expect(node.style.top).toBe("0px");
    handle.destroy();
  });

  it("honours a keep-out inset on every edge", () => {
    fakeWindow({ width: 1000, height: 800 });
    const node = fakeNode({ width: 100, height: 50 });
    const flush = popover(node, { anchor: { kind: "point", x: 0, y: 0 }, inset: 8 });
    expect(node.style).toEqual({ left: "8px", top: "8px" });
    flush.destroy();

    const overflowing = popover(node, { anchor: { kind: "point", x: 990, y: 795 }, inset: 8 });
    expect(node.style).toEqual({ left: "892px", top: "742px" });
    overflowing.destroy();
  });

  it("prefers the measured box over the estimate", () => {
    fakeWindow({ width: 1000, height: 800 });
    const node = fakeNode({ width: 100, height: 50 });
    const handle = popover(node, {
      anchor: { kind: "point", x: 980, y: 20 },
      estimate: { width: 400, height: 400 },
    });
    // 900, not 600: the estimate is a fallback, never a correction.
    expect(node.style.left).toBe("900px");
    handle.destroy();
  });

  it("falls back to the estimate only when the node has no layout box", () => {
    fakeWindow({ width: 1000, height: 800 });
    const node = fakeNode({ width: 0, height: 0 });
    const handle = popover(node, {
      anchor: { kind: "point", x: 980, y: 20 },
      estimate: { width: 200, height: 100 },
    });
    expect(node.style.left).toBe("800px");
    handle.destroy();
  });

  it("re-places when content changed the panel's size but not its anchor", () => {
    fakeWindow({ width: 1000, height: 800 });
    const node = fakeNode({ width: 100, height: 50 });
    const at = (revision: string): PopoverOptions => ({
      anchor: { kind: "point", x: 950, y: 20 },
      revision,
    });
    const handle = popover(node, at("root"));
    expect(node.style.left).toBe("900px");
    node.offsetWidth = 300;
    handle.update(at("submenu"));
    expect(node.style.left).toBe("700px");
    handle.destroy();
  });

  it("waits for an element anchor rather than placing against nothing", () => {
    fakeWindow();
    const node = fakeNode();
    const handle = popover(node, { anchor: { kind: "element", element: null } });
    expect(node.style).toEqual({});
    handle.destroy();
  });
});

describe("restoreFocusTo", () => {
  it("focuses the opener once the popover is gone", () => {
    fakeWindow();
    const focus = vi.fn();
    restoreFocusTo({ isConnected: true, focus } as unknown as HTMLElement);
    expect(focus).toHaveBeenCalledTimes(1);
  });

  it("never focuses a node that left the document", () => {
    // A background refresh can delete the row whose kebab opened the menu;
    // focusing a detached node silently drops focus to <body>.
    fakeWindow();
    const focus = vi.fn();
    restoreFocusTo({ isConnected: false, focus } as unknown as HTMLElement);
    restoreFocusTo(null);
    restoreFocusTo(undefined);
    expect(focus).not.toHaveBeenCalled();
  });
});

describe("popover outside a browser", () => {
  it("is inert under SSR instead of throwing", () => {
    vi.stubGlobal("window", undefined);
    const handle = popover(fakeNode(), { anchor: { kind: "point", x: 1, y: 2 }, dismiss: { resize: true } });
    expect(() => handle.update({})).not.toThrow();
    expect(() => handle.destroy()).not.toThrow();
  });
});
