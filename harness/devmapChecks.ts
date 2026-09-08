import { tick } from "svelte";
import type { GraphVizLoad, GraphVizPayload } from "../src/lib/codeintel/types";

/** Real client-runtime regression: a same-size/same-generation payload must repaint. */
export async function runDevmapChecks(setLoad: (load: GraphVizLoad) => void): Promise<string> {
  const payload: GraphVizPayload = {
    level: "file", generation_id: 7,
    nodes: [
      { id: "a", name: "Alpha", path: "src/a.ts", x: 100, y: 100 },
      { id: "b", name: "Beta", path: "src/b.ts", x: 200, y: 160 },
    ],
    links: [{source: "a", target: "b", kind: "imports"}],
  };
  const settle = async () => {
    await tick();
    for (let frame = 0; frame < 6; frame++) {
      await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
    }
  };
  setLoad({available: true, kind: "code_graph", payload});
  await settle();
  const canvas = document.querySelector("canvas");
  if (!canvas) throw new Error("Canvas did not mount");
  const before = canvas.toDataURL();
  setLoad({available: true, kind: "code_graph", payload: {
    ...payload, nodes: payload.nodes.map((n,i) => ({...n, x: (n.x ?? 0) + (i ? 180 : 0)})),
  }});
  await settle();
  if (before === canvas.toDataURL()) throw new Error("Same-count payload did not repaint");
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-a",payload});
  await settle();
  const search = document.querySelector<HTMLInputElement>('input[aria-label="Find a file or symbol"]');
  if (!search) throw new Error("Search did not mount");
  search.value = "Alpha";
  search.dispatchEvent(new Event("input",{bubbles:true}));
  await settle();
  search.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true}));
  await settle();
  if (!document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Alpha")) throw new Error("Search did not select Alpha");
  const selectedZoom = document.querySelector('.map-zoom')?.textContent;
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-a",payload:{...payload,nodes:payload.nodes.map(n=>({...n,name:`Updated ${n.name}`}))}});
  await settle();
  if (!document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Updated Alpha")) throw new Error("Same-repository refresh lost selection");
  if (document.querySelector('.map-zoom')?.textContent !== selectedZoom) throw new Error("Same-repository refresh lost zoom");
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-b",payload});
  await settle();
  if (document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Alpha")) throw new Error("Repository switch retained the previous selection");
  const click = async (selector: string) => {
    const element = document.querySelector<HTMLButtonElement>(selector);
    if (!element) throw new Error(`Missing control: ${selector}`);
    element.click(); await settle();
  };
  setLoad({available:true,kind:"code_graph",path:"/fixture/far",payload:{nodes:[{id:"far",name:"Far away",kind:"file",x:10000,y:10000}],links:[]}});
  await settle();
  for (let i=0;i<10;i++) canvas.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));
  await settle(); await click('.fit-button');
  const bounds = canvas.getBoundingClientRect();
  canvas.dispatchEvent(new MouseEvent('click',{clientX:bounds.left+bounds.width/2,clientY:bounds.top+bounds.height/2,bubbles:true}));
  await settle();
  if (!document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Far away")) throw new Error("Fit view did not bring supplied coordinates into view");
  const fixtureNodes = [
    {id:"root",name:"Main",kind:"file",path:"main.swift"},
    {id:"dependency",name:"Dependency",kind:"file",path:"core/lib.rs"},
    {id:"caller",name:"Caller",kind:"file",path:"app/Tests/MainTests.swift"},
    {id:"generated",name:"Generated",kind:"file",path:"generated/types.ts"},
    ...Array.from({length:32},(_,i)=>({id:`isolated-${i}`,name:`Isolated ${i}`,kind:"file",path:`misc/file-${i}.rs`})),
  ];
  const explorer: GraphVizLoad = {available:true,kind:"code_graph",path:"/fixture/explorer",payload:{nodes:fixtureNodes,links:[
    {source:"root",target:"dependency",kind:"calls"}, {source:"caller",target:"root",kind:"calls"},
  ]}};
  setLoad(explorer); await settle();
  await click('.map-languages button:nth-child(3)'); // Swift, after Rust's larger count.
  if (document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "2 of 36 match filters") throw new Error("Language filter lost Swift nodes");
  await click('.map-filters input[type="checkbox"]');
  if (document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "1 of 36 match filters") throw new Error("Test and language filters do not compose");
  setLoad({...explorer,payload:{...explorer.payload!,generation_id:8}}); await settle();
  if (document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "1 of 36 match filters") throw new Error("Refresh lost filters");
  await click('.map-filters button');
  await click('.map-filters label:nth-child(2) input');
  if (document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "35 of 36 match filters") throw new Error("Generated path filter failed");
  await click('.map-filters label:nth-child(3) input');
  if (document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "3 of 36 match filters") throw new Error("Connected-only filter retained isolated nodes");
  await click('.map-filters button');
  await click('.map-actions > button');
  await click('button[aria-label="Next nodes"]');
  if (!document.querySelector('.result-heading')?.textContent?.includes("31–36 shown")) throw new Error("Node browser cannot reach later pages");
  const currentSearch = document.querySelector<HTMLInputElement>('input[aria-label="Find a file or symbol"]');
  if (!currentSearch) throw new Error("Search did not survive refresh");
  currentSearch.value = "Main";
  currentSearch.dispatchEvent(new Event("input",{bubbles:true})); await settle();
  currentSearch.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true})); await settle();
  canvas.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true})); await settle();
  if (document.querySelector('[data-testid="opened-file"]')?.textContent !== "main.swift") throw new Error("Keyboard activation did not open root Swift file");
  const direction = document.querySelector<HTMLSelectElement>('select[aria-label="Trace direction"]');
  if (!direction) throw new Error("Connections did not mount");
  direction.value = "outgoing"; direction.dispatchEvent(new Event("change",{bubbles:true})); await settle();
  if (!document.querySelector('.neighbor-list')?.textContent?.includes("Dependency") || document.querySelector('.neighbor-list')?.textContent?.includes("Caller")) throw new Error("Outgoing connections contain incoming callers");
  direction.value = "incoming"; direction.dispatchEvent(new Event("change",{bubbles:true})); await settle();
  if (!document.querySelector('.neighbor-list')?.textContent?.includes("Caller") || document.querySelector('.neighbor-list')?.textContent?.includes("Dependency")) throw new Error("Incoming connections contain dependencies");
  await click('.neighbor-list button');
  if (!document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Caller")) throw new Error("Neighbor activation failed");
  setLoad({...explorer,payload:{nodes:fixtureNodes.filter(n=>n.id!=="caller"),links:[]}}); await settle();
  if (document.querySelector('.map-neighbors')) throw new Error("Removed selection retained a stale inspector");
  setLoad({available: true, kind: "code_graph", payload: JSON.parse('{"nodes":[null,{"id":"a","name":12,"language":{}},{"id":"b","name":"B"},{"id":"bad"},{"id":"bad"}],"links":[null,{"source":"a","target":"b"},{"source":"bad","target":"b"}]}')});
  await settle();
  if (!document.querySelector('[data-testid="code-graph-nodes-label"]')?.textContent?.includes("2 of 5")) {
    throw new Error("Malformed payload did not retain an honest rendered-node count");
  }
  if (!document.querySelector(".code-map")?.textContent?.includes("invalid or ambiguous")) {
    throw new Error("Malformed payload omissions were not explained");
  }
  setLoad({available: true, kind: "code_graph", payload: {nodes: [], links: []}});
  await settle();
  if (!document.querySelector(".map-empty")) throw new Error("Empty payload has no visible explanation");
  setLoad({available: false, kind: "code_graph", reason: "Fixture index unavailable"});
  await settle();
  if (!document.querySelector('[data-testid="code-graph-legend"]')?.textContent?.includes("Fixture index unavailable")) {
    throw new Error("Unavailable payload lost its reason");
  }
  return "PASS: repaint, refresh and repository isolation, fit supplied coordinates, combined filters, paged browsing, directed neighbors, keyboard file activation, removed selection, malformed/empty/unavailable states";
}

/** Measure mounted Svelte + canvas work, including actual frame completion. */
export async function stressDevmap(setLoad: (load: GraphVizLoad) => void): Promise<string> {
  const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  const results: string[] = [];
  const map = document.querySelector<HTMLElement>('.code-map');
  if (!map) throw new Error("Map did not mount");
  const originalWidth = map.style.width;
  try {
    for (const shape of ["sparse", "dense", "isolated"] as const) {
      const nodes = Array.from({length:5000},(_,i) => ({id:`n${i}`,name:`File ${i}`,path:`src/file-${i}.rs`,kind:"file",community:shape === "isolated" ? `group-${i}` : "core"}));
      const links = shape === "isolated" ? [] : nodes.flatMap((n,i) => Array.from({length:shape === "dense" ? 10 : 1},(_,j) => ({source:n.id,target:nodes[(i+j*137+1)%nodes.length].id,kind:"calls"})));
      for (const width of [320,1440]) {
        map.style.width = `${width}px`;
        await tick(); await frame(); await frame();
        const start = performance.now();
        setLoad({available:true,kind:"code_graph",path:`/stress/${shape}/${width}`,payload:{nodes,links}});
        await tick(); await frame(); await frame();
        const loadMs = performance.now() - start;
        const canvas = map.querySelector('canvas');
        if (!canvas) throw new Error("Stress canvas disappeared");
        const samples: number[] = [];
        for (let i=0;i<8;i++) {
          const began = performance.now();
          canvas.dispatchEvent(new KeyboardEvent('keydown',{key:i%2 ? 'ArrowRight' : '+',bubbles:true}));
          await tick(); await frame(); await frame();
          samples.push(performance.now()-began);
        }
        samples.sort((a,b)=>a-b);
        const worst = samples[samples.length-1];
        results.push(`${shape} ${width}px: load ${Math.round(loadMs)}ms, frame max ${Math.round(worst)}ms`);
        if (loadMs > 4000 || worst > 500) throw new Error(`Stress responsiveness exceeded its budget: ${results.join("; ")}`);
        if (map.querySelectorAll('.community-chip').length > 41) throw new Error(`Unbounded community controls: ${map.querySelectorAll('.community-chip').length}; ${results.join("; ")}`);
      }
    }
  } finally { map.style.width = originalWidth; }
  return `PASS: 5,000-node browser stress (0 / 5,000 / 50,000 links). ${results.join("; ")}`;
}
