import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { describe, expect, it } from "vitest";

const vizSource = readFileSync(
  new URL("../src-tauri/vendored/devmap-query/src/viz.rs", import.meta.url),
  "utf8",
);
const mapSource = readFileSync(
  new URL("../src-tauri/vendored/devmap-query/src/map_preview.html", import.meta.url),
  "utf8",
);

type Point = { x?: number; y?: number };
type CameraCall = { x: number; y: number; duration: number };
type ZoomCall = { zoom: number; duration: number };

function normalizeRustFormatJs(source: string) {
  return source.replaceAll("{{", "{").replaceAll("}}", "}");
}

function functionBody(source: string, name: string) {
  const start = source.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`missing function ${name}`);
  const open = source.indexOf("{", start);
  let depth = 0;
  let quote = "";
  let escaped = false;
  let lineComment = false;
  let blockComment = false;
  for (let index = open; index < source.length; index++) {
    const char = source[index];
    const next = source[index + 1];
    if (lineComment) {
      if (char === "\n") lineComment = false;
      continue;
    }
    if (blockComment) {
      if (char === "*" && next === "/") {
        blockComment = false;
        index++;
      }
      continue;
    }
    if (quote) {
      if (escaped) escaped = false;
      else if (char === "\\") escaped = true;
      else if (char === quote) quote = "";
      continue;
    }
    if (char === "/" && next === "/") {
      lineComment = true;
      index++;
    } else if (char === "/" && next === "*") {
      blockComment = true;
      index++;
    } else if (char === "'" || char === '"' || char === "`") {
      quote = char;
    } else if (char === "{") {
      depth++;
    } else if (char === "}" && --depth === 0) {
      return source.slice(start, index + 1);
    }
  }
  throw new Error(`unterminated function ${name}`);
}

function constant(source: string, name: string) {
  const match = source.match(new RegExp(`const\\s+${name}\\s*=\\s*[^;]+;`));
  if (!match) throw new Error(`missing constant ${name}`);
  return match[0];
}

function camera() {
  const centers: CameraCall[] = [];
  const zooms: ZoomCall[] = [];
  return {
    centers,
    zooms,
    centerAt: (x: number, y: number, duration: number) => centers.push({ x, y, duration }),
    zoom: (zoom: number, duration: number) => zooms.push({ zoom, duration }),
  };
}

const normalizedViz = normalizeRustFormatJs(vizSource);

describe("DevMap HTML documentation visibility", () => {
  it("keeps restore data pristine when the renderer replaces edge IDs with node objects", () => {
    const bootstrap = normalizedViz.match(/const graph = ForceGraph\(\)\(el\)[\s\S]+?;/)?.[0];
    if (!bootstrap) throw new Error("Missing graph initialization");
    const source = {nodes:[{id:"a"},{id:"b"}],links:[{source:"a",target:"b"}]};
    const renderer: object = new Proxy({}, {get:(_target,key) => key === "graphData"
      ? (data:{links:Array<{source:unknown;target:unknown}>}) => {
        for (const link of data.links) { link.source={id:link.source}; link.target={id:link.target}; }
        return renderer;
      }
      : () => renderer});
    runInNewContext(bootstrap, {source,ForceGraph:()=>()=>renderer,el:{},VIEW:{},colorFor:()=>"",showDetail:()=>{}});
    expect(source.links).toEqual([{source:"a",target:"b"}]);
  });

  const nodes = [
    {id:"code",name:"Code",documentation:false},
    {id:"note",name:"Note",documentation:true},
    {id:"mixed",name:"Mixed",documentation:false},
  ];
  const links = [{source:"code",target:"note"},{source:"code",target:"mixed"}];

  it("filters and restores graph nodes and edges, composes with search, and clears stale detail", () => {
    let data = {nodes,links};
    const q = {value:""};
    const hideNotes = {checked:true};
    const count = {textContent:""};
    const detail = {innerHTML:"<h2>Note</h2>"};
    const noMatch = {hidden:true};
    const context = {
      source:{nodes,links}, VIEW:{flag_filters:[]}, flagsOf:()=>[], detail, noMatch,
      document:{getElementById:(id:string) => id === "q" ? q : id === "hideNotes" ? hideNotes : count},
      graph:{graphData:(next:typeof data) => {data=next;}},
      setTimeout:()=>1, clearTimeout:()=>{},
    };
    const apply = () => runInNewContext(`{ let filterFitTimer; const FILTER_FIT_DELAY_MS=80; ${functionBody(normalizedViz,"apply")} apply(); }`, context);
    apply();
    expect(data.nodes.map(n=>n.id)).toEqual(["code","mixed"]);
    expect(data.links).toEqual([links[1]]);
    expect(detail.innerHTML).not.toContain("<h2>Note</h2>");
    expect(count.textContent).toBe("2 of 3 match filters");
    q.value="Note"; apply();
    expect(data.nodes).toEqual([]);
    expect(data.links).toEqual([]);
    expect(noMatch.hidden).toBe(false);
    hideNotes.checked=false; q.value=""; apply();
    expect(data.nodes).toHaveLength(3);
    expect(data.links).toEqual(links);
    expect(noMatch.hidden).toBe(true);
  });

  it("filters documentation-only subsystems even after force-graph mutates endpoints", () => {
    const hideNotes = {checked:true};
    const search = {value:""};
    const context = {
      DATA:{nodes,links:links.map(l=>({source:{id:l.source},target:{id:l.target}}))},
      document:{getElementById:(id:string)=>id==="hideNotes"?hideNotes:search},
      subsystemById:()=>({}), langFilter:"", result:{nodes:[],links:[]},
    };
    const apply = () => {
      runInNewContext(`${functionBody(mapSource,"lid")} ${functionBody(mapSource,"filtered")} result = filtered();`, context);
      return context.result;
    };
    expect(apply()).toMatchObject({nodes:[nodes[0],nodes[2]],links:[{source:{id:"code"},target:{id:"mixed"}}]});
    search.value="Note";
    expect(apply()).toEqual({nodes:[],links:[]});
    hideNotes.checked=false; search.value="";
    expect(apply().nodes).toHaveLength(3);
    expect(apply().links).toHaveLength(2);
  });
});

function runGraphFit(nodes: Point[], width = 1000, height = 800) {
  const graph = camera();
  runInNewContext(
    `${constant(normalizedViz, "MAX_FILTER_ZOOM")}\n${functionBody(normalizedViz, "fitFiltered")}\nfitFiltered(nodes);`,
    { nodes, el: { clientWidth: width, clientHeight: height }, graph },
  );
  return graph;
}

function runMapFit(nodes: Point[], width = 1000, height = 800) {
  const graph = camera();
  const frames = new Map<number, () => void>();
  let nextFrame = 1;
  const context = {
    g: { ...graph, graphData: () => ({ nodes }) },
    stage: { clientWidth: width, clientHeight: height },
    requestAnimationFrame: (callback: () => void) => {
      const id = nextFrame++;
      frames.set(id, callback);
      return id;
    },
    cancelAnimationFrame: (id: number | null) => {
      if (id !== null) frames.delete(id);
    },
  };
  runInNewContext(
    `${constant(mapSource, "MAX_AUTO_FIT_ZOOM")}\nlet fitFrame = null;\n${functionBody(mapSource, "fitView")}\nfitView();`,
    context,
  );
  for (const callback of frames.values()) callback();
  return graph;
}

function expectBounded(
  result: ReturnType<typeof camera>,
  center: { x: number; y: number },
  maximum = 4,
) {
  expect(result.centers).toHaveLength(1);
  expect(result.centers[0]).toMatchObject(center);
  expect(result.zooms).toHaveLength(1);
  expect(Number.isFinite(result.zooms[0].zoom)).toBe(true);
  expect(result.zooms[0].zoom).toBeGreaterThanOrEqual(0.05);
  expect(result.zooms[0].zoom).toBeLessThanOrEqual(maximum);
}

describe.each([
  ["code graph", runGraphFit],
  ["repository map", runMapFit],
] as const)("Dev Map %s camera", (_label, fit) => {
  it("keeps singleton and coincident layouts finite and centered", () => {
    expectBounded(fit([{ x: 12, y: -7 }]), { x: 12, y: -7 });
    expectBounded(
      fit([
        { x: 5, y: 9 },
        { x: 5, y: 9 },
      ]),
      { x: 5, y: 9 },
    );
  });

  it("ignores empty or wholly nonfinite layouts", () => {
    for (const nodes of [[], [{ x: Number.NaN, y: 0 }], [{ x: 0, y: Number.POSITIVE_INFINITY }]]) {
      const result = fit(nodes);
      expect(result.centers).toEqual([]);
      expect(result.zooms).toEqual([]);
    }
  });

  it("fits a large finite extent below the automatic ceiling", () => {
    const result = fit([
      { x: -500, y: -200 },
      { x: 500, y: 200 },
    ]);
    expectBounded(result, { x: 0, y: 0 });
    expect(result.zooms[0].zoom).toBeLessThan(1);
  });
});

it("cancels a stale code-graph filter before its camera callback runs", () => {
  const graph = camera();
  let data: { nodes: Point[]; links: unknown[] } = { nodes: [], links: [] };
  const timers = new Map<number, () => void>();
  const canceled: number[] = [];
  let nextTimer = 1;
  const q = { value: "alpha" };
  const context = {
    el: { clientWidth: 1000, clientHeight: 800 },
    graph: {
      ...graph,
      graphData: (next?: typeof data) => {
        if (next) data = next;
        return data;
      },
    },
    source: {
      nodes: [
        { id: "a", name: "alpha", path: "", x: 10, y: 20 },
        { id: "b", name: "beta", path: "", x: 30, y: 40 },
      ],
      links: [],
    },
    VIEW: { flag_filters: [] },
    flagsOf: () => [],
    detail: { innerHTML: "" },
    noMatch: { hidden: false },
    detail: { innerHTML: "" },
    document: { getElementById: () => q },
    setTimeout: (callback: () => void) => {
      const id = nextTimer++;
      timers.set(id, callback);
      return id;
    },
    clearTimeout: (id?: number) => {
      if (id !== undefined) {
        canceled.push(id);
        timers.delete(id);
      }
    },
  };
  runInNewContext(
    `${constant(normalizedViz, "MAX_FILTER_ZOOM")}
     ${constant(normalizedViz, "FILTER_FIT_DELAY_MS")}
     let filterFitTimer;
     ${functionBody(normalizedViz, "fitFiltered")}
     ${functionBody(normalizedViz, "apply")}
     apply(); q.value = "beta"; apply();`,
    { ...context, q },
  );
  expect(canceled).toEqual([1]);
  expect([...timers.keys()]).toEqual([2]);
  timers.get(2)?.();
  expectBounded(graph, { x: 30, y: 40 });
});

it("cancels a stale repository-map fit frame", () => {
  const graph = camera();
  const frames = new Map<number, () => void>();
  const canceled: number[] = [];
  let nextFrame = 1;
  runInNewContext(
    `${constant(mapSource, "MAX_AUTO_FIT_ZOOM")}
     let fitFrame = null;
     ${functionBody(mapSource, "fitView")}
     fitView(); fitView();`,
    {
      g: { ...graph, graphData: () => ({ nodes: [{ x: 3, y: 4 }] }) },
      stage: { clientWidth: 1000, clientHeight: 800 },
      requestAnimationFrame: (callback: () => void) => {
        const id = nextFrame++;
        frames.set(id, callback);
        return id;
      },
      cancelAnimationFrame: (id: number | null) => {
        if (id !== null) {
          canceled.push(id);
          frames.delete(id);
        }
      },
    },
  );
  expect(canceled).toEqual([1]);
  expect([...frames.keys()]).toEqual([2]);
  frames.get(2)?.();
  expectBounded(graph, { x: 3, y: 4 });
});

function renderLiveness(liveness: Record<string, unknown>) {
  const badge = { innerHTML: "" };
  runInNewContext(`${functionBody(mapSource, "renderLiveness")}\nrenderLiveness();`, {
    DATA: { liveness },
    fmt: (value: number) => String(value),
    document: { getElementById: () => badge },
  });
  return badge.innerHTML;
}

it("renders liveness totals without presenting a capped list as complete", () => {
  const items = Array.from({ length: 256 }, (_, index) => `src/${index}.rs`);
  expect(
    renderLiveness({
      unwired_candidates: items,
      counts: { unwired_candidates: { shown: 256, available: 400, total: 900, truncated: true } },
    }),
  ).toContain("unwired <b>256 of 900</b>");
  expect(renderLiveness({ unwired_candidates: ["src/a.rs", "src/b.rs"] })).toContain(
    "unwired <b>2 shown · total unknown</b>",
  );
});
