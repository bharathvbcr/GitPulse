import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { NON_TEXT_MIN, TEXT_MIN } from "../ui/color/guard";
import { contrastRatio, type Rgb } from "../ui/color/oklch";

/**
 * The status popover floats over whatever the user's desktop shows, so its
 * palette is solved against the worst wallpaper rather than the app theme.
 *
 * Composite model, per theme and per material:
 *   backdrop (pure black OR pure white, the two extremes a wallpaper can reach)
 *   -> panel fill at the material's own alpha (read from StatusPopover.svelte)
 *   -> optionally the specular sheen and hue tint passes, both included and
 *      excluded because they paint over only part of the panel
 *   -> an inner card (`--surface`) or the hover fill (`--soft`) on top.
 * Every text role is measured against every one of those backgrounds and the
 * worst ratio is the role's ratio. The hue field painted UNDER the panel is
 * left out on purpose: it adds colour at a fraction of the panel's 20% window
 * onto the wallpaper, and against black it lightens (helping dark text) while
 * against white it darkens (helping light text), so a bare black/white
 * backdrop is the bound.
 *
 * Opaque material and Reduce Transparency have no backdrop, so they are
 * measured on the solid base instead. Disabled controls are exempt (WCAG
 * 1.4.3) and the hairline edge is decorative, so it is reported but not
 * asserted.
 *
 * Nothing here repairs a colour: a failing role fails the test. Thresholds are
 * the ones the colour engine and accents.contract.test.ts share.
 */

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "StatusPopover.svelte"), "utf8");
const css = readFileSync(join(here, "../../app.css"), "utf8");

type Theme = "light" | "dark";
type Arm = "glass" | "opaque" | "reduced";

const BLOCK_START = "/* ===== STATUS POPOVER TOKENS";
const BLOCK_END = "/* ===== END STATUS POPOVER TOKENS";

function tokenBlock(): string {
  const start = css.indexOf(BLOCK_START);
  const end = css.indexOf(BLOCK_END);
  expect(start, "status popover token block missing from app.css").toBeGreaterThan(-1);
  expect(end).toBeGreaterThan(start);
  return css.slice(start, end);
}

function rules(text: string): { selector: string; body: string }[] {
  return [...text.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map(([, selector, body]) => ({
    selector: selector.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\s+/g, " ").trim(),
    body,
  }));
}

function triplets(body: string): Record<string, Rgb> {
  const out: Record<string, Rgb> = {};
  for (const [, name, r, g, b] of body.matchAll(/--c-pop-([a-z-]+):\s*(\d+)\s+(\d+)\s+(\d+)/g)) {
    out[name] = [Number(r), Number(g), Number(b)];
  }
  return out;
}

function palettes(): Record<Theme, Record<string, Rgb>> {
  const block = tokenBlock();
  const all = rules(block);
  const root = all.find((rule) => rule.selector === ":root");
  const dark = all.find((rule) => rule.selector === "html.dark");
  const media = all.find((rule) => rule.selector === "html:not(.light)");
  expect(root && dark && media, "pop token rules :root, html.dark and html:not(.light) are all required").toBeTruthy();
  const light = triplets(root!.body);
  const darkOverrides = triplets(dark!.body);
  // The pre-snapshot `prefers-color-scheme: dark` arm has to say what html.dark says.
  expect(triplets(media!.body)).toEqual(darkOverrides);
  return { light, dark: { ...light, ...darkOverrides } };
}

/** Alphas the component paints its glass at, read from its own source. */
function alphas(theme: Theme): { bg: number; surface: number; soft: number; sheen: number; tint: number } {
  const styles = source.slice(source.indexOf("<style>")).replace(/\/\*[\s\S]*?\*\//g, "");
  const found = rules(styles);
  const material = found.filter((rule) => rule.selector.includes("data-material"));
  // The light arm is the one that EXCLUDES the dark selectors: `:not(:global(...))`.
  const ladder = material.find((rule) => rule.body.includes("--bg:") && !rule.selector.includes(":not(:global"));
  const darkArm = material.find((rule) => rule.body.includes("--glass-sheen-top") && rule.selector.includes(".dark") && !rule.selector.includes(":not(:global"));
  const lightArm = material.find((rule) => rule.body.includes("--glass-sheen-top") && rule.selector.includes(":not(:global"));
  expect(ladder && darkArm && lightArm, "glass ladder and both appearance arms are required").toBeTruthy();
  const arm = theme === "dark" ? darkArm! : lightArm!;
  const merged = new Map<string, string>();
  for (const body of [ladder!.body, arm.body]) {
    for (const [, name, value] of body.matchAll(/(--[a-z-]+):\s*([^;]+);/g)) merged.set(name, value.trim());
  }
  const alpha = (name: string) => {
    const value = merged.get(name);
    const match = value ? /\/\s*(\.?\d*\.?\d+)\s*\)\s*$/.exec(value) : null;
    if (!match) throw new Error(`No alpha readable for ${name} in the ${theme} arm: ${value}`);
    return Number(match[1]);
  };
  return { bg: alpha("--bg"), surface: alpha("--surface"), soft: alpha("--soft"), sheen: alpha("--glass-sheen-top"), tint: alpha("--glass-tint") };
}

function over(front: Rgb, alpha: number, back: Rgb): Rgb {
  return [0, 1, 2].map((i) => front[i] * alpha + back[i] * (1 - alpha)) as unknown as Rgb;
}

interface Row { arm: Arm; theme: Theme; role: string; against: string; ratio: number; min: number; assert: boolean }

function measure(): Row[] {
  const rows: Row[] = [];
  const pal = palettes();
  for (const theme of ["light", "dark"] as const) {
    const p = pal[theme];
    const a = alphas(theme);
    const backgrounds: Record<Arm, Record<string, Rgb>> = { glass: {}, opaque: {}, reduced: {} };
    for (const backdrop of [0, 255]) {
      const wall: Rgb = [backdrop, backdrop, backdrop];
      const plain = over(p.base, a.bg, wall);
      const lit = over(p.tint, a.tint, over(p.white, a.sheen, plain));
      for (const [label, panel] of [["plain", plain], ["sheen", lit]] as const) {
        const tag = `${backdrop === 0 ? "black" : "white"}/${label}`;
        backgrounds.glass[`panel@${tag}`] = panel;
        backgrounds.glass[`card@${tag}`] = over(p.card, a.surface, panel);
        backgrounds.glass[`hover@${tag}`] = over(p.card, a.soft, panel);
      }
    }
    // Both fallbacks drop the background-image (no sheen, tint or hue field),
    // so there is exactly one panel: the solid base.
    backgrounds.reduced["panel"] = p.base;
    backgrounds.reduced["card"] = p.card;
    backgrounds.reduced["hover"] = over(p.card, a.soft, p.base);
    backgrounds.opaque["panel"] = p.base;
    backgrounds.opaque["card"] = p.card;
    backgrounds.opaque["hover"] = p.soft;
    const ink = (arm: Arm): Record<string, Rgb> => {
      const solid = arm === "opaque";
      const muted = arm === "glass" ? p["muted-glass"] : p["muted-solid"];
      const tone = solid ? "solid" : "glass";
      return { text: p.text, muted, blue: p[`blue-${tone}`], green: p[`green-${tone}`], amber: p[`amber-${tone}`] };
    };
    for (const arm of ["glass", "opaque", "reduced"] as const) {
      const fills = backgrounds[arm];
      const colors = ink(arm);
      for (const [role, fg] of Object.entries(colors)) {
        let worst = Infinity;
        let at = "";
        for (const [name, bg] of Object.entries(fills)) {
          const ratio = contrastRatio(fg, bg);
          if (ratio < worst) { worst = ratio; at = name; }
        }
        rows.push({ arm, theme, role: `text: ${role}`, against: at, ratio: worst, min: TEXT_MIN, assert: true });
      }
      // The Conflicts card tints its fill 7% toward amber, and the number inside is amber.
      let attention = Infinity;
      let attentionAt = "";
      for (const [name, bg] of Object.entries(fills)) {
        if (!name.startsWith("card")) continue;
        const ratio = contrastRatio(colors.amber, over(colors.amber, 0.07, bg));
        if (ratio < attention) { attention = ratio; attentionAt = name; }
      }
      rows.push({ arm, theme, role: "text: amber on attention card", against: attentionAt, ratio: attention, min: TEXT_MIN, assert: true });
      // Non-text: the keyboard focus outline and the status dots must be seen against the panel.
      for (const role of ["blue", "green", "amber", "muted"] as const) {
        let worst = Infinity;
        let at = "";
        for (const [name, bg] of Object.entries(fills)) {
          if (!name.startsWith("panel") && !name.startsWith("card")) continue;
          const ratio = contrastRatio(colors[role], bg);
          if (ratio < worst) { worst = ratio; at = name; }
        }
        rows.push({ arm, theme, role: role === "blue" ? "non-text: focus outline / busy dot" : `non-text: ${role} dot`, against: at, ratio: worst, min: NON_TEXT_MIN, assert: true });
      }
      // The hairline edge: decorative, reported not asserted.
      const edgeAlpha = theme === "dark" ? 0.14 : 0.24;
      const edge = theme === "dark" ? p.white : p.card;
      let edgeWorst = Infinity;
      let edgeAt = "";
      for (const [name, bg] of Object.entries(fills)) {
        if (!name.startsWith("panel")) continue;
        const ratio = contrastRatio(over(edge, arm === "glass" ? edgeAlpha : 1, bg), bg);
        if (ratio < edgeWorst) { edgeWorst = ratio; edgeAt = name; }
      }
      rows.push({ arm, theme, role: "info: hairline edge (decorative)", against: edgeAt, ratio: edgeWorst, min: NON_TEXT_MIN, assert: false });
    }
    // Primary action: text on the fill, and the fill against the panel it sits on.
    for (const fill of ["action", "action-hover"] as const) {
      rows.push({ arm: "opaque", theme, role: `text: on-${fill}`, against: fill, ratio: contrastRatio(p["on-action"], p[fill]), min: TEXT_MIN, assert: true });
    }
    for (const arm of ["glass", "opaque", "reduced"] as const) {
      let worst = Infinity;
      let at = "";
      for (const [name, bg] of Object.entries(backgrounds[arm])) {
        if (!name.startsWith("panel")) continue;
        const ratio = contrastRatio(p.action, bg);
        if (ratio < worst) { worst = ratio; at = name; }
      }
      // Reported, not asserted. WCAG 1.4.11 asks for the boundary only where it
      // is what identifies the control; the primary button is identified by its
      // label, and that label (white on the fill) is asserted at 4.5:1 above.
      // A fill that carries 4.5:1 white text cannot also sit 3:1 off a dark
      // glass panel, so in dark mode this ratio is structurally low.
      rows.push({ arm, theme, role: "info: primary fill vs panel (label carries identity)", against: at, ratio: worst, min: NON_TEXT_MIN, assert: false });
    }
  }
  return rows;
}

describe("status popover contrast over any wallpaper", () => {
  const rows = measure();

  it("measured a full set of roles in every theme and material", () => {
    for (const theme of ["light", "dark"] as const) {
      for (const arm of ["glass", "opaque", "reduced"] as const) {
        const own = rows.filter((row) => row.theme === theme && row.arm === arm);
        // 5 inks + attention + 4 non-text + edge (+ fill vs panel); on-action rows ride the opaque arm.
        expect(own.length, `${theme}/${arm}`).toBeGreaterThanOrEqual(11);
      }
    }
  });

  it("clears 4.5:1 for text and 3:1 for non-text in every role, worst case", () => {
    const failing = rows
      .filter((row) => row.assert && row.ratio < row.min)
      .map((row) => `${row.theme}/${row.arm} ${row.role} on ${row.against}: ${row.ratio.toFixed(2)} < ${row.min}`);
    expect(failing).toEqual([]);
  });

  it("states the material alpha as a floor no lighter than the one the roles were solved at", () => {
    // Solved minimum alphas over black and white were .787 light and .795 dark,
    // so anything under .8 here has to re-solve the palette, not just this number.
    for (const theme of ["light", "dark"] as const) expect(alphas(theme).bg).toBeGreaterThanOrEqual(0.8);
  });

  it.runIf(Boolean(process.env.STATUS_CONTRAST_REPORT))("writes the table for the lane report", () => {
    const lines = ["| theme | material | role | worst on | ratio | needs |", "|---|---|---|---|---|---|"];
    for (const row of rows) lines.push(`| ${row.theme} | ${row.arm} | ${row.role} | ${row.against} | ${row.ratio.toFixed(2)} | ${row.assert ? row.min : `${row.min} (not asserted)`} |`);
    writeFileSync(process.env.STATUS_CONTRAST_REPORT as string, `${lines.join("\n")}\n`);
  });
});
