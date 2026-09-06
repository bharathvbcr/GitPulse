import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The macOS material has two rules that are easy to state and impossible to
 * see once they are broken, because breaking either costs frames rather than
 * pixels. A filter that cannot see anything renders exactly like one that can.
 *
 *  1. A dialog blurs ONCE. A `backdrop-filter` makes its element a backdrop
 *     ROOT, so a full-viewport scrim that blurs leaves the card inside it
 *     sampling the scrim's own flat wash — the card pays for a filter whose
 *     input is a solid colour. Verified in Chromium 148 with an `invert(1)`
 *     probe: a `translateZ(0)`, `isolation`, `contain: layout paint` or
 *     `will-change: transform` ancestor passes the backdrop through, while an
 *     ancestor with `backdrop-filter`, `opacity < 1` or `filter` cuts it off.
 *     So every scrim routes through `.gp-scrim`, which is the one place the
 *     Mac rule can turn the outer blur off.
 *
 *  2. Always-on CHROME carries no backdrop filter. Chrome is laid out beside
 *     and above the panes, never over them, so the only thing in its backdrop
 *     is the shell's own hue field — and blurring a smooth gradient returns
 *     the same gradient. Only float surfaces (menus, popovers, toasts, dialog
 *     cards), which do sit over commit rows and diffs, are filtered.
 *
 * Both are asserted against the source rather than a hand-kept list: the scrim
 * check discovers dialogs by finding the markup shape itself, so a new dialog
 * is covered the day it is written.
 */
const componentsDir = fileURLToPath(new URL("../src/lib/components/", import.meta.url));
const css = readFileSync(new URL("../src/app.css", import.meta.url), "utf8");
const cargoToml = readFileSync(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
const baseConf = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
const macConf = JSON.parse(
  readFileSync(new URL("../src-tauri/tauri.macos.conf.json", import.meta.url), "utf8"),
);

function svelteFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const child = join(dir, entry.name);
    if (entry.isDirectory()) return svelteFiles(child);
    return entry.name.endsWith(".svelte") ? [child] : [];
  });
}

/**
 * A full-screen plate, however it is spelled. Matching only `gp-scrim` would
 * make this suite unable to see the very thing it exists to forbid — a dialog
 * written with the raw utilities — and matching only the utilities went blind
 * the moment the eight existing dialogs adopted the class, which is how this
 * was found: four assertions below passed against zero scrims. The union is
 * the honest rule, because both spellings describe the same shape.
 */
const SCRIM_SHAPE = /class="([^"]*(?:\bgp-scrim\b|\bfixed\b[^"]*\binset-0\b)[^"]*)"/g;

const scrims = svelteFiles(componentsDir).flatMap((file) => {
  const source = readFileSync(file, "utf8");
  return [...source.matchAll(SCRIM_SHAPE)].map((match) => ({
    file: relative(componentsDir, file).split(sep).join("/"),
    classes: match[1],
  }));
});

describe("macOS material", () => {
  it("finds the dialog scrims it is meant to be checking", () => {
    // A discovery test that discovers nothing passes every other assertion
    // below vacuously. Eight dialogs carry a scrim today.
    expect(scrims.length).toBeGreaterThanOrEqual(8);
  });

  it("routes every full-screen scrim through .gp-scrim", () => {
    const strays = scrims.filter((scrim) => !scrim.classes.includes("gp-scrim"));
    expect(strays.map((s) => `${s.file}: ${s.classes}`)).toEqual([]);
  });

  it("never re-adds a scrim blur beside the one .gp-scrim owns", () => {
    // `backdrop-blur-*` here would restore the double filter regardless of
    // what the Mac rule says, because a utility and the owner both apply.
    const doubled = scrims.filter((scrim) => /\bbackdrop-blur-/.test(scrim.classes));
    expect(doubled.map((s) => s.file)).toEqual([]);
  });

  it("turns the scrim's own filter off on macOS so the card is the only blur", () => {
    expect(css).toMatch(/html\.macos \.gp-scrim \{[^}]*backdrop-filter: none;/);
  });

  it("filters float surfaces only, never always-on chrome", () => {
    const supports = css.slice(css.indexOf("@supports ((backdrop-filter"));
    const filtered = [...supports.matchAll(/^ {2}(html\.macos [^{]+)\{([^}]*)\}/gm)]
      .filter(([, , body]) => /backdrop-filter: blur\(/.test(body))
      .map(([, selector]) => selector.trim());

    expect(filtered).toEqual(["html.macos :is(.gp-menu, .gp-card.shadow-float)"]);
    expect(filtered.join()).not.toContain(".gp-glass");
  });

  it("keeps the -webkit- prefix on every backdrop-filter it declares", () => {
    // WKWebView needed the prefix before Safari 18; autoprefixer is configured
    // to keep authored prefixes, not to add missing ones.
    const declarations = [...css.matchAll(/^(\s*)(-webkit-)?backdrop-filter:/gm)];
    const unprefixed = declarations.filter(([, , prefix]) => !prefix).length;
    const prefixed = declarations.length - unprefixed;
    expect(prefixed).toBe(unprefixed);
  });

  it("drops the material entirely under reduced transparency and forced colours", () => {
    const query = css.slice(css.indexOf("@media (prefers-reduced-transparency: reduce)"));
    const block = query.slice(0, query.indexOf("\n}\n") + 3);
    for (const surface of [".gp-shell", ".gp-glass", ".gp-menu", ".gp-scrim", ".gp-liquid-tabs"]) {
      expect(block).toContain(surface);
    }
  });

  /*
   * Native window transparency: three things that only work together.
   *
   * Tauri merges `tauri.macos.conf.json` over the base with JSON Merge Patch
   * (RFC 7396), which REPLACES arrays rather than merging them — so the macOS
   * file has to restate the whole `windows` entry, and every future edit to
   * the base window silently fails to reach macOS builds. That is the drift
   * this checks, by deriving the expected object instead of listing keys.
   */
  const MAC_ONLY_WINDOW_KEYS = ["transparent", "windowEffects"];

  it("restates the base window exactly, adding only the macOS-only keys", () => {
    const base = baseConf.app.windows[0];
    const mac = macConf.app.windows[0];
    expect(Object.keys(mac).filter((key) => !(key in base)).sort()).toEqual(
      [...MAC_ONLY_WINDOW_KEYS].sort(),
    );
    for (const [key, value] of Object.entries(base)) {
      expect(mac[key], `tauri.macos.conf.json drifted from the base window at "${key}"`).toEqual(
        value,
      );
    }
  });

  it("asks for transparency, the private API and a blur material together", () => {
    // Any one of the three alone is inert: without the feature the webview is
    // opaque, without `transparent` there is nothing to see through, and
    // without an effect there is nothing behind it but the desktop unblurred.
    expect(macConf.app.macOSPrivateApi).toBe(true);
    expect(macConf.app.windows[0].transparent).toBe(true);
    expect(macConf.app.windows[0].windowEffects.effects.length).toBeGreaterThan(0);
    // tauri-build reads the FIRST dependency table naming the crate and stops,
    // so this has to be the `[dependencies]` entry, not a target-scoped one.
    expect(cargoToml).toMatch(/^\[dependencies\][\s\S]*?^tauri = \{[^}]*macos-private-api/m);
  });

  it("keeps a veil under the content so an unknown desktop cannot set the contrast", () => {
    // Measured over a pure white desktop: muted text held 4.74:1 at worst.
    // A fully clear shell would hand that number to the wallpaper.
    const veil = css.match(/--mac-shell-veil:\s*([\d.]+)/g) ?? [];
    expect(veil.length).toBe(2);
    for (const declaration of veil) {
      expect(Number(declaration.split(":")[1])).toBeGreaterThanOrEqual(0.4);
    }
    expect(css).toMatch(/background-color: rgb\(var\(--c-bg\) \/ var\(--mac-shell-veil\)\)/);
  });
});