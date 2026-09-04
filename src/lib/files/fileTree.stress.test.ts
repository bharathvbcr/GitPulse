import { describe, expect, it } from "vitest";
import { STRESS_TIMEOUT_MS, expectWithinBudget } from "../__tests__/perfBudget";
import { buildFileTree, flattenFileTree } from "./fileTree";
import type { FileTree, FileTreeDir } from "./fileTree";

/** Recursive dir/file totals — the exactness check for scale builds. */
function countTree(tree: FileTree): { dirs: number; files: number } {
  let dirs = 0;
  let files = tree.files.length;
  const walkDir = (dir: FileTreeDir): void => {
    dirs += 1;
    files += dir.files.length;
    dir.dirs.forEach(walkDir);
  };
  tree.dirs.forEach(walkDir);
  return { dirs, files };
}

describe("buildFileTree + flattenFileTree stress: 100k synthetic paths", () => {
  // 20 top dirs x 10 mid dirs x 10 leaf dirs x 50 files = 100_000 files;
  // dir total = 20 + 200 + 2000 = 2220. Some unicode tops and spaced
  // file names ride along to keep collator paths honest.
  function syntheticCorpus(): string[] {
    const paths: string[] = [];
    const unicodeTops = ["données", "日本語リソース"];
    for (let t = 0; t < 20; t += 1) {
      const top = t < unicodeTops.length ? unicodeTops[t] : `t${String(t).padStart(2, "0")}`;
      for (let m = 0; m < 10; m += 1) {
        for (let l = 0; l < 10; l += 1) {
          const base = `${top}/mid-${m}/leaf-${l}`;
          for (let f = 0; f < 50; f += 1) {
            if (f % 10 === 0) {
              paths.push(`${base}/café notes ${f}.txt`);
            } else {
              paths.push(`${base}/file-${String(f).padStart(3, "0")}.ts`);
            }
          }
        }
      }
    }
    return paths;
  }

  it("builds, flattens, and stays deterministic under generous bounds", () => {
    const corpus = syntheticCorpus();
    expect(corpus).toHaveLength(100_000);

    const buildStartedAt = performance.now();
    const tree = buildFileTree(corpus);
    const buildMs = performance.now() - buildStartedAt;

    // Exact totals: 2220 dirs, 100k files.
    const totals = countTree(tree);
    expect(totals).toEqual({ dirs: 2220, files: 100_000 });

    const flattenStartedAt = performance.now();
    const rows = flattenFileTree(tree, () => false);
    const flattenMs = performance.now() - flattenStartedAt;

    expectWithinBudget(buildMs, 1000, "buildFileTree over 10k paths");
    expectWithinBudget(flattenMs, 1000, "flattenTree over 10k paths");
    expect(rows).toHaveLength(totals.dirs + totals.files);

    // Memory sanity: every row key is distinct despite the prefix scheme.
    const keys = rows.map((r) => r.key);
    expect(new Set(keys).size).toBe(rows.length);

    // Determinism: a second full pipeline run deep-equals the first.
    const secondRows = flattenFileTree(buildFileTree(corpus), () => false);
    expect(JSON.stringify(secondRows)).toBe(JSON.stringify(rows));
  }, STRESS_TIMEOUT_MS);

  it("keeps unicode and spaced names sorted via the shared collator", () => {
    const tree = buildFileTree([
      "日本語リソース/README.md",
      "données/notes.txt",
      "zeta/a.ts",
      "alpha b/c.ts",
    ]);
    const topNames = tree.dirs.map((d) => d.name);
    expect(new Set(topNames).size).toBe(topNames.length);
    expect(topNames).toContain("alpha b");
    expect(topNames.indexOf("alpha b")).toBeLessThan(topNames.indexOf("données"));
    expect(topNames.indexOf("données")).toBeLessThan(topNames.indexOf("zeta"));
    expect(topNames.indexOf("zeta")).toBeLessThan(topNames.indexOf("日本語リソース"));
  });
});

describe("buildFileTree stress: adversarial shapes", () => {
  it("survives 10k paths sharing one 200-segment-deep chain under bound", () => {
    const spine = Array.from({ length: 200 }, (_, k) => `d${String(k).padStart(3, "0")}`);
    const paths = Array.from(
      { length: 10_000 },
      (_, i) => `${[...spine, `f${i}`].join("/")}`
    );

    const startedAt = performance.now();
    const tree = buildFileTree(paths);
    const rows = flattenFileTree(tree, () => false);
    const elapsedMs = performance.now() - startedAt;

    expectWithinBudget(elapsedMs, 1000, "10k paths on one 200-segment chain");
    // One chain: 200 dir headers + 10k file rows.
    expect(countTree(tree)).toEqual({ dirs: 200, files: 10_000 });
    expect(rows).toHaveLength(200 + 10_000);
    const deepest = rows.filter((r) => r.kind === "dir" && r.depth === 199);
    expect(deepest).toHaveLength(1);
    expect(deepest[0].kind === "dir" && deepest[0].path).toBe(spine.join("/"));
  }, STRESS_TIMEOUT_MS);

  it("collapses duplicate-heavy input (same 100 paths x1000) to the unique-only tree", () => {
    const unique = Array.from({ length: 100 }, (_, i) => `mod${i % 10}/pkg${i}/file${i}.ts`);
    const storm: string[] = [];
    for (let rep = 0; rep < 1000; rep += 1) storm.push(...unique);
    expect(storm).toHaveLength(100_000);

    const fromStorm = buildFileTree(storm);
    const fromUnique = buildFileTree(unique);
    expect(JSON.stringify(fromStorm)).toBe(JSON.stringify(fromUnique));
    expect(countTree(fromStorm)).toEqual({ dirs: 110, files: 100 });
  });

  it("produces zero rows without throwing on pathological names", () => {
    const pathological = ["..", ".", "//", "\\", "   ", "", "a/../../b", "/abs", "./."];
    let rows: ReturnType<typeof flattenFileTree> = [];
    expect(() => {
      const tree = buildFileTree(pathological);
      rows = flattenFileTree(tree, () => false);
    }).not.toThrow();
    expect(buildFileTree(pathological)).toEqual({ dirs: [], files: [] });
    expect(rows).toEqual([]);
  });

  it("keeps pathological entries out while their well-formed siblings survive", () => {
    const tree = buildFileTree(["../evil", ".", "//", "keep.ts", "src\\ok\\x.ts"]);
    expect(tree.files.sort()).toEqual(["keep.ts"]);
    // keep.ts at root plus src/ok/x.ts inside its two dirs.
    expect(countTree(tree)).toEqual({ dirs: 2, files: 2 });
  });
});
