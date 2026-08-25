import { describe, expect, it } from "vitest";
import { LAYERS } from "./layers";

describe("LAYERS", () => {
  it("stacks drop overlay < menus/modals < prompt < tooltip", () => {
    expect(LAYERS.DROP_OVERLAY).toBeLessThan(LAYERS.MENU);
    expect(LAYERS.DROP_OVERLAY).toBeLessThan(LAYERS.MODAL);
    expect(LAYERS.MENU).toBe(LAYERS.MODAL);
    expect(LAYERS.MODAL).toBeLessThan(LAYERS.PROMPT);
    expect(LAYERS.PROMPT).toBeLessThan(LAYERS.TOOLTIP);
  });
});
