import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The one way into History › Graph › Suspects from elsewhere, and the places
 * that use it: Blame's file, a diff hunk's symbol, a code-graph node, and the
 * command palette.
 */

const calls: string[] = [];
vi.mock("../stores/repoStore", async () => {
  const { readable } = await import("svelte/store");
  return {
    currentRepoPath: readable("/r/a"),
    repoStore: {
      setActiveTab: (tab: string, section: string) => calls.push(`tab:${tab}/${section}`),
      setHistoryList: (list: string) => calls.push(`list:${list}`),
    },
  };
});
vi.mock("../stores/interfaceStore", () => ({
  interfaceStore: { setGlobalSurface: (surface: string) => calls.push(`surface:${surface}`) },
}));
vi.mock("./suspectsQuery", () => ({
  suspectsQuery: {
    askAbout: (path: string, symptom: string) => {
      calls.push(`ask:${path}:${symptom}`);
      return Promise.resolve();
    },
    requestFocus: () => calls.push("focus"),
  },
}));

const { openSuspectsFor } = await import("./openSuspects");

beforeEach(() => {
  calls.length = 0;
});

describe("openSuspectsFor", () => {
  it("lands on Graph's Suspects list, list after section, and asks about the symptom", () => {
    openSuspectsFor("  src/parser.rs::parse  ");
    expect(calls).toEqual([
      "surface:repository",
      "tab:history/graph",
      // After the section: naming Graph resets its list to the commit graph.
      "list:suspects",
      "ask:/r/a:src/parser.rs::parse",
    ]);
  });

  it("puts the cursor in the symptom field when nothing is named", () => {
    openSuspectsFor();
    openSuspectsFor("   ");
    openSuspectsFor(null);
    expect(calls.filter((c) => c === "focus")).toHaveLength(3);
    expect(calls.some((c) => c.startsWith("ask:"))).toBe(false);
  });
});

const here = dirname(fileURLToPath(import.meta.url));
const read = (...parts: string[]) => readFileSync(join(here, "..", ...parts), "utf8");

describe("the ways in", () => {
  it("Blame offers its file as the symptom", () => {
    const blame = read("components", "BlameViewer.svelte");
    expect(blame).toContain("onclick={() => openSuspectsFor(filePath)}");
    expect(blame).toContain("{#if filePath}");
  });

  it("a diff hunk offers the symbol it changes", () => {
    const diff = read("components", "DiffViewer.svelte");
    expect(diff).toContain("onclick={() => openSuspectsFor(currentHunkSymbol)}");
    const at = diff.indexOf("openSuspectsFor(currentHunkSymbol)");
    expect(diff.lastIndexOf("{#if currentHunkSymbol}", at)).toBeGreaterThan(-1);
  });

  it("a code-graph node offers its DevMap id, and only on the code graph", () => {
    // Subsystem (map preview) and doc nodes are not DevMap symbol or file ids,
    // so the walk could not resolve them as a symptom.
    const canvas = read("components", "CodeGraphCanvas.svelte");
    const at = canvas.indexOf("openSuspectsFor(selectedNode.id)");
    expect(at).toBeGreaterThan(-1);
    expect(canvas.lastIndexOf('{#if load?.kind === "code_graph"}', at)).toBeGreaterThan(-1);
  });

  it("the palette opens it bare and for the open file", () => {
    const catalog = read("palette", "catalog.ts");
    expect(catalog).toContain('id: "history:suspects"');
    expect(catalog).toContain("action: () => openSuspectsFor(),");
    expect(catalog).toContain('id: "history:suspects-file"');
    expect(catalog).toContain("action: () => openSuspectsFor(state.selectedFilePath),");
    expect(catalog).toContain('"Open a file in Explorer first"');
  });
});
