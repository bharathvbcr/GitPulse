#!/usr/bin/env node
/**
 * Single-source brand asset generator for Waylode (Concept 2: Lodestone Way-Arc).
 *
 * Reads `brand/waylode-mark.json` and deterministically syncs:
 *   - src/lib/components/Logo.svelte
 *   - index.html (favicon link + #gp-boot SVG)
 *   - public/waylode-mark.svg
 *   - public/waylode-icon.svg
 *   - src-tauri/icons/{32x32,128x128,128x128@2x,icon}.png
 *   - src-tauri/icons/icon.ico (16, 24, 32, 48, 64, 256 @ 32bpp)
 *   - src-tauri/icons/icon.icns (Apple HIG squircle-padded 824/1024 with rx=185)
 *   - plugins/waylode/assets/logo.png & plugins/waylode-agy/assets/logo.svg (when present)
 *
 * Usage:
 *   node scripts/generate-brand-assets.mjs          generate/update all assets
 *   node scripts/generate-brand-assets.mjs --check  exit 1 if any generated output differs
 *
 * PNG, ICO and ICNS outputs are compared by what they hold (pixels, chunk
 * metadata, container entries), not by their compressed bytes: deflate output
 * differs between zlib builds, so the same image written by Node 26 and Node 22
 * is not byte-identical, and a byte check fails on every machine whose Node
 * differs from the one that last wrote the assets.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { deflateSync, inflateSync } from "node:zlib";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SPEC_PATH = path.join(REPO_ROOT, "brand", "waylode-mark.json");

/** @param {string} hex */
function parseHex(hex) {
  const clean = hex.replace("#", "");
  return [
    Number.parseInt(clean.slice(0, 2), 16),
    Number.parseInt(clean.slice(2, 4), 16),
    Number.parseInt(clean.slice(4, 6), 16),
  ];
}

/** @param {number} v @param {number} lo @param {number} hi */
function clamp(v, lo, hi) {
  return v < lo ? lo : v > hi ? hi : v;
}

/**
 * Evaluate a multi-stop linear gradient at `(x, y)`.
 * @param {number} x
 * @param {number} y
 * @param {number} x1
 * @param {number} y1
 * @param {number} x2
 * @param {number} y2
 * @param {{ t: number, rgb: number[] }[]} stops
 * @returns {[number, number, number]}
 */
function sampleLinearGradient(x, y, x1, y1, x2, y2, stops) {
  const dx = x2 - x1;
  const dy = y2 - y1;
  const len2 = dx * dx + dy * dy;
  const t = len2 === 0 ? 0 : clamp(((x - x1) * dx + (y - y1) * dy) / len2, 0, 1);
  if (t <= stops[0].t) return [stops[0].rgb[0], stops[0].rgb[1], stops[0].rgb[2]];
  for (let i = 1; i < stops.length; i++) {
    const prev = stops[i - 1];
    const curr = stops[i];
    if (t <= curr.t) {
      const span = curr.t - prev.t;
      const f = span === 0 ? 0 : (t - prev.t) / span;
      return [
        prev.rgb[0] + (curr.rgb[0] - prev.rgb[0]) * f,
        prev.rgb[1] + (curr.rgb[1] - prev.rgb[1]) * f,
        prev.rgb[2] + (curr.rgb[2] - prev.rgb[2]) * f,
      ];
    }
  }
  const last = stops[stops.length - 1];
  return [last.rgb[0], last.rgb[1], last.rgb[2]];
}

/**
 * Signed distance from `(x, y)` to a rounded rectangle `[rx0, rx0+rw] x [ry0, ry0+rh]` with corner radius `r`.
 * Negative inside, positive outside.
 * @param {number} x
 * @param {number} y
 * @param {number} rx0
 * @param {number} ry0
 * @param {number} rw
 * @param {number} rh
 * @param {number} r
 */
function sdfRoundedRect(x, y, rx0, ry0, rw, rh, r) {
  const cx = rx0 + rw / 2;
  const cy = ry0 + rh / 2;
  const hx = rw / 2 - r;
  const hy = rh / 2 - r;
  const qx = Math.abs(x - cx) - hx;
  const qy = Math.abs(y - cy) - hy;
  const ox = Math.max(qx, 0);
  const oy = Math.max(qy, 0);
  return Math.hypot(ox, oy) + Math.min(Math.max(qx, qy), 0) - r;
}

/**
 * Minimum Euclidean distance from `(px, py)` to segment `(x1, y1)->(x2, y2)`.
 * @param {number} px
 * @param {number} py
 * @param {number} x1
 * @param {number} y1
 * @param {number} x2
 * @param {number} y2
 */
function distToSegment(px, py, x1, y1, x2, y2) {
  const dx = x2 - x1;
  const dy = y2 - y1;
  const len2 = dx * dx + dy * dy;
  if (len2 === 0) return Math.hypot(px - x1, py - y1);
  const t = clamp(((px - x1) * dx + (py - y1) * dy) / len2, 0, 1);
  return Math.hypot(px - (x1 + t * dx), py - (y1 + t * dy));
}

/**
 * Compute SVG F.6.5/F.6.6 center-parameterized circular arc from `(x1, y1)` to `(x2, y2)` with radius `r`.
 * @param {number} x1
 * @param {number} y1
 * @param {number} r
 * @param {number} largeArc
 * @param {number} sweep
 * @param {number} x2
 * @param {number} y2
 */
function svgCircleArcCenter(x1, y1, r, largeArc, sweep, x2, y2) {
  const dx2 = (x1 - x2) / 2;
  const dy2 = (y1 - y2) / 2;
  const d2 = dx2 * dx2 + dy2 * dy2;
  const r2 = r * r;
  const factor = Math.sqrt(Math.max(0, (r2 - d2) / d2));
  const sign = largeArc === sweep ? -1 : 1;
  const cx = (x1 + x2) / 2 + sign * factor * dy2;
  const cy = (y1 + y2) / 2 - sign * factor * dx2;
  const a1 = Math.atan2(y1 - cy, x1 - cx);
  let a2 = Math.atan2(y2 - cy, x2 - cx);
  if (sweep === 0 && a2 > a1) a2 -= 2 * Math.PI;
  if (sweep === 1 && a2 < a1) a2 += 2 * Math.PI;
  return { cx, cy, r, a1, a2, x1, y1, x2, y2, sweep };
}

/**
 * Distance from `(px, py)` to a circular arc parameterized by `svgCircleArcCenter`.
 * @param {number} px
 * @param {number} py
 * @param {ReturnType<typeof svgCircleArcCenter>} arc
 */
function distToArc(px, py, arc) {
  const a = Math.atan2(py - arc.cy, px - arc.cx);
  let inside = false;
  if (arc.sweep === 0) {
    let diff = arc.a1 - a;
    while (diff < 0) diff += 2 * Math.PI;
    while (diff >= 2 * Math.PI) diff -= 2 * Math.PI;
    inside = diff <= arc.a1 - arc.a2 + 1e-6;
  } else {
    let diff = a - arc.a1;
    while (diff < 0) diff += 2 * Math.PI;
    while (diff >= 2 * Math.PI) diff -= 2 * Math.PI;
    inside = diff <= arc.a2 - arc.a1 + 1e-6;
  }
  if (inside) return Math.abs(Math.hypot(px - arc.cx, py - arc.cy) - arc.r);
  return Math.min(Math.hypot(px - arc.x1, py - arc.y1), Math.hypot(px - arc.x2, py - arc.y2));
}

/**
 * Parse `translate(tx ty) scale(s)` transform string.
 * @param {string} str
 */
function parseTransform(str) {
  const t = /translate\(([-.\d]+)\s+([-.\d]+)\)/.exec(str);
  const s = /scale\(([-.\d]+)\)/.exec(str);
  return {
    tx: t ? Number.parseFloat(t[1]) : 0,
    ty: t ? Number.parseFloat(t[2]) : 0,
    scale: s ? Number.parseFloat(s[1]) : 1,
  };
}

/**
 * Parse the Concept 2 mark path (`M... A... H... L... L...`) into arc + line segments.
 * Throws if an unexpected SVG command appears, keeping generation faithful to `brand/waylode-mark.json`.
 * @param {string} d
 */
function parseWayArcPath(d) {
  const tokens = d.match(/[A-Za-z]|[-+]?\d*\.?\d+/g) ?? [];
  let i = 0;
  let cx = 0;
  let cy = 0;
  /** @type {ReturnType<typeof svgCircleArcCenter> | null} */
  let arc = null;
  /** @type {[number, number, number, number][]} */
  const segments = [];
  while (i < tokens.length) {
    const cmd = tokens[i++];
    if (cmd === "M") {
      cx = Number.parseFloat(tokens[i++]);
      cy = Number.parseFloat(tokens[i++]);
    } else if (cmd === "A") {
      const rx = Number.parseFloat(tokens[i++]);
      i += 2; // circular arc: ry === rx and x-axis rotation is unused
      const largeArc = Number.parseInt(tokens[i++], 10);
      const sweep = Number.parseInt(tokens[i++], 10);
      const x2 = Number.parseFloat(tokens[i++]);
      const y2 = Number.parseFloat(tokens[i++]);
      arc = svgCircleArcCenter(cx, cy, rx, largeArc, sweep, x2, y2);
      cx = x2;
      cy = y2;
    } else if (cmd === "H") {
      const x2 = Number.parseFloat(tokens[i++]);
      segments.push([cx, cy, x2, cy]);
      cx = x2;
    } else if (cmd === "L") {
      const x2 = Number.parseFloat(tokens[i++]);
      const y2 = Number.parseFloat(tokens[i++]);
      segments.push([cx, cy, x2, y2]);
      cx = x2;
      cy = y2;
    } else {
      throw new Error(`Unsupported SVG path command "${cmd}" in waylode-mark.json`);
    }
  }
  return { arc, segments };
}

/**
 * CRC32 table for PNG chunk checksums.
 */
const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = (c & 1) !== 0 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

/** @param {Buffer} buf */
function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** @param {string} type @param {Buffer} data */
function pngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([len, typeBuf, data, crc]);
}

/**
 * Encode an 8-bit RGBA buffer (`colorType = 6`, `bitDepth = 8`, filter 0) as PNG.
 * @param {number} width
 * @param {number} height
 * @param {Uint8Array} rgba
 * @returns {Buffer}
 */
function encodePngRgba(width, height, rgba) {
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bitDepth
  ihdr[9] = 6; // colorType RGBA
  ihdr[10] = 0; // compression
  ihdr[11] = 0; // filter
  ihdr[12] = 0; // interlace

  const stride = width * 4;
  const raw = Buffer.alloc(height * (1 + stride));
  for (let y = 0; y < height; y++) {
    const rowStart = y * (1 + stride);
    raw[rowStart] = 0; // filter type 0 (None)
    Buffer.from(rgba.buffer, rgba.byteOffset + y * stride, stride).copy(raw, rowStart + 1);
  }
  const idat = deflateSync(raw, { level: 9 });
  return Buffer.concat([
    signature,
    pngChunk("IHDR", ihdr),
    pngChunk("IDAT", idat),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

/**
 * Render the Waylode badge/icon into an 8-bit RGBA byte buffer.
 * @param {any} spec
 * @param {number} size
 * @param {{ rounded: boolean, border: boolean, insetRatio?: number, rxRatio?: number, supersample?: number }} opts
 * @returns {Uint8Array}
 */
function rasterizeBadge(spec, size, opts) {
  const ss = opts.supersample ?? (size <= 256 ? 4 : 2);
  const { arc, segments } = parseWayArcPath(spec.path);
  const halfStroke = spec.strokeWidth / 2;
  const circle = spec.circle;
  const transform = parseTransform(spec.artTransform.badge);

  const strokeStops = spec.gradient.stops.map((/** @type {{ offset: string, color: string }} */ s) => ({
    t: Number.parseFloat(s.offset) / 100,
    rgb: parseHex(s.color),
  }));
  const plateStops = [
    { t: 0, rgb: parseHex(spec.plate.dark[0]) },
    { t: 1, rgb: parseHex(spec.plate.dark[1]) },
  ];
  const glow = spec.plate.radialGlow;
  const glowRgb = parseHex(glow.color);
  const border = spec.plate.border;
  const borderRgb = parseHex(border.stroke);

  const inset = opts.insetRatio ?? 0; // normalized [0, 0.5)
  const plateSizeNorm = 1 - 2 * inset;
  const plateOriginSvg = inset * 32;
  const plateSpanSvg = plateSizeNorm * 32;
  const rxSvg = opts.rounded ? (opts.rxRatio !== undefined ? opts.rxRatio * 32 : spec.plate.rx) : 0;

  const rgba = new Uint8Array(size * size * 4);
  const invN = 1 / (ss * ss);

  for (let py = 0; py < size; py++) {
    for (let px = 0; px < size; px++) {
      let accR = 0;
      let accG = 0;
      let accB = 0;
      let accA = 0;

      for (let sy = 0; sy < ss; sy++) {
        const svgY = ((py + (sy + 0.5) / ss) / size) * 32;
        const localY = ((svgY - plateOriginSvg) / plateSpanSvg) * 32;
        const artY = (localY - transform.ty) / transform.scale;

        for (let sx = 0; sx < ss; sx++) {
          const svgX = ((px + (sx + 0.5) / ss) / size) * 32;

          // Plate containment in global 32x32 viewBox (supports Apple HIG squircle padding)
          if (opts.rounded || inset > 0) {
            const dPlate = sdfRoundedRect(svgX, svgY, plateOriginSvg, plateOriginSvg, plateSpanSvg, plateSpanSvg, rxSvg);
            if (dPlate > 0) continue;
          }

          // Plate base gradient + radial glow in local 0..32 plate coordinates
          const localX = ((svgX - plateOriginSvg) / plateSpanSvg) * 32;
          let [r, g, b] = sampleLinearGradient(localX, localY, 0, 0, 32, 32, plateStops);

          const glowDist = Math.hypot(localX - glow.cx, localY - glow.cy);
          if (glowDist < glow.r) {
            const ga = glow.startOpacity * (1 - glowDist / glow.r);
            r = glowRgb[0] * ga + r * (1 - ga);
            g = glowRgb[1] * ga + g * (1 - ga);
            b = glowRgb[2] * ga + b * (1 - ga);
          }

          if (opts.border) {
            const dBorder = sdfRoundedRect(
              localX,
              localY,
              border.x,
              border.y,
              border.width,
              border.height,
              border.rx,
            );
            if (Math.abs(dBorder) <= border.strokeWidth / 2) {
              const ba = border.strokeOpacity;
              r = borderRgb[0] * ba + r * (1 - ba);
              g = borderRgb[1] * ba + g * (1 - ba);
              b = borderRgb[2] * ba + b * (1 - ba);
            }
          }

          // Mark SDF in art coordinates
          const artX = (localX - transform.tx) / transform.scale;
          let minDist = Math.hypot(artX - circle.cx, artY - circle.cy) - circle.r;
          if (minDist > 0 && arc) {
            minDist = Math.min(minDist, distToArc(artX, artY, arc) - halfStroke);
          }
          if (minDist > 0) {
            for (const [x1, y1, x2, y2] of segments) {
              minDist = Math.min(minDist, distToSegment(artX, artY, x1, y1, x2, y2) - halfStroke);
              if (minDist <= 0) break;
            }
          }

          if (minDist <= 0) {
            const [sr, sg, sb] = sampleLinearGradient(
              artX,
              artY,
              spec.gradient.x1,
              spec.gradient.y1,
              spec.gradient.x2,
              spec.gradient.y2,
              strokeStops,
            );
            r = sr;
            g = sg;
            b = sb;
          }

          accR += r;
          accG += g;
          accB += b;
          accA += 1;
        }
      }

      const outIdx = (py * size + px) * 4;
      if (accA === 0) {
        rgba[outIdx] = 0;
        rgba[outIdx + 1] = 0;
        rgba[outIdx + 2] = 0;
        rgba[outIdx + 3] = 0;
      } else {
        const aNorm = accA * invN;
        rgba[outIdx] = clamp(Math.round(accR / accA), 0, 255);
        rgba[outIdx + 1] = clamp(Math.round(accG / accA), 0, 255);
        rgba[outIdx + 2] = clamp(Math.round(accB / accA), 0, 255);
        rgba[outIdx + 3] = clamp(Math.round(aNorm * 255), 0, 255);
      }
    }
  }

  return rgba;
}

/**
 * Build a multi-resolution Windows `.ico` wrapping 32bpp PNG entries.
 * @param {{ size: number, png: Buffer }[]} entries
 * @returns {Buffer}
 */
function encodeIco(entries) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type = icon
  header.writeUInt16LE(entries.length, 4);

  const dir = Buffer.alloc(entries.length * 16);
  let offset = 6 + dir.length;
  for (let i = 0; i < entries.length; i++) {
    const { size, png } = entries[i];
    const base = i * 16;
    dir[base] = size >= 256 ? 0 : size;
    dir[base + 1] = size >= 256 ? 0 : size;
    dir[base + 2] = 0; // palette colors
    dir[base + 3] = 0; // reserved
    dir.writeUInt16LE(1, base + 4); // planes
    dir.writeUInt16LE(32, base + 6); // bpp
    dir.writeUInt32LE(png.length, base + 8);
    dir.writeUInt32LE(offset, base + 12);
    offset += png.length;
  }
  return Buffer.concat([header, dir, ...entries.map((e) => e.png)]);
}

/**
 * Build an Apple `.icns` container from PNG chunks (`ic11`..`ic14`, `ic07`..`ic10`).
 * @param {{ type: string, png: Buffer }[]} chunks
 * @returns {Buffer}
 */
function encodeIcns(chunks) {
  const packed = chunks.map(({ type, png }) => {
    const head = Buffer.alloc(8);
    head.write(type, 0, 4, "ascii");
    head.writeUInt32BE(8 + png.length, 4);
    return Buffer.concat([head, png]);
  });
  const total = 8 + packed.reduce((sum, b) => sum + b.length, 0);
  const header = Buffer.alloc(8);
  header.write("icns", 0, 4, "ascii");
  header.writeUInt32BE(total, 4);
  return Buffer.concat([header, ...packed]);
}

/** @param {number} n */
function fmtCoord(n) {
  return Number.isInteger(n) ? n.toFixed(1) : String(n);
}

/** @param {any} spec */
function renderWaylodeMarkSvg(spec) {
  const s = spec.gradient.stops;
  const g = spec.gradient;
  const p = spec.plate;
  const b = p.border;
  const c = spec.circle;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${spec.viewBox}" width="32" height="32" fill="none" role="img" aria-label="${spec.name}">
  <title>${spec.name}</title>
  <defs>
    <linearGradient id="wl-stroke" x1="${g.x1}" y1="${g.y1}" x2="${g.x2}" y2="${g.y2}" gradientUnits="userSpaceOnUse">
      <stop offset="${s[0].offset}" stop-color="${s[0].color}"/>
      <stop offset="${s[1].offset}" stop-color="${s[1].color}"/>
      <stop offset="${s[2].offset}" stop-color="${s[2].color}"/>
    </linearGradient>
    <linearGradient id="wl-badge" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="${p.dark[0]}"/>
      <stop offset="100%" stop-color="${p.dark[1]}"/>
    </linearGradient>
    <radialGradient id="wl-glow" cx="${p.radialGlow.cx}" cy="${p.radialGlow.cy}" r="${p.radialGlow.r}" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="${p.radialGlow.color}" stop-opacity="${p.radialGlow.startOpacity}"/>
      <stop offset="100%" stop-color="${p.radialGlow.color}" stop-opacity="${p.radialGlow.endOpacity}"/>
    </radialGradient>
  </defs>
  <rect width="32" height="32" rx="${p.rx}" fill="url(#wl-badge)"/>
  <rect width="32" height="32" rx="${p.rx}" fill="url(#wl-glow)"/>
  <rect x="${b.x}" y="${b.y}" width="${b.width}" height="${b.height}" rx="${b.rx}" fill="none" stroke="${b.stroke}" stroke-opacity="${b.strokeOpacity}" stroke-width="${b.strokeWidth}"/>
  <g transform="${spec.artTransform.badge}">
    <path d="${spec.path}" stroke="url(#wl-stroke)" stroke-width="${spec.strokeWidth}" stroke-linecap="${spec.strokeLinecap}" stroke-linejoin="${spec.strokeLinejoin}"/>
    <circle cx="${fmtCoord(c.cx)}" cy="${fmtCoord(c.cy)}" r="${fmtCoord(c.r)}" fill="url(#wl-stroke)"/>
  </g>
</svg>
`;
}

/** @param {any} spec */
function renderWaylodeIconSvg(spec) {
  const s = spec.gradient.stops;
  const g = spec.gradient;
  const p = spec.plate;
  const c = spec.circle;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${spec.viewBox}" width="1024" height="1024" fill="none" role="img" aria-label="${spec.name}">
  <title>${spec.name}</title>
  <!-- The source of src-tauri/icons. The plate is deliberately the whole
       square, with no corner radius and no hairline: the baked icons have
       always filled their canvas, and this keeps that. The rounded badge, for
       anywhere a page draws the logo, is waylode-mark.svg. -->
  <defs>
    <linearGradient id="wl-stroke" x1="${g.x1}" y1="${g.y1}" x2="${g.x2}" y2="${g.y2}" gradientUnits="userSpaceOnUse">
      <stop offset="${s[0].offset}" stop-color="${s[0].color}"/>
      <stop offset="${s[1].offset}" stop-color="${s[1].color}"/>
      <stop offset="${s[2].offset}" stop-color="${s[2].color}"/>
    </linearGradient>
    <linearGradient id="wl-badge" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="${p.dark[0]}"/>
      <stop offset="100%" stop-color="${p.dark[1]}"/>
    </linearGradient>
    <radialGradient id="wl-glow" cx="${p.radialGlow.cx}" cy="${p.radialGlow.cy}" r="${p.radialGlow.r}" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="${p.radialGlow.color}" stop-opacity="${p.radialGlow.startOpacity}"/>
      <stop offset="100%" stop-color="${p.radialGlow.color}" stop-opacity="${p.radialGlow.endOpacity}"/>
    </radialGradient>
  </defs>
  <rect width="32" height="32" fill="url(#wl-badge)"/>
  <rect width="32" height="32" fill="url(#wl-glow)"/>
  <g transform="${spec.artTransform.badge}">
    <path d="${spec.path}" stroke="url(#wl-stroke)" stroke-width="${spec.strokeWidth}" stroke-linecap="${spec.strokeLinecap}" stroke-linejoin="${spec.strokeLinejoin}"/>
    <circle cx="${fmtCoord(c.cx)}" cy="${fmtCoord(c.cy)}" r="${fmtCoord(c.r)}" fill="url(#wl-stroke)"/>
  </g>
</svg>
`;
}

/**
 * @param {string} existingSvelte
 * @param {any} spec
 */
function syncLogoSvelte(existingSvelte, spec) {
  const s = spec.gradient.stops;
  const g = spec.gradient;
  const p = spec.plate;
  const c = spec.circle;

  const defsBlock = `<!-- waylode-mark-defs:start -->
    {#if activePaletteId === "cobalt"}
      <linearGradient id="{uid}-stroke" x1="${g.x1}" y1="${g.y1}" x2="${g.x2}" y2="${g.y2}" gradientUnits="userSpaceOnUse">
        <stop offset="${s[0].offset}" stop-color="${s[0].color}" />
        <stop offset="${s[1].offset}" stop-color="${s[1].color}" />
        <stop offset="${s[2].offset}" stop-color="${s[2].color}" />
      </linearGradient>
    {:else}
      <linearGradient id="{uid}-stroke" x1="${g.x1}" y1="${g.y1}" x2="${g.x2}" y2="${g.y2}" gradientUnits="userSpaceOnUse">
        <stop offset="0%" stop-color={palette.lit[0]} />
        <stop offset="52%" stop-color={palette.lit[1]} />
        <stop offset="100%" stop-color={palette.lit[2]} />
      </linearGradient>
    {/if}
    {#if activeStyle !== "way-arc"}
      <linearGradient id="{uid}-shade" x1="60" y1="60" x2="260" y2="260" gradientUnits="userSpaceOnUse">
        <stop offset="0%" stop-color={palette.shade[0]} />
        <stop offset="100%" stop-color={palette.shade[1]} />
      </linearGradient>
    {/if}
    {#if variant === "badge"}
      {#if activePaletteId === "cobalt"}
        <linearGradient id="{uid}-badge" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color="${p.dark[0]}" />
          <stop offset="100%" stop-color="${p.dark[1]}" />
        </linearGradient>
        <radialGradient id="{uid}-glow" cx="${p.radialGlow.cx}" cy="${p.radialGlow.cy}" r="${p.radialGlow.r}" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color="${p.radialGlow.color}" stop-opacity="${p.radialGlow.startOpacity}" />
          <stop offset="100%" stop-color="${p.radialGlow.color}" stop-opacity="${p.radialGlow.endOpacity}" />
        </radialGradient>
      {:else}
        <linearGradient id="{uid}-badge" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color={palette.plate[0]} />
          <stop offset="100%" stop-color={palette.plate[1]} />
        </linearGradient>
        <radialGradient id="{uid}-glow" cx="${p.radialGlow.cx}" cy="${p.radialGlow.cy}" r="${p.radialGlow.r}" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color={palette.glow} stop-opacity="${p.radialGlow.startOpacity}" />
          <stop offset="100%" stop-color={palette.glow} stop-opacity="${p.radialGlow.endOpacity}" />
        </radialGradient>
      {/if}
    {/if}
    <!-- waylode-mark-defs:end -->`;

  const geomBlock = `<!-- waylode-mark-geom:start -->
      <path
        d="${spec.path}"
        pathLength="100"
        stroke="url(#{uid}-stroke)"
        stroke-width="${spec.strokeWidth}"
        stroke-linecap="${spec.strokeLinecap}"
        stroke-linejoin="${spec.strokeLinejoin}"
        class={animated ? "wl-trace" : ""}
      />
      <circle cx="${fmtCoord(c.cx)}" cy="${fmtCoord(c.cy)}" r="${fmtCoord(c.r)}" fill="url(#{uid}-stroke)" />
      <!-- waylode-mark-geom:end -->`;

  return existingSvelte
    .replace(/<!-- waylode-mark-defs:start -->[\s\S]*?<!-- waylode-mark-defs:end -->/, defsBlock)
    .replace(/<!-- waylode-mark-geom:start -->[\s\S]*?<!-- waylode-mark-geom:end -->/, geomBlock)
    .replace(
      /variant === "badge" \? "[^"]+" : "[^"]+"/,
      `variant === "badge" ? "${spec.artTransform.badge}" : "${spec.artTransform.mark}"`,
    );
}

/**
 * @param {string} existingHtml
 * @param {any} spec
 */
function updateIndexHtml(existingHtml, spec) {
  const s = spec.gradient.stops;
  const g = spec.gradient;
  const c = spec.circle;
  const faviconUpdated = existingHtml.replace(
    /<link rel="icon" type="image\/svg\+xml" href="[^"]*"\s*\/>/,
    '<link rel="icon" type="image/svg+xml" href="/waylode-mark.svg" />',
  );
  const bootSvgBlock = `<svg class="gp-boot-logo" xmlns="http://www.w3.org/2000/svg" viewBox="${spec.viewBox}" fill="none">
        <defs>
          <linearGradient id="gp-boot-stroke" x1="${g.x1}" y1="${g.y1}" x2="${g.x2}" y2="${g.y2}" gradientUnits="userSpaceOnUse">
            <stop offset="${s[0].offset}" stop-color="${s[0].color}" />
            <stop offset="${s[1].offset}" stop-color="${s[1].color}" />
            <stop offset="${s[2].offset}" stop-color="${s[2].color}" />
          </linearGradient>
        </defs>
        <g transform="${spec.artTransform.badge}">
          <path d="${spec.path}" stroke="url(#gp-boot-stroke)" stroke-width="${spec.strokeWidth}" stroke-linecap="${spec.strokeLinecap}" stroke-linejoin="${spec.strokeLinejoin}" />
          <circle cx="${fmtCoord(c.cx)}" cy="${fmtCoord(c.cy)}" r="${fmtCoord(c.r)}" fill="url(#gp-boot-stroke)" />
        </g>
      </svg>`;
  return faviconUpdated.replace(
    /<svg class="gp-boot-logo"[\s\S]*?<\/svg>/,
    bootSvgBlock,
  );
}

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

/**
 * A length-prefixed part, so concatenated parts cannot run into each other.
 * @param {Buffer} buf
 * @returns {Buffer}
 */
function framed(buf) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(buf.length, 0);
  return Buffer.concat([len, buf]);
}

/**
 * What a PNG holds, independent of how its pixels were compressed: every
 * chunk but IDAT as written, then the IDAT stream inflated. `null` when the
 * bytes are not a well-formed PNG.
 * @param {Buffer} buf
 * @returns {Buffer | null}
 */
function pngContent(buf) {
  if (buf.length < 8 || !buf.subarray(0, 8).equals(PNG_SIGNATURE)) return null;
  const parts = [];
  const idat = [];
  let at = 8;
  let ended = false;
  while (at + 12 <= buf.length) {
    const len = buf.readUInt32BE(at);
    const type = buf.toString("ascii", at + 4, at + 8);
    const end = at + 12 + len;
    if (end > buf.length) return null;
    const data = buf.subarray(at + 8, at + 8 + len);
    if (type === "IDAT") idat.push(data);
    else parts.push(framed(Buffer.from(type, "ascii")), framed(data));
    at = end;
    if (type === "IEND") {
      ended = true;
      break;
    }
  }
  if (!ended || at !== buf.length) return null;
  try {
    parts.push(framed(Buffer.from("IDAT", "ascii")), framed(inflateSync(Buffer.concat(idat))));
  } catch {
    return null;
  }
  return Buffer.concat(parts);
}

/**
 * What an `.ico` of PNG entries holds: each entry's fixed fields (size,
 * colours, planes, depth) and its PNG's content. The byte size and offset
 * fields follow the compression and are left out.
 * @param {Buffer} buf
 * @returns {Buffer | null}
 */
function icoContent(buf) {
  if (buf.length < 6 || buf.readUInt16LE(0) !== 0 || buf.readUInt16LE(2) !== 1) return null;
  const count = buf.readUInt16LE(4);
  if (buf.length < 6 + count * 16) return null;
  const parts = [framed(buf.subarray(0, 6))];
  for (let i = 0; i < count; i++) {
    const base = 6 + i * 16;
    const size = buf.readUInt32LE(base + 8);
    const offset = buf.readUInt32LE(base + 12);
    if (offset + size > buf.length) return null;
    const png = pngContent(buf.subarray(offset, offset + size));
    if (!png) return null;
    parts.push(framed(buf.subarray(base, base + 8)), framed(png));
  }
  return Buffer.concat(parts);
}

/**
 * What an `.icns` of PNG elements holds: each element's type and its PNG's
 * content, in order. Element and file lengths follow the compression.
 * @param {Buffer} buf
 * @returns {Buffer | null}
 */
function icnsContent(buf) {
  if (buf.length < 8 || buf.toString("ascii", 0, 4) !== "icns") return null;
  const parts = [];
  let at = 8;
  while (at + 8 <= buf.length) {
    const type = buf.toString("ascii", at, at + 4);
    const len = buf.readUInt32BE(at + 4);
    if (len < 8 || at + len > buf.length) return null;
    const png = pngContent(buf.subarray(at + 8, at + len));
    if (!png) return null;
    parts.push(framed(Buffer.from(type, "ascii")), framed(png));
    at += len;
  }
  return at === buf.length ? Buffer.concat(parts) : null;
}

/**
 * Whether two versions of a generated asset hold the same thing: identical
 * bytes, or, for PNG, ICO and ICNS, the same content (see `pngContent`).
 * @param {string} relPath
 * @param {Buffer} current
 * @param {Buffer} expected
 * @returns {boolean}
 */
export function sameAssetContent(relPath, current, expected) {
  if (current.equals(expected)) return true;
  const ext = path.extname(relPath).toLowerCase();
  const content = ext === ".png" ? pngContent : ext === ".ico" ? icoContent : ext === ".icns" ? icnsContent : null;
  if (!content) return false;
  const a = content(current);
  const b = content(expected);
  return a !== null && b !== null && a.equals(b);
}

/**
 * Sync or verify a target file. Returns `true` when already up-to-date.
 * @param {string} relPath
 * @param {string | Buffer} expected
 * @param {boolean} checkOnly
 * @returns {boolean}
 */
function syncFile(relPath, expected, checkOnly) {
  const abs = path.join(REPO_ROOT, relPath);
  const expectedBuf = Buffer.isBuffer(expected) ? expected : Buffer.from(expected, "utf8");
  const currentBuf = existsSync(abs) ? readFileSync(abs) : null;
  if (currentBuf && sameAssetContent(relPath, currentBuf, expectedBuf)) return true;
  if (checkOnly) return false;
  mkdirSync(path.dirname(abs), { recursive: true });
  writeFileSync(abs, expectedBuf);
  return false;
}

function main() {
  const checkOnly = process.argv.includes("--check");
  const spec = JSON.parse(readFileSync(SPEC_PATH, "utf8"));

  const markSvg = renderWaylodeMarkSvg(spec);
  const iconSvg = renderWaylodeIconSvg(spec);
  const logoSvelte = syncLogoSvelte(
    readFileSync(path.join(REPO_ROOT, "src", "lib", "components", "Logo.svelte"), "utf8"),
    spec,
  );
  const indexHtml = updateIndexHtml(readFileSync(path.join(REPO_ROOT, "index.html"), "utf8"), spec);

  // Desktop square icons (16, 24, 32, 48, 64, 128, 256, 512)
  const squarePngs = new Map();
  for (const sz of [16, 24, 32, 48, 64, 128, 256, 512]) {
    squarePngs.set(
      sz,
      encodePngRgba(sz, sz, rasterizeBadge(spec, sz, { rounded: false, border: false })),
    );
  }
  const icoBuf = encodeIco([16, 24, 32, 48, 64, 256].map((size) => ({ size, png: squarePngs.get(size) })));

  // Apple HIG squircle-padded icons for icon.icns (~824/1024 inside 1024x1024, rx=185/1024)
  const insetRatio = (spec.macIcon.size - spec.macIcon.plateSize) / (2 * spec.macIcon.size);
  const rxRatio = spec.macIcon.rx / spec.macIcon.size;
  const macPngs = new Map();
  for (const sz of [32, 64, 128, 256, 512, 1024]) {
    macPngs.set(
      sz,
      encodePngRgba(
        sz,
        sz,
        rasterizeBadge(spec, sz, {
          rounded: true,
          border: true,
          insetRatio,
          rxRatio,
          supersample: sz <= 256 ? 4 : 2,
        }),
      ),
    );
  }
  const icnsBuf = encodeIcns([
    { type: "ic11", png: macPngs.get(32) },
    { type: "ic12", png: macPngs.get(64) },
    { type: "ic07", png: macPngs.get(128) },
    { type: "ic08", png: macPngs.get(256) },
    { type: "ic13", png: macPngs.get(256) },
    { type: "ic09", png: macPngs.get(512) },
    { type: "ic14", png: macPngs.get(512) },
    { type: "ic10", png: macPngs.get(1024) },
  ]);

  /** @type {[string, string | Buffer][]} */
  const targets = [
    ["src/lib/components/Logo.svelte", logoSvelte],
    ["index.html", indexHtml],
    ["public/waylode-mark.svg", markSvg],
    ["public/waylode-icon.svg", iconSvg],
    ["src-tauri/icons/32x32.png", squarePngs.get(32)],
    ["src-tauri/icons/128x128.png", squarePngs.get(128)],
    ["src-tauri/icons/128x128@2x.png", squarePngs.get(256)],
    ["src-tauri/icons/icon.png", squarePngs.get(512)],
    ["src-tauri/icons/icon.ico", icoBuf],
    ["src-tauri/icons/icon.icns", icnsBuf],
  ];

  // Plugin assets are written whenever plugins/waylode* directories exist.
  if (existsSync(path.join(REPO_ROOT, "plugins", "waylode"))) {
    const pluginPng = encodePngRgba(
      256,
      256,
      rasterizeBadge(spec, 256, { rounded: true, border: true, supersample: 4 }),
    );
    targets.push(["plugins/waylode/assets/logo.png", pluginPng]);
  }
  if (existsSync(path.join(REPO_ROOT, "plugins", "waylode-agy"))) {
    targets.push(["plugins/waylode-agy/assets/logo.svg", markSvg]);
  }

  const drifted = [];
  for (const [rel, content] of targets) {
    if (!syncFile(rel, content, checkOnly)) {
      drifted.push(rel);
    }
  }

  if (checkOnly) {
    if (drifted.length > 0) {
      for (const rel of drifted) console.error(`brand asset out of date: ${rel}`);
      process.exitCode = 1;
    } else {
      console.log(`all ${targets.length} Waylode brand assets match brand/waylode-mark.json`);
    }
  } else {
    console.log(
      drifted.length > 0
        ? `updated ${drifted.length} Waylode brand asset(s): ${drifted.join(", ")}`
        : `all ${targets.length} Waylode brand assets already up to date`,
    );
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
