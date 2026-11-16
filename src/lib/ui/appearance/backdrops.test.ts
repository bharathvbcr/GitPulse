import { describe, expect, it } from "vitest";
import {
  BUILTIN_BACKDROPS,
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
