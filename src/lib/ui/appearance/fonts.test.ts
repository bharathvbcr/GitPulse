import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  DEFAULT_MONO_STACK,
  DEFAULT_UI_STACK,
  fontStack,
  isFamilyInstalled,
  normalizeFamily,
} from "./fonts";
import { densityFromLegacy, isDensity, rowDensity } from "./density";

const css = readFileSync(new URL("../../../app.css", import.meta.url), "utf8");

describe("normalizeFamily", () => {
  it("accepts one local family name and strips quotes", () => {
    expect(normalizeFamily("JetBrains Mono")).toBe("JetBrains Mono");
    expect(normalizeFamily('  "SF  Pro"  ')).toBe("SF Pro");
    expect(normalizeFamily("Source Han Sans-Light")).toBe("Source Han Sans-Light");
  });

  it('treats blank as "the system font"', () => {
    expect(normalizeFamily("")).toBe("");
    expect(normalizeFamily("   ")).toBe("");
  });

  it.each([
    "Inter; color: red",
    "url(https://evil.example/f.woff2)",
    "https://evil.example/f",
    "a, b",
    "Inter }",
    "x\\2f",
    "<b>",
    "a".repeat(65),
    "var(--x)",
  ])("refuses %j", (text) => {
    expect(normalizeFamily(text)).toBeNull();
  });

  it("refuses a non-string", () => {
    expect(normalizeFamily(7)).toBeNull();
    expect(normalizeFamily(undefined)).toBeNull();
  });
});

describe("stacks", () => {
  it("puts the family first, quoted, ahead of the fallback", () => {
    expect(fontStack("Inter", DEFAULT_UI_STACK)).toBe(`"Inter", ${DEFAULT_UI_STACK}`);
  });

  it("matches the stacks app.css falls back to", () => {
    expect(css).toContain(`--font-sans: var(--gp-font-ui, ${DEFAULT_UI_STACK});`);
    expect(css).toContain(`--font-mono: var(--gp-font-mono, ${DEFAULT_MONO_STACK});`);
  });

  it("routes the macOS body and the Markdown code font through the hooks", () => {
    expect(css).toMatch(/html\.macos body \{\s*font-family: var\(--gp-font-ui, /);
    expect(css.match(/font-family: var\(--gp-font-mono, /g)?.length).toBe(3);
  });
});

describe("isFamilyInstalled", () => {
  it("answers null, not true, when it cannot measure", () => {
    expect(isFamilyInstalled("Inter", null)).toBeNull();
    expect(isFamilyInstalled("")).toBeNull();
    expect(isFamilyInstalled("a;b")).toBeNull();
  });

  /** A canvas whose width depends on whether the first family in the font string is "installed". */
  function fakeCanvas(installed: ReadonlySet<string>): CanvasRenderingContext2D {
    const state = { font: "" };
    return {
      get font() {
        return state.font;
      },
      set font(value: string) {
        state.font = value;
      },
      measureText() {
        const first = /^72px "([^"]+)"/.exec(state.font)?.[1];
        const generic = /(monospace|serif|sans-serif)$/.exec(state.font)?.[1] ?? "";
        const base = { monospace: 100, serif: 120, "sans-serif": 110 }[generic as "monospace"] ?? 100;
        return { width: first && installed.has(first) ? base + 17 : base };
      },
    } as unknown as CanvasRenderingContext2D;
  }

  it("tells an installed family from one that fell back", () => {
    const canvas = fakeCanvas(new Set(["Menlo"]));
    expect(isFamilyInstalled("Menlo", canvas)).toBe(true);
    expect(isFamilyInstalled("Nonexistent Font", canvas)).toBe(false);
  });
});

describe("density", () => {
  it("maps the three steps onto the two-valued row tables", () => {
    expect(rowDensity("compact")).toBe("compact");
    expect(rowDensity("comfortable")).toBe("spacious");
    expect(rowDensity("spacious")).toBe("spacious");
  });

  it("migrates the pre-merge value", () => {
    expect(densityFromLegacy("spacious")).toBe("spacious");
    expect(densityFromLegacy("compact")).toBe("compact");
    expect(densityFromLegacy(null)).toBe("comfortable");
    expect(densityFromLegacy("tiny")).toBe("comfortable");
  });

  it("recognises exactly the three steps", () => {
    for (const value of ["compact", "comfortable", "spacious"]) expect(isDensity(value)).toBe(true);
    for (const value of ["Compact", "", null, 1]) expect(isDensity(value)).toBe(false);
  });
});
