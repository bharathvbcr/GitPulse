import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const srcRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function productionSources(directory: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...productionSources(path));
    } else if (
      /\.(?:ts|svelte)$/.test(entry.name) &&
      !/\.(?:test|spec|contract)\.ts$/.test(entry.name)
    ) {
      files.push(path);
    }
  }
  return files;
}

function callSites(pattern: RegExp): Array<{ location: string; body: string }> {
  const sites: Array<{ location: string; body: string }> = [];
  for (const path of productionSources(srcRoot)) {
    const source = readFileSync(path, "utf8");
    for (const match of source.matchAll(pattern)) {
      const line = source.slice(0, match.index).split("\n").length;
      sites.push({
        location: `${relative(srcRoot, path)}:${line}`,
        body: match[1],
      });
    }
  }
  return sites;
}

describe("repository attribution contracts", () => {
  it("gives every journal action an explicit path captured by its caller", () => {
    const sites = callSites(/harnessStore\.recordAction\(\{([\s\S]*?)\}\);/g);
    expect(sites.length).toBeGreaterThan(0);
    const violations = sites
      .filter(
        ({ body }) =>
          !/(?:^|\n)\s*repoPath(?:\s*:|\s*,)/.test(body) ||
          body.includes("$repoStore.currentPath"),
      )
      .map(({ location }) => location);
    expect(violations).toEqual([]);
  });

  it("gives every badge verdict an explicit path captured by its caller", () => {
    const sites = callSites(/harnessStore\.recordVerdict\(([\s\S]*?)\);/g);
    expect(sites.length).toBeGreaterThan(0);
    const violations = sites
      .filter(
        ({ body }) =>
          !body.includes(",") || body.includes("$repoStore.currentPath"),
      )
      .map(({ location }) => location);
    expect(violations).toEqual([]);
  });
});
