import { readFileSync } from "node:fs";
import { get, writable } from "svelte/store";
import { afterEach, describe, expect, it } from "vitest";
import {
  APPEARANCE_DEFAULTS,
  FLOAT_BLUR_MAX,
  MANAGED_ATTRIBUTES,
  MANAGED_VARS,
  MATERIAL_FLOOR,
  SHIPPED_FIELD,
  appearanceNotes,
  installAppearance,
  nearestPreset,
  planAppearance,
  resolveAccent,
  sanitizeAppearance,
  writeAppearance,
  type AppearancePrefs,
  type AppearanceTarget,
} from "./apply";
import { setAccentOverride } from "../accents";
import { contrastRatio, parseHex } from "../color/oklch";
import { SHIPPED_LADDER } from "../color/derive";
import { TEXT_MIN } from "../color/guard";
import { TRANSPARENCY_ATTRIBUTE, applyTransparency, setUserTransparencyReduced } from "../transparencyPreference";
import { exportTheme } from "./themeSchema";

const css = readFileSync(new URL("../../../app.css", import.meta.url), "utf8");

const mac = { theme: "dark", nativeMaterial: true } as const;
const macLight = { theme: "light", nativeMaterial: true } as const;
const elsewhere = { theme: "dark", nativeMaterial: false } as const;

const both = (n: number) => ({ light: n, dark: n });

function prefs(changes: Partial<AppearancePrefs> = {}): AppearancePrefs {
  return { ...APPEARANCE_DEFAULTS, ...changes };
}

class FakeRoot implements AppearanceTarget {
  readonly values = new Map<string, string>();
  readonly attributes = new Map<string, string>();
  readonly style = {
    setProperty: (name: string, value: string) => void this.values.set(name, value),
    removeProperty: (name: string) => void this.values.delete(name),
  };
  setAttribute(name: string, value: string) {
    this.attributes.set(name, value);
  }
  removeAttribute(name: string) {
    this.attributes.delete(name);
  }
}

afterEach(() => {
  setAccentOverride(null);
  setUserTransparencyReduced(false, new FakeRoot());
  setUserTransparencyReduced(false);
  applyTransparency("standard");
});

describe("planAppearance: defaults", () => {
  it("changes nothing a fresh install does not already draw", () => {
    const plan = planAppearance(prefs(), mac);
    expect(plan.vars).toEqual({});
    expect(plan.accentOverride).toBeNull();
    expect(plan.transparencyReduced).toBe(false);
    expect(plan.attributes).toEqual({ "data-density": "comfortable", "data-radius-scale": null, "data-float-blur": null });
  });
});

describe("planAppearance: float blur", () => {
  it("yields none, not blur(0), at 0", () => {
    const plan = planAppearance(prefs({ floatBlur: 0 }), mac);
    expect(plan.attributes["data-float-blur"]).toBe("off");
    expect(plan.vars["--gp-float-blur"]).toBeUndefined();
  });

  it("writes the radius for anything between, and nothing at the shipped 34", () => {
    expect(planAppearance(prefs({ floatBlur: 20 }), mac).vars["--gp-float-blur"]).toBe("20px");
    expect(planAppearance(prefs({ floatBlur: FLOAT_BLUR_MAX }), mac).vars["--gp-float-blur"]).toBe("40px");
    expect(planAppearance(prefs({ floatBlur: 34 }), mac).vars["--gp-float-blur"]).toBeUndefined();
  });

  it("clamps into 0..40", () => {
    expect(planAppearance(prefs({ floatBlur: 400 }), mac).vars["--gp-float-blur"]).toBe("40px");
    expect(planAppearance(prefs({ floatBlur: -5 }), mac).attributes["data-float-blur"]).toBe("off");
  });
});

describe("planAppearance: material intensity", () => {
  it("never draws thinner than the measured floor, and says so", () => {
    const plan = planAppearance(prefs({ materialIntensity: both(0) }), macLight);
    expect(Number(plan.vars["--gp-material"])).toBeCloseTo(MATERIAL_FLOOR.light, 5);
    expect(plan.notes.map((note) => note.id)).toContain("material-floor");
  });

  it("lets the dark theme sit at the shipped alphas with no note", () => {
    const plan = planAppearance(prefs({ materialIntensity: both(0) }), mac);
    expect(plan.vars["--gp-material"]).toBeUndefined();
    expect(plan.notes).toEqual([]);
  });

  it("passes a higher intensity through unchanged", () => {
    expect(planAppearance(prefs({ materialIntensity: both(0.6) }), mac).vars["--gp-material"]).toBe("0.6");
    expect(planAppearance(prefs({ materialIntensity: both(5) }), mac).vars["--gp-material"]).toBe("1");
  });
});

describe("material intensity is stored per appearance", () => {
  it("lets light sit at its floor while dark keeps its own value", () => {
    const stored = prefs({ materialIntensity: { light: 0, dark: 0.6 } });
    expect(Number(planAppearance(stored, macLight).vars["--gp-material"])).toBeCloseTo(MATERIAL_FLOOR.light, 5);
    expect(planAppearance(stored, mac).vars["--gp-material"]).toBe("0.6");
  });

  it("seeds both appearances from the single number older builds stored", () => {
    expect(sanitizeAppearance({ materialIntensity: 0.7 }).materialIntensity).toEqual({ light: 0.7, dark: 0.7 });
  });

  it("clamps each half of a stored pair on its own and ignores junk", () => {
    expect(sanitizeAppearance({ materialIntensity: { light: 9, dark: "x" } }).materialIntensity).toEqual({ light: 1, dark: 0 });
    expect(sanitizeAppearance({ materialIntensity: null }).materialIntensity).toEqual({ light: 0, dark: 0 });
  });
});

describe("planAppearance: capability gating", () => {
  it("writes no glass token where the host has no native material", () => {
    const plan = planAppearance(
      prefs({ materialIntensity: both(0.8), floatBlur: 0, fieldLevel: "off", fieldHue: 90, reduceTransparency: true }),
      elsewhere,
    );
    expect(plan.vars).toEqual({});
    expect(plan.attributes["data-float-blur"]).toBeNull();
    expect(plan.transparencyReduced).toBe(false);
  });

  it("still applies density, corner style and fonts everywhere", () => {
    const plan = planAppearance(prefs({ density: "compact", radiusScale: 0.5, fontUi: "Inter" }), elsewhere);
    expect(plan.attributes["data-density"]).toBe("compact");
    expect(plan.vars["--gp-radius-scale"]).toBe("0.5");
    expect(plan.attributes["data-radius-scale"]).toBe("0.5");
    expect(plan.vars["--gp-font-ui"]).toMatch(/^"Inter", ui-sans-serif/);
  });
});

describe("planAppearance: background field", () => {
  it("never exceeds the shipped gain, and scales it by level", () => {
    expect(planAppearance(prefs({ fieldLevel: "vivid" }), mac).vars["--mac-hue-gain"]).toBeUndefined();
    expect(planAppearance(prefs({ fieldLevel: "subtle" }), mac).vars["--mac-hue-gain"]).toBe("0.5");
    expect(planAppearance(prefs({ fieldLevel: "off" }), mac).vars["--mac-hue-gain"]).toBe("0");
    // Light ships at 0.5; vivid is that, subtle half of it.
    expect(planAppearance(prefs({ fieldLevel: "subtle" }), macLight).vars["--mac-hue-gain"]).toBe("0.25");
  });

  it("rotates all four hues together and keeps them in gamut", () => {
    const plan = planAppearance(prefs({ fieldHue: 120 }), mac);
    for (const name of ["--mac-hue-a", "--mac-hue-b", "--mac-hue-c", "--mac-hue-d"]) {
      const channels = plan.vars[name].split(" ").map(Number);
      expect(channels).toHaveLength(3);
      for (const channel of channels) {
        expect(channel).toBeGreaterThanOrEqual(0);
        expect(channel).toBeLessThanOrEqual(255);
      }
    }
    expect(plan.vars["--mac-hue-a"]).not.toBe(SHIPPED_FIELD.dark.hues[0].join(" "));
  });

  it("transcribes the hues and gain app.css ships", () => {
    const dark = css.slice(css.indexOf("html.macos {\n  /* The hue field"));
    const light = css.slice(css.indexOf("html.macos.light {\n  /* Light surfaces invert"));
    for (const [text, appearance] of [
      [dark, "dark"],
      [light, "light"],
    ] as const) {
      const hues = ["a", "b", "c", "d"].map((key) => {
        const match = new RegExp(`--mac-hue-${key}:\\s*(\\d+) (\\d+) (\\d+)`).exec(text);
        expect(match, `${appearance} hue ${key}`).toBeTruthy();
        return [Number(match![1]), Number(match![2]), Number(match![3])];
      });
      expect(hues).toEqual(SHIPPED_FIELD[appearance].hues.map((hue) => [...hue]));
      const gain = /--mac-hue-gain:\s*([\d.]+)/.exec(text);
      expect(Number(gain![1])).toBe(SHIPPED_FIELD[appearance].gain);
    }
  });
});

describe("planAppearance: type and shape", () => {
  it("drops a font name that is not a family name", () => {
    const plan = planAppearance(prefs({ fontUi: "x; background:url(//e)", fontMono: "JetBrains Mono" }), mac);
    expect(plan.vars["--gp-font-ui"]).toBeUndefined();
    expect(plan.vars["--gp-font-mono"]).toBe(
      '"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
    );
  });

  it("scales the radius ladder only off the shipped 1", () => {
    expect(planAppearance(prefs({ radiusScale: 1.25 }), mac).vars["--gp-radius-scale"]).toBe("1.25");
    expect(planAppearance(prefs({ radiusScale: 1 }), mac).vars["--gp-radius-scale"]).toBeUndefined();
    expect(planAppearance(prefs({ radiusScale: 9 }), mac).vars["--gp-radius-scale"]).toBe("1.25");
  });
});

describe("accent through the contrast guard", () => {
  it("passes a colour that already reads", () => {
    const outcome = resolveAccent(prefs({ customAccent: "#8aa4ff" }), "dark");
    expect(outcome.status).toBe("passes");
  });

  it("adjusts a colour that does not, to something that reads on every surface", () => {
    // Near-black on a near-black ground.
    const outcome = resolveAccent(prefs({ customAccent: "#1d2a52" }), "dark");
    expect(outcome.status).toBe("adjusted");
    if (outcome.status !== "adjusted") return;
    const resolved = parseHex(outcome.resolved)!;
    for (const surface of [SHIPPED_LADDER.dark.bg, SHIPPED_LADDER.dark.surface, SHIPPED_LADDER.dark.surfaceHover]) {
      expect(contrastRatio(resolved, surface)).toBeGreaterThanOrEqual(TEXT_MIN);
    }
    const plan = planAppearance(prefs({ customAccent: "#1d2a52" }), mac);
    expect(plan.accentOverride).not.toBeNull();
    expect(plan.notes.some((note) => note.id === "accent-adjusted")).toBe(true);
  });

  it("derives a separate shade per theme", () => {
    const dark = planAppearance(prefs({ customAccent: "#3b5bdb" }), mac).accentOverride;
    const light = planAppearance(prefs({ customAccent: "#3b5bdb" }), macLight).accentOverride;
    expect(dark).not.toBeNull();
    expect(light).not.toBeNull();
    expect(dark).not.toBe(light);
  });

  it("leaves a preset to the stylesheet and applyAccent", () => {
    expect(planAppearance(prefs({ accent: "rose" }), mac).accentOverride).toBeNull();
  });

  it("picks the nearest preset by hue and lightness", () => {
    expect(nearestPreset("#e0606e", "dark")).toBe("rose");
    expect(nearestPreset("not a colour", "dark")).toBe("blue");
  });
});

describe("imported theme", () => {
  const exported = exportTheme({
    name: "Dusk",
    appearance: "dark",
    colors: { base: "#14101f", accent: "#c08cff", contrast: 0.5 },
  });
  if (!exported.ok) throw new Error("fixture theme invalid");
  const theme = exported.theme;

  it("draws only in its own appearance", () => {
    const dark = planAppearance(prefs({ customTheme: theme }), mac);
    expect(dark.vars["--c-bg"]).toBeDefined();
    expect(dark.vars["--c-surface"]).toBeDefined();
    const light = planAppearance(prefs({ customTheme: theme }), macLight);
    expect(light.vars["--c-bg"]).toBeUndefined();
    expect(light.notes.map((note) => note.id)).toContain("theme-other-mode");
  });

  it("hands the accent to applyAccent, the one writer of --c-accent", () => {
    const plan = planAppearance(prefs({ customTheme: theme }), mac);
    expect(plan.vars["--c-accent"]).toBeUndefined();
    expect(plan.accentOverride).not.toBeNull();
  });

  it("is refused, with the reason, when the guard cannot honour it", () => {
    const mid = exportTheme({ name: "Mid", appearance: "dark", colors: { base: "#808080", accent: "#808080" } });
    if (!mid.ok) throw new Error("fixture theme invalid");
    const plan = planAppearance(prefs({ customTheme: mid.theme }), mac);
    expect(plan.vars["--c-bg"]).toBeUndefined();
    const note = plan.notes.find((entry) => entry.id === "theme-refused");
    expect(note?.tone).toBe("warn");
    expect(note?.text).toContain("Mid");
  });
});

describe("writeAppearance", () => {
  it("sets what the plan sets and clears what it no longer sets", () => {
    const root = new FakeRoot();
    writeAppearance(planAppearance(prefs({ floatBlur: 0, materialIntensity: both(0.5), density: "compact" }), mac), root);
    expect(root.attributes.get("data-float-blur")).toBe("off");
    expect(root.values.get("--gp-material")).toBe("0.5");
    expect(root.attributes.get("data-density")).toBe("compact");
    writeAppearance(planAppearance(prefs(), mac), root);
    expect(root.values.size).toBe(0);
    expect(root.attributes.has("data-float-blur")).toBe(false);
    expect(root.attributes.get("data-density")).toBe("comfortable");
  });

  it("only ever touches the properties it declares", () => {
    const root = new FakeRoot();
    root.values.set("--c-surface-tint", "keep");
    root.values.set("--mac-fill-surface", "keep");
    writeAppearance(planAppearance(prefs({ materialIntensity: both(1) }), mac), root);
    expect(root.values.get("--c-surface-tint")).toBe("keep");
    // The OUTPUT variable the Reduce Transparency blocks redeclare is never written inline.
    expect(root.values.get("--mac-fill-surface")).toBe("keep");
    expect(MANAGED_VARS).not.toContain("--mac-fill-surface");
    expect(MANAGED_VARS).not.toContain("--mac-glass-fill");
    expect(MANAGED_VARS).not.toContain("--mac-glass-blur-float");
    expect(MANAGED_VARS).not.toContain("--c-accent");
    expect(MANAGED_ATTRIBUTES).not.toContain(TRANSPARENCY_ATTRIBUTE);
  });
});

describe("reduced transparency still wins", () => {
  it("is added to by the reader's choice, never taken from the system", () => {
    const root = new FakeRoot();
    applyTransparency("standard", root);
    expect(root.attributes.has(TRANSPARENCY_ATTRIBUTE)).toBe(false);
    setUserTransparencyReduced(true, root);
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced");
    // The window regains focus and the system answer is re-read as standard.
    applyTransparency("standard", root);
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced");
    // Turning the choice off hands control back to the system.
    setUserTransparencyReduced(false, root);
    expect(root.attributes.has(TRANSPARENCY_ATTRIBUTE)).toBe(false);
    applyTransparency("reduced", root);
    setUserTransparencyReduced(false, root);
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced");
    applyTransparency("standard", root);
  });

  it("treats an unknown system answer as no reason to override the reader", () => {
    const root = new FakeRoot();
    applyTransparency("unknown", root);
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("unknown");
    setUserTransparencyReduced(true, root);
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced");
    setUserTransparencyReduced(false, root);
    applyTransparency("standard", root);
  });

  it("keeps the opaque overrides in the stylesheet and the inputs out of them", () => {
    // The outputs are computed from inputs in the normal blocks...
    expect(css).toContain("--mac-fill-surface: rgb(var(--c-surface) / calc(0.5 + 0.5 * var(--gp-material, 0)))");
    // ...and the reduced blocks redeclare them as opaque colours, which an input cannot override.
    const reduced = css.slice(css.indexOf('html.macos[data-transparency="reduced"],\nhtml.macos.light[data-transparency="reduced"] {'));
    expect(reduced).toMatch(/--mac-fill-surface: rgb\(var\(--c-surface\)\);/);
    expect(reduced).not.toMatch(/--gp-material/);
    // Float blur 0 only ever adds a `none`.
    expect(css).toMatch(
      /html\.macos\[data-float-blur="off"\] :is\(\.gp-menu, \[class~="shadow-float"\], \[class~="shadow-pop"\]\) \{\s*-webkit-backdrop-filter: none;\s*backdrop-filter: none;/,
    );
    expect(css).toContain("--mac-glass-blur-float: var(--gp-float-blur, 34px);");
  });
});

describe("sanitizeAppearance", () => {
  it("turns garbage into defaults and clamps ranges", () => {
    expect(sanitizeAppearance({})).toEqual(APPEARANCE_DEFAULTS);
    const read = sanitizeAppearance({
      accent: "nope",
      customAccent: "javascript:1",
      customTheme: { schema: "gitpulse-theme/1", name: "x", appearance: "dark", colors: { base: "url(x)", accent: "#fff" } },
      fontUi: "a;b",
      fontMono: 5,
      density: "huge",
      radiusScale: 100,
      materialIntensity: both(-3),
      floatBlur: 900,
      fieldLevel: "loud",
      fieldHue: 725,
      reduceTransparency: "yes",
    });
    expect(read.accent).toBe("blue");
    expect(read.customAccent).toBe("");
    expect(read.customTheme).toBeNull();
    expect(read.fontUi).toBe("");
    expect(read.fontMono).toBe("");
    expect(read.density).toBe("comfortable");
    expect(read.radiusScale).toBe(1.25);
    expect(read.materialIntensity).toEqual({ light: 0, dark: 0 });
    expect(read.floatBlur).toBe(40);
    expect(read.fieldLevel).toBe("vivid");
    expect(read.fieldHue).toBe(5);
    expect(read.reduceTransparency).toBe(false);
  });

  it("re-validates a stored theme", () => {
    const ok = exportTheme({ name: "Ok", appearance: "light", colors: { base: "#f3f4fa", accent: "#3040af" } });
    if (!ok.ok) throw new Error("fixture invalid");
    expect(sanitizeAppearance({ customTheme: ok.theme }).customTheme?.name).toBe("Ok");
  });
});

describe("installAppearance", () => {
  it("keeps the document in step with the stores, and with the theme", () => {
    const root = new FakeRoot();
    const store = writable<AppearancePrefs>(prefs());
    const theme = writable<"dark" | "light">("dark");
    const platform = writable({ native_material: true });
    const stop = installAppearance({ prefs: store, theme, platform, target: root });
    expect(root.attributes.get("data-density")).toBe("comfortable");
    store.set(prefs({ materialIntensity: both(0) }));
    expect(root.values.has("--gp-material")).toBe(false);
    theme.set("light");
    expect(Number(root.values.get("--gp-material"))).toBeCloseTo(MATERIAL_FLOOR.light, 5);
    expect(get(appearanceNotes).map((note) => note.id)).toContain("material-floor");
    store.set(prefs({ reduceTransparency: true }));
    // The reader's choice reaches the bridge, which owns the attribute on the real document.
    const probe = new FakeRoot();
    applyTransparency("standard", probe);
    expect(probe.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced");
    stop();
    store.set(prefs({ density: "compact" }));
    expect(root.attributes.get("data-density")).toBe("comfortable");
  });
});
