import { describe, expect, it } from "vitest";
import { shouldDismissOverlay } from "./dismiss";
import {
  FLOATING_LAYER_SELECTOR,
  floatingPanelDismiss,
  isWindowDragDoubleClick,
  pageDoubleClickHidesPanel,
} from "./panelDismiss";

/**
 * A node tree without a DOM: `closest` walks up, matching a comma-separated
 * selector against each node's class names, attributes and `data-*` flags.
 */
interface FakeNode {
  tokens: string[];
  parent: FakeNode | null;
  closest(selector: string): FakeNode | null;
}

function node(tokens: string[], parent: FakeNode | null = null): FakeNode {
  const self: FakeNode = {
    tokens,
    parent,
    closest(selector) {
      const wanted = selector.split(",").map((part) => part.trim());
      for (let at: FakeNode | null = self; at; at = at.parent) {
        if (at.tokens.some((token) => wanted.includes(token))) return at;
      }
      return null;
    },
  };
  return self;
}

const options = floatingPanelDismiss("[data-sidebar-panel]");
const dismisses = (target: unknown) => shouldDismissOverlay(target, options.inside ?? "");

describe("floating panel outside-click dismiss", () => {
  const body = node(["body"]);
  const workspace = node([".gp-workspace"], body);
  const panel = node(["[data-sidebar-panel]"], body);
  const insidePanel = node(["button"], node(["div"], panel));

  it("listens on pointerdown (capture) and leaves Escape to the panel", () => {
    expect(options.pointer).toBe("pointerdown");
    expect(options.escape).toBe("none");
  });

  it("does not dismiss for a pointerdown inside the panel", () => {
    expect(dismisses(insidePanel)).toBe(false);
  });

  it("does not dismiss for a portaled menu the panel opened, which lives under <body>", () => {
    const menu = node([".gp-menu"], body);
    const menuItem = node([".gp-menu-item"], menu);
    expect(menuItem.closest("[data-sidebar-panel]")).toBeNull();
    expect(dismisses(menuItem)).toBe(false);
    expect(dismisses(node(["span"], node(['[role="menu"]'], body)))).toBe(false);
  });

  it("does not dismiss for a dialog or popover layer either", () => {
    expect(dismisses(node(["x"], node(['[role="dialog"]'], body)))).toBe(false);
    expect(dismisses(node(["x"], node([".gp-pop"], body)))).toBe(false);
    for (const part of [".gp-menu", ".gp-pop", ".gp-scrim", '[role="menu"]', '[role="dialog"]']) {
      expect(FLOATING_LAYER_SELECTOR).toContain(part);
    }
  });

  it("dismisses for a pointerdown truly outside", () => {
    expect(dismisses(node(["div"], workspace))).toBe(true);
    expect(dismisses(body)).toBe(true);
  });

  it("fails closed on a missing target", () => {
    expect(dismisses(null)).toBe(true);
  });

  it("counts a named trigger as inside, so its own click does not close and reopen the panel", () => {
    const withTrigger = floatingPanelDismiss("[data-terminal-dock]", "[data-terminal-pill]");
    const pill = node(["[data-terminal-pill]"], body);
    expect(shouldDismissOverlay(pill, withTrigger.inside ?? "")).toBe(false);
    expect(shouldDismissOverlay(node(["div"], workspace), withTrigger.inside ?? "")).toBe(true);
  });
});

describe("double-click on the page hides the terminal dock", () => {
  const body = node(["body"]);
  const viewBar = node(["[data-viewbar]"], body);
  const workspace = node([".gp-workspace"], body);
  const view = node([".gp-view"], workspace);
  const dock = node(["[data-terminal-dock]"], workspace);
  const sidebar = node(["aside"], body);
  // The column the dock shares with the view: `contains` walks up, as the DOM's does.
  const column = {
    contains(candidate: unknown) {
      for (let at = candidate as FakeNode | null; at; at = at.parent) if (at === workspace) return true;
      return false;
    },
  };
  const scope = {
    panel: "[data-terminal-dock]",
    trigger: "[data-terminal-pill], [data-terminal-pill-menu]",
    column,
    page: "[data-viewbar]",
  };
  const hides = (target: unknown) => pageDoubleClickHidesPanel(target, scope);

  it("hides for a double-click anywhere on the page", () => {
    expect(hides(view)).toBe(true);
    expect(hides(node(["button"], node(["div"], view)))).toBe(true);
  });

  it("hides for a double-click on the page tab bar", () => {
    expect(hides(viewBar)).toBe(true);
    expect(hides(node(['[role="tab"]'], node(['[role="tablist"]'], viewBar)))).toBe(true);
  });

  it("keeps the dock for a double-click on its own pill, whose two clicks already toggled it", () => {
    expect(hides(node(["button"], node(["[data-terminal-pill]"], viewBar)))).toBe(false);
    expect(hides(node(["[data-terminal-pill-menu]"], body))).toBe(false);
  });

  it("follows a text node up to its element", () => {
    expect(hides({ parentElement: node(["span"], view) })).toBe(true);
    expect(hides({ parentElement: node(["span"], viewBar) })).toBe(true);
  });

  it("keeps the dock for a double-click outside the page and its tab bar", () => {
    expect(hides(sidebar)).toBe(false);
    expect(hides(node(["button"], sidebar))).toBe(false);
    expect(hides(body)).toBe(false);
  });

  it("keeps the dock for a double-click inside it, such as selecting a word of output", () => {
    expect(hides(node(["div"], dock))).toBe(false);
  });

  it("keeps the dock for a double-click on a menu or dialog the page opened", () => {
    expect(hides(node(["button"], node([".gp-menu"], body)))).toBe(false);
    expect(hides(node(["button"], node([".gp-menu"], viewBar)))).toBe(false);
    expect(hides(node(['[role="dialog"]'], view))).toBe(false);
  });

  it("keeps the dock when the double-click selects a word in a text field", () => {
    expect(hides(node(["input"], view))).toBe(false);
    expect(hides(node(["input"], viewBar))).toBe(false);
    expect(hides(node(["textarea"], view))).toBe(false);
    expect(hides(node(["span"], node(['[contenteditable]:not([contenteditable="false"])'], view)))).toBe(false);
  });

  it("does nothing without a target", () => {
    expect(hides(null)).toBe(false);
  });
});

/**
 * Elements as the drag script sees them: a tag, attributes, and a role. The
 * path is target first, then each ancestor, as `event.composedPath()` gives it.
 */
function element(tagName: string, attrs: Record<string, string> = {}) {
  return {
    tagName,
    getAttribute: (name: string) => (name in attrs ? attrs[name] : null),
    hasAttribute: (name: string) => name in attrs,
  };
}

describe("a double-click the window takes for its zoom", () => {
  const bar = element("NAV", { "data-tauri-drag-region": "" });
  const strip = element("DIV", { "data-tauri-drag-region": "" });
  const tab = element("BUTTON", { role: "tab" });
  const label = element("SPAN");

  it("is a direct hit on a bare drag region", () => {
    expect(isWindowDragDoubleClick([bar])).toBe(true);
    expect(isWindowDragDoubleClick([strip, bar])).toBe(true);
    expect(isWindowDragDoubleClick([element("DIV", { "data-tauri-drag-region": "true" }), bar])).toBe(true);
  });

  it("is not a hit on something inside a bare drag region", () => {
    // A bare region drags only on a direct hit, so a label in it is the page's.
    expect(isWindowDragDoubleClick([label, strip, bar])).toBe(false);
  });

  it("is not a hit on a tab or a button, which keeps its own clicks", () => {
    expect(isWindowDragDoubleClick([tab, strip, bar])).toBe(false);
    expect(isWindowDragDoubleClick([label, tab, strip, bar])).toBe(false);
    expect(isWindowDragDoubleClick([element("DIV", { tabindex: "0" }), bar])).toBe(false);
    expect(isWindowDragDoubleClick([element("DIV", { contenteditable: "" }), bar])).toBe(false);
  });

  it("is a hit anywhere under a deep drag region, unless something clickable comes first", () => {
    const deep = element("DIV", { "data-tauri-drag-region": "deep" });
    expect(isWindowDragDoubleClick([label, deep])).toBe(true);
    expect(isWindowDragDoubleClick([label, tab, deep])).toBe(false);
  });

  it("is not a hit where the region is switched off", () => {
    expect(isWindowDragDoubleClick([element("DIV", { "data-tauri-drag-region": "false" }), bar])).toBe(false);
  });

  it("is a hit on a clickable element that is itself marked as a drag region", () => {
    expect(isWindowDragDoubleClick([element("BUTTON", { "data-tauri-drag-region": "" }), bar])).toBe(true);
  });

  it("is not a hit off any drag region, or with no path at all", () => {
    expect(isWindowDragDoubleClick([label, element("MAIN")])).toBe(false);
    expect(isWindowDragDoubleClick([])).toBe(false);
    expect(isWindowDragDoubleClick([null, "text", 3])).toBe(false);
  });
});
