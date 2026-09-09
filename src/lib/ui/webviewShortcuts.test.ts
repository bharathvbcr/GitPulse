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

  it("recognizes Ctrl+Alt+Digit jumps", () => {
    expect(
      classifyShortcut(key({ key: "3", code: "Digit3", ctrlKey: true, altKey: true })),
    ).toBe("jumpToTab");
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

  it("keeps webview-only families alive under Tauri", () => {
    expect(
      shouldSkipWebviewShortcut(
        key({ key: "3", code: "Digit3", ctrlKey: true, altKey: true }),
        true,
      ),
    ).toBe(false);
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
