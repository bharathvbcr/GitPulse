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

/** Declared in a stylesheet or a component `<style>` block, or set from JS. */
const defined = new Set([
  ...collect(/^[ \t]*(--[A-Za-z0-9_-]+)[ \t]*:/gm, files).keys(),
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
