import { describe, expect, it } from "vitest";
import { MAX_SAVED_THEMES, readSavedThemes, savedThemeKey, withSavedTheme, withoutSavedTheme } from "./savedThemes";
import { THEME_SCHEMA_ID, validateTheme, type GitPulseTheme } from "./themeSchema";

function theme(name: string, appearance: "dark" | "light" = "dark", accent = "#4cc9ff"): GitPulseTheme {
  const checked = validateTheme({ schema: THEME_SCHEMA_ID, name, appearance, colors: { base: "#101010", accent } });
  if (!checked.ok) throw new Error("fixture invalid");
  return checked.theme;
}

describe("readSavedThemes", () => {
  it("reads nothing from anything that is not a list", () => {
    for (const raw of [undefined, null, {}, "x", 3]) expect(readSavedThemes(raw)).toEqual([]);
  });

  it("drops entries that fail the theme validator instead of repairing them", () => {
    const evil = { schema: THEME_SCHEMA_ID, name: "Evil", appearance: "dark", colors: { base: "url(x)", accent: "#fff" } };
    const css = { ...theme("Css"), css: "body{}" };
    expect(readSavedThemes([theme("Good"), evil, css, 7, null]).map((entry) => entry.name)).toEqual(["Good"]);
  });

  it("keeps one entry per name and appearance, first one wins", () => {
    const list = readSavedThemes([theme("Night"), theme("night", "dark", "#ff0000"), theme("Night", "light")]);
    expect(list.map((entry) => `${entry.name}/${entry.appearance}`)).toEqual(["Night/dark", "Night/light"]);
  });

  it("is bounded", () => {
    const many = Array.from({ length: MAX_SAVED_THEMES * 3 }, (_, index) => theme(`T${index}`));
    expect(readSavedThemes(many)).toHaveLength(MAX_SAVED_THEMES);
  });
});

describe("the shelf", () => {
  it("adds a new theme to the front", () => {
    const result = withSavedTheme([theme("A")], theme("B"));
    expect(result.outcome).toBe("added");
    expect(result.list.map((entry) => entry.name)).toEqual(["B", "A"]);
  });

  it("replaces a theme of the same name and appearance in place", () => {
    const result = withSavedTheme([theme("A"), theme("B")], theme("b", "dark", "#ff0000"));
    expect(result.outcome).toBe("replaced");
    expect(result.list.map((entry) => entry.name)).toEqual(["A", "b"]);
    expect(result.list[1].colors.accent).toBe("#ff0000");
  });

  it("refuses rather than evicting when full", () => {
    const full = Array.from({ length: MAX_SAVED_THEMES }, (_, index) => theme(`T${index}`));
    const result = withSavedTheme(full, theme("New"));
    expect(result.outcome).toBe("full");
    expect(result.list).toBe(full);
  });

  it("removes by name and appearance only", () => {
    const list = [theme("A"), theme("A", "light"), theme("B")];
    expect(withoutSavedTheme(list, { name: "a", appearance: "dark" }).map((entry) => savedThemeKey(entry))).toEqual([
      savedThemeKey(theme("A", "light")),
      savedThemeKey(theme("B")),
    ]);
  });
});
