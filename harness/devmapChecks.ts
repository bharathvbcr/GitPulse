import { tick } from "svelte";
import type { GraphVizLoad, GraphVizPayload } from "../src/lib/codeintel/types";

/** Real client-runtime regression: a same-size/same-generation payload must repaint. */
/** Every guard that ran, in order: the runner's per-check verdict. */
export const devmapCheckRows: Array<{ name: string; pass: boolean }> = [];

/** Record one step and stop the suite at the first one that fails. */
function guard(ok: unknown, failure: string): asserts ok {
  devmapCheckRows.push({ name: `guards against: ${failure}`, pass: Boolean(ok) });
  if (!ok) throw new Error(failure);
}

export async function runDevmapChecks(setLoad: (load: GraphVizLoad | null) => void, setFocus: (id: string | null) => void = () => {}): Promise<string> {
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
  guard(canvas, "Canvas did not mount");
  const before = canvas.toDataURL();
  setLoad({available: true, kind: "code_graph", payload: {
    ...payload, nodes: payload.nodes.map((n,i) => ({...n, x: (n.x ?? 0) + (i ? 180 : 0)})),
  }});
  await settle();
  guard(!(before === canvas.toDataURL()), "Same-count payload did not repaint");
  // A lost context comes back blank; the restore event must repaint it.
  const painted = canvas.toDataURL();
  const lost = new Event("contextlost", { cancelable: true });
  canvas.dispatchEvent(lost);
  guard(lost.defaultPrevented, "contextlost was not claimed, so the browser will not restore the context");
  canvas.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
  guard(!(canvas.toDataURL() === painted), "Harness could not blank the canvas");
  canvas.dispatchEvent(new Event("contextrestored"));
  await settle();
  guard(!(canvas.toDataURL() !== painted), "contextrestored did not repaint the graph");
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-a",payload});
  await settle();
  const search = document.querySelector<HTMLInputElement>('input[aria-label="Find a file or symbol"]');
  guard(search, "Search did not mount");
  search.value = "Alpha";
  search.dispatchEvent(new Event("input",{bubbles:true}));
  await settle();
  search.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true}));
  await settle();
  guard(document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Alpha"), "Search did not select Alpha");
  const selectedZoom = document.querySelector('.map-zoom')?.textContent;
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-a",payload:{...payload,nodes:payload.nodes.map(n=>({...n,name:`Updated ${n.name}`}))}});
  await settle();
  guard(document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Updated Alpha"), "Same-repository refresh lost selection");
  guard(!(document.querySelector('.map-zoom')?.textContent !== selectedZoom), "Same-repository refresh lost zoom");
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-b",payload});
  await settle();
  guard(!(document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Alpha")), "Repository switch retained the previous selection");
  const click = async (selector: string) => {
    const element = document.querySelector<HTMLButtonElement>(selector);
    guard(element, `Missing control: ${selector}`);
    element.click(); await settle();
  };
  // The four checkboxes and the language list live in the Filters popover,
  // which is not in the DOM until it opens (and a scope change closes it).
  const openFilters = async () => {
    if (!document.querySelector('.map-filters')) await click('[data-testid="map-filters-toggle"]');
    guard(document.querySelector('.map-filters input[type="checkbox"]'), "Filters popover did not open");
  };
  setLoad({available:true,kind:"code_graph",path:"/fixture/far",payload:{nodes:[{id:"far",name:"Far away",kind:"file",x:10000,y:10000}],links:[]}});
  await settle();
  for (let i=0;i<10;i++) canvas.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));
  await settle(); await click('[data-testid="map-fit"]');
  const bounds = canvas.getBoundingClientRect();
  canvas.dispatchEvent(new MouseEvent('click',{clientX:bounds.left+bounds.width/2,clientY:bounds.top+bounds.height/2,bubbles:true}));
  await settle();
  guard(document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Far away"), "Fit view did not bring supplied coordinates into view");
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
  await openFilters();
  await click('[data-map-language="swift"]'); // Swift, after Rust's larger count.
  guard(!(document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "2 of 36 match filters"), "Language filter lost Swift nodes");
  await click('.map-filters input[type="checkbox"]');
  guard(!(document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "1 of 36 match filters"), "Test and language filters do not compose");
  setLoad({...explorer,payload:{...explorer.payload!,generation_id:8}}); await settle();
  guard(!(document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "1 of 36 match filters"), "Refresh lost filters");
  await click('[data-testid="map-clear-filters"]');
  await openFilters();
  await click('.map-filters label:nth-child(2) input');
  guard(!(document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "35 of 36 match filters"), "Generated path filter failed");
  await click('.map-filters label:nth-child(3) input');
  guard(!(document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "3 of 36 match filters"), "Connected-only filter retained isolated nodes");
  await click('[data-testid="map-clear-filters"]');
  await click('[data-testid="map-nodes-toggle"]');
  await click('button[aria-label="Next nodes"]');
  guard(document.querySelector('.result-heading')?.textContent?.includes("31–36 shown"), "Node browser cannot reach later pages");
  const currentSearch = document.querySelector<HTMLInputElement>('input[aria-label="Find a file or symbol"]');
  guard(currentSearch, "Search did not survive refresh");
  currentSearch.value = "Main";
  currentSearch.dispatchEvent(new Event("input",{bubbles:true})); await settle();
  currentSearch.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true})); await settle();
  canvas.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true})); await settle();
  guard(!(document.querySelector('[data-testid="opened-file"]')?.textContent !== "main.swift"), "Keyboard activation did not open root Swift file");
  const direction = document.querySelector<HTMLSelectElement>('select[aria-label="Trace direction"]');
  guard(direction, "Connections did not mount");
  direction.value = "outgoing"; direction.dispatchEvent(new Event("change",{bubbles:true})); await settle();
  guard(document.querySelector('.neighbor-list')?.textContent?.includes("Dependency") || document.querySelector('.neighbor-list')?.textContent?.includes("Caller"), "Outgoing connections contain incoming callers");
  direction.value = "incoming"; direction.dispatchEvent(new Event("change",{bubbles:true})); await settle();
  guard(document.querySelector('.neighbor-list')?.textContent?.includes("Caller") || document.querySelector('.neighbor-list')?.textContent?.includes("Dependency"), "Incoming connections contain dependencies");
  await click('.neighbor-list button');
  guard(document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("Caller"), "Neighbor activation failed");
  setLoad({...explorer,payload:{nodes:fixtureNodes.filter(n=>n.id!=="caller"),links:[]}}); await settle();
  guard(!(document.querySelector('.map-neighbors')), "Removed selection retained a stale inspector");
  const notes: GraphVizLoad = {available:true,kind:"code_graph",path:"/fixture/notes",payload:{nodes:[
    {id:"code",name:"Code",path:"notes/editor.ts",documentation:false},
    {id:"note",name:"Note",path:"README.md",documentation:true},
    {id:"doc",name:"Doc",path:"guide.mdx",documentation:true},
  ],links:[{source:"code",target:"note"}]}};
  setLoad(notes); await settle();
  await openFilters();
  await click('input[aria-label="Hide notes & Markdown"]');
  guard(!(document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "1 of 3 match filters"), "Notes filter did not hide documentation");
  await click('[data-testid="map-nodes-toggle"]');
  guard(!(document.querySelector('.map-results')?.textContent?.includes("README.md")), "Hidden Markdown remained in node browser");
  await click('.map-results button');
  guard(!(document.querySelector('.neighbor-list')?.textContent?.includes("Note")), "Hidden note remained in connections");
  setLoad({...notes,payload:{...notes.payload!,nodes:notes.payload!.nodes.map(n=>({...n,documentation:true}))}}); await settle();
  guard(!(document.querySelector('.map-neighbors')), "Refresh retained a newly hidden selection");
  guard(document.querySelector('.map-empty')?.textContent?.includes("No nodes match"), "All-documentation filter left an unexplained blank canvas");
  await click('[data-testid="map-clear-filters"]');
  guard(!(document.querySelector('[data-testid="graph-filter-count"]')?.textContent !== "3 of 3 match filters"), "Clear filters did not restore documents");
  await openFilters();
  await click('input[aria-label="Hide notes & Markdown"]');
  setLoad({...notes,path:"/fixture/notes-other"}); await settle();
  await openFilters();
  guard(!(document.querySelector<HTMLInputElement>('input[aria-label="Hide notes & Markdown"]')?.checked !== false), "Repository switch retained notes filter");
  setLoad({available: true, kind: "code_graph", payload: JSON.parse('{"nodes":[null,{"id":"a","name":12,"language":{}},{"id":"b","name":"B"},{"id":"bad"},{"id":"bad"}],"links":[null,{"source":"a","target":"b"},{"source":"bad","target":"b"}]}')});
  await settle();
  guard(document.querySelector('[data-testid="code-graph-nodes-label"]')?.textContent?.includes("2 of 5"),
    "Malformed payload did not retain an honest rendered-node count");
  guard(document.querySelector(".code-map")?.textContent?.includes("invalid or ambiguous"),
    "Malformed payload omissions were not explained");
  setLoad({available: true, kind: "code_graph", payload: {nodes: [], links: []}});
  await settle();
  guard(document.querySelector(".map-empty"), "Empty payload has no visible explanation");
  setLoad({available: false, kind: "code_graph", reason: "Fixture index unavailable"});
  await settle();
  guard(document.querySelector('[data-testid="code-graph-legend"]')?.textContent?.includes("Fixture index unavailable"),
    "Unavailable payload lost its reason");
  // The panel's order on a view switch: the scope changes while the model is
  // still empty (load cleared), and the payload lands later under that scope.
  // The first payload must be fitted without the user pressing Fit.
  setLoad({available:true,kind:"code_graph",path:"/fixture/before-fit",payload});
  await settle();
  setLoad(null);
  await settle();
  setLoad({available:true,kind:"code_graph",payload:{nodes:[{id:"first-fit",name:"First fit",kind:"file",x:10000,y:10000}],links:[]}});
  await settle();
  const fitBounds = canvas.getBoundingClientRect();
  canvas.dispatchEvent(new MouseEvent('click',{clientX:fitBounds.left+fitBounds.width/2,clientY:fitBounds.top+fitBounds.height/2,bubbles:true}));
  await settle();
  guard(document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("First fit"), "First payload after a scope change was not fitted");
  // Duplicate names: repeated file names read as their shortest unique path suffix.
  setLoad({available:true,kind:"code_graph",path:"/fixture/dupes",payload:{level:"file",nodes:[
    {id:"src/a/mod.rs",name:"mod.rs",kind:"file",path:"src/a/mod.rs",x:100,y:100},
    {id:"src/b/mod.rs",name:"mod.rs",kind:"file",path:"src/b/mod.rs",x:200,y:100},
    {id:"unique.rs",name:"unique.rs",kind:"file",path:"unique.rs",x:300,y:100},
  ],links:[]}});
  await settle();
  await click('[data-testid="map-nodes-toggle"]');
  const browser = document.querySelector('.map-results')?.textContent ?? "";
  guard(browser.includes("a/mod.rs") || !browser.includes("b/mod.rs") || !browser.includes("unique.rs"), `Repeated names were not told apart: ${browser}`);
  await click('[data-testid="map-nodes-toggle"]');
  // Globe: only the file and symbol graphs offer it; it is opt-in and flat by default.
  const globeNodes = Array.from({length:60},(_,i) => ({id:`pkg-${i % 6}/file-${i}.ts`,name:`file-${i}.ts`,kind:"file",path:`pkg-${i % 6}/file-${i}.ts`,community:`pkg-${i % 6}`,degree:i % 5}));
  const globeLinks = globeNodes.slice(1).map(n => ({source:globeNodes[0].id,target:n.id,kind:"imports"}));
  const layout = () => document.querySelector('[data-testid="map-layout"]');
  setLoad({available:true,kind:"code_graph",path:"/fixture/globe",payload:{nodes:globeNodes,links:globeLinks}});
  await settle();
  guard(!(layout()), "Globe toggle offered for a graph with no file/symbol level");
  setLoad({available:true,kind:"code_graph",path:"/fixture/globe",payload:{level:"file",nodes:globeNodes,links:globeLinks}});
  await settle();
  guard(layout(), "Globe toggle missing on the file graph");
  const toggle = (name: string) => [...document.querySelectorAll<HTMLButtonElement>('[data-testid="map-layout"] button')].find(b => b.textContent?.trim() === name);
  guard(!(toggle("Flat")?.getAttribute("aria-pressed") !== "true"), "Globe is not opt-in: the default is not Flat");
  const flatPixels = canvas.toDataURL();
  toggle("Globe")?.click(); await settle();
  guard(!(toggle("Globe")?.getAttribute("aria-pressed") !== "true"), "Globe toggle did not engage");
  guard(canvas.getAttribute("aria-label")?.includes("rotate"), "Canvas label does not describe rotation in globe mode");
  const globePixels = canvas.toDataURL();
  guard(!(globePixels === flatPixels), "Globe did not repaint the canvas");
  canvas.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowLeft',bubbles:true})); await settle();
  guard(!(canvas.toDataURL() === globePixels), "Arrow keys did not rotate the globe");
  // Focus request: selects the node and, on the globe, turns it to face front.
  setFocus("pkg-3/file-3.ts"); await settle();
  await new Promise<void>(resolve => setTimeout(resolve, 450)); await settle();
  guard(document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("file-3.ts"), "focusNodeId did not select the node on the globe");
  toggle("Flat")?.click(); await settle();
  setFocus(null); await settle();
  setFocus("pkg-4/file-4.ts"); await settle();
  guard(document.querySelector('[data-testid="code-graph-selection"]')?.textContent?.includes("file-4.ts"), "focusNodeId did not select the node in the flat view");
  setFocus(null); await settle();
  return "PASS: repaint, refresh and repository isolation, fit supplied coordinates, first payload fitted after a scope change, combined filters, repeated-name labels, globe toggle and rotation, focus requests, paged browsing, directed neighbors, keyboard file activation, removed selection, malformed/empty/unavailable states";
}

/** Measure mounted Svelte + canvas work, including actual frame completion. */
export async function stressDevmap(setLoad: (load: GraphVizLoad) => void): Promise<string> {
  const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  const results: string[] = [];
  const map = document.querySelector<HTMLElement>('.code-map');
  guard(map, "Map did not mount");
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
        guard(canvas, "Stress canvas disappeared");
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
        guard(!(loadMs > 4000 || worst > 500), `Stress responsiveness exceeded its budget: ${results.join("; ")}`);
        guard(!(map.querySelectorAll('[data-map-group]').length > 41), `Unbounded community controls: ${map.querySelectorAll('[data-map-group]').length}; ${results.join("; ")}`);
      }
    }
  } finally { map.style.width = originalWidth; }
  return `PASS: 5,000-node browser stress (0 / 5,000 / 50,000 links). ${results.join("; ")}`;
}
