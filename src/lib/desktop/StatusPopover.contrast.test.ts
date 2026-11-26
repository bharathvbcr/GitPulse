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

/** CSS `color-mix(in srgb, rgb(front) 10%, var(--surface))` composited over `back`. */
function chipOverSurface(front: Rgb, card: Rgb, surfAlpha: number, back: Rgb): Rgb {
  const outAlpha = 0.1 + 0.9 * surfAlpha;
  return [0, 1, 2].map((i) => 0.1 * front[i] + 0.9 * surfAlpha * card[i] + (1 - outAlpha) * back[i]) as unknown as Rgb;
}

type ChipTone = "ok" | "info" | "warn" | "danger";

/** Chip tones in StatusPopover.svelte whose background composites 10% tone over `var(--surface)`. */
function surfaceBackedChipTones(): Set<ChipTone> {
  const styles = source.slice(source.indexOf("<style>")).replace(/\/\*[\s\S]*?\*\//g, "");
  const backed = new Set<ChipTone>();
  for (const rule of rules(styles)) {
    for (const tone of ["ok", "info", "warn", "danger"] as const) {
      if (
        rule.selector.includes(`.gp-chip[data-tone="${tone}"]`) &&
        new RegExp(`background(?:-color)?\\s*:\\s*color-mix\\(\\s*in srgb\\s*,\\s*rgb\\(\\s*var\\(--c-${tone}\\)\\s*\\)\\s*10%\\s*,\\s*var\\(--surface\\)\\s*\\)`).test(rule.body)
      ) {
        backed.add(tone);
      }
    }
  }
  return backed;
}

interface Row { arm: Arm; theme: Theme; role: string; against: string; ratio: number; min: number; assert: boolean }

function measure(): Row[] {
  const rows: Row[] = [];
  const pal = palettes();
  const backedChips = surfaceBackedChipTones();
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
      return {
        text: p.text,
        muted,
        blue: p[`blue-${tone}`],
        green: p[`green-${tone}`],
        amber: p[`amber-${tone}`],
        red: p[`red-${tone}`],
      };
    };
    for (const arm of ["glass", "opaque", "reduced"] as const) {
      const fills = backgrounds[arm];
      const colors = ink(arm);
      const surfAlpha = arm === "glass" ? a.surface : 1;
      for (const [role, fg] of Object.entries(colors)) {
        let worst = Infinity;
        let at = "";
        for (const [name, bg] of Object.entries(fills)) {
          const ratio = contrastRatio(fg, bg);
          if (ratio < worst) { worst = ratio; at = name; }
        }
        rows.push({ arm, theme, role: `text: ${role}`, against: at, ratio: worst, min: TEXT_MIN, assert: true });
      }
      // The Conflicts card and blocking request cards tint 7% toward amber; error request cards tint 7% toward red.
      let attention = Infinity;
      let attentionAt = "";
      let errorCard = Infinity;
      let errorCardAt = "";
      for (const [name, bg] of Object.entries(fills)) {
        if (!name.startsWith("card")) continue;
        const ratio = contrastRatio(colors.amber, over(colors.amber, 0.07, bg));
        if (ratio < attention) { attention = ratio; attentionAt = name; }
        const cardRatio = contrastRatio(colors.red, over(colors.red, 0.07, bg));
        if (cardRatio < errorCard) { errorCard = cardRatio; errorCardAt = name; }
      }
      rows.push({ arm, theme, role: "text: amber on attention card", against: attentionAt, ratio: attention, min: TEXT_MIN, assert: true });
      rows.push({ arm, theme, role: "text: red on error request card", against: errorCardAt, ratio: errorCard, min: TEXT_MIN, assert: true });

      // All 4 chip tones across panel (rendered over --surface when backed), card, and hover.
      const chipSpecs: { tone: ChipTone; inkName: string; fg: Rgb }[] = [
        { tone: "ok", inkName: "green", fg: colors.green },
        { tone: "info", inkName: "blue", fg: colors.blue },
        { tone: "warn", inkName: "amber", fg: colors.amber },
        { tone: "danger", inkName: "red", fg: colors.red },
      ];
      for (const chip of chipSpecs) {
        let worst = Infinity;
        let at = "";
        for (const [name, bg] of Object.entries(fills)) {
          const chipBg = backedChips.has(chip.tone)
            ? chipOverSurface(chip.fg, p.card, surfAlpha, bg)
            : over(chip.fg, 0.1, bg);
          const ratio = contrastRatio(chip.fg, chipBg);
          if (ratio < worst) { worst = ratio; at = name; }
        }
        rows.push({ arm, theme, role: `text: ${chip.inkName} on ${chip.tone} chip`, against: at, ratio: worst, min: TEXT_MIN, assert: true });

        // Also pin 10% tone directly on card and hover (--soft) so opaque hover contrast holds without relying on --surface shielding.
        let hoverWorst = Infinity;
        let hoverAt = "";
        for (const [name, bg] of Object.entries(fills)) {
          if (!name.startsWith("card") && !name.startsWith("hover")) continue;
          const ratio = contrastRatio(chip.fg, over(chip.fg, 0.1, bg));
          if (ratio < hoverWorst) { hoverWorst = ratio; hoverAt = name; }
        }
        rows.push({ arm, theme, role: `text: ${chip.inkName} on ${chip.tone} chip (card/hover)`, against: hoverAt, ratio: hoverWorst, min: TEXT_MIN, assert: true });
      }

      // Request-row chips inside 7%-tinted blocking (amber) and error (red) cards.
      for (const reqChip of [
        { tone: "warn" as const, inkName: "amber", cardName: "7% amber card", fg: colors.amber },
        { tone: "danger" as const, inkName: "red", cardName: "7% red card", fg: colors.red },
      ]) {
        let worst = Infinity;
        let at = "";
        for (const [name, bg] of Object.entries(fills)) {
          if (!name.startsWith("card")) continue;
          const rowBg = over(reqChip.fg, 0.07, bg);
          const chipBg = backedChips.has(reqChip.tone)
            ? chipOverSurface(reqChip.fg, p.card, surfAlpha, rowBg)
            : over(reqChip.fg, 0.1, rowBg);
          const ratio = contrastRatio(reqChip.fg, chipBg);
          if (ratio < worst) { worst = ratio; at = name; }
        }
        rows.push({ arm, theme, role: `text: ${reqChip.inkName} on ${reqChip.tone} chip (${reqChip.cardName})`, against: at, ratio: worst, min: TEXT_MIN, assert: true });
      }
      // Non-text: the keyboard focus outline and the status dots must be seen against the panel.
      for (const role of ["blue", "green", "amber", "red", "muted"] as const) {
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

function balancedBlock(text: string, headerPattern: RegExp): string {
  const match = headerPattern.exec(text);
  expect(match, `missing block matching ${headerPattern}`).toBeTruthy();
  const open = text.indexOf("{", match!.index);
  expect(open).toBeGreaterThan(-1);
  let depth = 0;
  for (let i = open; i < text.length; i += 1) {
    if (text[i] === "{") depth += 1;
    if (text[i] === "}" && --depth === 0) return text.slice(open + 1, i);
  }
  throw new Error(`unterminated block for ${headerPattern}`);
}

/** Extract `--c-*` tokens referenced directly or via `@theme` color utilities in a CSS block. */
function consumedColorTokens(cssBlock: string, themeColorMap: Map<string, string>): Set<string> {
  const tokens = new Set<string>();
  for (const [, name] of cssBlock.matchAll(/var\(\s*(--c-[a-z-]+)\s*\)/g)) {
    tokens.add(name);
  }
  for (const [, applyBody] of cssBlock.matchAll(/@apply\s+([^;]+);/g)) {
    for (const token of applyBody.trim().split(/\s+/)) {
      const m = /^(?:bg|text|border)-([A-Za-z0-9-]+?)(?:\/[0-9.[\]]+)?$/.exec(token);
      if (!m) continue;
      const mapped = themeColorMap.get(m[1]);
      if (mapped) tokens.add(mapped);
    }
  }
  return tokens;
}

/** Parse `@theme` `--color-<name>: ... var(--c-...)` mappings from app.css. */
function themeColorTokenMap(): Map<string, string> {
  const themeBody = balancedBlock(css, /@theme\s*\{/);
  const map = new Map<string, string>();
  for (const [, colorName, tokenName] of themeBody.matchAll(/--color-([A-Za-z0-9-]+)\s*:\s*[^;]*var\(\s*(--c-[a-z-]+)\s*\)/g)) {
    map.set(colorName, tokenName);
  }
  return map;
}

/** Extract `<style>` inner CSS from StatusPopover.svelte with comments stripped. */
function componentStyles(): string {
  const startTag = "<style>";
  const start = source.indexOf(startTag);
  const end = source.lastIndexOf("</style>");
  expect(start).toBeGreaterThan(-1);
  expect(end).toBeGreaterThan(start);
  return source.slice(start + startTag.length, end).replace(/\/\*[\s\S]*?\*\//g, "");
}

/** Parse `--var: val` declarations from `.status-shell` in StatusPopover.svelte `<style>`. */
function statusShellTokenMap(): Map<string, string> {
  const styles = componentStyles();
  const shellRule = rules(styles).find((rule) => rule.selector === ".status-shell");
  expect(shellRule, ".status-shell rule missing in StatusPopover.svelte").toBeTruthy();
  const map = new Map<string, string>();
  for (const [, name, value] of shellRule!.body.matchAll(/(--[a-z-]+)\s*:\s*([^;]+);/g)) {
    map.set(name, value.trim());
  }
  return map;
}

describe("status popover contrast over any wallpaper", () => {
  const rows = measure();

  it("maps --c-on-accent and every --c-* token consumed by shared gp-* primitives inside .status-shell", () => {
    const styles = componentStyles();
    const markup = source.slice(0, source.indexOf("<style>"));
    const shellTokens = statusShellTokenMap();

    // Primary CTA buttons (.gp-btn[data-variant="primary"]) paint text via --c-on-accent on --brand-gradient (--c-pop-action).
    expect(shellTokens.get("--c-on-accent")).toBe("var(--c-pop-on-action)");

    // Ensure no other style rule inside StatusPopover.svelte overrides --c-on-accent or --c-pop-on-action.
    for (const rule of rules(styles)) {
      if (rule.selector === ".status-shell") continue;
      expect(rule.body, `unexpected --c-on-accent override in ${rule.selector}`).not.toMatch(/--c-on-accent\s*:/);
      expect(rule.body, `unexpected --c-pop-on-action override in ${rule.selector}`).not.toMatch(/--c-pop-on-action\s*:/);
    }

    // Collect every --c-* token consumed by gp-btn, gp-chip, and gp-icon-btn blocks active in StatusPopover.svelte.
    const themeMap = themeColorTokenMap();
    const usedVariants = new Set<string>();
    for (const match of markup.matchAll(/class="[^"]*\bgp-btn\b[^"]*"[^>]*data-variant="([^"]+)"|data-variant="([^"]+)"[^>]*class="[^"]*\bgp-btn\b[^"]*"/g)) {
      const variant = match[1] ?? match[2];
      if (variant) usedVariants.add(variant);
    }
    const usedChipTones = new Set<string>(["ok", "info", "warn", "danger", "neutral"]);
    for (const match of markup.matchAll(/class="[^"]*\bgp-chip\b[^"]*"[^>]*data-tone="([^"]+)"|data-tone="([^"]+)"[^>]*class="[^"]*\bgp-chip\b[^"]*"/g)) {
      const tone = match[1] ?? match[2];
      if (tone && /^[a-z-]+$/.test(tone)) usedChipTones.add(tone);
    }
    const usedIconBtnTones = new Set<string>();
    for (const match of markup.matchAll(/class="[^"]*\bgp-icon-btn\b[^"]*"[^>]*data-tone="([^"]+)"|data-tone="([^"]+)"[^>]*class="[^"]*\bgp-icon-btn\b[^"]*"/g)) {
      const tone = match[1] ?? match[2];
      if (tone && /^[a-z-]+$/.test(tone)) usedIconBtnTones.add(tone);
    }

    const activeBlocks: string[] = [];
    for (const util of ["gp-btn", "gp-chip", "gp-icon-btn"] as const) {
      const utilBody = balancedBlock(css, new RegExp(`@utility\\s+${util}\\s*\\{`));
      // Strip nested &[data-variant=...] / &[data-tone=...] blocks, then re-include only the active ones.
      const baseAndPseudo = utilBody.replace(/&\[data-(?:variant|tone)="[^"]+"\]\s*\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}/g, "");
      activeBlocks.push(baseAndPseudo);
      if (util === "gp-btn") {
        for (const variant of usedVariants) {
          const match = utilBody.match(new RegExp(`&\\[data-variant="${variant}"\\]\\s*\\{([^{}]*(?:\\{[^{}]*\\}[^{}]*)*)\\}`));
          if (match) activeBlocks.push(match[1]);
        }
      }
      if (util === "gp-chip") {
        for (const tone of usedChipTones) {
          const match = utilBody.match(new RegExp(`&\\[data-tone="${tone}"\\]\\s*\\{([^{}]*(?:\\{[^{}]*\\}[^{}]*)*)\\}`));
          if (match) activeBlocks.push(match[1]);
        }
      }
      if (util === "gp-icon-btn") {
        for (const tone of usedIconBtnTones) {
          const match = utilBody.match(new RegExp(`&\\[data-tone="${tone}"\\](?::hover)?\\s*\\{([^{}]*(?:\\{[^{}]*\\}[^{}]*)*)\\}`));
          if (match) activeBlocks.push(match[1]);
        }
      }
    }

    const requiredTokens = new Set<string>();
    for (const block of activeBlocks) {
      for (const tok of consumedColorTokens(block, themeMap)) {
        requiredTokens.add(tok);
      }
    }
    expect(requiredTokens.size).toBeGreaterThanOrEqual(8);

    const pal = palettes();
    const popTokenNames = new Set(Object.keys(pal.light).map((k) => `--c-pop-${k}`));
    const resolveToPopToken = (token: string): string | null => {
      let cur = token;
      for (let hop = 0; hop < 6; hop += 1) {
        const rhs = shellTokens.get(cur);
        if (!rhs) return null;
        const m = /^var\(\s*(--[a-z-]+)\s*\)$/.exec(rhs);
        if (!m) return null;
        if (popTokenNames.has(m[1])) return m[1];
        cur = m[1];
      }
      return null;
    };

    for (const token of requiredTokens) {
      expect(shellTokens.has(token), `${token} consumed by gp-btn/gp-chip/gp-icon-btn is missing from .status-shell`).toBe(true);
      expect(resolveToPopToken(token), `${token} in .status-shell does not resolve to a --c-pop-* token`).toBeTruthy();
    }
  });

  it("measured a full set of roles in every theme and material", () => {
    for (const theme of ["light", "dark"] as const) {
      for (const arm of ["glass", "opaque", "reduced"] as const) {
        const own = rows.filter((row) => row.theme === theme && row.arm === arm);
        // 6 inks + 2 tinted cards (7% amber/red) + 4 backed chips (panel/card/hover) + 4 unbacked chips (card/hover) + 2 request-row chips (7% amber/red) + 5 non-text + 2 decorative/info; on-action rows ride the opaque arm.
        expect(own.length, `${theme}/${arm}`).toBeGreaterThanOrEqual(25);
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
