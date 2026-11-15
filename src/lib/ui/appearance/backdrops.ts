/**
 * Window wallpapers (Settings -> Appearance -> Material -> Wallpaper).
 *
 * A wallpaper is painted on `html.macos body`, BENEATH the shell veil, so it
 * takes the place of the desktop the translucent window would otherwise show.
 * That placement is the whole contrast argument: `materialModel.ts` already
 * proves the stack over a pure white and a pure black desktop, the extremes of
 * any image, so a wallpaper under the veil is covered by the same proof and
 * needs no measurement of its own. Painting it anywhere above the veil would
 * not be.
 *
 * The built-in art is original and generated here as static SVG: a fixed
 * seed, no animation, no script, no external reference. It is drawn once into a
 * `data:` URL and composited like any other background image, which is the
 * cost model app.css asks of the window field (no perpetual rendering loop).
 *
 * A wallpaper is a separate preference from a theme file on purpose: the
 * `gitpulse-theme/1` format refuses `url()` by rule, and a theme stays a small
 * shareable palette.
 */

export const BUILTIN_BACKDROP_IDS = ["neural", "reactor", "hex", "circuit", "horizon"] as const;
export type BuiltinBackdropId = (typeof BUILTIN_BACKDROP_IDS)[number];

/** `none`, one of the built-ins, or the reader's own image (kept in IndexedDB). */
export type BackdropId = "none" | BuiltinBackdropId | "custom";

export function isBuiltinBackdrop(value: unknown): value is BuiltinBackdropId {
  return (BUILTIN_BACKDROP_IDS as readonly string[]).includes(value as string);
}

export function isBackdropId(value: unknown): value is BackdropId {
  return value === "none" || value === "custom" || isBuiltinBackdrop(value);
}

export interface BuiltinBackdrop {
  readonly id: BuiltinBackdropId;
  readonly label: string;
  readonly description: string;
}

export const BUILTIN_BACKDROPS: readonly BuiltinBackdrop[] = [
  { id: "neural", label: "Neural HUD", description: "A node graph under heads-up display rings, in cyan." },
  { id: "reactor", label: "Reactor", description: "A glowing ringed core on red and gold plating." },
  { id: "hex", label: "Hex plate", description: "Hexagonal armour plating, lit in red." },
  { id: "circuit", label: "Circuit", description: "Phosphor-green circuit traces." },
  { id: "horizon", label: "Horizon", description: "A retro sun over a perspective grid." },
];

// --- drawing helpers ---------------------------------------------------------

const W = 1600;
const H = 1000;

/** Mulberry32: a tiny seeded PRNG so every build draws the same picture. */
function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Two decimals: enough for a 1600-wide picture, and it keeps the URL short. */
function n(value: number): string {
  return String(Math.round(value * 100) / 100);
}

function svg(defs: string, body: string): string {
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid slice">` +
    `<defs>${defs}</defs>${body}</svg>`
  );
}

/** Ring of `count` tick marks around (cx, cy) between radii r1 and r2. */
function ticks(cx: number, cy: number, r1: number, r2: number, count: number, stroke: string, width: number): string {
  let path = "";
  for (let i = 0; i < count; i += 1) {
    const a = (i / count) * Math.PI * 2;
    const c = Math.cos(a);
    const s = Math.sin(a);
    path += `M${n(cx + c * r1)} ${n(cy + s * r1)}L${n(cx + c * r2)} ${n(cy + s * r2)}`;
  }
  return `<path d="${path}" stroke="${stroke}" stroke-width="${width}" fill="none"/>`;
}

/** An arc of radius r from angle a0 to a1 (degrees, clockwise from +x). */
function arc(cx: number, cy: number, r: number, a0: number, a1: number): string {
  const rad = (deg: number) => (deg * Math.PI) / 180;
  const x0 = cx + Math.cos(rad(a0)) * r;
  const y0 = cy + Math.sin(rad(a0)) * r;
  const x1 = cx + Math.cos(rad(a1)) * r;
  const y1 = cy + Math.sin(rad(a1)) * r;
  const large = a1 - a0 > 180 ? 1 : 0;
  return `M${n(x0)} ${n(y0)}A${n(r)} ${n(r)} 0 ${large} 1 ${n(x1)} ${n(y1)}`;
}

// --- the pictures ------------------------------------------------------------

function neural(): string {
  const random = rng(0x6a52);
  const nodes: [number, number, number][] = [];
  for (let i = 0; i < 90; i += 1) {
    nodes.push([random() * W, random() * H, 1.5 + random() * 3.5]);
  }
  let edges = "";
  const link = 190;
  for (let i = 0; i < nodes.length; i += 1) {
    for (let j = i + 1; j < nodes.length; j += 1) {
      const dx = nodes[i][0] - nodes[j][0];
      const dy = nodes[i][1] - nodes[j][1];
      if (dx * dx + dy * dy < link * link) {
        edges += `M${n(nodes[i][0])} ${n(nodes[i][1])}L${n(nodes[j][0])} ${n(nodes[j][1])}`;
      }
    }
  }
  const dots = nodes
    .map(([x, y, r]) => `<circle cx="${n(x)}" cy="${n(y)}" r="${n(r)}"/>`)
    .join("");
  const halos = nodes
    .filter((_, index) => index % 6 === 0)
    .map(([x, y, r]) => `<circle cx="${n(x)}" cy="${n(y)}" r="${n(r * 5)}"/>`)
    .join("");
  const cx = 1130;
  const cy = 470;
  const hud =
    `<g fill="none" stroke="#5fd4ff">` +
    `<circle cx="${cx}" cy="${cy}" r="300" stroke-opacity=".35" stroke-width="2"/>` +
    `<circle cx="${cx}" cy="${cy}" r="250" stroke-opacity=".6" stroke-width="2.5" stroke-dasharray="4 10"/>` +
    `<circle cx="${cx}" cy="${cy}" r="170" stroke-opacity=".25" stroke-width="1"/>` +
    `<path d="${arc(cx, cy, 275, -60, 40)}" stroke-opacity=".7" stroke-width="5"/>` +
    `<path d="${arc(cx, cy, 275, 120, 200)}" stroke-opacity=".5" stroke-width="5"/>` +
    `<path d="${arc(cx, cy, 210, 210, 330)}" stroke-opacity=".45" stroke-width="2"/>` +
    `<path d="${arc(cx, cy, 130, 20, 160)}" stroke-opacity=".55" stroke-width="3"/>` +
    `</g>` +
    ticks(cx, cy, 300, 316, 72, "#5fd4ff", 1.2).replace("<path ", `<path stroke-opacity=".4" `) +
    `<circle cx="${cx}" cy="${cy}" r="60" fill="url(#core)"/>`;
  return svg(
    `<radialGradient id="bg" cx="70%" cy="45%" r="85%"><stop offset="0" stop-color="#0b2a44"/><stop offset=".55" stop-color="#06121f"/><stop offset="1" stop-color="#02060c"/></radialGradient>` +
      `<radialGradient id="core"><stop offset="0" stop-color="#d8f6ff" stop-opacity=".9"/><stop offset=".4" stop-color="#5fd4ff" stop-opacity=".35"/><stop offset="1" stop-color="#5fd4ff" stop-opacity="0"/></radialGradient>`,
    `<rect width="${W}" height="${H}" fill="url(#bg)"/>` +
      `<path d="${edges}" stroke="#4cc9ff" stroke-opacity=".42" stroke-width="1.4" fill="none"/>` +
      `<g fill="#4cc9ff" fill-opacity=".12">${halos}</g>` +
      `<g fill="#9be6ff" fill-opacity=".85">${dots}</g>` +
      hud,
  );
}

function reactor(): string {
  const cx = W / 2;
  const cy = H / 2;
  // Ten wedge coils between two rings, drawn as annular sectors.
  let coils = "";
  const rad = (deg: number) => (deg * Math.PI) / 180;
  for (let i = 0; i < 10; i += 1) {
    const a0 = i * 36 + 4;
    const a1 = i * 36 + 32;
    const r0 = 118;
    const r1 = 196;
    const p = (r: number, a: number) => `${n(cx + Math.cos(rad(a)) * r)} ${n(cy + Math.sin(rad(a)) * r)}`;
    coils += `M${p(r0, a0)}L${p(r1, a0)}A${r1} ${r1} 0 0 1 ${p(r1, a1)}L${p(r0, a1)}A${r0} ${r0} 0 0 0 ${p(r0, a0)}Z`;
  }
  const random = rng(0x2e4c);
  let plates = "";
  for (let i = 0; i < 14; i += 1) {
    const x = random() * W;
    const y = random() * H;
    const w = 120 + random() * 260;
    plates += `M${n(x)} ${n(y)}h${n(w)}l${n(w * 0.25)} ${n(w * 0.25)}`;
  }
  return svg(
    `<radialGradient id="bg" cx="50%" cy="50%" r="75%"><stop offset="0" stop-color="#2a0c0c"/><stop offset=".6" stop-color="#120506"/><stop offset="1" stop-color="#060203"/></radialGradient>` +
      `<radialGradient id="glow"><stop offset="0" stop-color="#ffffff"/><stop offset=".18" stop-color="#c9f6ff"/><stop offset=".45" stop-color="#4fd8ff" stop-opacity=".55"/><stop offset="1" stop-color="#4fd8ff" stop-opacity="0"/></radialGradient>` +
      `<linearGradient id="gold" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#f1c45a"/><stop offset="1" stop-color="#8a5a12"/></linearGradient>`,
    `<rect width="${W}" height="${H}" fill="url(#bg)"/>` +
      `<path d="${plates}" stroke="url(#gold)" stroke-opacity=".38" stroke-width="2.5" fill="none"/>` +
      `<circle cx="${cx}" cy="${cy}" r="430" fill="url(#glow)" opacity=".6"/>` +
      `<circle cx="${cx}" cy="${cy}" r="240" fill="none" stroke="url(#gold)" stroke-width="10" stroke-opacity=".75"/>` +
      `<circle cx="${cx}" cy="${cy}" r="214" fill="none" stroke="#8fe9ff" stroke-width="2" stroke-opacity=".5"/>` +
      `<path d="${coils}" fill="#0e2a33" stroke="#8fe9ff" stroke-width="2.5" stroke-opacity=".85"/>` +
      `<circle cx="${cx}" cy="${cy}" r="108" fill="none" stroke="#c9f6ff" stroke-width="4"/>` +
      `<circle cx="${cx}" cy="${cy}" r="96" fill="url(#glow)"/>` +
      ticks(cx, cy, 252, 266, 60, "#f1c45a", 2).replace("<path ", `<path stroke-opacity=".55" `),
  );
}

function hex(): string {
  const r = 46;
  const w = Math.sqrt(3) * r;
  let path = "";
  const random = rng(0x4e11);
  let lit = "";
  for (let row = -1; row * r * 1.5 < H + r; row += 1) {
    for (let col = -1; col * w < W + w; col += 1) {
      const x = col * w + (row % 2 === 0 ? 0 : w / 2);
      const y = row * r * 1.5;
      let cell = "";
      for (let k = 0; k < 6; k += 1) {
        const a = ((60 * k - 30) * Math.PI) / 180;
        cell += `${k === 0 ? "M" : "L"}${n(x + Math.cos(a) * (r - 3))} ${n(y + Math.sin(a) * (r - 3))}`;
      }
      cell += "Z";
      path += cell;
      if (random() < 0.07) lit += cell;
    }
  }
  return svg(
    `<radialGradient id="bg" cx="30%" cy="20%" r="110%"><stop offset="0" stop-color="#2b0707"/><stop offset=".5" stop-color="#110304"/><stop offset="1" stop-color="#040101"/></radialGradient>` +
      `<radialGradient id="vig" cx="50%" cy="50%" r="70%"><stop offset=".6" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".7"/></radialGradient>`,
    `<rect width="${W}" height="${H}" fill="url(#bg)"/>` +
      `<path d="${path}" fill="none" stroke="#ff3b3b" stroke-opacity=".38" stroke-width="2.5"/>` +
      `<path d="${lit}" fill="#ff2a2a" fill-opacity=".12" stroke="#ff6a5a" stroke-opacity=".6" stroke-width="2"/>` +
      `<rect width="${W}" height="${H}" fill="url(#vig)"/>`,
  );
}

function circuit(): string {
  const random = rng(0xc1c7);
  const grid = 40;
  let traces = "";
  let pads = "";
  for (let i = 0; i < 70; i += 1) {
    let x = Math.round((random() * W) / grid) * grid;
    let y = Math.round((random() * H) / grid) * grid;
    let d = `M${x} ${y}`;
    pads += `<circle cx="${x}" cy="${y}" r="4"/>`;
    const steps = 3 + Math.floor(random() * 5);
    for (let s = 0; s < steps; s += 1) {
      const horizontal = random() < 0.5;
      const length = grid * (1 + Math.floor(random() * 5)) * (random() < 0.5 ? -1 : 1);
      if (horizontal) x += length;
      else y += length;
      // A 45-degree jog every so often, as routed copper does.
      if (random() < 0.35) {
        x += grid;
        y += grid;
      }
      d += `L${x} ${y}`;
    }
    traces += d;
    pads += `<circle cx="${x}" cy="${y}" r="5" fill="none" stroke="#3dff8a" stroke-width="2"/>`;
  }
  return svg(
    `<radialGradient id="bg" cx="50%" cy="40%" r="90%"><stop offset="0" stop-color="#05230f"/><stop offset=".6" stop-color="#021009"/><stop offset="1" stop-color="#010603"/></radialGradient>` +
      `<pattern id="dots" width="${grid}" height="${grid}" patternUnits="userSpaceOnUse"><circle cx="1" cy="1" r="1" fill="#3dff8a" fill-opacity=".12"/></pattern>`,
    `<rect width="${W}" height="${H}" fill="url(#bg)"/>` +
      `<rect width="${W}" height="${H}" fill="url(#dots)"/>` +
      `<path d="${traces}" stroke="#3dff8a" stroke-opacity=".55" stroke-width="2.5" fill="none" stroke-linejoin="round"/>` +
      `<g fill="#3dff8a" fill-opacity=".6">${pads}</g>`,
  );
}

function horizon(): string {
  const horizonY = 610;
  let grid = "";
  for (let i = -24; i <= 24; i += 1) {
    grid += `M${n(W / 2 + i * 34)} ${horizonY}L${n(W / 2 + i * 260)} ${H}`;
  }
  for (let k = 1; k <= 14; k += 1) {
    const y = horizonY + (H - horizonY) * (k / 14) ** 1.8;
    grid += `M0 ${n(y)}H${W}`;
  }
  let bands = "";
  for (let k = 0; k < 6; k += 1) {
    const y = 470 + k * 22;
    bands += `<rect x="560" y="${y}" width="480" height="${4 + k * 2}" fill="#12081f"/>`;
  }
  const random = rng(0x5717);
  let stars = "";
  for (let i = 0; i < 120; i += 1) {
    stars += `<circle cx="${n(random() * W)}" cy="${n(random() * (horizonY - 40))}" r="${n(0.6 + random() * 1.4)}"/>`;
  }
  return svg(
    `<linearGradient id="sky" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#0a0418"/><stop offset=".55" stop-color="#2a0a3d"/><stop offset=".61" stop-color="#4a0e4e"/><stop offset=".61" stop-color="#0c0414"/><stop offset="1" stop-color="#12081f"/></linearGradient>` +
      `<linearGradient id="sun" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffd36b"/><stop offset=".5" stop-color="#ff6aa8"/><stop offset="1" stop-color="#b03cff"/></linearGradient>`,
    `<rect width="${W}" height="${H}" fill="url(#sky)"/>` +
      `<g fill="#ffffff" fill-opacity=".55">${stars}</g>` +
      `<circle cx="${W / 2}" cy="${horizonY - 40}" r="240" fill="url(#sun)"/>` +
      bands +
      `<rect y="${horizonY}" width="${W}" height="${H - horizonY}" fill="#0c0414"/>` +
      `<path d="${grid}" stroke="#ff4fd8" stroke-opacity=".55" stroke-width="1.5" fill="none"/>` +
      `<path d="M0 ${horizonY}H${W}" stroke="#ff9be6" stroke-width="2"/>`,
  );
}

const DRAW: Readonly<Record<BuiltinBackdropId, () => string>> = { neural, reactor, hex, circuit, horizon };

const cache = new Map<BuiltinBackdropId, string>();

/** The SVG markup of a built-in wallpaper. Deterministic: the same string every call. */
export function builtinBackdropSvg(id: BuiltinBackdropId): string {
  return DRAW[id]();
}

/**
 * A `data:` URL for a built-in wallpaper, drawn once per session. Percent-encoded
 * with `encodeURIComponent`, so the URL can never contain a `"`, a backslash or
 * whitespace and is safe inside `url("…")`.
 */
export function builtinBackdropUrl(id: BuiltinBackdropId): string {
  const known = cache.get(id);
  if (known !== undefined) return known;
  const url = `data:image/svg+xml,${encodeURIComponent(builtinBackdropSvg(id))}`;
  cache.set(id, url);
  return url;
}

/**
 * The only URL shapes the applier writes into `--gp-backdrop`: our own SVG data
 * URLs and the object URLs `wallpaperStore.ts` mints. Anything else is refused,
 * so a value can never close the `url("…")` it is written into.
 */
const SAFE_URL = /^(?:data:image\/svg\+xml,|blob:)[^"\\\s]*$/;

export function isSafeBackdropUrl(url: string): boolean {
  return SAFE_URL.test(url) && url.length < 2_000_000;
}

/** `url("…")` for `--gp-backdrop`, or null when the URL is not one this module trusts. */
export function backdropCssValue(url: string): string | null {
  return isSafeBackdropUrl(url) ? `url("${url}")` : null;
}
