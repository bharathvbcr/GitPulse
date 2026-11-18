import { describe, expect, it } from "vitest";
import {
  BACKDROP_MOTION,
  BUILTIN_BACKDROPS,
  MOTION_TEXTURE_IDS,
  motionTextureSvg,
  motionTextureUrl,
  BUILTIN_BACKDROP_IDS,
  backdropCssValue,
  builtinBackdropSvg,
  builtinBackdropUrl,
  isBackdropId,
  isSafeBackdropUrl,
} from "./backdrops";

/** Open/close tag balance of an SVG string; null when balanced, else the first problem. */
function unbalanced(svg: string): string | null {
  const stack: string[] = [];
  for (const match of svg.matchAll(/<(\/?)([a-zA-Z][\w:-]*)([^>]*?)(\/?)>/g)) {
    const [, closing, name, , selfClosing] = match;
    if (selfClosing) continue;
    if (closing) {
      const open = stack.pop();
      if (open !== name) return `</${name}> closes <${open ?? "nothing"}>`;
    } else {
      stack.push(name);
    }
  }
  return stack.length === 0 ? null : `<${stack.at(-1)}> is never closed`;
}

describe.each(BUILTIN_BACKDROP_IDS.map((id) => [id] as const))("built-in wallpaper %s", (id) => {
  const svg = builtinBackdropSvg(id);

  it("is the same picture every time (seeded, not random)", () => {
    expect(builtinBackdropSvg(id)).toBe(svg);
    expect(builtinBackdropUrl(id)).toBe(builtinBackdropUrl(id));
  });

  it("is a well-formed, self-contained, static SVG", () => {
    expect(svg.startsWith("<svg ")).toBe(true);
    expect(unbalanced(svg)).toBeNull();
    // No script, no animation, nothing fetched: a picture, not a document.
    for (const forbidden of [/<script/i, /<animate/i, /<set\b/i, /<image/i, /<foreignObject/i, /\bon[a-z]+=/i, /href=/i, /@import/i]) {
      expect(svg).not.toMatch(forbidden);
    }
    // Every url() reference is to a local gradient or pattern id.
    for (const match of svg.matchAll(/url\(([^)]*)\)/g)) expect(match[1]).toMatch(/^#[a-z]+$/);
    expect(svg).not.toMatch(/NaN|Infinity|undefined/);
  });

  it("stays small enough to inline as a data URL", () => {
    expect(builtinBackdropUrl(id).length).toBeLessThan(400_000);
  });

  it("produces a URL the applier accepts", () => {
    const url = builtinBackdropUrl(id);
    expect(isSafeBackdropUrl(url)).toBe(true);
    expect(backdropCssValue(url)).toBe(`url("${url}")`);
  });
});

describe("wallpaper ids and URLs", () => {
  it("lists every built-in once, with a label", () => {
    expect(BUILTIN_BACKDROPS.map((entry) => entry.id)).toEqual([...BUILTIN_BACKDROP_IDS]);
    for (const entry of BUILTIN_BACKDROPS) expect(entry.label.trim()).not.toBe("");
  });

  it("recognises only the known ids", () => {
    for (const id of ["none", "custom", ...BUILTIN_BACKDROP_IDS]) expect(isBackdropId(id)).toBe(true);
    for (const id of ["", "None", "url(x)", 3, null, undefined, "neural "]) expect(isBackdropId(id)).toBe(false);
  });

  it("refuses any URL that could escape url(\"…\") or load something else", () => {
    for (const url of [
      'blob:http://x/a")',
      "blob:http://x/a\\",
      "blob:http://x/a b",
      "data:text/html,<script>",
      "data:image/png;base64,AAAA",
      "https://example.com/a.png",
      "javascript:alert(1)",
      "file:///etc/passwd",
      "",
    ]) {
      expect(isSafeBackdropUrl(url), url).toBe(false);
      expect(backdropCssValue(url), url).toBeNull();
    }
    expect(isSafeBackdropUrl("blob:http://localhost:1420/5b0c0f3e-2c1d-4b8e-9b9a-1f2e3d4c5b6a")).toBe(true);
  });
});

const STATIC_FORBIDDEN = [/<script/i, /<animate/i, /<set\b/i, /<image/i, /<foreignObject/i, /\bon[a-z]+=/i, /href=/i, /@import/i, /@keyframes/i, /animation/i];

describe.each(BUILTIN_BACKDROP_IDS.map((id) => [id] as const))("motion for built-in wallpaper %s", (id) => {
  const layers = BACKDROP_MOTION[id];

  it("has a plan with at least one accent", () => {
    expect(layers.length).toBeGreaterThanOrEqual(2);
    expect(layers.some((layer) => layer.texture !== "base")).toBe(true);
  });

  it("starts on the still: only the base drifts, every accent starts transparent, no delay is negative", () => {
    for (const layer of layers) {
      if (layer.kind === "drift") expect(layer.texture).toBe("base");
      else expect(layer.texture).not.toBe("base");
      expect(layer.seconds).toBeGreaterThan(0);
      expect(layer.delay ?? 0).toBeGreaterThanOrEqual(0);
    }
  });

  it("names only textures that exist", () => {
    for (const layer of layers) {
      if (layer.texture !== "base") expect(MOTION_TEXTURE_IDS).toContain(layer.texture);
    }
  });
});

describe.each(MOTION_TEXTURE_IDS.map((id) => [id] as const))("motion texture %s", (id) => {
  const svg = motionTextureSvg(id);

  it("is a well-formed, static, self-contained SVG on the stills' canvas", () => {
    expect(svg.startsWith('<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="1000"')).toBe(true);
    expect(svg).toContain('preserveAspectRatio="xMidYMid slice"');
    expect(unbalanced(svg)).toBeNull();
    // Moving is app.css's job: a texture that animated itself would repaint every frame.
    for (const forbidden of STATIC_FORBIDDEN) expect(svg).not.toMatch(forbidden);
    for (const match of svg.matchAll(/url\(([^)]*)\)/g)) expect(match[1]).toMatch(/^#[a-z]+$/);
    expect(svg).not.toMatch(/NaN|Infinity|undefined/);
  });

  it("is deterministic and the applier accepts its URL", () => {
    expect(motionTextureSvg(id)).toBe(svg);
    expect(isSafeBackdropUrl(motionTextureUrl(id))).toBe(true);
    expect(motionTextureUrl(id).length).toBeLessThan(400_000);
  });

  it("is used by some wallpaper", () => {
    expect(BUILTIN_BACKDROP_IDS.some((backdrop) => BACKDROP_MOTION[backdrop].some((layer) => layer.texture === id))).toBe(true);
  });
});
