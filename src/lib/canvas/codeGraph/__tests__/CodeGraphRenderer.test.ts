import { describe, expect, it, vi } from "vitest";
import {
  hitTestCodeGraph,
  paintCodeGraph,
  DEFAULT_CODE_GRAPH_THEME,
  buildCodeGraphModel,
  type ProjectedPoint,
} from "../../../codeintel/codeMap";
import type { GraphVizPayload } from "../../../codeintel/types";
import { getLanguageIconColor } from "../../../language/languageLogos";

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
  it("omits direction arrowheads for undirected subsystem neighbors", () => {
    const {ctx,calls}=recordingCtx();
    const model=buildCodeGraphModel({...payload,links:[{source:"a",target:"b",kind:"neighbor"}]});
    paintCodeGraph(ctx,{model,widthCss:400,heightCss:300,panX:0,panY:0,scale:1,selectedId:"a",hoveredId:null,theme:DEFAULT_CODE_GRAPH_THEME});
    expect(calls.filter(c=>c==="lineTo")).toHaveLength(1);
  });
  it("does not submit offscreen geometry, but retains edges crossing the viewport", () => {
    const {ctx,calls} = recordingCtx();
    const model = buildCodeGraphModel({...payload,nodes:payload.nodes.map(n=>({...n,x:(n.x ?? 0)+1000}))});
    paintCodeGraph(ctx,{model,widthCss:400,heightCss:300,panX:0,panY:0,scale:1,selectedId:null,hoveredId:null,theme:DEFAULT_CODE_GRAPH_THEME});
    expect(calls.filter(c=>c==="arc")).toHaveLength(0);
    expect(calls.filter(c=>c==="lineTo")).toHaveLength(0);
    model.nodes[0].x = -100;
    paintCodeGraph(ctx,{model,widthCss:400,heightCss:300,panX:0,panY:0,scale:1,selectedId:null,hoveredId:null,theme:DEFAULT_CODE_GRAPH_THEME});
    expect(calls.filter(c=>c==="lineTo")).toHaveLength(1);
  });
  it("never paints or picks nodes and links excluded by a hard filter", () => {
    const model=buildCodeGraphModel(payload,400,300), visibleIds=new Set(["a"]);
    const {ctx,calls}=recordingCtx();
    paintCodeGraph(ctx,{model,widthCss:400,heightCss:300,panX:0,panY:0,scale:1,selectedId:null,hoveredId:null,visibleIds,showLabels:false,theme:DEFAULT_CODE_GRAPH_THEME});
    expect(calls.filter(c=>c==="arc")).toHaveLength(1);
    expect(calls.filter(c=>c==="lineTo")).toHaveLength(0);
    expect(hitTestCodeGraph(model,200,100,0,0,1,visibleIds)).toBeNull();
    expect(hitTestCodeGraph(model,150,100,0,0,1,visibleIds)).toBeNull();
    expect(hitTestCodeGraph(model,100,100,0,0,1,visibleIds)?.id).toBe("a");
  });
  it("keeps a selected neighborhood highlighted while hovering another node", () => {
    const model = buildCodeGraphModel({nodes:[
      {id:"a",name:"A",x:100,y:100},{id:"b",name:"B",x:150,y:100},{id:"c",name:"C",x:250,y:100},
    ],links:[{source:"a",target:"b"}]});
    const {ctx} = recordingCtx();
    const opacity:number[] = [];
    vi.spyOn(ctx,"fill").mockImplementation(() => {opacity.push(ctx.globalAlpha);});
    paintCodeGraph(ctx,{model,widthCss:400,heightCss:300,panX:0,panY:0,scale:1,selectedId:"a",hoveredId:"c",theme:DEFAULT_CODE_GRAPH_THEME,showLabels:false});
    expect(opacity[2]).toBe(1); // B remains emphasized with the selected A.
  });

  it("culls offscreen label candidates before applying the label budget", () => {
    const nodes = Array.from({length:100},(_,i)=>({id:`n${i}`,name:`N${i}`,x:i<99?-1000:100,y:100,degree:200-i}));
    const {ctx} = recordingCtx();
    const labels=vi.spyOn(ctx,"fillText");
    paintCodeGraph(ctx,{model:buildCodeGraphModel({nodes,links:[]}),widthCss:400,heightCss:300,panX:0,panY:0,scale:2,selectedId:null,hoveredId:null,theme:DEFAULT_CODE_GRAPH_THEME});
    expect(labels.mock.calls.some(([name])=>name==="N99")).toBe(true);
  });
  it.each([false, true])("colors nodes by language across communities (light=%s)", (light) => {
    const model = buildCodeGraphModel({ nodes: [
      { id: "a", name: "a", path: "a.swift", language: "Swift", community: "first", x: 100, y: 100 },
      { id: "b", name: "b", path: "b.swift", language: "swift", community: "second", x: 200, y: 100 },
      { id: "c", name: "c", path: "c.rs", community: "first", x: 300, y: 100, flags: [{flag: "dead"}] },
    ], links: [] });
    const { ctx } = recordingCtx();
    const fills = vi.spyOn(ctx, "fillStyle", "set");
    paintCodeGraph(ctx, {
      model, widthCss: 400, heightCss: 300, panX: 0, panY: 0, scale: 1,
      selectedId: null, hoveredId: null, theme: DEFAULT_CODE_GRAPH_THEME, showLabels: false, light,
    });
    const theme = light ? "light" : "dark";
    expect(fills.mock.calls.map(([color]) => color)).toEqual([
      getLanguageIconColor("swift", theme), getLanguageIconColor("swift", theme), getLanguageIconColor("rust", theme),
    ]);
  });

  it("keeps selected neighbors visible when selection came from a search", () => {
    const model = buildCodeGraphModel({ ...payload, nodes: [...payload.nodes,
      { id: "c", name: "c", x: 300, y: 200 },
    ] }, 400, 300);
    const { ctx } = recordingCtx();
    const opacity: number[] = [];
    vi.spyOn(ctx, "fill").mockImplementation(() => { opacity.push(ctx.globalAlpha); });
    paintCodeGraph(ctx, {
      model, widthCss: 400, heightCss: 300, panX: 0, panY: 0, scale: 1,
      selectedId: "a", hoveredId: null, matchingIds: new Set(["a"]),
      theme: DEFAULT_CODE_GRAPH_THEME, showLabels: false,
    });
    expect(opacity).toEqual([1, 1, 1, 0.16]);
  });

  it("picks the closest node when enlarged hit targets overlap", () => {
    const model = buildCodeGraphModel({ nodes: [
      { id: "a", name: "a", x: 100, y: 100 },
      { id: "b", name: "b", x: 108, y: 100 },
    ], links: [] });
    expect(hitTestCodeGraph(model, 100, 100, 0, 0, 1)?.id).toBe("a");
  });

  it("labels the hovered node even when dense graph labels are disabled", () => {
    const model = buildCodeGraphModel(payload, 400, 300);
    const { ctx } = recordingCtx();
    const labels = vi.spyOn(ctx, "fillText");
    paintCodeGraph(ctx, {
      model, widthCss: 400, heightCss: 300, panX: 0, panY: 0, scale: 1,
      selectedId: null, hoveredId: "a", theme: DEFAULT_CODE_GRAPH_THEME, showLabels: false,
    });
    expect(labels.mock.calls.some(([label]) => label === "a")).toBe(true);
  });

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

describe("CodeGraphRenderer globe mode", () => {
  const model = buildCodeGraphModel({ nodes: [
    { id: "near", name: "near", kind: "file", path: "a/near.ts", community: "g", degree: 4 },
    { id: "limb", name: "limb", kind: "file", path: "a/limb.ts", community: "g", degree: 3 },
    { id: "far", name: "far", kind: "file", path: "a/far.ts", community: "g", degree: 2 },
  ], links: [{ source: "near", target: "far", kind: "imports" }, { source: "near", target: "limb", kind: "imports" }] });
  const at = (x: number, y: number, depth: number): ProjectedPoint => ({ x, y, depth, front: depth > 0 });
  const frame = { projection: [at(100, 100, 0.9), at(300, 100, 0.1), at(200, 200, -0.8)], cx: 200, cy: 150, radius: 140 };
  const base = { model, widthCss: 400, heightCss: 300, panX: 999, panY: 999, scale: 9, selectedId: null, hoveredId: null, theme: DEFAULT_CODE_GRAPH_THEME, globe: frame };

  it("never hits a back-hemisphere node or a link into it, and ignores pan and scale", () => {
    expect(hitTestCodeGraph(model, 200, 200, 0, 0, 1, null, frame)).toBeNull();
    expect(hitTestCodeGraph(model, 150, 150, 0, 0, 1, null, frame)).toBeNull();
    expect(hitTestCodeGraph(model, 100, 100, 0, 0, 1, null, frame)?.id).toBe("near");
    expect(hitTestCodeGraph(model, 100, 100, 500, -300, 7, null, frame)?.id).toBe("near");
    expect(hitTestCodeGraph(model, 200, 100, 0, 0, 1, null, frame)?.kind).toBe("link");
  });

  it("paints the limb, draws the far hemisphere first and dimmer, and skips edges into it", () => {
    const { ctx, calls } = recordingCtx();
    const alphas: number[] = [];
    vi.spyOn(ctx, "fill").mockImplementation(() => { alphas.push(ctx.globalAlpha); });
    paintCodeGraph(ctx, { ...base, showLabels: false });
    expect(calls.filter(c => c === "arc")).toHaveLength(4); // limb + three nodes
    expect(alphas[0]).toBeCloseTo(0.18); // far, painted first
    expect(alphas.slice(1)).toEqual([1, 1]);
    expect(calls.filter(c => c === "lineTo")).toHaveLength(1); // near-limb link; near-far hidden
  });

  it("never labels back-face or limb-hugging nodes", () => {
    const { ctx } = recordingCtx();
    const labels = vi.spyOn(ctx, "fillText");
    paintCodeGraph(ctx, { ...base, hoveredId: null });
    const text = labels.mock.calls.map(([t]) => t);
    expect(text).toContain("near");
    expect(text).not.toContain("far");
    expect(text).not.toContain("limb");
  });

  it("labels flat nodes with their disambiguated display label", () => {
    const flat = buildCodeGraphModel({ nodes: [
      { id: "a/mod.rs", name: "mod.rs", kind: "file", path: "a/mod.rs", x: 50, y: 50 },
      { id: "b/mod.rs", name: "mod.rs", kind: "file", path: "b/mod.rs", x: 150, y: 50 },
    ], links: [] });
    const { ctx } = recordingCtx();
    const labels = vi.spyOn(ctx, "fillText");
    paintCodeGraph(ctx, { model: flat, widthCss: 400, heightCss: 300, panX: 0, panY: 0, scale: 1, selectedId: null, hoveredId: null, theme: DEFAULT_CODE_GRAPH_THEME });
    expect(labels.mock.calls.map(([t]) => t).sort()).toEqual(["a/mod.rs", "b/mod.rs"]);
  });
});
