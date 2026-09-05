import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import LanguageLogo from "./LanguageLogo.svelte";
import { ICON_KEYS, type LanguageIconKey } from "../language/languageLogos";

describe("LanguageLogo", () => {
  it("renders SVG logo for given language name", () => {
    const { body } = render(LanguageLogo, {
      props: { language: "Rust", size: 16 },
    });
    expect(body).toContain("<svg");
    expect(body).toContain('width="16"');
    expect(body).toContain('height="16"');
    expect(body).toContain('title="Rust"');
  });

  it("resolves language logo from filePath", () => {
    const { body } = render(LanguageLogo, {
      props: { filePath: "src/App.svelte" },
    });
    expect(body).toContain("<svg");
    expect(body).toContain('title="Svelte"');
  });

  it("resolves TypeScript from .ts and .tsx files", () => {
    const ts = render(LanguageLogo, {
      props: { filePath: "main.ts" },
    });
    expect(ts.body).toContain('title="TypeScript"');

    const tsx = render(LanguageLogo, {
      props: { filePath: "Component.tsx" },
    });
    expect(tsx.body).toContain('title="TypeScript"');
  });

  it("handles fallback gracefully for unknown files", () => {
    const { body } = render(LanguageLogo, {
      props: { filePath: "unknown.xyz" },
    });
    expect(body).toContain("<svg");
    expect(body).toContain('title="File"');
  });

  it("applies custom class and title overrides", () => {
    const { body } = render(LanguageLogo, {
      props: { language: "Go", class: "my-custom-class", title: "Custom Go Tooltip" },
    });
    expect(body).toContain("my-custom-class");
    expect(body).toContain('title="Custom Go Tooltip"');
  });
});

/* ------------------------------------------------------------------ *
 * Geometry contract
 *
 * The set these replaced looked fine in review and wrong on screen: Git's
 * mark reached x≈30, C++'s second plus sat outside its own circle, and the
 * browser silently clipped both at the viewBox edge. Nothing failed — there
 * was nothing asking. These sweep the rendered markup of every key, which is
 * why `ICON_KEYS` is exported from the module rather than re-typed here: a
 * new key joins the sweep by existing, not by someone remembering.
 * ------------------------------------------------------------------ */

const VIEWBOX_MIN = 0;
const VIEWBOX_MAX = 24;

interface Extent {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
}

const EMPTY: Extent = {
  minX: Number.POSITIVE_INFINITY,
  maxX: Number.NEGATIVE_INFINITY,
  minY: Number.POSITIVE_INFINITY,
  maxY: Number.NEGATIVE_INFINITY,
};

function extend(extent: Extent, x: number, y: number): Extent {
  return {
    minX: Math.min(extent.minX, x),
    maxX: Math.max(extent.maxX, x),
    minY: Math.min(extent.minY, y),
    maxY: Math.max(extent.maxY, y),
  };
}

function grow(extent: Extent, by: number): Extent {
  if (extent.minX === Number.POSITIVE_INFINITY) return extent;
  return {
    minX: extent.minX - by,
    maxX: extent.maxX + by,
    minY: extent.minY - by,
    maxY: extent.maxY + by,
  };
}

function merge(a: Extent, b: Extent): Extent {
  return {
    minX: Math.min(a.minX, b.minX),
    maxX: Math.max(a.maxX, b.maxX),
    minY: Math.min(a.minY, b.minY),
    maxY: Math.max(a.maxY, b.maxY),
  };
}

/** Coordinates consumed by each absolute path command, in (x, y) pairs. */
const PAIRS_PER_COMMAND: Record<string, number> = { M: 1, L: 1, T: 1, S: 2, Q: 2, C: 3 };

/**
 * Exact bounds of an unrotated elliptical arc, via the endpoint-to-centre
 * conversion in SVG 1.1 F.6.5. The extremes of an ellipse lie at its four
 * axis points, so the arc's bounds are its two endpoints plus whichever of
 * those four the sweep actually passes through — no sampling, no padding.
 */
function arcExtent(
  x1: number,
  y1: number,
  rxIn: number,
  ryIn: number,
  largeArc: boolean,
  sweep: boolean,
  x2: number,
  y2: number,
): Extent {
  let extent = extend(extend(EMPTY, x1, y1), x2, y2);
  let rx = Math.abs(rxIn);
  let ry = Math.abs(ryIn);
  if (rx === 0 || ry === 0) return extent;

  const dx = (x1 - x2) / 2;
  const dy = (y1 - y2) / 2;
  const scale = (dx * dx) / (rx * rx) + (dy * dy) / (ry * ry);
  if (scale > 1) {
    const factor = Math.sqrt(scale);
    rx *= factor;
    ry *= factor;
  }

  const numerator = rx * rx * ry * ry - rx * rx * dy * dy - ry * ry * dx * dx;
  const denominator = rx * rx * dy * dy + ry * ry * dx * dx;
  const radicand = denominator === 0 ? 0 : Math.max(0, numerator / denominator);
  const coefficient = (largeArc === sweep ? -1 : 1) * Math.sqrt(radicand);
  const centreX = coefficient * ((rx * dy) / ry) + (x1 + x2) / 2;
  const centreY = coefficient * ((-ry * dx) / rx) + (y1 + y2) / 2;

  const startAngle = Math.atan2((y1 - centreY) / ry, (x1 - centreX) / rx);
  const endAngle = Math.atan2((y2 - centreY) / ry, (x2 - centreX) / rx);
  const TWO_PI = Math.PI * 2;
  let delta = endAngle - startAngle;
  if (sweep && delta < 0) delta += TWO_PI;
  if (!sweep && delta > 0) delta -= TWO_PI;

  for (const quarter of [0, 0.25, 0.5, 0.75, 1]) {
    const angle = quarter * TWO_PI;
    // How far past the start this axis point sits, in the sweep's direction.
    let travelled = sweep ? angle - startAngle : startAngle - angle;
    travelled = ((travelled % TWO_PI) + TWO_PI) % TWO_PI;
    if (travelled > Math.abs(delta)) continue;
    extent = extend(
      extent,
      centreX + rx * Math.cos(angle),
      centreY + ry * Math.sin(angle),
    );
  }
  return extent;
}

/**
 * Every point a path can reach. A Bézier is bounded by the convex hull of its
 * own control points, so collecting the control points over-approximates the
 * curve and can only ever be too strict — which is the safe direction. Arcs
 * are the one command whose numbers are not all coordinates, so the radii and
 * flags are skipped and only the endpoint counts; an arc bulges outside the
 * chord, so `A` is additionally capped at the radius.
 */
function pathExtent(d: string): Extent {
  const tokens = d.match(/[A-Za-z]|-?\d*\.?\d+(?:e-?\d+)?/g) ?? [];
  let extent = EMPTY;
  let command = "";
  let cursorX = 0;
  let cursorY = 0;
  let index = 0;

  const nextNumber = (): number => Number(tokens[index++]);

  while (index < tokens.length) {
    const token = tokens[index];
    if (/^[A-Za-z]$/.test(token)) {
      command = token;
      index += 1;
      if (command === "Z" || command === "z") continue;
    }
    if (command === "H") {
      cursorX = nextNumber();
      extent = extend(extent, cursorX, cursorY);
    } else if (command === "V") {
      cursorY = nextNumber();
      extent = extend(extent, cursorX, cursorY);
    } else if (command === "A") {
      const rx = nextNumber();
      const ry = nextNumber();
      const rotation = nextNumber();
      const largeArc = nextNumber();
      const sweep = nextNumber();
      const endX = nextNumber();
      const endY = nextNumber();
      if (rotation !== 0) {
        // The exact solution below assumes an unrotated ellipse. Rather than
        // silently fall back to a looser bound, refuse: a check that quietly
        // changed what it proved would be worse than no check.
        throw new Error(`rotated arc is unsupported by this sweep: ${d}`);
      }
      extent = merge(
        extent,
        arcExtent(cursorX, cursorY, rx, ry, largeArc === 1, sweep === 1, endX, endY),
      );
      cursorX = endX;
      cursorY = endY;
    } else if (PAIRS_PER_COMMAND[command]) {
      for (let pair = 0; pair < PAIRS_PER_COMMAND[command]; pair += 1) {
        cursorX = nextNumber();
        cursorY = nextNumber();
        extent = extend(extent, cursorX, cursorY);
      }
    } else {
      // An unrecognised command means the parser is wrong about this glyph,
      // and a bounds check that silently skipped it would read as a pass.
      throw new Error(`unhandled path command "${command}" in: ${d}`);
    }
  }
  return extent;
}

function attribute(tag: string, name: string): number | undefined {
  const match = tag.match(new RegExp(`\\b${name}="(-?[\\d.]+)"`));
  return match ? Number(match[1]) : undefined;
}

interface Drawing {
  extent: Extent;
  count: number;
}

/**
 * Bounds of everything the glyph paints, stroke width included: a stroke is
 * centred on its path, so half of it lies outside the coordinates.
 */
function drawnExtent(markup: string): Drawing {
  const tags = markup.match(/<(?:path|circle|ellipse|rect|line|polygon|polyline)\b[^>]*>/g) ?? [];
  let extent = EMPTY;
  let count = 0;

  // A stroke declared on a wrapping <g> applies to the children inside it.
  const groupStroke = markup.match(/<g\b[^>]*stroke-width="([\d.]+)"/)?.[1];

  for (const tag of tags) {
    const strokeWidth =
      tag.includes("stroke-width=")
        ? Number(tag.match(/stroke-width="([\d.]+)"/)?.[1] ?? 0)
        : tag.includes("stroke=") || groupStroke
          ? Number(groupStroke ?? 0)
          : 0;
    const pad = strokeWidth / 2;

    let shape = EMPTY;
    const d = tag.match(/\bd="([^"]+)"/)?.[1];
    if (d) {
      shape = pathExtent(d);
    } else if (tag.startsWith("<circle")) {
      const cx = attribute(tag, "cx") ?? 0;
      const cy = attribute(tag, "cy") ?? 0;
      const r = attribute(tag, "r") ?? 0;
      shape = extend(extend(EMPTY, cx - r, cy - r), cx + r, cy + r);
    } else if (tag.startsWith("<ellipse")) {
      const cx = attribute(tag, "cx") ?? 0;
      const cy = attribute(tag, "cy") ?? 0;
      const rx = attribute(tag, "rx") ?? 0;
      const ry = attribute(tag, "ry") ?? 0;
      shape = extend(extend(EMPTY, cx - rx, cy - ry), cx + rx, cy + ry);
    } else if (tag.startsWith("<rect")) {
      const x = attribute(tag, "x") ?? 0;
      const y = attribute(tag, "y") ?? 0;
      shape = extend(
        extend(EMPTY, x, y),
        x + (attribute(tag, "width") ?? 0),
        y + (attribute(tag, "height") ?? 0),
      );
    } else {
      throw new Error(`unmeasured shape: ${tag}`);
    }

    extent = merge(extent, grow(shape, pad));
    count += 1;
  }
  return { extent, count };
}

function renderKey(key: LanguageIconKey): string {
  return render(LanguageLogo, { props: { iconKey: key, size: 24 } }).body;
}

describe("LanguageLogo geometry", () => {
  it("covers every icon key the resolver can return", () => {
    expect(ICON_KEYS.length).toBeGreaterThan(30);
    expect(new Set(ICON_KEYS).size).toBe(ICON_KEYS.length);
  });

  it.each(ICON_KEYS)("draws %s entirely inside the 24×24 viewBox", (key) => {
    const { extent, count } = drawnExtent(renderKey(key));

    // A glyph that drew nothing would trivially satisfy a bounds check.
    expect(count).toBeGreaterThan(0);
    expect(extent.minX).toBeGreaterThanOrEqual(VIEWBOX_MIN);
    expect(extent.minY).toBeGreaterThanOrEqual(VIEWBOX_MIN);
    expect(extent.maxX).toBeLessThanOrEqual(VIEWBOX_MAX);
    expect(extent.maxY).toBeLessThanOrEqual(VIEWBOX_MAX);
  });

  it.each(ICON_KEYS)("fills the optical square for %s", (key) => {
    const { extent } = drawnExtent(renderKey(key));
    // Marks that occupy wildly different areas read as different weights in a
    // column of file rows; the old outline icons vanished beside the badges.
    expect(Math.max(extent.maxX - extent.minX, extent.maxY - extent.minY)).toBeGreaterThanOrEqual(
      17,
    );
  });

  it.each(ICON_KEYS)("uses only absolute path commands for %s", (key) => {
    const markup = renderKey(key);
    for (const d of markup.match(/\bd="([^"]+)"/g) ?? []) {
      // Relative commands make a coordinate sweep meaningless, because a
      // number in the data is then an offset rather than a point.
      expect(d).not.toMatch(/[mlhvcsqtaz]/);
    }
  });

  it("never renders glyph detail as host-resolved text", () => {
    for (const key of ICON_KEYS) {
      expect(renderKey(key)).not.toContain("<text");
    }
  });

  it("gives every key a mark of its own", () => {
    const geometry = new Map<string, LanguageIconKey>();
    for (const key of ICON_KEYS) {
      // Strip colours so the comparison is of shape, not of palette: two keys
      // that differ only by hue would still be two drawings of one glyph.
      const shape = (renderKey(key).match(/\bd="[^"]+"|<circle[^>]*>|<ellipse[^>]*>/g) ?? [])
        .join("|")
        .replace(/fill="[^"]*"|stroke="[^"]*"/g, "");
      const seen = geometry.get(shape);
      expect(seen, `${key} draws the same mark as ${seen}`).toBeUndefined();
      geometry.set(shape, key);
    }
  });
});
