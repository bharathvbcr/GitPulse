import { describe, expect, it } from "vitest";
import {
  hitTestCodeGraph,
  paintCodeGraph,
  DEFAULT_CODE_GRAPH_THEME,
} from "../CodeGraphRenderer";
import { buildCodeGraphModel } from "../../../codeintel/graphPayload";
import type { GraphVizPayload } from "../../../codeintel/types";

function recordingCtx() {
  const calls: string[] = [];
  const ctx = {
    save: () => calls.push("save"),
    restore: () => calls.push("restore"),
    clearRect: () => calls.push("clearRect"),
    fillRect: () => calls.push("fillRect"),
    beginPath: () => calls.push("beginPath"),
    moveTo: () => calls.push("moveTo"),
    lineTo: () => calls.push("lineTo"),
    arc: () => calls.push("arc"),
    fill: () => calls.push("fill"),
    stroke: () => calls.push("stroke"),
    fillText: () => calls.push("fillText"),
    translate: () => calls.push("translate"),
    scale: () => calls.push("scale"),
    set fillStyle(_v: string) {},
    set strokeStyle(_v: string) {},
    set lineWidth(_v: number) {},
    set font(_v: string) {},
    set textAlign(_v: string) {},
    set textBaseline(_v: string) {},
  } as unknown as CanvasRenderingContext2D;
  return { ctx, calls };
}

const payload: GraphVizPayload = {
  level: "file",
  counts: {
    nodes_shown: 2,
    nodes_total: 10,
    nodes_truncated: true,
    max_nodes: 2,
  },
  nodes: [
    { id: "a", name: "a", kind: "file", path: "a.ts", community: "core", x: 100, y: 100, degree: 2 },
    { id: "b", name: "b", kind: "file", path: "b.ts", community: "core", x: 200, y: 100, degree: 2 },
  ],
  links: [{ source: "a", target: "b", kind: "imports" }],
};

describe("CodeGraphRenderer", () => {
  it("paints links and nodes without using the commit lane renderer", () => {
    const model = buildCodeGraphModel(payload, 400, 300);
    const { ctx, calls } = recordingCtx();
    paintCodeGraph(ctx, {
      model,
      widthCss: 400,
      heightCss: 300,
      panX: 0,
      panY: 0,
      scale: 1,
      selectedId: null,
      hoveredId: null,
      theme: DEFAULT_CODE_GRAPH_THEME,
    });
    expect(calls).toContain("arc");
    expect(calls).toContain("lineTo");
    expect(calls).toContain("fillText");
  });

  it("hit-tests nodes and links (not commit kinds)", () => {
    const model = buildCodeGraphModel(payload, 400, 300);
    const nodeHit = hitTestCodeGraph(model, 100, 100, 0, 0, 1);
    expect(nodeHit?.kind).toBe("node");
    expect(nodeHit?.id).toBe("a");

    const linkHit = hitTestCodeGraph(model, 150, 100, 0, 0, 1);
    expect(linkHit?.kind).toBe("link");
    expect(linkHit?.id).toContain("→");

    const miss = hitTestCodeGraph(model, 10, 10, 0, 0, 1);
    expect(miss).toBeNull();
  });
});
