import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Every custom property the app reads has to be one something defines.
 *
 * A misspelt token is invisible in a way a misspelt class name is not: CSS
 * resolves `var(--typo, #8b5cf6)` to the fallback and paints a plausible
 * colour, so the element looks designed rather than broken, and the JS readers
 * (`v()`, `read()`) take a default the same way. `--accent` — which nothing has
 * ever defined, the token being `--accent-color` — painted the storage
 * sparkline a fixed purple that ignored the user's accent for as long as that
 * panel existed. Nothing failed; it just quietly stopped following the theme.
 *
 * Both surfaces are swept, and the definitions are read from the whole tree
 * rather than app.css alone, so a token declared inside a component's own
 * `<style>` block still counts.
 */
const SRC = fileURLToPath(new URL("../../", import.meta.url));

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(full);
    return /\.(css|svelte|ts|js)$/.test(entry.name) ? [full] : [];
  });
}

const files = sourceFiles(SRC).map((path) => ({
  path,
  isTest: /\.(test|contract\.test)\.[tj]s$/.test(path),
  text: readFileSync(path, "utf8"),
}));

function collect(pattern: RegExp, from: typeof files): Map<string, string> {
  const found = new Map<string, string>();
  for (const file of from) {
    for (const match of file.text.matchAll(pattern)) {
      if (!found.has(match[1])) found.set(match[1], file.path.slice(SRC.length));
    }
  }
  return found;
}

/**
 * Where a custom-property declaration is allowed to begin: the start of a
 * line, immediately after the `{` that opens a block, or immediately after the
 * `;` that closed the declaration before it. That is the whole of CSS's
 * answer, and matching the boundary instead of the line is what lets this see
 * the second and later declarations on a line.
 *
 * Anchoring to the line alone under-collected for as long as the compact
 * one-line block style has been in the tree — `.status-shell[data-material]`
 * has declared three tokens on one line since before this guard existed. It
 * stayed invisible because every token written that way also appeared
 * first-on-a-line somewhere else, so the sweep found it anyway. The first
 * tokens that did not, `--hue-b`/`--hue-c`/`--hue-d`, were reported as
 * undefined from three characters after the `--hue-a` it did find, with all
 * four declared together on one line.
 *
 * The boundary is also what keeps prose out: a token named in a `*`-prefixed
 * doc comment follows none of the three, so describing a token still never
 * counts as declaring one.
 */
const DECLARATION = /(?:^|[{;])[ \t]*(--[A-Za-z0-9_-]+)[ \t]*:/gm;

/** Declared in a stylesheet or a component `<style>` block, or set from JS. */
const defined = new Set([
  ...collect(DECLARATION, files).keys(),
  ...collect(/setProperty\(\s*["'`](--[A-Za-z0-9_-]+)/g, files).keys(),
]);

/** Read by CSS, and by the helpers that pull a token through getComputedStyle. */
const production = files.filter((file) => !file.isTest);
const cssReferences = collect(/var\(\s*(--[A-Za-z0-9_-]+)/g, production);
const scriptReferences = collect(
  /(?:\bv|\bread|getPropertyValue)\(\s*["'`](--[A-Za-z0-9_-]+)/g,
  production,
);

describe("custom properties resolve to something that defines them", () => {
  it("sweeps a realistic surface, so a broken pattern cannot pass vacuously", () => {
    expect(files.length).toBeGreaterThan(100);
    expect(defined.size).toBeGreaterThan(20);
    expect(cssReferences.size).toBeGreaterThan(20);
    expect(scriptReferences.size).toBeGreaterThan(0);
  });

  it("counts a declaration that is not the first one on its line", () => {
    // What this guard is for is catching a token nothing declares, so the half
    // of it that finds declarations has to find all of them. A line-anchored
    // sweep saw only the first declaration of a compact block and reported the
    // rest as undefined — a missing-token report for tokens a few characters
    // from one it had just accepted.
    const block = ".shell {\n  --probe-one:1; --probe-two:2; --probe-three:3;\n}\n";
    expect([...block.matchAll(DECLARATION)].map((match) => match[1])).toEqual([
      "--probe-one",
      "--probe-two",
      "--probe-three",
    ]);
  });

  it("does not count a token named in prose as a declaration", () => {
    // The other half of the boundary: matching `--token:` anywhere would let a
    // comment vouch for the very name it is warning about, and this guard's
    // whole value is that a misspelt token has nothing to hide behind.
    const prose = "/**\n * --probe-documented: described here, declared nowhere.\n */\n";
    expect([...prose.matchAll(DECLARATION)]).toEqual([]);
  });

  it("defines every token reached through var()", () => {
    const undefinedTokens = [...cssReferences]
      .filter(([token]) => !defined.has(token))
      .map(([token, where]) => `${token} (${where})`);
    expect(
      undefinedTokens,
      "var() on an undefined token silently paints its fallback instead of failing",
    ).toEqual([]);
  });

  it("defines every token read from script", () => {
    const undefinedTokens = [...scriptReferences]
      .filter(([token]) => !defined.has(token))
      .map(([token, where]) => `${token} (${where})`);
    expect(
      undefinedTokens,
      "a token read through getComputedStyle returns empty and falls back to the default",
    ).toEqual([]);
  });
});
