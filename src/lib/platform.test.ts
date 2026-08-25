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
});

