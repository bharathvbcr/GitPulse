import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { parse } from "svelte/compiler";
import { describe, expect, it } from "vitest";
import { keyedList } from "../src/lib/ui/eachKeys";

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function span(source: string, node: unknown): string {
  if (!record(node) || typeof node.start !== "number" || typeof node.end !== "number") {
    throw new Error("Expected a source-located Svelte node");
  }
  return source.slice(node.start, node.end);
}

/** Evaluate the actual template's iterable and key, rather than a copy of it. */
function templateKeys(file: string, iterable: string, scope: Record<string, unknown>): unknown[] {
  const source = readFileSync(fileURLToPath(new URL(`../src/lib/components/${file}`, import.meta.url)), "utf8");
  const found: Record<string, unknown>[] = [];
  const visit = (node: unknown): void => {
    if (!record(node)) return;
    if (node.type === "EachBlock" && span(source, node.expression).includes(iterable)) found.push(node);
    for (const child of Object.values(node)) {
      if (Array.isArray(child)) child.forEach(visit);
      else if (record(child)) visit(child);
    }
  };
  visit(parse(source, { modern: true }));
  expect(found, `Missing/ambiguous each block for ${file}: ${iterable}`).toHaveLength(1);
  const block = found[0];
  const evaluate = new Function("keyedList", ...Object.keys(scope),
    `return (${span(source, block.expression)}).map((${span(source, block.context)}, ${typeof block.index === "string" ? block.index : "__index"}) => (${span(source, block.key)}));`);
  const keys: unknown = evaluate(keyedList, ...Object.values(scope));
  if (!Array.isArray(keys)) throw new Error("Template did not produce keys");
  return keys;
}

describe("repeatable provider rows keep unique Svelte identities", () => {
  const cases: Array<{ file: string; iterable: string; scope: Record<string, unknown>; count: number }> = [
    { file: "RepoMapPanel.svelte", iterable: "brokenDisplay.shown", scope: { brokenDisplay: { shown: [{ source: "A.md", target: "missing" }, { source: "A.md", target: "missing" }] } }, count: 2 },
    { file: "RepoMapPanel.svelte", iterable: "docsHits", scope: { docsHits: [{ path: "A.md", line: 1, score: 1 }, { path: "A.md", line: 1, score: 1 }] }, count: 2 },
    { file: "RepoMapPanel.svelte", iterable: "linkResult.links", scope: { linkResult: { links: Array.from({ length: 2 }, () => ({ from_repo: "a", from_file: "f", module_specifier: "x", to_repo: "b" })) } }, count: 2 },
    { file: "files/MarkDevViewer.svelte", iterable: "backlinks", scope: { backlinks: [{ path: "A.md", line: 1, offset: 0 }, { path: "A.md", line: 1, offset: 20 }] }, count: 2 },
    { file: "OperationBanner.svelte", iterable: "operation.warnings", scope: { operation: { warnings: ["busy", "busy", "busy#1"] } }, count: 3 },
    { file: "GitHubPanel.svelte", iterable: "ciReport.steps", scope: { ciReport: { steps: [{ name: "check" }, { name: "check" }] } }, count: 2 },
    { file: "StoragePanel.svelte", iterable: "report.reclaim", scope: { report: { reclaim: [{ category: "ab", label: "c" }, { category: "a", label: "bc" }, { category: "ab", label: "c" }] } }, count: 3 },
  ];
  for (const { file, iterable, scope, count } of cases) {
    it(`${file}: ${iterable} survives repeated identities without dropping rows`, () => {
      const keys = templateKeys(file, iterable, scope);
      expect(keys).toHaveLength(count);
      expect(new Set(keys).size).toBe(count);
    });
  }
});
