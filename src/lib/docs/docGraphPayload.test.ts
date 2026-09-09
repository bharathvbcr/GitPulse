import { describe, expect, it } from "vitest";
import {
  DOC_GRAPH_EXTENT,
  docGraphToLoad,
  docGraphToVizPayload,
} from "./docGraphPayload";
import type { DocGraph } from "./client";
import { buildCodeGraphModel } from "../codeintel/graphPayload";
import { buildGraphIndex, filterGraphNodes } from "../codeintel/graphNavigation";

const sample: DocGraph = {
  nodes: [
    {
      path: "docs/a.md",
      title: "A",
      tags: [],
      degree: 2,
      depth: 0,
      x: 0,
      y: 0,
    },
    {
      path: "docs/b.md",
      title: "B",
      tags: ["t"],
      degree: 1,
      depth: 1,
      x: DOC_GRAPH_EXTENT,
      y: DOC_GRAPH_EXTENT,
    },
  ],
  edges: [{ source: 0, target: 1 }],
  total_notes: 10,
  truncated: true,
};

describe("docGraphToVizPayload", () => {
  it("carries the note classification through to the visibility filter without changing coverage", () => {
    const payload = docGraphToVizPayload(sample);
    const model = buildCodeGraphModel(payload);
    expect(filterGraphNodes(model,buildGraphIndex(model),{hideNotes:true})).toEqual([]);
    expect(payload.counts?.nodes_total).toBe(10);
    expect(payload.counts?.nodes_truncated).toBe(true);
  });
  it("keeps 1000×1000 coords and truncation honesty for the canvas", () => {
    const payload = docGraphToVizPayload(sample);
    expect(payload.level).toBe("doc");
    expect(payload.nodes).toHaveLength(2);
    expect(payload.links).toHaveLength(1);
    expect(payload.links[0].source).toBe("docs/a.md");
    expect(payload.links[0].target).toBe("docs/b.md");
    expect(payload.counts?.nodes_shown).toBe(2);
    expect(payload.counts?.nodes_total).toBe(10);
    expect(payload.counts?.nodes_truncated).toBe(true);
    expect(payload.nodes[0].x).toBe(0);
    expect(payload.nodes[1].x).toBe(DOC_GRAPH_EXTENT);
  });

  it("treats shown < total_notes as truncated even without the flag", () => {
    const payload = docGraphToVizPayload({
      ...sample,
      truncated: false,
      total_notes: 5,
    });
    expect(payload.counts?.nodes_truncated).toBe(true);
  });
});

describe("docGraphToLoad", () => {
  it("marks unavailable when the graph is null", () => {
    const load = docGraphToLoad(null, "boom");
    expect(load.available).toBe(false);
    expect(load.kind).toBe("doc_graph");
    expect(load.reason).toBe("boom");
  });

  it("wraps a graph as an available load", () => {
    const load = docGraphToLoad(sample, null);
    expect(load.available).toBe(true);
    expect(load.kind).toBe("doc_graph");
    expect(load.payload?.nodes).toHaveLength(2);
  });
});

describe("doc graph layout scaling", () => {
  it("scales vault coords into the canvas when level is doc", () => {
    const payload = docGraphToVizPayload(sample);
    const model = buildCodeGraphModel(payload, 524, 424);
    expect(model.nodes[0].x).toBeCloseTo(24, 0);
    expect(model.nodes[0].y).toBeCloseTo(24, 0);
    expect(model.nodes[1].x).toBeCloseTo(500, 0);
    expect(model.nodes[1].y).toBeCloseTo(400, 0);
  });
});
