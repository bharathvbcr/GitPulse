import { describe, expect, it } from "vitest";
import {
  ICON_KEYS,
  contrastRatio,
  getLanguageBrandColor,
  getLanguageDisplayName,
  getLanguageIconColor,
  getLanguageInkColor,
  resolveLanguageIconKey,
} from "./languageLogos";

describe("languageLogos resolver", () => {
  it("resolves from common language names", () => {
    expect(resolveLanguageIconKey("Rust")).toBe("rust");
    expect(resolveLanguageIconKey("TypeScript")).toBe("typescript");
    expect(resolveLanguageIconKey("JavaScript")).toBe("javascript");
    expect(resolveLanguageIconKey("Python")).toBe("python");
    expect(resolveLanguageIconKey("Go")).toBe("go");
    expect(resolveLanguageIconKey("Svelte")).toBe("svelte");
    expect(resolveLanguageIconKey("HTML")).toBe("html");
    expect(resolveLanguageIconKey("CSS")).toBe("css");
    expect(resolveLanguageIconKey("C")).toBe("c");
    expect(resolveLanguageIconKey("C++")).toBe("cpp");
    expect(resolveLanguageIconKey("C#")).toBe("csharp");
    expect(resolveLanguageIconKey("Java")).toBe("java");
    expect(resolveLanguageIconKey("Ruby")).toBe("ruby");
    expect(resolveLanguageIconKey("PHP")).toBe("php");
    expect(resolveLanguageIconKey("Swift")).toBe("swift");
    expect(resolveLanguageIconKey("Kotlin")).toBe("kotlin");
    expect(resolveLanguageIconKey("Shell")).toBe("shell");
    expect(resolveLanguageIconKey("SQL")).toBe("sql");
    expect(resolveLanguageIconKey("JSON")).toBe("json");
    expect(resolveLanguageIconKey("YAML")).toBe("yaml");
    expect(resolveLanguageIconKey("TOML")).toBe("toml");
    expect(resolveLanguageIconKey("Markdown")).toBe("markdown");
    expect(resolveLanguageIconKey("Docker")).toBe("docker");
  });

  it("resolves from file paths and filenames", () => {
    expect(resolveLanguageIconKey("src-tauri/src/main.rs")).toBe("rust");
    expect(resolveLanguageIconKey("src/App.svelte")).toBe("svelte");
    expect(resolveLanguageIconKey("index.ts")).toBe("typescript");
    expect(resolveLanguageIconKey("Component.tsx")).toBe("typescript");
    expect(resolveLanguageIconKey("server.js")).toBe("javascript");
    expect(resolveLanguageIconKey("App.jsx")).toBe("javascript");
    expect(resolveLanguageIconKey("main.py")).toBe("python");
    expect(resolveLanguageIconKey("cmd/main.go")).toBe("go");
    expect(resolveLanguageIconKey("Dockerfile")).toBe("docker");
    expect(resolveLanguageIconKey("docker-compose.yml")).toBe("docker");
    expect(resolveLanguageIconKey(".gitignore")).toBe("git");
    expect(resolveLanguageIconKey("Cargo.lock")).toBe("lock");
    expect(resolveLanguageIconKey("package-lock.json")).toBe("lock");
    expect(resolveLanguageIconKey("icon.svg")).toBe("svg");
    expect(resolveLanguageIconKey("banner.png")).toBe("image");
  });

  it("resolves brand colors and display names", () => {
    expect(getLanguageBrandColor("rust")).toBe("#dea584");
    expect(getLanguageBrandColor("typescript")).toBe("#3178c6");
    expect(getLanguageDisplayName("rust")).toBe("Rust");
    expect(getLanguageDisplayName("typescript")).toBe("TypeScript");
  });

  it("handles unknown or empty inputs gracefully", () => {
    expect(resolveLanguageIconKey("")).toBe("file");
    expect(resolveLanguageIconKey("unknown_file.xyz")).toBe("file");
    expect(getLanguageBrandColor("file")).toBe("#6b7280");
  });
});

/* ------------------------------------------------------------------ *
 * Display-colour contract
 *
 * Brand hexes are picked against a white page. Painted straight onto this
 * app's surfaces, several of them vanished — Lua's `#000080` and Docker's
 * `#384d54` into the dark rows, JavaScript's `#f7df1e` and the image tint
 * `#a2d9ff` into the light ones. The fix derives the display colour instead
 * of curating a second table, so this asserts the derivation rather than the
 * 68 values it produces: a new key is covered the day it is added.
 * ------------------------------------------------------------------ */

/** Lightest dark row (`--c-surface-hover`) and lightest light row (`--c-surface`). */
const WORST_ROW = { dark: "#1f273a", light: "#ffffff" } as const;

describe("icon display colours", () => {
  it.each(ICON_KEYS)("keeps %s readable against both themes' worst-case row", (key) => {
    for (const theme of ["dark", "light"] as const) {
      const colour = getLanguageIconColor(key, theme);
      expect(
        contrastRatio(colour, WORST_ROW[theme]),
        `${key} on ${theme}: ${colour}`,
      ).toBeGreaterThanOrEqual(3);
    }
  });

  it.each(ICON_KEYS)("keeps %s's detail readable against its own body", (key) => {
    for (const theme of ["dark", "light"] as const) {
      const body = getLanguageIconColor(key, theme);
      const ink = getLanguageInkColor(key, theme);
      expect(contrastRatio(ink, body), `${key} ink on ${theme}`).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("leaves a brand colour alone when it already clears the bar", () => {
    // Most do. Moving a colour that did not need moving would be a second
    // palette by accident.
    expect(getLanguageIconColor("rust", "dark")).toBe(getLanguageBrandColor("rust"));
    expect(getLanguageIconColor("typescript", "light")).toBe(getLanguageBrandColor("typescript"));
  });

  it("moves only the colours that fail, and keeps their hue", () => {
    // Lua's navy is invisible on the dark surface; the fix lightens it rather
    // than replacing it, so it still reads as Lua blue.
    const lua = getLanguageIconColor("lua", "dark");
    expect(lua).not.toBe(getLanguageBrandColor("lua"));
    const [r, g, b] = [1, 3, 5].map((offset) => Number.parseInt(lua.slice(offset, offset + 2), 16));
    expect(b).toBeGreaterThan(r);
    expect(b).toBeGreaterThan(g);
  });

  it("reports contrast symmetrically", () => {
    expect(contrastRatio("#ffffff", "#000000")).toBeCloseTo(21, 5);
    expect(contrastRatio("#000000", "#ffffff")).toBeCloseTo(21, 5);
    expect(contrastRatio("#123456", "#123456")).toBeCloseTo(1, 5);
  });
});
