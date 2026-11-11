import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The entry chunk has a hard byte budget (`MAX_PRODUCTION_CHUNK_BYTES` in
 * vite.config.ts), and `vite build` is the only thing that measures it — neither
 * `bun run check` nor this suite builds. These three components are the ones
 * moved out of the entry chunk to get back under it (663 KB against 640 KB), so
 * this pins the move. It is deliberately stricter than the budget: it rejects any
 * static import, including one from inside an already-lazy chunk (harmless to the
 * entry chunk today, but one refactor from not being), so a new static importer
 * has to be a conscious change to this list.
 */
const src = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(full);
    if (!/\.(svelte|ts)$/.test(entry.name) || entry.name.endsWith(".test.ts")) return [];
    return [full];
  });
}

const DEFERRED = ["onboarding/ProductTour", "MergeBranchDialog", "ViewGuideArt"];
const files = sourceFiles(src).map((path) => ({ path, text: readFileSync(path, "utf8") }));

describe("entry-chunk budget: components kept out of the startup bundle", () => {
  for (const component of DEFERRED) {
    const leaf = component.split("/").pop();
    it(`${leaf} is only ever reached through a dynamic import`, () => {
      const staticImport = new RegExp(
        `^\\s*import\\s+(?!type\\b)[^;]*?from\\s+["'][^"']*/${leaf}\\.svelte["']|^\\s*import\\s+["'][^"']*/${leaf}\\.svelte["']`,
        "m",
      );
      const offenders = files.filter((file) => staticImport.test(file.text)).map((file) => file.path);
      expect(offenders).toEqual([]);
      const dynamic = new RegExp(`import\\(\\s*["'][^"']*/${leaf}\\.svelte["']\\s*\\)`);
      expect(files.some((file) => dynamic.test(file.text))).toBe(true);
    });
  }
});
