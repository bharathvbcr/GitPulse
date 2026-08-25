import { describe, it, expect } from "vitest";
import { get } from "svelte/store";
import { themeStore } from "../themeStore";

describe("themeStore", () => {
  it("toggles theme between dark and light", () => {
    themeStore.setTheme("dark");
    expect(get(themeStore)).toBe("dark");

    themeStore.toggle();
    expect(get(themeStore)).toBe("light");

    themeStore.toggle();
    expect(get(themeStore)).toBe("dark");
  });

  it("sets specific theme", () => {
    themeStore.setTheme("light");
    expect(get(themeStore)).toBe("light");
    themeStore.setTheme("dark");
    expect(get(themeStore)).toBe("dark");
  });

  it("follows an explicit system preference", () => {
    themeStore.setPreference("system");
    expect(["dark", "light"]).toContain(get(themeStore));
    expect(themeStore.preference()).toBe("system");
  });
});
