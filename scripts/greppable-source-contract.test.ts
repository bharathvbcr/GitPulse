import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A raw NUL byte in a source file makes it invisible to search.
 *
 * ripgrep — and `grep` without `-a` — sniff for NUL and classify the whole
 * file as binary, then skip it in silence. The result is not an error message
 * but an empty result set: a search for a symbol that IS in the file reports
 * that the file does not use it, which is exactly the shape of evidence people
 * act on. `DiffViewer.svelte` carried four of them (cache keys joined on a
 * literal NUL) and `fileRail.ts` one, and every `rg` over those files came
 * back clean.
 *
 * The escape `\u0000` produces the same string at runtime and leaves the file
 * plain text, so there is never a reason to type the byte itself.
 *
 * Derived by walking the tree rather than by listing the two files that had
 * it, so the next one is caught without anyone remembering this test exists.
 */
const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));
const ROOTS = ["scripts", "src", "contracts", "src-tauri/src", "docs"];
const SKIP = new Set(["node_modules", "dist", ".git", "coverage", "target"]);
const SOURCE = /\.(ts|mts|cts|mjs|cjs|js|svelte|rs|css|md|json|toml|yml|yaml)$/;

function sourceFiles(dir: string): string[] {
  let entries: string[];
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
const rel = (file: string) => relative(REPO_ROOT, file).split(sep).join("/");

describe("every source file stays searchable", () => {
  it("scans a real tree, so a passing run is not a vacuous one", () => {
    // Without this, a broken walk turns the rest of the file into tests that
    // assert nothing over an empty list and report success.
    expect(FILES.length).toBeGreaterThan(300);
  });

  it("contains no raw NUL byte, which would make ripgrep skip the file", () => {
    const offenders = FILES.filter((file) => readFileSync(file).includes(0)).map(rel);
    expect(offenders, "write \u0000 instead of the byte itself").toEqual([]);
  });
});
