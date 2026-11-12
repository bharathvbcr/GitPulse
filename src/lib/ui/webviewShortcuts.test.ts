import { describe, expect, it } from "vitest";
import {
  classifyShortcut,
  shouldSkipWebviewShortcut,
  type ShortcutKeyState,
} from "./webviewShortcuts";

function key(overrides: Partial<ShortcutKeyState> = {}): ShortcutKeyState {
  return {
    key: "",
    code: "",
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    shiftKey: false,
    ...overrides,
  };
}

describe("classifyShortcut", () => {
  it("recognizes the close-tab chord on both Cmd and Ctrl variants", () => {
    expect(classifyShortcut(key({ key: "w", metaKey: true, shiftKey: true }))).toBe(
      "closeActiveTab",
    );
    expect(classifyShortcut(key({ key: "W", ctrlKey: true, shiftKey: true }))).toBe(
      "closeActiveTab",
    );
  });

  it("recognizes Ctrl+Tab cycling with and without Shift", () => {
    expect(classifyShortcut(key({ key: "Tab", code: "Tab", ctrlKey: true }))).toBe(
      "cycleTabs",
    );
    expect(
      classifyShortcut(key({ key: "Tab", code: "Tab", ctrlKey: true, shiftKey: true })),
    ).toBe("cycleTabs");
  });

  it("recognizes the host's command key plus a digit as a repository-tab jump", () => {
    // Command on macOS, Control elsewhere; no alias, because Control+digit on
    // macOS belongs to the terminal.
    expect(
      classifyShortcut(key({ key: "3", code: "Digit3", metaKey: true }), true),
    ).toBe("jumpToTab");
    expect(
      classifyShortcut(key({ key: "3", code: "Digit3", ctrlKey: true }), false),
    ).toBe("jumpToTab");
    expect(classifyShortcut(key({ key: "3", code: "Digit3", ctrlKey: true }), true)).toBeNull();
    expect(classifyShortcut(key({ key: "3", code: "Digit3", metaKey: true }), false)).toBeNull();
  });

  it("does not take the view chords or the old Ctrl+Alt jump for a tab jump", () => {
    // ⌥⌘digit on macOS, Ctrl+Shift+digit elsewhere select views.
    expect(
      classifyShortcut(key({ key: "3", code: "Digit3", metaKey: true, altKey: true }), true),
    ).toBeNull();
    expect(
      classifyShortcut(key({ key: "#", code: "Digit3", ctrlKey: true, shiftKey: true }), false),
    ).toBeNull();
    // Ctrl+Alt is AltGr on many layouts and no longer a chord at all.
    expect(
      classifyShortcut(key({ key: "3", code: "Digit3", ctrlKey: true, altKey: true }), false),
    ).toBeNull();
    // Digit zero is zoom reset, not a jump.
    expect(classifyShortcut(key({ key: "0", code: "Digit0", metaKey: true }), true)).toBe("zoom");
  });

  it("recognizes the new/open-repo chord (Cmd/Ctrl+T)", () => {
    expect(classifyShortcut(key({ key: "t", metaKey: true }))).toBe("openRepo");
    expect(classifyShortcut(key({ key: "t", ctrlKey: true }))).toBe("openRepo");
  });

  it("rejects near-misses for every family", () => {
    // Shift+T is not openRepo.
    expect(classifyShortcut(key({ key: "T", metaKey: true, shiftKey: true }))).toBeNull();
    // Alt+Tab without Ctrl is not cycleTabs; plain Tab neither.
    expect(classifyShortcut(key({ key: "Tab", code: "Tab", altKey: true }))).toBeNull();
    expect(classifyShortcut(key({ key: "Tab", code: "Tab" }))).toBeNull();
    // Ctrl+Alt+letter is not a jump.
    expect(
      classifyShortcut(key({ key: "a", code: "KeyA", ctrlKey: true, altKey: true })),
    ).toBeNull();
    // Plain keys are nothing.
    expect(classifyShortcut(key({ key: "w" }))).toBeNull();
  });
});

describe("shouldSkipWebviewShortcut", () => {
  it("lets the native menu own its new zoom and shortcuts chords exactly once", () => {
    for (const value of ["=", "-", "0", "/"]) {
      for (const modifier of [{ metaKey: true }, { ctrlKey: true }]) {
        const event = key({ key: value, ...modifier });
        expect(shouldSkipWebviewShortcut(event, true, "metaKey" in modifier)).toBe(true);
        expect(shouldSkipWebviewShortcut(event, true, !("metaKey" in modifier))).toBe(false);
        expect(shouldSkipWebviewShortcut(event, false)).toBe(false);
        expect(shouldSkipWebviewShortcut({ ...event, altKey: true }, true, "metaKey" in modifier)).toBe(false);
        expect(shouldSkipWebviewShortcut({ ...event, shiftKey: true }, true, "metaKey" in modifier)).toBe(false);
      }
    }
    // Shift+= is an existing webview alias; the menu binds the unshifted key.
    expect(shouldSkipWebviewShortcut(key({ key: "+", metaKey: true, shiftKey: true }), true)).toBe(false);
    expect(shouldSkipWebviewShortcut(key({ key: "?", shiftKey: true }), true)).toBe(false);
  });
  it("stands down for native-owned chords under Tauri (double-fire guard)", () => {
    expect(
      shouldSkipWebviewShortcut(key({ key: "w", metaKey: true, shiftKey: true }), true, true),
    ).toBe(true);
    expect(
      shouldSkipWebviewShortcut(key({ key: "Tab", code: "Tab", ctrlKey: true }), true),
    ).toBe(true);
    expect(
      shouldSkipWebviewShortcut(
        key({ key: "Tab", code: "Tab", ctrlKey: true, shiftKey: true }),
        true,
      ),
    ).toBe(true);
  });

  it("stands down for the repository-tab digits the native menu binds, on the host's own modifier only", () => {
    const command = (macOS: boolean) =>
      key({ key: "3", code: "Digit3", ...(macOS ? { metaKey: true } : { ctrlKey: true }) });
    expect(shouldSkipWebviewShortcut(command(true), true, true)).toBe(true);
    expect(shouldSkipWebviewShortcut(command(false), true, false)).toBe(true);
    // Plain browser builds have no native menu, so the webview handles it.
    expect(shouldSkipWebviewShortcut(command(true), false, true)).toBe(false);
    // The view chords are native-only too and never classify as a jump, so the
    // webview does not act on them either.
    expect(
      shouldSkipWebviewShortcut(key({ key: "3", code: "Digit3", metaKey: true, altKey: true }), true, true),
    ).toBe(false);
  });

  it("keeps webview-only families alive under Tauri", () => {
    expect(shouldSkipWebviewShortcut(key({ key: "t", metaKey: true }), true)).toBe(
      false,
    );
  });

  it("does not swallow a webview alias the native menu cannot receive", () => {
    expect(shouldSkipWebviewShortcut(key({ key: "w", ctrlKey: true, shiftKey: true }), true, true)).toBe(false);
    expect(shouldSkipWebviewShortcut(key({ key: "w", metaKey: true, shiftKey: true }), true, false)).toBe(false);
    expect(shouldSkipWebviewShortcut(key({ key: "w", metaKey: true, shiftKey: true, altKey: true }), true, true)).toBe(false);
  });

  it("never skips in plain browser builds", () => {
    expect(
      shouldSkipWebviewShortcut(key({ key: "w", metaKey: true, shiftKey: true }), false),
    ).toBe(false);
    expect(
      shouldSkipWebviewShortcut(key({ key: "Tab", code: "Tab", ctrlKey: true }), false),
    ).toBe(false);
    expect(shouldSkipWebviewShortcut(key({ key: "x", ctrlKey: true }), false)).toBe(
      false,
    );
  });
});
