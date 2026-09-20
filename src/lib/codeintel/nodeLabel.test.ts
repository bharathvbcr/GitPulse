import { describe, expect, it } from "vitest";
import { looksLikePath, nodeFilePath, nodeLabel, nodeLabelText } from "./nodeLabel";
import { filterGraphNodes, graphNodeOpenPath } from "./graphNavigation";
import { nodeLanguageKey } from "./graphPayload";
import type { GraphVizNode } from "./types";

describe("looksLikePath", () => {
  it("accepts a separator or a trailing extension", () => {
    expect(looksLikePath("src/lib/a.ts")).toBe(true);
    expect(looksLikePath("a.ts")).toBe(true);
    expect(looksLikePath("src\\lib\\a.ts")).toBe(true);
  });

  it("rejects a bare namespace", () => {
    expect(looksLikePath("SelectionReader")).toBe(false);
    expect(looksLikePath("std")).toBe(false);
  });
});

describe("nodeLabel", () => {
  it("puts the symbol first and keeps the full path for a tooltip", () => {
    const label = nodeLabel(
      "Sources/ExpanderEngine/AI/SelectionReader.swift::SelectionReader.readSelectionImpl",
    );
    expect(label.symbol).toBe("SelectionReader.readSelectionImpl");
    expect(label.file).toBe("SelectionReader.swift");
    expect(label.path).toBe("Sources/ExpanderEngine/AI/SelectionReader.swift");
  });

  it("splits on the FIRST separator so a C++ symbol keeps its own", () => {
    const label = nodeLabel("src/engine.cpp::outer::inner");
    expect(label.symbol).toBe("outer::inner");
    expect(label.file).toBe("engine.cpp");
  });

  it("treats a separator-free id as all symbol", () => {
    expect(nodeLabel("readSelection")).toEqual({
      symbol: "readSelection",
      file: null,
      path: null,
    });
  });

  it("does not mistake a bare namespace prefix for a file", () => {
    const label = nodeLabel("SelectionReader::readSelection");
    expect(label.symbol).toBe("SelectionReader::readSelection");
    expect(label.file).toBeNull();
    expect(label.path).toBeNull();
  });

  it("falls back to the filename when nothing follows the separator", () => {
    const label = nodeLabel("src/lib/a.ts::");
    expect(label.symbol).toBe("a.ts");
    expect(label.file).toBe("a.ts");
  });

  it("normalizes Windows separators before taking the basename", () => {
    expect(nodeLabel("src\\lib\\a.ts::Thing.run").file).toBe("a.ts");
  });

  it("survives empty and whitespace ids without throwing", () => {
    for (const id of ["", "   ", "::", "::x"]) {
      expect(() => nodeLabel(id)).not.toThrow();
    }
    expect(nodeLabel("").symbol).toBe("");
    // A leading "::" has no prefix, so there is no file to name.
    expect(nodeLabel("::x").file).toBeNull();
  });

  it("never returns a symbol longer than the id it came from", () => {
    for (const id of ["a/b.ts::C.d", "plain", "x::y", "a.ts::"]) {
      expect(nodeLabel(id).symbol.length).toBeLessThanOrEqual(id.length);
    }
  });
});

describe("nodeLabelText", () => {
  it("joins symbol and file, and omits a file equal to the symbol", () => {
    expect(nodeLabelText("a/b.ts::C.d")).toBe("C.d · b.ts");
    expect(nodeLabelText("a/b.ts::")).toBe("b.ts");
    expect(nodeLabelText("plain")).toBe("plain");
  });
});

describe("a namespace is not a path, and two filters used to judge nodes by one", () => {
  const model = (ids: string[]) => ({
    nodes: ids.map((id) => ({
      id,
      name: id,
      kind: "symbol",
      x: 0,
      y: 0,
      radius: 4,
      colorIndex: 0,
    })),
    links: [],
    communities: [],
    counts: null,
    level: null,
    generationId: null,
    warnings: [],
  }) as unknown as Parameters<typeof filterGraphNodes>[0];

  const index = {
    nodes: new Map(),
    incoming: new Map(),
    outgoing: new Map(),
    related: new Map(),
  } as unknown as Parameters<typeof filterGraphNodes>[1];

  const kept = (ids: string[], filters: Record<string, unknown>) =>
    filterGraphNodes(model(ids), index, filters as Parameters<typeof filterGraphNodes>[2]).map(
      (n) => n.id,
    );

  it("does not hide a production symbol whose NAMESPACE ends in Spec", () => {
    // `SpecialCaseSpec::run` has no file. Splitting on "::" invented the path
    // "SpecialCaseSpec", whose stem matches isGraphTestPath's /Specs?$/.
    expect(kept(["SpecialCaseSpec::run"], { hideTests: true })).toEqual(["SpecialCaseSpec::run"]);
    expect(kept(["MyTest::helper"], { hideTests: true })).toEqual(["MyTest::helper"]);
  });

  it("still hides a symbol that really does live in a test file", () => {
    expect(kept(["src/__tests__/a.ts::run"], { hideTests: true })).toEqual([]);
    expect(kept(["src/a.test.ts::run"], { hideTests: true })).toEqual([]);
  });

  it("does not hide a production symbol whose NAMESPACE is Vendor", () => {
    expect(kept(["Vendor::helper"], { hideGenerated: true })).toEqual(["Vendor::helper"]);
    expect(kept(["Build::step"], { hideGenerated: true })).toEqual(["Build::step"]);
  });

  it("still hides something that really is generated or vendored", () => {
    expect(kept(["vendor/lib/a.ts::run"], { hideGenerated: true })).toEqual([]);
    expect(kept(["src/a.generated.ts::run"], { hideGenerated: true })).toEqual([]);
  });

  it("keeps search working on the id even though the fake path is gone", () => {
    // The haystack already carried node.id, so dropping the invented path
    // costs the query nothing.
    expect(kept(["SpecialCaseSpec::run"], { query: "specialcase" })).toEqual([
      "SpecialCaseSpec::run",
    ]);
    expect(kept(["src/deep/a.ts::run"], { query: "deep" })).toEqual(["src/deep/a.ts::run"]);
  });
});

describe("nodeLanguageKey consults the prefix only when it names a file", () => {
  const node = (over: Partial<GraphVizNode>): GraphVizNode =>
    ({ id: "x", kind: "symbol", name: "x", ...over }) as GraphVizNode;

  it("does not label a symbol by a namespace that shares a language's name", () => {
    // "Rust::new" is a namespace, not rust source. Handing "Rust" to the
    // resolver labelled the node by its namespace rather than its file.
    expect(nodeLanguageKey(node({ id: "Rust::new" }))).toBe("file");
    expect(nodeLanguageKey(node({ id: "Swift::make" }))).toBe("file");
  });

  it("still labels a symbol by its real file prefix", () => {
    expect(nodeLanguageKey(node({ id: "src/lib/a.ts::run" }))).not.toBe("file");
  });

  it("explicit metadata still wins over the id", () => {
    expect(nodeLanguageKey(node({ id: "Rust::new", language: "TypeScript" }))).not.toBe("file");
  });
});

describe("graphNodeOpenPath shares the one predicate", () => {
  const node = (over: Partial<GraphVizNode>): GraphVizNode =>
    ({ id: "x", kind: "symbol", name: "x", ...over }) as GraphVizNode;

  it("agrees with nodeFilePath on qualified symbol ids", () => {
    for (const id of [
      "src/lib/a.ts::Thing.run",
      "SelectionReader::readSelection",
      "a.ts::run",
      "bare",
    ]) {
      expect(graphNodeOpenPath(node({ id }))).toBe(nodeFilePath(id));
    }
  });

  it("still prefers an explicit path and still refuses a subsystem", () => {
    expect(graphNodeOpenPath(node({ id: "a::b", path: "real/path.ts" }))).toBe("real/path.ts");
    expect(graphNodeOpenPath(node({ kind: "subsystem", id: "src/lib/a.ts::T" }))).toBeNull();
  });
});
