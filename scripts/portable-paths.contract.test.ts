import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A file: URL's pathname property is not a filesystem path.
 *
 * On POSIX it happens to be one, which is why this keeps getting written and
 * why it always passes review. On Windows it is "/D:/a/repo/…": `existsSync`
 * says no, so a directory scan returns `[]` and the assertion built on it
 * passes vacuously; `join` against it produces "D:\D:\a\repo\…" and throws
 * ENOENT. Both shapes shipped here and only the Windows CI job caught them —
 * one as a silent empty result, which is the worse of the two.
 *
 * The chained form `new URL(...).pathname` is the original tell. The split
 * form — bind the URL, then read `.pathname` — shipped in two contract tests
 * and the chained-only regex missed them. Both are derived by scanning the
 * tree rather than listing known offenders.
 */
const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));
const ROOTS = ["scripts", "src", "contracts"];
const SKIP = new Set(["node_modules", "dist", ".git", "coverage", "target"]);
const SOURCE = /\.(ts|mts|cts|mjs|cjs|js|svelte)$/;

function sourceFiles(dir: string): string[] {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return [];
  }
  return entries.flatMap((entry) => {
    if (SKIP.has(entry)) return [];
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return SOURCE.test(entry) ? [path] : [];
  });
}

const FILES = ROOTS.flatMap((root) => sourceFiles(join(REPO_ROOT, root)));

/**
 * Drop comments before matching, so prose describing the defect — this file's
 * own header included — is not mistaken for the defect. Self-exclusion would
 * be the cheaper fix and a worse one: it would blind the check to real code
 * that lands here later.
 */
function code(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

/**
 * True when `source` treats a file URL's pathname property as a filesystem
 * path. Regex literals that mention `\.pathname` (this file) do not count:
 * the tell is an unescaped `.pathname` property read.
 */
export function usesFileUrlPathname(source: string): boolean {
  const body = code(source);
  if (/new URL\([^\n]*?\)\s*\.pathname/.test(body)) return true;
  if (!/\bimport\.meta\.url\b/.test(body) && !/new URL\s*\(/.test(body)) return false;
  return /(?<!\\)\.pathname\b/.test(body);
}

describe("filesystem paths are derived portably", () => {
  it("scans a real tree, so a passing run is not a vacuous one", () => {
    expect(FILES.length).toBeGreaterThan(100);
  });

  it("never treats a file: URL's pathname as a filesystem path", () => {
    const offenders = FILES.filter((file) => usesFileUrlPathname(readFileSync(file, "utf8"))).map(
      (file) => relative(REPO_ROOT, file).split(sep).join("/"),
    );

    expect(
      offenders,
      "use fileURLToPath(new URL(...)) — the pathname property breaks on Windows, and breaks " +
        "silently when the result is fed to a directory scan",
    ).toEqual([]);
  });

  it("catches pathname taken from a URL variable, not only a chained call", () => {
    const prop = "pathname";
    const split = `
      import { join } from "node:path";
      import { readFileSync } from "node:fs";
      const components = new URL("./", import.meta.url);
      readFileSync(join(components.${prop}, "x"), "utf8");
    `;
    expect(usesFileUrlPathname(split)).toBe(true);
    expect(usesFileUrlPathname(`const p = new URL("./", import.meta.url).${prop};`)).toBe(true);
    expect(
      usesFileUrlPathname(`readFileSync(fileURLToPath(new URL("./x.ts", import.meta.url)));`),
    ).toBe(false);
  });
});
