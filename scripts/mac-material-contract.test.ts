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

    expect(filtered).toHaveLength(1);
    expect(filtered[0]).not.toContain(".gp-glass");

    /*
     * A float is a surface wearing a float shadow, and the tier is derived
     * from that rather than from a class list. Written as
     * `.gp-card.shadow-float` it named the dialogs and silently missed every
     * other float: the commit tooltip carries `shadow-pop`, and the toasts,
     * the coach mark and the go-to-line popover carry `shadow-float` without
     * `.gp-card`. The tooltip was reported as unreadably see-through — it was
     * the one float in the app compositing with no blur at all, which looks
     * identical to a blur that is working until you put content behind it.
     */
    const shadows = new Set<string>();
    for (const file of svelteFiles(componentsDir)) {
      const source = readFileSync(file, "utf8");
      for (const [, classes] of source.matchAll(/class="([^"]*)"/g)) {
        for (const token of classes.split(/\s+/)) {
          if (token === "shadow-float" || token === "shadow-pop") shadows.add(token);
        }
      }
    }
    expect(shadows.size).toBeGreaterThan(0);
    for (const shadow of shadows) {
      expect(filtered[0], `a ${shadow} surface floats over content with no blur`).toContain(
        `[class~="${shadow}"]`,
      );
    }
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

  /*
   * `tauri-build` re-derives, on EVERY platform it builds, the Cargo features
   * the config implies, and aborts when they differ from the ones declared on
   * the `[dependencies] tauri` entry. The manifest is one file for all
   * platforms; the config is not — Tauri merges `tauri.<platform>.conf.json`
   * over the base. So a feature-implying key that lives only in the macOS
   * override builds here and fails everywhere else, and a macOS `ci:local`
   * cannot see it: the first release to carry this feature died on the Linux
   * pre-flight with "remove the `macos-private-api` feature", before a single
   * platform had started building.
   *
   * The mapping mirrors tauri-utils' `AppConfig::features()`, and both entries
   * tauri-build checks are covered: `tauri` and its own `tauri-build`.
   * `tray-icon` is filtered out of both sides by tauri-build, so it never
   * participates.
   */
  type TauriAppConfig = {
    macOSPrivateApi?: boolean;
    security?: { assetProtocol?: { enable?: boolean }; pattern?: { use?: string } };
  };

  /** Every feature tauri-build manages, and the config key that implies it. */
  const IMPLIED_BY_CONFIG: Record<string, (app: TauriAppConfig) => boolean> = {
    "macos-private-api": (app) => app.macOSPrivateApi === true,
    "protocol-asset": (app) => app.security?.assetProtocol?.enable === true,
    isolation: (app) => app.security?.pattern?.use === "isolation",
  };

  /** Both entries tauri-build checks, and which features it manages on each. */
  const ALLOWLISTED_DEPENDENCIES = [
    {
      crate: "tauri",
      table: "dependencies",
      managed: ["macos-private-api", "protocol-asset", "isolation"],
    },
    { crate: "tauri-build", table: "build-dependencies", managed: ["isolation"] },
  ] as const;

  /** RFC 7396 JSON Merge Patch — the rule Tauri layers platform configs with. */
  function mergePatch(target: unknown, patch: unknown): unknown {
    if (patch === null || typeof patch !== "object" || Array.isArray(patch)) return patch;
    const merged: Record<string, unknown> =
      target !== null && typeof target === "object" && !Array.isArray(target)
        ? { ...(target as Record<string, unknown>) }
        : {};
    for (const [key, value] of Object.entries(patch)) {
      if (value === null) delete merged[key];
      else merged[key] = mergePatch(merged[key], value);
    }
    return merged;
  }

  /** The `app` config a build actually sees: base alone off macOS, merged on it. */
  function appConfigFor(platform: "other" | "macos"): TauriAppConfig {
    if (platform === "other") return baseConf.app;
    return (mergePatch(baseConf, macConf) as { app: TauriAppConfig }).app;
  }

  it("declares the tauri features that EVERY platform's config implies", () => {
    for (const { crate, table, managed } of ALLOWLISTED_DEPENDENCIES) {
      const entry = cargoToml.match(
        new RegExp(String.raw`^\[${table}\][\s\S]*?^${crate} = \{([^}]*)\}`, "m"),
      )?.[1];
      expect(entry, `no \`${crate}\` entry in [${table}]`).toBeTruthy();
      const declared = [
        ...(entry?.match(/features\s*=\s*\[([^\]]*)\]/)?.[1] ?? "").matchAll(/"([^"]+)"/g),
      ]
        .map((match) => match[1])
        .filter((feature) => (managed as readonly string[]).includes(feature))
        .sort();

      for (const platform of ["other", "macos"] as const) {
        const app = appConfigFor(platform);
        const implied = Object.entries(IMPLIED_BY_CONFIG)
          .filter(([feature, enabled]) => (managed as readonly string[]).includes(feature) && enabled(app))
          .map(([feature]) => feature)
          .sort();
        const source =
          platform === "macos" ? "tauri.conf.json ⊕ tauri.macos.conf.json" : "tauri.conf.json";
        expect(
          declared,
          `on ${platform === "macos" ? "macOS" : "Linux/Windows"} the config (${source}) implies ` +
            `[${implied}] for \`${crate}\` but [${table}] declares [${declared}]; tauri-build ` +
            `aborts the build wherever these differ, so a key like this belongs in the BASE config.`,
        ).toEqual(implied);
      }
    }
  });

  it("asks for transparency, the private API and a blur material together", () => {
    // Any one of the three alone is inert: without the feature the webview is
    // opaque, without `transparent` there is nothing to see through, and
    // without an effect there is nothing behind it but the desktop unblurred.
    expect(appConfigFor("macos").macOSPrivateApi).toBe(true);
    expect(macConf.app.windows[0].transparent).toBe(true);
    expect(macConf.app.windows[0].windowEffects.effects.length).toBeGreaterThan(0);
    // tauri-build reads the FIRST dependency table naming the crate and stops,
    // so this has to be the `[dependencies]` entry, not a target-scoped one.
    expect(cargoToml).toMatch(/^\[dependencies\][\s\S]*?^tauri = \{[^}]*macos-private-api/m);
  });

  /*
   * A base plate inside a base plate is a repaint, not depth — free while both
   * were opaque, a visible darkening once neither is. The graph gutter is the
   * case that was reported. The exception is an OCCLUDER: a positioned element
   * sharing its parent's colour is covering content that scrolls under it, not
   * repainting the ground, so it has to stay filled.
   *
   * The exclusion list is derived rather than written down: every positioning
   * keyword that actually appears beside `bg-background` in a component must
   * be in it, so a new occluder idiom fails here instead of going transparent
   * in a view nobody screenshotted.
   */
  const POSITIONING = ["sticky", "absolute", "fixed"] as const;

  const nestedRule =
    css.match(
      /html\.macos\s+:where\(\[class~="bg-background"\]\)\s+:where\(\[class~="bg-background"\]\):not\(([^)]*)\)\s*\{([^}]*)\}/,
    ) ?? null;

  it("stops a base plate from repainting the base plate it sits in", () => {
    expect(nestedRule, "the nested bg-background rule is missing from app.css").not.toBeNull();
    expect(nestedRule?.[2]).toContain("background-color: transparent");
  });

  it("keeps every positioned occluder painted", () => {
    const used = new Set<string>();
    for (const file of svelteFiles(componentsDir)) {
      const source = readFileSync(file, "utf8");
      for (const [, classes] of source.matchAll(/class="([^"]*\bbg-background\b[^"]*)"/g)) {
        for (const keyword of POSITIONING) {
          if (new RegExp(`(^|[\\s:])${keyword}(\\s|$)`).test(classes)) used.add(keyword);
        }
      }
    }
    // If nothing is discovered the assertion below is vacuous; the diff's
    // sticky gutter means at least one occluder exists today.
    expect(used.size).toBeGreaterThan(0);
    for (const keyword of used) {
      expect(nestedRule?.[1], `an occluder uses "${keyword}" but the rule does not exempt it`)
        .toContain(`[class~="${keyword}"]`);
    }
  });

  it("turns every low-alpha base recess into a shade, and leaves the occluders alone", () => {
    /*
     * `bg-background/50` and `/60` are recesses — the base colour thinned
     * against the surface panel around them. Opaque that is a colour step;
     * translucent it is coverage, and coverage is what darkens a glass stack.
     * `/80` and `/90` are controls and floating fields, where covering what is
     * behind is the point, so they keep their fill.
     *
     * The band is derived from the components, so a new `/70` recess fails
     * here rather than compositing to a dark rectangle nobody screenshotted.
     */
    const RECESS_CEILING = 60;
    const alphas = new Set<number>();
    for (const file of svelteFiles(componentsDir)) {
      for (const [, alpha] of readFileSync(file, "utf8").matchAll(/bg-background\/(\d+)/g)) {
        alphas.add(Number(alpha));
      }
    }
    expect(alphas.size).toBeGreaterThan(0);

    const rule = css.match(
      /html\.macos\s+:where\(([^)]*bg-background\/[^)]*)\):not\([^)]*\)\s*\{([^}]*)\}/,
    );
    expect(rule?.[2], "the recess rule is missing or paints something else").toContain(
      "var(--mac-recess)",
    );
    for (const alpha of [...alphas].filter((a) => a <= RECESS_CEILING)) {
      expect(rule?.[1], `bg-background/${alpha} is a recess with no shade rule`).toContain(
        `[class~="bg-background/${alpha}"]`,
      );
    }
    for (const alpha of [...alphas].filter((a) => a > RECESS_CEILING)) {
      expect(rule?.[1], `bg-background/${alpha} covers on purpose and must keep its fill`).not.toContain(
        `[class~="bg-background/${alpha}"]`,
      );
    }
  });

  it("never fades an edge from a full-alpha surface colour", () => {
    // `from-background` is alpha 1, so an overflow cue becomes an opaque band
    // at the edge of a translucent pane; Tailwind's `to-transparent` is
    // `rgb(0 0 0 / 0)`, so the ramp also travels through black on light
    // themes. `.gp-edge-fade` owns both ends instead.
    const strays: string[] = [];
    for (const file of svelteFiles(componentsDir)) {
      const source = readFileSync(file, "utf8");
      if (/\b(?:from|via|to)-(?:background|surface|surfaceHover)\b/.test(source)) {
        strays.push(relative(componentsDir, file).split(sep).join("/"));
      }
    }
    expect(strays).toEqual([]);
    expect(css).toMatch(/\.gp-edge-fade-start \{/);
    expect(css).toMatch(/\.gp-edge-fade-end \{/);
    expect(css).toMatch(/\.gp-edge-fade-top \{/);
    expect(css).toMatch(/\.gp-edge-fade-bottom \{/);
    expect(css).toMatch(/html\.macos \.gp-edge-fade-start \{[^}]*var\(--mac-edge-shade\)/);
    expect(css).toMatch(/html\.macos \.gp-edge-fade-top \{[^}]*var\(--mac-edge-shade\)/);
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