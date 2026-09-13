import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CoachMark from "./CoachMark.svelte";
import { resetHostPlatformForTests } from "../stores/platformStore";
import type { HostOS } from "../platform";

/**
 * Callers author coach-mark chords in macOS notation, the way every other
 * shortcut table in this app does, and CoachMark translates them once. That
 * makes the raw ⌘ in `App.svelte`'s call site authoring notation rather than
 * output — which is only true for as long as this holds.
 */
function bodyFor(os: HostOS): string {
  resetHostPlatformForTests({ os, dock_hiding: os === "macos" });
  return render(CoachMark, {
    props: {
      id: "coach-test-platform",
      title: "Command Palette",
      description: "Press ⌘K anytime to search files.",
      shortcut: "⌘K",
    },
  }).body;
}

afterEach(() => resetHostPlatformForTests());

describe("coach mark chords per host", () => {
  it("keeps macOS notation on macOS", () => {
    const body = bodyFor("macos");
    expect(body).toContain("⌘K");
    expect(body).toContain("Press ⌘K anytime");
  });

  it.each<HostOS>(["windows", "linux", "unknown"])("translates the chord on %s", (os) => {
    const body = bodyFor(os);
    // Sanity: the mark really rendered, so a pass cannot come from empty output.
    expect(body).toContain("Command Palette");
    expect(body).not.toMatch(/[⌘⌥⌃]/);
    expect(body).toContain("Ctrl+K");
  });

  /**
   * The chord is named twice — in the keycap and again in the prose — and the
   * prose is the half that is easy to forget.
   */
  it("translates the chord inside the description too", () => {
    expect(bodyFor("windows")).toContain("Press Ctrl+K anytime");
  });
});
