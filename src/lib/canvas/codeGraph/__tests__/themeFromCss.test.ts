import { describe, expect, it } from "vitest";
import { DEFAULT_CODE_GRAPH_THEME } from "../CodeGraphRenderer";
import { codeGraphThemeFromTokens } from "../themeFromCss";

describe("codeGraphThemeFromTokens", () => {
  it("wraps bare triplets in rgb() with the alpha each role needs", () => {
    const tokens: Record<string, string> = {
      "--c-text": "233 237 248", "--c-text-muted": "148 161 190", "--c-surface": "20 26 41",
      "--c-accent": "128 158 255", "--c-status-red": "153 27 27",
    };
    const theme = codeGraphThemeFromTokens((name) => tokens[name] ?? "");
    expect(theme.label).toBe("rgb(233 237 248 / 0.9)");
    expect(theme.selection).toBe("rgb(233 237 248)");
    expect(theme.plate).toBe("rgb(20 26 41)");
    expect(theme.dead).toBe("rgb(153 27 27)");
    expect(theme.linkMuted).toBe("rgb(148 161 190 / 0.45)");
  });

  it("keeps the neutral default for a token that is unset or not a triplet", () => {
    const theme = codeGraphThemeFromTokens((name) => (name === "--c-text" ? "#fff" : ""));
    expect(theme).toEqual(DEFAULT_CODE_GRAPH_THEME);
  });

  it("never emits a hex-alpha-appendable string the renderer could corrupt", () => {
    const theme = codeGraphThemeFromTokens(() => "1 2 3");
    for (const value of [theme.link, theme.label, theme.field, theme.dead]) expect(value.startsWith("rgb(")).toBe(true);
  });
});
