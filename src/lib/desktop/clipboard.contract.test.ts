import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const libRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

function sourceFiles(root: string): string[] {
  return readdirSync(root).flatMap((entry) => {
    const path = join(root, entry);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    if (![".svelte", ".ts"].includes(extname(path)) || path.endsWith(".test.ts")) return [];
    return [path];
  });
}

describe("clipboard ownership contract", () => {
  it("routes every UI copy through the resilient clipboard seam", () => {
    const violations = sourceFiles(libRoot)
      .filter((path) => !path.endsWith(join("desktop", "clipboard.ts")))
      .filter((path) => readFileSync(path, "utf8").includes("navigator.clipboard"))
      .map((path) => relative(libRoot, path));

    expect(violations).toEqual([]);
  });
});
