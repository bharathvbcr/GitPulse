import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ShortcutsModal from "./ShortcutsModal.svelte";
import { resetHostPlatformForTests } from "../stores/platformStore";
import type { HostOS } from "../platform";

/**
 * The Keyboard Shortcuts dialog is the app's own reference for its shortcuts.
 * The accelerators are bound as `CmdOrCtrl` and have always resolved to Control
 * off macOS, so the keys worked — this dialog printed ⌘ on every host and so
 * documented keys that do not exist there.
 */
function bodyFor(os: HostOS): string {
  resetHostPlatformForTests({ os, dock_hiding: os === "macos" });
  return render(ShortcutsModal, { props: { isOpen: true } }).body;
}

afterEach(() => resetHostPlatformForTests());

const MAC_GLYPHS = /[⌘⌥⌃]/;

describe("shortcut reference per host", () => {
  it.each<HostOS>(["windows", "linux", "unknown"])("prints no macOS glyph on %s", (os) => {
    const body = bodyFor(os);
    // Sanity: the dialog really rendered its rows, so a pass cannot come from
    // an empty document.
    expect(body).toContain("Open Command Palette");
    expect(body).not.toMatch(MAC_GLYPHS);
  });

  it("prints Ctrl and Shift where macOS shows ⌘ and ⇧", () => {
    const body = bodyFor("windows");
    expect(body).toContain("Ctrl");
    expect(body).toContain("Shift");
    expect(body).not.toContain("⇧");
  });

  it("keeps the macOS glyphs on macOS", () => {
    const body = bodyFor("macos");
    expect(body).toContain("⌘");
    expect(body).toContain("⇧");
  });

  /**
   * `Ctrl+Tab` is literally Control on every platform, so it must read the same
   * on macOS as elsewhere — the fix maps glyphs, it does not rewrite key names.
   */
  it("shows Ctrl+Tab identically on every host", () => {
    for (const os of ["macos", "windows", "linux"] as const) {
      expect(bodyFor(os)).toContain("Cycle to next repository tab");
      expect(bodyFor(os)).toContain("Ctrl");
    }
  });
});
