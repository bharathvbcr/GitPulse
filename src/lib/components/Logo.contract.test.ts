import { readFileSync } from "node:fs";
import { inflateSync } from "node:zlib";
import { render } from "svelte/server";
import { describe, expect, it } from "vitest";
import Logo from "./Logo.svelte";

/**
 * The Waylode mark is written out four times, because three of its users
 * cannot import a component: index.html paints the boot screen before any
 * script has loaded, and the two files in public/ are the favicon and the
 * source of the app icons. A redraw that reaches only some of them ships two
 * logos, so this holds every copy to the one in Logo.svelte.
 */
const read = (path: string): string => readFileSync(new URL(path, import.meta.url), "utf8");

const component = read("./Logo.svelte");
const copies: ReadonlyArray<readonly [string, string]> = [
  ["index.html", read("../../../index.html")],
  ["public/waylode-mark.svg", read("../../../public/waylode-mark.svg")],
  ["public/waylode-icon.svg", read("../../../public/waylode-icon.svg")],
];

/** The first capture of `pattern` in `text`, or a failure that names what was missing. */
function capture(text: string, pattern: RegExp, what: string): string {
  const hit = pattern.exec(text);
  if (!hit) throw new Error(`no ${what}`);
  return hit[1];
}

const trunk = (text: string) => capture(text, /<path\s[^>]*?\bd="([^"]+)"[^>]*stroke-linejoin="round"/, "stroked trunk path");
const node = (text: string) => {
  const hit = /<circle cx="([\d.]+)" cy="([\d.]+)" r="([\d.]+)"/.exec(text);
  if (!hit) throw new Error("no node circle");
  return hit.slice(1, 4).join(" ");
};
const stops = (text: string) => {
  const gradient = capture(text, /<linearGradient id="[^"]*stroke"[^>]*>([\s\S]*?)<\/linearGradient>/, "stroke gradient");
  return [...gradient.matchAll(/stop-color="(#[0-9a-f]{6})"/g)].map((m) => m[1]);
};
const badgeInset = capture(component, /variant === "badge" \? "([^"]+)"/, "badge inset in Logo.svelte");

describe("the Waylode mark's copies", () => {
  it("has a drawing to compare against", () => {
    expect(trunk(component)).toBe("M23.8 9.0A10.5 10.5 0 1 0 26.5 16.0H21.0L16.2 11.6L11.4 18.8");
    expect(node(component)).toBe("23.8 9.0 2.8");
    expect(stops(component)).toEqual(["#7dd3fc", "#38bdf8", "#3b82f6"]);
  });

  it.each(copies)("%s draws the same stroke, node and gradient as Logo.svelte", (_name, text) => {
    expect(trunk(text)).toBe(trunk(component));
    expect(node(text)).toBe(node(component));
    expect(stops(text)).toEqual(stops(component));
  });

  it.each(copies)("%s insets the drawing as the badge does", (_name, text) => {
    expect(capture(text, /<g transform="([^"]+)"/, "art transform")).toBe(badgeInset);
  });

  it("traces by a normalised length, so the dash cannot go stale when the path is redrawn", () => {
    expect(component).toContain('pathLength="100"');
    expect(component).toMatch(/stroke-dasharray:\s*100;[\s\S]*stroke-dashoffset:\s*100;/);
  });

  it("gives the app icon a full square plate and the favicon a rounded one", () => {
    const mark = copies[1][1];
    const icon = copies[2][1];
    expect(icon).toContain('<rect width="32" height="32" fill="url(#wl-badge)"/>');
    expect(mark).toContain('<rect width="32" height="32" rx="10" fill="url(#wl-badge)"/>');
    // The plate must be measured in the viewBox's units, not the output's
    // pixels: a 1024-unit rect in a 32-unit box paints over every corner.
    expect(icon).not.toMatch(/<rect width="1024"/);
  });
});

interface DecodedPng {
  width: number;
  height: number;
  bitDepth: number;
  colorType: number;
  pixel: (x: number, y: number) => readonly [number, number, number, number];
}

function decodePngRgba(relativePath: string): DecodedPng {
  const bytes = readFileSync(new URL(relativePath, import.meta.url));
  const magic = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
  for (let i = 0; i < magic.length; i++) {
    expect(bytes[i]).toBe(magic[i]);
  }

  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  let interlace = 0;
  const idat: Buffer[] = [];

  let offset = 8;
  while (offset + 12 <= bytes.length) {
    const len = bytes.readUInt32BE(offset);
    const kind = bytes.subarray(offset + 4, offset + 8).toString("ascii");
    const data = bytes.subarray(offset + 8, offset + 8 + len);
    offset += 12 + len;
    if (kind === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8]!;
      colorType = data[9]!;
      interlace = data[12]!;
    } else if (kind === "IDAT") {
      idat.push(data);
    } else if (kind === "IEND") {
      break;
    }
  }

  expect(width).toBeGreaterThan(0);
  expect(height).toBeGreaterThan(0);
  expect(bitDepth).toBe(8);
  expect(colorType).toBe(6);
  expect(interlace).toBe(0);

  const bpp = 4;
  const stride = width * bpp;
  const raw = inflateSync(Buffer.concat(idat));
  expect(raw.length).toBe(height * (1 + stride));

  const rgba = new Uint8Array(width * height * bpp);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (1 + stride)]!;
    const srcRow = y * (1 + stride) + 1;
    const dstRow = y * stride;
    for (let i = 0; i < stride; i++) {
      const x = raw[srcRow + i]!;
      const a = i >= bpp ? rgba[dstRow + i - bpp]! : 0;
      const b = y > 0 ? rgba[dstRow - stride + i]! : 0;
      const c = y > 0 && i >= bpp ? rgba[dstRow - stride + i - bpp]! : 0;
      let recon = x;
      if (filter === 1) recon = (x + a) & 0xff;
      else if (filter === 2) recon = (x + b) & 0xff;
      else if (filter === 3) recon = (x + ((a + b) >> 1)) & 0xff;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        const pr = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
        recon = (x + pr) & 0xff;
      } else if (filter !== 0) {
        throw new Error(`unsupported PNG filter ${filter}`);
      }
      rgba[dstRow + i] = recon;
    }
  }

  return {
    width,
    height,
    bitDepth,
    colorType,
    pixel: (x, y) => {
      const idx = (y * width + x) * 4;
      return [rgba[idx]!, rgba[idx + 1]!, rgba[idx + 2]!, rgba[idx + 3]!];
    },
  };
}

function assertWayArcCobaltPixels(png: DecodedPng): void {
  // Light-sky (#7dd3fc) portion along the lower-left arc.
  for (const [x, y] of [
    [80, 176],
    [73, 167],
  ] as const) {
    const [r, g, b, a] = png.pixel(x, y);
    expect(a, `sky-start stroke alpha at (${x},${y})`).toBeGreaterThan(200);
    expect(b, `sky-start stroke B>R at (${x},${y})`).toBeGreaterThan(r);
    expect(r, `sky-start stroke R at (${x},${y})`).toBeGreaterThanOrEqual(80);
    expect(g, `sky-start stroke G at (${x},${y})`).toBeGreaterThanOrEqual(180);
    expect(b, `sky-start stroke B at (${x},${y})`).toBeGreaterThanOrEqual(230);
  }

  // Sky-to-cobalt (#38bdf8 -> #3b82f6) portion along the top arc, lodestar node, and right arm.
  for (const [x, y] of [
    [128, 60],
    [179, 83],
    [196, 128],
  ] as const) {
    const [r, g, b, a] = png.pixel(x, y);
    expect(a, `cobalt stroke alpha at (${x},${y})`).toBeGreaterThan(200);
    expect(b, `cobalt stroke B>R at (${x},${y})`).toBeGreaterThan(r);
    expect(b, `cobalt stroke B at (${x},${y})`).toBeGreaterThanOrEqual(230);
    expect(g, `cobalt stroke G at (${x},${y})`).toBeGreaterThanOrEqual(140);
  }

  // Negative-space dark-plate interior remains dark (not overwritten by stray stroke).
  for (const [x, y] of [
    [134, 144],
    [157, 157],
  ] as const) {
    const [r, g, b, a] = png.pixel(x, y);
    expect(a, `plate interior alpha at (${x},${y})`).toBe(255);
    expect(r, `plate interior R at (${x},${y})`).toBeLessThan(40);
    expect(g, `plate interior G at (${x},${y})`).toBeLessThan(75);
    expect(b, `plate interior B at (${x},${y})`).toBeLessThan(90);
  }
}

describe("bundled PNG logo assets and notification surfaces", () => {
  it("renders plugins/waylode/assets/logo.png as a 256x256 RGBA badge with transparent corners and the Way-Arc", () => {
    const png = decodePngRgba("../../../plugins/waylode/assets/logo.png");
    expect(png.width).toBe(256);
    expect(png.height).toBe(256);
    expect(png.pixel(0, 0)[3]).toBe(0);
    assertWayArcCobaltPixels(png);
  });

  it.each([
    ["../../../src-tauri/icons/32x32.png", 32],
    ["../../../src-tauri/icons/128x128.png", 128],
    ["../../../src-tauri/icons/128x128@2x.png", 256],
    ["../../../src-tauri/icons/icon.png", 512],
  ] as const)("%s matches %ix%i 8-bit RGBA", (rel, size) => {
    const png = decodePngRgba(rel);
    expect(png.width).toBe(size);
    expect(png.height).toBe(size);
  });

  it("src-tauri/icons/128x128@2x.png draws the Cobalt light-sky->cobalt Way-Arc", () => {
    const png = decodePngRgba("../../../src-tauri/icons/128x128@2x.png");
    assertWayArcCobaltPixels(png);
  });

  it("assigns distinct wl-* gradient IDs and matching url(#...) references across multiple Logo badge instances", () => {
    const first = render(Logo, { props: { variant: "badge", size: 18 } }).body;
    const second = render(Logo, { props: { variant: "badge", size: 20 } }).body;

    const extractIds = (html: string) =>
      [...html.matchAll(/id="(wl-[a-z0-9]+-(?:stroke|badge|glow))"/g)].map((m) => m[1]!);
    const extractRefs = (html: string) =>
      [...html.matchAll(/url\(#(wl-[a-z0-9]+-(?:stroke|badge|glow))\)/g)].map((m) => m[1]!);

    const ids1 = extractIds(first);
    const ids2 = extractIds(second);
    expect(ids1).toHaveLength(3);
    expect(ids2).toHaveLength(3);
    for (const id of ids1) {
      expect(ids2).not.toContain(id);
    }
    for (const ref of extractRefs(first)) {
      expect(ids1).toContain(ref);
    }
    for (const ref of extractRefs(second)) {
      expect(ids2).toContain(ref);
    }
  });

  it.each([
    "./ToastContainer.svelte",
    "./NativeNotificationBridgeView.svelte",
    "./SessionNotificationBridge.svelte",
    "./AttentionInbox.svelte",
  ])("%s imports and renders <Logo", (file) => {
    const svelte = read(file);
    expect(svelte).toMatch(/import Logo from "\.\/Logo\.svelte";/);
    expect(svelte).toMatch(/<Logo\b/);
  });

  it("renders distinct geometry for each runtime-switchable logoStyle", () => {
    const wayArc = render(Logo, { props: { logoStyle: "way-arc", variant: "badge" } }).body;
    expect(wayArc).toContain("M23.8 9.0A10.5 10.5 0 1 0 26.5 16.0H21.0L16.2 11.6L11.4 18.8");
    expect(wayArc).toMatch(/<circle cx="23\.8" cy="9(?:\.0)?" r="2\.8"/);

    const foldedRibbon = render(Logo, { props: { logoStyle: "folded-ribbon", variant: "badge" } }).body;
    expect(foldedRibbon).toContain(
      "M58 97L100 223L136 223L160 151L184 223L220 223L262 97L226 97L202 169L178 97L142 97L118 169L94 97Z",
    );
    expect(foldedRibbon).toContain("M100 223L136 223L178 97L142 97ZM184 223L220 223L262 97L226 97Z");

    const lodeCompass = render(Logo, { props: { logoStyle: "lode-compass", variant: "badge" } }).body;
    expect(lodeCompass).toContain("M150 56L54 152L90 188L150 128Z");
    expect(lodeCompass).toContain("M170 264L266 168L230 132L170 192Z");
    expect(lodeCompass).toContain("M160 136L184 160L160 184L136 160Z");
  });

  it.each([
    ["cobalt", ["#7dd3fc", "#38bdf8", "#3b82f6"], "#0f172a"],
    ["lodestar", ["#fbbf24", "#2dd4bf", "#38bdf8"], "#151d2b"],
    ["oxblood", ["#f87171", "#ef4444", "#b91c1c"], "#170b0e"],
    ["gilded", ["#fef08a", "#fbbf24", "#d97706"], "#1c160b"],
  ] as const)("renders %s palette stops and badge plate", (paletteId, expectedStops, plateTop) => {
    const html = render(Logo, { props: { logoPalette: paletteId, variant: "badge" } }).body;
    expect(stops(html)).toEqual([...expectedStops]);
    expect(html).toContain(`stop-color="${plateTop}"`);
  });
});
