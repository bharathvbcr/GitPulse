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
  WALLPAPER_INTENSITY_RANGE,
  backdropDim,
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
import { SHIPPED_LADDER, SYNTAX_KEYS } from "../color/derive";
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
    expect(plan.attributes).toEqual({
      "data-density": "comfortable",
      "data-radius-scale": null,
      "data-float-blur": null,
      "data-backdrop": null,
      "data-backdrop-dim": null,
    });
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

  it("writes the guarded syntax colours with the palette, and clears them when no palette applies", () => {
    const on = planAppearance(prefs({ customTheme: theme }), mac);
    for (const key of SYNTAX_KEYS) {
      expect(on.vars[key], key).toMatch(/^rgb\(\d+ \d+ \d+\)$/);
      expect(MANAGED_VARS).toContain(key);
    }
    const off = planAppearance(prefs(), mac);
    for (const key of SYNTAX_KEYS) expect(off.vars[key], key).toBeUndefined();
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

describe("the wallpaper", () => {
  it("writes the input variable and attribute only where the window material exists", () => {
    const plan = planAppearance(prefs({ backdrop: "neural" }), mac);
    expect(plan.vars["--gp-backdrop"]).toMatch(/^url\("data:image\/svg\+xml,/);
    expect(plan.attributes["data-backdrop"]).toBe("neural");
    const other = planAppearance(prefs({ backdrop: "neural" }), elsewhere);
    expect(other.vars["--gp-backdrop"]).toBeUndefined();
    expect(other.attributes["data-backdrop"]).toBeNull();
    expect(other.notes.map((note) => note.id)).toContain("backdrop-host");
  });

  it("at full intensity writes no fade, so the still is drawn exactly as before", () => {
    const plan = planAppearance(prefs({ backdrop: "neural" }), mac);
    expect(plan.vars["--gp-backdrop-dim"]).toBeUndefined();
    expect(plan.attributes["data-backdrop-dim"]).toBeNull();
  });

  it("below full intensity fades by the remainder, through one variable the still and the moving layer share", () => {
    const plan = planAppearance(prefs({ backdrop: "neural", wallpaperIntensity: 0.6 }), mac);
    expect(plan.vars["--gp-backdrop-dim"]).toBe("0.4");
    expect(plan.attributes["data-backdrop-dim"]).toBe("");
    expect(MANAGED_VARS).toContain("--gp-backdrop-dim");
    expect(MANAGED_ATTRIBUTES).toContain("data-backdrop-dim");
    // Nothing to fade without a wallpaper.
    const none = planAppearance(prefs({ wallpaperIntensity: 0.6 }), mac);
    expect(none.vars["--gp-backdrop-dim"]).toBeUndefined();
  });

  it("holds intensity to its range on read: it can only fade, never brighten or vanish", () => {
    expect(backdropDim(1)).toBe(0);
    expect(backdropDim(5)).toBe(0);
    expect(backdropDim(0)).toBe(Number((1 - WALLPAPER_INTENSITY_RANGE.min).toFixed(3)));
    expect(backdropDim(Number.NaN)).toBe(0);
    expect(sanitizeAppearance({ wallpaperIntensity: 7 }).wallpaperIntensity).toBe(1);
    expect(sanitizeAppearance({ wallpaperIntensity: -1 }).wallpaperIntensity).toBe(WALLPAPER_INTENSITY_RANGE.min);
    expect(sanitizeAppearance({ wallpaperIntensity: "loud" }).wallpaperIntensity).toBe(1);
    expect(sanitizeAppearance({}).wallpaperMotion).toBe(false);
    expect(sanitizeAppearance({ wallpaperMotion: "yes" }).wallpaperMotion).toBe(false);
    expect(sanitizeAppearance({ wallpaperMotion: true }).wallpaperMotion).toBe(true);
  });

  it("clears both when the wallpaper goes back to none", () => {
    const root = new FakeRoot();
    writeAppearance(planAppearance(prefs({ backdrop: "hex" }), mac), root);
    expect(root.values.has("--gp-backdrop")).toBe(true);
    writeAppearance(planAppearance(prefs(), mac), root);
    expect(root.values.has("--gp-backdrop")).toBe(false);
    expect(root.attributes.has("data-backdrop")).toBe(false);
    expect(MANAGED_VARS).toContain("--gp-backdrop");
    expect(MANAGED_ATTRIBUTES).toContain("data-backdrop");
  });

  it("draws the reader's image only once it is ready, and says so when it is gone", () => {
    const loading = planAppearance(prefs({ backdrop: "custom" }), { ...mac, customBackdrop: { status: "loading" } });
    expect(loading.vars["--gp-backdrop"]).toBeUndefined();
    expect(loading.notes).toEqual([]);
    const ready = planAppearance(prefs({ backdrop: "custom" }), {
      ...mac,
      customBackdrop: { status: "ready", url: "blob:http://localhost/abc-123" },
    });
    expect(ready.vars["--gp-backdrop"]).toBe('url("blob:http://localhost/abc-123")');
    expect(ready.attributes["data-backdrop"]).toBe("custom");
    for (const status of ["none", "unavailable"] as const) {
      const gone = planAppearance(prefs({ backdrop: "custom" }), { ...mac, customBackdrop: { status } });
      expect(gone.vars["--gp-backdrop"]).toBeUndefined();
      expect(gone.notes.map((note) => note.id)).toContain("backdrop-missing");
    }
  });

  it("never writes a URL that could break out of url(\"…\")", () => {
    const plan = planAppearance(prefs({ backdrop: "custom" }), {
      ...mac,
      customBackdrop: { status: "ready", url: 'blob:x") ; background: red' },
    });
    expect(plan.vars["--gp-backdrop"]).toBeUndefined();
  });

  it("tells the reader when reduced transparency or an opaque material hides it", () => {
    expect(
      planAppearance(prefs({ backdrop: "neural", reduceTransparency: true }), mac).notes.map((note) => note.id),
    ).toContain("backdrop-hidden");
    expect(
      planAppearance(prefs({ backdrop: "neural", materialIntensity: both(1) }), mac).notes.map((note) => note.id),
    ).toContain("backdrop-hidden");
    expect(planAppearance(prefs({ backdrop: "neural" }), mac).notes.map((note) => note.id)).not.toContain(
      "backdrop-hidden",
    );
  });

  it("reads a stored choice it does not know as none, and keeps saved themes validated", () => {
    expect(sanitizeAppearance({ backdrop: "url(evil)" }).backdrop).toBe("none");
    expect(sanitizeAppearance({ backdrop: "reactor" }).backdrop).toBe("reactor");
    expect(sanitizeAppearance({ savedThemes: [{ name: "x" }] }).savedThemes).toEqual([]);
  });

  it("is painted on body beneath the shell veil, and solid surfaces remove it", () => {
    // Beneath the veil is the contrast argument: the material model's
    // white/black-desktop proof covers a picture only from below the veil.
    const selector = "html.macos[data-backdrop] body {";
    const draw = css.indexOf(`${selector}\n  background-image: var(--gp-backdrop);`);
    expect(draw).toBeGreaterThan(-1);
    expect(css).not.toMatch(/\.gp-shell[^{]*\{[^}]*--gp-backdrop/);
    // The solid-surface preferences remove it with the same selector, later in the cascade.
    const media = css.indexOf("@media (prefers-reduced-transparency: reduce), (prefers-contrast: more), (forced-colors: active) {");
    const hide = css.indexOf(`  ${selector}\n    background-image: none;`, media);
    expect(media).toBeGreaterThan(draw);
    expect(hide).toBeGreaterThan(media);
    // And the in-app choice, keyed on the attribute (more specific, so it wins anywhere).
    expect(css).toContain('html.macos[data-transparency="reduced"][data-backdrop] body {\n  background-image: none;');
  });

  it("removes the faded and the moving forms under the same preferences", () => {
    // The fade's selector is more specific than the plain hide, so it needs its own.
    const media = css.indexOf("@media (prefers-reduced-transparency: reduce), (prefers-contrast: more), (forced-colors: active) {");
    expect(css.indexOf("  html.macos[data-backdrop][data-backdrop-dim] body {\n    background-image: none;", media)).toBeGreaterThan(media);
    expect(css.indexOf("  html.macos .gp-wallpaper-motion {\n    display: none;", media)).toBeGreaterThan(media);
    expect(css).toContain('html.macos[data-transparency="reduced"][data-backdrop][data-backdrop-dim] body {\n  background-image: none;');
    expect(css).toContain('html.macos[data-transparency="reduced"] .gp-wallpaper-motion {\n  display: none;');
    // The moving layer sits under body, below every in-flow box, and moves only transform and opacity.
    const layer = /\.gp-wallpaper-motion \{([^}]*)\}/.exec(css)?.[1] ?? "";
    expect(layer).toMatch(/position: fixed;/);
    expect(layer).toMatch(/z-index: -1;/);
    for (const [, name, body] of css.matchAll(/@keyframes (gp-wp-[a-z]+) \{([\s\S]*?)\n\}/g)) {
      const properties = [...body.matchAll(/^\s*([a-z-]+):/gm)].map((match) => match[1]);
      expect(properties.every((property) => property === "transform" || property === "opacity"), name).toBe(true);
    }
    // Movement is declared only where motion is welcome.
    const motionBlock = css.indexOf("html:not([data-motion=\"reduced\"]) .gp-wallpaper-motion > .gp-wp-drift");
    expect(css.lastIndexOf("@media (prefers-reduced-motion: no-preference) {", motionBlock)).toBeGreaterThan(-1);
    expect(css).not.toMatch(/\.gp-wallpaper-motion[^{]*\{[^}]*\bfilter:/);
  });
});

describe("the material floor follows the palette in force", () => {
  it("uses the shipped floor with no theme, and a measured one with a theme", () => {
    expect(planAppearance(prefs(), mac).materialFloor).toBe(MATERIAL_FLOOR.dark);
    expect(planAppearance(prefs(), macLight).materialFloor).toBe(MATERIAL_FLOOR.light);
    // A pure black palette: its veil and plates are black, so over a white
    // desktop the thin glass lifts the ground toward its muted ink. The
    // shipped dark floor (0) would let the slider reach a stack that fails AA.
    const black = exportTheme({ name: "Black", appearance: "dark", colors: { base: "#000000", accent: "#d4d4d8" } });
    if (!black.ok) throw new Error("fixture invalid");
    const plan = planAppearance(prefs({ customTheme: black.theme, materialIntensity: both(0) }), mac);
    expect(plan.materialFloor).toBeGreaterThan(MATERIAL_FLOOR.dark);
    // And the window is held there, with the note the reader is owed.
    expect(Number(plan.vars["--gp-material"])).toBe(plan.materialFloor);
    expect(plan.notes.map((note) => note.id)).toContain("material-floor");
  });
});

describe("brand logo style & palette appearance tokens", () => {
  it("sanitizes unknown logoStyle and logoPalette values back to defaults", () => {
    const clean = sanitizeAppearance({ logoStyle: "compass", logoPalette: "neon" });
    expect(clean.logoStyle).toBe("way-arc");
    expect(clean.logoPalette).toBe("cobalt");

    const valid = sanitizeAppearance({ logoStyle: "lode-compass", logoPalette: "oxblood" });
    expect(valid.logoStyle).toBe("lode-compass");
    expect(valid.logoPalette).toBe("oxblood");
  });

  it("leaves --brand-gradient unset for default cobalt palette and writes it for non-default palettes", () => {
    expect(MANAGED_VARS).toContain("--brand-gradient");
    expect(planAppearance(prefs(), mac).vars["--brand-gradient"]).toBeUndefined();

    const lodestarDark = planAppearance(prefs({ logoPalette: "lodestar" }), mac);
    expect(lodestarDark.vars["--brand-gradient"]).toBe(
      "linear-gradient(135deg, #fbbf24 0%, #2dd4bf 52%, #38bdf8 100%)",
    );

    const lodestarLight = planAppearance(prefs({ logoPalette: "lodestar" }), macLight);
    expect(lodestarLight.vars["--brand-gradient"]).toBe(
      "linear-gradient(135deg, #d97706 0%, #0d9488 52%, #0284c7 100%)",
    );

    const oxbloodDark = planAppearance(prefs({ logoPalette: "oxblood" }), mac);
    expect(oxbloodDark.vars["--brand-gradient"]).toContain("#f87171");

    const gildedDark = planAppearance(prefs({ logoPalette: "gilded" }), mac);
    expect(gildedDark.vars["--brand-gradient"]).toContain("#fef08a");
  });
});
