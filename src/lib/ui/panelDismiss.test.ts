import { describe, expect, it } from "vitest";
import { shouldDismissOverlay } from "./dismiss";
import { FLOATING_LAYER_SELECTOR, floatingPanelDismiss } from "./panelDismiss";

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
