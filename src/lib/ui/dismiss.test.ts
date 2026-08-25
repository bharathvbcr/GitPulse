import { describe, expect, it } from "vitest";
import { eventTargetElement, shouldDismissOverlay } from "./dismiss";

class FakeEl {
  constructor(private readonly inside: string[] = []) {}
  closest(selector: string): FakeEl | null {
    return this.inside.includes(selector) ? this : null;
  }
}

describe("eventTargetElement", () => {
  it("returns an element that already has closest", () => {
    const el = new FakeEl();
    expect(eventTargetElement(el)).toBe(el);
  });

  it("walks from a text node to its parentElement", () => {
    const parent = new FakeEl();
    const text = { parentElement: parent };
    expect(eventTargetElement(text)).toBe(parent);
  });

  it("returns null for bare values", () => {
    expect(eventTargetElement(null)).toBeNull();
    expect(eventTargetElement(undefined)).toBeNull();
    expect(eventTargetElement("div")).toBeNull();
    expect(eventTargetElement({ parentElement: null })).toBeNull();
  });
});

describe("shouldDismissOverlay", () => {
  const inside = "[data-view-nav-menu], [data-view-nav-trigger]";

  it("dismisses when the target is missing", () => {
    expect(shouldDismissOverlay(null, inside)).toBe(true);
  });

  it("does not dismiss a click inside the menu or its trigger", () => {
    const trigger = new FakeEl([inside]);
    const menu = new FakeEl([inside]);
    expect(shouldDismissOverlay(trigger, inside)).toBe(false);
    expect(shouldDismissOverlay(menu, inside)).toBe(false);
  });

  it("dismisses a click on a sibling tab that is not the trigger", () => {
    const tab = new FakeEl();
    expect(shouldDismissOverlay(tab, inside)).toBe(true);
  });

  it("does not dismiss a text-node click whose parent is inside", () => {
    const menu = new FakeEl([inside]);
    expect(shouldDismissOverlay({ parentElement: menu }, inside)).toBe(false);
  });

  it("dismisses a text-node click whose parent is outside", () => {
    const row = new FakeEl();
    expect(shouldDismissOverlay({ parentElement: row }, inside)).toBe(true);
  });
});
