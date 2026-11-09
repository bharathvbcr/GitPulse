import { describe, expect, it } from "vitest";
import {
  MAX_KEYS_PER_OBJECT,
  THEME_SCHEMA_ID,
  exportTheme,
  validateTheme,
  type GitPulseTheme,
} from "./themeSchema";

const MINIMAL = {
  schema: THEME_SCHEMA_ID,
  name: "Midnight",
  appearance: "dark",
  colors: { base: "#0b0e17", accent: "#809eff" },
};

const FULL = {
  schema: THEME_SCHEMA_ID,
  name: "Solarized (dark)",
  appearance: "light",
  colors: {
    base: "#FDF6E3",
    accent: "#268bd2",
    contrast: 0.7,
    ok: "#859900",
    warn: "#b58900",
    danger: "#dc322f",
    info: "#2aa198",
    text: "#073642",
    textMuted: "#586e75",
  },
  field: { hues: ["#112233", "#445566", "#778899", "#aabbcc"], gain: 0.5 },
  shape: { radiusScale: 1.25 },
  fonts: { ui: '"Inter"', mono: "JetBrains Mono" },
};

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function failures(input: unknown): string[] {
  const result = validateTheme(input);
  if (result.ok) throw new Error("expected the theme to be rejected");
  return result.errors.map((error) => `${error.path}: ${error.message}`);
}

function paths(input: unknown): string[] {
  const result = validateTheme(input);
  return result.ok ? [] : result.errors.map((error) => error.path);
}

describe("validateTheme: accepted themes", () => {
  it("accepts the minimal theme and returns a fresh object", () => {
    const result = validateTheme(MINIMAL);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.theme).toEqual(MINIMAL);
    expect(result.theme).not.toBe(MINIMAL);
    expect(result.theme.colors).not.toBe(MINIMAL.colors);
    expect(result.ignoredKeys).toEqual([]);
  });

  it("accepts every field, lowercases hex, expands nothing, strips font quotes", () => {
    const result = validateTheme(FULL);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.theme.colors.base).toBe("#fdf6e3");
    expect(result.theme.fonts).toEqual({ ui: "Inter", mono: "JetBrains Mono" });
    expect(result.theme.field?.hues).toHaveLength(4);
  });

  it("accepts #rgb shorthand and normalizes it to #rrggbb", () => {
    const result = validateTheme({ ...MINIMAL, colors: { base: "#abc", accent: "#fa0" } });
    expect(result.ok && result.theme.colors).toEqual({ base: "#aabbcc", accent: "#ffaa00" });
  });

  it("accepts the range boundaries", () => {
    for (const contrast of [0, 1]) {
      expect(validateTheme({ ...MINIMAL, colors: { ...MINIMAL.colors, contrast } }).ok).toBe(true);
    }
    for (const gain of [0, 1]) {
      expect(validateTheme({ ...MINIMAL, field: { hues: ["#000", "#111", "#222", "#333"], gain } }).ok).toBe(true);
    }
    for (const radiusScale of [0.5, 1.25]) {
      expect(validateTheme({ ...MINIMAL, shape: { radiusScale } }).ok).toBe(true);
    }
  });

  it("accepts unicode names and font families", () => {
    expect(validateTheme({ ...MINIMAL, name: "Nacht – Übung" }).ok).toBe(false); // en dash is not a name character
    expect(validateTheme({ ...MINIMAL, name: "Nacht Übung", fonts: { ui: "Noto Sans JP", mono: "Fira-Code" } }).ok).toBe(true);
  });
});

describe("validateTheme: required structure", () => {
  it.each([null, undefined, 0, "theme", true, [], [MINIMAL]])("rejects a non-object root %j", (input) => {
    expect(validateTheme(input).ok).toBe(false);
  });

  it("rejects an empty object with every required key named", () => {
    expect(paths({})).toEqual(expect.arrayContaining(["schema", "name", "appearance", "colors"]));
  });

  it.each(["schema", "name", "appearance", "colors"])("rejects a missing %s", (key) => {
    const input: Record<string, unknown> = clone(MINIMAL);
    delete input[key];
    expect(paths(input)).toContain(key);
  });

  it("rejects the wrong schema id, including the next version", () => {
    expect(paths({ ...MINIMAL, schema: "gitpulse-theme/2" })).toContain("schema");
    expect(paths({ ...MINIMAL, schema: 1 })).toContain("schema");
  });

  it("rejects an unknown appearance", () => {
    expect(paths({ ...MINIMAL, appearance: "auto" })).toContain("appearance");
    expect(paths({ ...MINIMAL, appearance: "DARK" })).toContain("appearance");
  });

  it("requires colors.base and colors.accent", () => {
    expect(paths({ ...MINIMAL, colors: {} })).toEqual(expect.arrayContaining(["colors.base", "colors.accent"]));
  });

  it("requires both halves of field, shape and fonts when the block is present", () => {
    expect(paths({ ...MINIMAL, field: { gain: 0.5 } })).toContain("field.hues");
    expect(paths({ ...MINIMAL, field: { hues: ["#000", "#000", "#000", "#000"] } })).toContain("field.gain");
    expect(paths({ ...MINIMAL, shape: {} })).toContain("shape.radiusScale");
    expect(paths({ ...MINIMAL, fonts: { ui: "Inter" } })).toContain("fonts.mono");
  });

  it("reports every error at once, with paths", () => {
    const found = paths({ schema: "x", name: "", appearance: 3, colors: { base: "red", accent: 1, contrast: 9 } });
    expect(found).toEqual(
      expect.arrayContaining(["schema", "name", "appearance", "colors.base", "colors.accent", "colors.contrast"]),
    );
  });
});

describe("validateTheme: colors", () => {
  it.each([
    "",
    "red",
    "#12",
    "#12345",
    "#1234567",
    "#11223344",
    "rgb(1,2,3)",
    "hsl(0 0% 0%)",
    "oklch(0.5 0.1 100)",
    "color-mix(in srgb, red, blue)",
    "#fff;",
    "#fff}",
    "#fff url(x)",
    "var(--c-bg)",
    "url(javascript:alert(1))",
    "#ggg",
    " #fff",
    "#fff\n",
    "transparent",
    "currentColor",
  ])("rejects the color %j", (value) => {
    expect(paths({ ...MINIMAL, colors: { ...MINIMAL.colors, accent: value } })).toContain("colors.accent");
  });

  it.each([null, 0, 1, {}, [], true, ["#fff"]])("rejects the non-string color %j", (value) => {
    expect(paths({ ...MINIMAL, colors: { ...MINIMAL.colors, ok: value } })).toContain("colors.ok");
  });

  it.each([Number.NaN, Number.POSITIVE_INFINITY, -0.001, 1.001, "0.5", null, {}, [0.5]])(
    "rejects contrast %j",
    (value) => {
      expect(paths({ ...MINIMAL, colors: { ...MINIMAL.colors, contrast: value } })).toContain("colors.contrast");
    },
  );

  it("rejects an explicit undefined-less null for optional hex keys instead of skipping them", () => {
    expect(paths({ ...MINIMAL, colors: { ...MINIMAL.colors, text: null } })).toContain("colors.text");
  });
});

describe("validateTheme: numbers", () => {
  it.each([Number.NaN, -1, 1.5, "0.5", Number.POSITIVE_INFINITY])("rejects gain %j", (gain) => {
    expect(paths({ ...MINIMAL, field: { hues: ["#000", "#000", "#000", "#000"], gain } })).toContain("field.gain");
  });

  it.each([0.49, 1.26, 0, -1, Number.NaN, Number.NEGATIVE_INFINITY, "1", null])("rejects radiusScale %j", (radiusScale) => {
    expect(paths({ ...MINIMAL, shape: { radiusScale } })).toContain("shape.radiusScale");
  });

  it("rejects field.hues of the wrong length or content, naming the index", () => {
    expect(paths({ ...MINIMAL, field: { hues: ["#000"], gain: 0 } })).toContain("field.hues");
    expect(paths({ ...MINIMAL, field: { hues: ["#000", "#000", "#000", "#000", "#000"], gain: 0 } })).toContain("field.hues");
    expect(paths({ ...MINIMAL, field: { hues: ["#000", "x", "#000", "#000"], gain: 0 } })).toContain("field.hues[1]");
    expect(paths({ ...MINIMAL, field: { hues: "#000", gain: 0 } })).toContain("field.hues");
  });
});

describe("validateTheme: strings that could reach CSS", () => {
  const nameCases = [
    "url(https://evil.example/x.png)",
    "a url(x)",
    "var(--c-bg)",
    "calc(1px)",
    "expression(alert(1))",
    "x; color: red",
    "x{y}",
    "x } body { display:none",
    "z-index: 9999",
    "zIndex",
    "@import 'x'",
    "https://example.com",
    "//example.com/font.woff2",
    "/* c */",
    "back\\slash",
    "<script>alert(1)</script>",
    "<img src=x onerror=1>",
    "",
    "   ",
    "line\nbreak",
    "tab\there",
    "\u0000nul",
    "a".repeat(65),
    "a".repeat(100_000),
  ];

  it.each(nameCases)("rejects the name %j", (value) => {
    expect(paths({ ...MINIMAL, name: value })).toContain("name");
  });

  it("accepts a 64-character name and refuses 65", () => {
    expect(validateTheme({ ...MINIMAL, name: "a".repeat(64) }).ok).toBe(true);
    expect(validateTheme({ ...MINIMAL, name: "a".repeat(65) }).ok).toBe(false);
  });

  it("trims a name", () => {
    const result = validateTheme({ ...MINIMAL, name: "  Midnight  " });
    expect(result.ok && result.theme.name).toBe("Midnight");
  });

  const fontCases = [
    "url(https://example.com/font.woff2)",
    "https://example.com/font.woff2",
    "//cdn.example/f.ttf",
    "Inter; color: red",
    "Inter, sans-serif",
    "Inter, 'Evil'",
    "Inter } body {",
    "var(--x)",
    "local(Inter)",
    "font.woff2",
    "../font",
    "Inter\\00",
    "",
    "''",
    '""',
    "Inter\nMono",
    "a".repeat(65),
    "a".repeat(1_000_000),
  ];

  it.each(fontCases)("rejects the font family %j", (value) => {
    expect(paths({ ...MINIMAL, fonts: { ui: value, mono: "Menlo" } })).toContain("fonts.ui");
    expect(paths({ ...MINIMAL, fonts: { ui: "Menlo", mono: value } })).toContain("fonts.mono");
  });

  it("strips matched and stray quotes from a family name", () => {
    const result = validateTheme({ ...MINIMAL, fonts: { ui: "'SF Pro Text'", mono: '"Fira Code"' } });
    expect(result.ok && result.theme.fonts).toEqual({ ui: "SF Pro Text", mono: "Fira Code" });
  });

  it.each([null, 12, {}, [], true])("rejects the non-string font %j", (value) => {
    expect(paths({ ...MINIMAL, fonts: { ui: value, mono: "Menlo" } })).toContain("fonts.ui");
  });
});

describe("validateTheme: forbidden and unknown keys", () => {
  it.each(["zIndex", "z-index", "z_index", "duration", "durations", "transition", "animation", "easing", "filter", "backdropFilter", "blur", "url", "fontUrl", "fontFace", "css", "style", "import"])(
    "rejects a top-level %s key as an error, not an ignore",
    (key) => {
      const result = validateTheme({ ...MINIMAL, [key]: "anything" });
      expect(result.ok).toBe(false);
      if (!result.ok) expect(result.errors.map((e) => e.path)).toContain(key);
    },
  );

  it("rejects forbidden keys inside nested blocks", () => {
    expect(paths({ ...MINIMAL, shape: { radiusScale: 1, blur: 4 } })).toContain("shape.blur");
    expect(paths({ ...MINIMAL, fonts: { ui: "A", mono: "B", url: "https://x" } })).toContain("fonts.url");
    expect(paths({ ...MINIMAL, colors: { ...MINIMAL.colors, filter: "blur(4px)" } })).toContain("colors.filter");
  });

  it("reports and ignores unknown keys, at every level", () => {
    const result = validateTheme({
      ...clone(MINIMAL),
      author: "me",
      colors: { ...MINIMAL.colors, purple: "#800080" },
      shape: { radiusScale: 1, extra: 1 },
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect([...result.ignoredKeys].sort()).toEqual(["author", "colors.purple", "shape.extra"]);
    expect(JSON.stringify(result.theme)).not.toContain("author");
    expect(JSON.stringify(result.theme)).not.toContain("purple");
  });

  it("handles 1000 extra keys: all reported, none leak into the theme", () => {
    const input: Record<string, unknown> = clone(MINIMAL);
    for (let i = 0; i < 1000; i += 1) input[`extra${i}`] = { nested: i };
    const result = validateTheme(input);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.ignoredKeys).toHaveLength(1000);
    expect(Object.keys(result.theme).sort()).toEqual(["appearance", "colors", "name", "schema"]);
  });

  it("refuses an object with more keys than the limit rather than scanning it", () => {
    const input: Record<string, unknown> = clone(MINIMAL);
    for (let i = 0; i <= MAX_KEYS_PER_OBJECT; i += 1) input[`k${i}`] = i;
    const errors = failures(input);
    expect(errors.join("\n")).toContain("limit");
    const nested = { ...clone(MINIMAL), colors: { ...MINIMAL.colors, ...Object.fromEntries(Array.from({ length: 5000 }, (_, i) => [`c${i}`, 1])) } };
    expect(failures(nested).join("\n")).toContain("colors");
  });

  it("truncates a very long unknown key in the report", () => {
    const result = validateTheme({ ...MINIMAL, ["k".repeat(100_000)]: 1 });
    expect(result.ok && result.ignoredKeys[0].length).toBeLessThan(100);
  });
});

describe("validateTheme: prototype pollution and exotic objects", () => {
  it("treats an own __proto__ key as an ignored unknown key and pollutes nothing", () => {
    const input: unknown = JSON.parse(
      '{"__proto__":{"polluted":true},"constructor":{"prototype":{"polluted":true}},"schema":"gitpulse-theme/1","name":"x","appearance":"dark","colors":{"base":"#000","accent":"#fff","__proto__":{"polluted":true}}}',
    );
    const result = validateTheme(input);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.ignoredKeys).toEqual(expect.arrayContaining(["__proto__", "constructor", "colors.__proto__"]));
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
    expect(Object.getPrototypeOf(result.theme)).toBe(Object.prototype);
    expect(Object.hasOwn(result.theme, "polluted")).toBe(false);
  });

  it("does not read inherited values as if they were present", () => {
    const inherited = Object.create({ schema: THEME_SCHEMA_ID, name: "x", appearance: "dark", colors: MINIMAL.colors });
    expect(validateTheme(inherited).ok).toBe(false);
  });

  it("does not read through a prototype-supplied color", () => {
    const colors = Object.create({ base: "#000000", accent: "#ffffff" });
    expect(paths({ ...MINIMAL, colors })).toEqual(expect.arrayContaining(["colors.base", "colors.accent"]));
  });

  it.each([
    ["colors as a string", { ...MINIMAL, colors: "#000" }, "colors"],
    ["colors as an array", { ...MINIMAL, colors: [] }, "colors"],
    ["colors as null", { ...MINIMAL, colors: null }, "colors"],
    ["base as an object", { ...MINIMAL, colors: { base: { value: "#000" }, accent: "#fff" } }, "colors.base"],
    ["base as an array", { ...MINIMAL, colors: { base: ["#000"], accent: "#fff" } }, "colors.base"],
    ["name as an object", { ...MINIMAL, name: { toString: "x" } }, "name"],
    ["field as an array", { ...MINIMAL, field: [] }, "field"],
    ["shape as a number", { ...MINIMAL, shape: 1 }, "shape"],
    ["fonts as a string", { ...MINIMAL, fonts: "Inter" }, "fonts"],
    ["appearance as an array", { ...MINIMAL, appearance: ["dark"] }, "appearance"],
  ])("rejects %s", (_label, input, path) => {
    expect(paths(input)).toContain(path);
  });

  it("never throws on hostile values", () => {
    const hostile: unknown[] = [
      { ...MINIMAL, name: { toString() { throw new Error("boom"); } } },
      { ...MINIMAL, colors: { base: Symbol("x"), accent: 1n } },
      Object.freeze(clone(MINIMAL)),
      new Map(),
      new Proxy({}, { ownKeys: () => ["schema"], getOwnPropertyDescriptor: () => ({ configurable: true, enumerable: true, value: 1 }) }),
    ];
    for (const input of hostile) expect(() => validateTheme(input)).not.toThrow();
  });
});

describe("exportTheme", () => {
  const inputs: Omit<GitPulseTheme, "schema"> = {
    name: "Round trip",
    appearance: "dark",
    colors: { base: "#0b0e17", accent: "#809eff", contrast: 0.5, ok: "#33cc77" },
    field: { hues: ["#112233", "#445566", "#778899", "#aabbcc"], gain: 0.5 },
    shape: { radiusScale: 1 },
    fonts: { ui: "Inter", mono: "Menlo" },
  };

  it("round-trips through the validator without loss", () => {
    const exported = exportTheme(inputs);
    expect(exported.ok).toBe(true);
    if (!exported.ok) return;
    const again = validateTheme(JSON.parse(exported.json));
    expect(again.ok).toBe(true);
    if (!again.ok) return;
    expect(again.theme).toEqual(exported.theme);
    expect(again.ignoredKeys).toEqual([]);
    expect(exported.theme).toEqual({ schema: THEME_SCHEMA_ID, ...inputs });
  });

  it("round-trips a minimal theme and normalizes shorthand hex on the way out", () => {
    const exported = exportTheme({ name: "Mini", appearance: "light", colors: { base: "#fff", accent: "#00f" } });
    expect(exported.ok && exported.theme.colors).toEqual({ base: "#ffffff", accent: "#0000ff" });
  });

  it("refuses to write a theme the validator would refuse", () => {
    const exported = exportTheme({ ...inputs, colors: { base: "url(x)", accent: "#fff" } });
    expect(exported.ok).toBe(false);
    if (!exported.ok) expect(exported.errors.map((e) => e.path)).toContain("colors.base");
  });

  it("does not let a caller override the schema id", () => {
    const exported = exportTheme({ ...inputs, schema: "other" } as Omit<GitPulseTheme, "schema">);
    expect(exported.ok && exported.theme.schema).toBe(THEME_SCHEMA_ID);
  });
});
