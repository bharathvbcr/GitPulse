import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CoachMark from "./CoachMark.svelte";
import { resetHostPlatformForTests } from "../stores/platformStore";
import { LAYERS } from "../ui/layers";

/**
 * A coach mark floats over the shell. It is mounted as the shell column's
 * last child, after the status bar, so if it ever sits in flow again it takes
 * its own height out of that column and pushes the status bar up while it
 * shows. The card owns its float, so a caller's placement classes
 * (`bottom-10 right-6`) only ever say where, never whether.
 */
function rootTag(): string {
  resetHostPlatformForTests({ os: "macos", dock_hiding: true, browser_sessions: true, native_material: false });
  const body = render(CoachMark, {
    props: {
      id: "coach-test-layer",
      title: "Command Palette",
      description: "Press ⌘K anytime to search files.",
      shortcut: "⌘K",
      class: "bottom-10 right-6",
    },
  }).body;
  const match = body.match(/<div[^>]*role="tooltip"[^>]*>/);
  expect(match, "the coach mark rendered").not.toBeNull();
  return match![0];
}

afterEach(() => resetHostPlatformForTests());

describe("coach mark layer", () => {
  it("floats out of the layout flow", () => {
    const tag = rootTag();
    expect(tag).toMatch(/class="[^"]*\bfixed\b/);
    expect(tag).toContain("bottom-10 right-6");
  });

  it("stacks on the tour layer, not a literal z class", () => {
    const tag = rootTag();
    expect(tag).toContain(`z-index: ${LAYERS.TOUR}`);
    expect(tag).not.toMatch(/\bz-\d+\b/);
  });
});
