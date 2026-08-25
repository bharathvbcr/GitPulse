import { describe, it, expect } from "vitest";
import { isMacOS, isTauri } from "./platform";

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

