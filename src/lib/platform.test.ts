import { describe, it, expect } from "vitest";
import {
  fallbackHostPlatform,
  hostOSFromUserAgent,
  isHostOS,
  isMacOS,
  isTauri,
  parseHostPlatform,
} from "./platform";

/** Runs `body` with `navigator` stubbed, restoring the real one after. */
function withNavigator(value: unknown, body: () => void): void {
  const original = globalThis.navigator;
  Object.defineProperty(globalThis, "navigator", { value, configurable: true });
  try {
    body();
  } finally {
    Object.defineProperty(globalThis, "navigator", { value: original, configurable: true });
  }
}

describe("platform", () => {
  it("does not report Tauri inside the Node test runner", () => {
    expect(isTauri()).toBe(false);
  });

  it("detects macOS based on userAgent or platform", () => {
    const origNav = globalThis.navigator;
    try {
      Object.defineProperty(globalThis, "navigator", {
        value: { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" },
        configurable: true,
      });
      expect(isMacOS()).toBe(true);

      Object.defineProperty(globalThis, "navigator", {
        value: { platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" },
        configurable: true,
      });
      expect(isMacOS()).toBe(false);
    } finally {
      Object.defineProperty(globalThis, "navigator", {
        value: origNav,
        configurable: true,
      });
    }
  });

  it("applies platform class to document element", async () => {
    const { applyPlatformClass } = await import("./platform");
    // No document environment
    expect(() => applyPlatformClass()).not.toThrow();

    // With document
    const classes = new Set<string>();
    (globalThis as Record<string, unknown>).document = {
      documentElement: {
        classList: {
          toggle: (cls: string, force: boolean) => {
            if (force) classes.add(cls);
            else classes.delete(cls);
          },
        },
      },
    };
    try {
      applyPlatformClass();
      expect(classes.has("macos")).toBe(isMacOS());
    } finally {
      delete (globalThis as Record<string, unknown>).document;
    }
  });
});

describe("host OS identity", () => {
  it("names each desktop host GitPulse ships to", () => {
    withNavigator(
      { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" },
      () => expect(hostOSFromUserAgent()).toBe("macos"),
    );
    withNavigator({ platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" }, () =>
      expect(hostOSFromUserAgent()).toBe("windows"),
    );
    withNavigator({ platform: "Linux x86_64", userAgent: "Mozilla/5.0 (X11; Linux x86_64)" }, () =>
      expect(hostOSFromUserAgent()).toBe("linux"),
    );
  });

  /**
   * The gap `isMacOS()` alone cannot express, and the reason this exists:
   * Windows and Linux are different answers, not one "not Mac" bucket.
   */
  it("separates Windows from Linux", () => {
    expect(hostOSFromUserAgent("Win32", "Mozilla/5.0 (Windows NT 10.0)")).toBe("windows");
    expect(hostOSFromUserAgent("Linux x86_64", "Mozilla/5.0 (X11; Linux x86_64)")).toBe("linux");
  });

  it("does not mistake an Android UA for desktop Linux", () => {
    // Android's UA contains "Linux" but is not a host GitPulse ships to, so it
    // must stay unknown rather than inherit desktop Linux behaviour.
    expect(hostOSFromUserAgent("Linux armv8l", "Mozilla/5.0 (Linux; Android 14) Mobile")).toBe(
      "unknown",
    );
  });

  it("answers unknown rather than guessing when the webview says nothing", () => {
    expect(hostOSFromUserAgent("", "")).toBe("unknown");
  });

  it("claims no native capability before the backend answers", () => {
    withNavigator({ platform: "MacIntel", userAgent: "Macintosh; Intel Mac OS X" }, () => {
      const fallback = fallbackHostPlatform();
      expect(fallback.os).toBe("macos");
      // Even on macOS: the webview cannot know whether the code compiled in.
      expect(fallback.dock_hiding).toBe(false);
    });
  });

  it("recognises exactly the four host names", () => {
    for (const value of ["macos", "windows", "linux", "unknown"]) expect(isHostOS(value)).toBe(true);
    for (const value of ["darwin", "win32", "MacOS", "", null, undefined, 7])
      expect(isHostOS(value)).toBe(false);
  });
});

describe("host platform envelope", () => {
  it("reads the backend's snake_case capability fields", () => {
    expect(parseHostPlatform({ os: "windows", dock_hiding: false })).toEqual({
      os: "windows",
      dock_hiding: false,
    });
    expect(parseHostPlatform({ os: "macos", dock_hiding: true })).toEqual({
      os: "macos",
      dock_hiding: true,
    });
  });

  /**
   * A check that could not run must never read like one that ran and passed.
   * Every malformed shape must deny the capability, never grant it.
   */
  it.each([
    ["null", null],
    ["a string", "macos"],
    ["an array", []],
    ["an empty object", {}],
    // The camelCase spelling the app uses internally is NOT the wire name;
    // reading it would make a rename on either side look like a live capability.
    ["a camelCased field name", { os: "macos", dockHiding: true }],
    ["a truthy non-boolean", { os: "macos", dock_hiding: "yes" }],
    ["a numeric flag", { os: "macos", dock_hiding: 1 }],
  ])("denies the capability given %s", (_label, value) => {
    expect(parseHostPlatform(value).dock_hiding).toBe(false);
  });

  it("reports an OS name it does not recognise as unknown", () => {
    // Rust may name a host this union does not list. Inheriting the webview's
    // guess would claim a platform the backend just contradicted.
    expect(parseHostPlatform({ os: "freebsd", dock_hiding: false }).os).toBe("unknown");
    expect(parseHostPlatform({ dock_hiding: false }).os).toBe("unknown");
  });
});
