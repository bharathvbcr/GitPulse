import { tick } from "svelte";
import type { GraphVizLoad, GraphVizPayload } from "../src/lib/codeintel/types";

/** Records one named assertion. The page passes `createChecks().check` so a
 * failed assertion is a row in the verdict; the thunk runs inside it, so a
 * throw is attributed to the check that caused it. */
export type DevmapCheck = (name: string, assertion: () => unknown) => void;

const textOf = (selector: string): string => document.querySelector(selector)?.textContent ?? "";

/** Real client-runtime regression: a same-size/same-generation payload must repaint. */
export async function runDevmapChecks(
  setLoad: (load: GraphVizLoad | null) => void,
  check: DevmapCheck,
  setFocus: (id: string | null) => void = () => {},
): Promise<string> {
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
  const selection = () => textOf('[data-testid="code-graph-selection"]');
  const filterCount = () => textOf('[data-testid="graph-filter-count"]');
  const zoom = () => textOf(".map-zoom");
  const click = async (selector: string) => {
    const element = document.querySelector<HTMLButtonElement>(selector);
    if (!element) throw new Error(`Missing control: ${selector}`);
    element.click(); await settle();
  };
  /** The canvas the page holds now. The component owns it and may replace it
   * when the load is cleared, so a reference taken earlier can be detached. */
  const liveCanvas = (): HTMLCanvasElement => {
    const found = document.querySelector("canvas");
    if (!found) throw new Error("Canvas is not mounted");
    return found;
  };
  /** Click the middle of the canvas, where a fitted single node lands. */
  const clickCentre = async () => {
    const target = liveCanvas();
    const bounds = target.getBoundingClientRect();
    target.dispatchEvent(new MouseEvent("click", { clientX: bounds.left + bounds.width / 2, clientY: bounds.top + bounds.height / 2, bubbles: true }));
    await settle();
  };
  // The four checkboxes and the language list live in the Filters popover,
  // which is not in the DOM until it opens (and a scope change closes it).
  const openFilters = async () => {
    if (!document.querySelector(".map-filters")) await click('[data-testid="map-filters-toggle"]');
    check("the Filters popover is open", () => !!document.querySelector('.map-filters input[type="checkbox"]'));
  };

  setLoad({available: true, kind: "code_graph", payload});
  await settle();
  const canvas = liveCanvas();
  check("the canvas mounts", () => canvas.isConnected);
  const before = canvas.toDataURL();
  setLoad({available: true, kind: "code_graph", payload: {
    ...payload, nodes: payload.nodes.map((n,i) => ({...n, x: (n.x ?? 0) + (i ? 180 : 0)})),
  }});
  await settle();
  check("a same-count payload repaints", () => before !== canvas.toDataURL());
  // A lost context comes back blank; the restore event must repaint it.
  const painted = canvas.toDataURL();
  const lost = new Event("contextlost", { cancelable: true });
  canvas.dispatchEvent(lost);
  check("contextlost is claimed, so the browser can restore the context", () => lost.defaultPrevented);
  canvas.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
  check("the harness can blank the canvas", () => canvas.toDataURL() !== painted);
  canvas.dispatchEvent(new Event("contextrestored"));
  await settle();
  check("contextrestored repaints the graph", () => canvas.toDataURL() === painted);
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-a",payload});
  await settle();
  const search = document.querySelector<HTMLInputElement>('input[aria-label="Find a file or symbol"]');
  check("search mounts", () => !!search);
  if (!search) return "";
  search.value = "Alpha";
  search.dispatchEvent(new Event("input",{bubbles:true}));
  await settle();
  search.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true}));
  await settle();
  check("search selects Alpha", () => selection().includes("Alpha"));
  const selectedZoom = zoom();
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-a",payload:{...payload,nodes:payload.nodes.map(n=>({...n,name:`Updated ${n.name}`}))}});
  await settle();
  check("a same-repository refresh keeps the selection", () => selection().includes("Updated Alpha"));
  check("a same-repository refresh keeps the zoom", () => zoom() === selectedZoom);
  setLoad({available:true,kind:"code_graph",path:"/fixture/repo-b",payload});
  await settle();
  check("a repository switch drops the previous selection", () => !selection().includes("Alpha"));
  setLoad({available:true,kind:"code_graph",path:"/fixture/far",payload:{nodes:[{id:"far",name:"Far away",kind:"file",x:10000,y:10000}],links:[]}});
  await settle();
  for (let i=0;i<10;i++) liveCanvas().dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));
  await settle(); await click('[data-testid="map-fit"]');
  await clickCentre();
  check("Fit view brings supplied coordinates into view", () => selection().includes("Far away"));
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
  check("the language filter keeps the Swift nodes", () => filterCount() === "2 of 36 match filters");
  await click('.map-filters input[aria-label="Hide tests"]');
  check("the test and language filters compose", () => filterCount() === "1 of 36 match filters");
  setLoad({...explorer,payload:{...explorer.payload!,generation_id:8}}); await settle();
  check("a refresh keeps the filters", () => filterCount() === "1 of 36 match filters");
  await click('[data-testid="map-clear-filters"]');
  await openFilters();
  await click('.map-filters input[aria-label="Hide generated/vendor"]');
  check("the generated path filter hides generated files", () => filterCount() === "35 of 36 match filters");
  await click('.map-filters input[aria-label="Connected only"]');
  check("connected-only drops the isolated nodes", () => filterCount() === "3 of 36 match filters");
  await click('[data-testid="map-clear-filters"]');
  await click('[data-testid="map-nodes-toggle"]');
  await click('button[aria-label="Next nodes"]');
  check("the node browser reaches later pages", () => textOf(".result-heading").includes("31–36 shown"));
  const currentSearch = document.querySelector<HTMLInputElement>('input[aria-label="Find a file or symbol"]');
  check("search survives a refresh", () => !!currentSearch);
  if (!currentSearch) return "";
  currentSearch.value = "Main";
  currentSearch.dispatchEvent(new Event("input",{bubbles:true})); await settle();
  currentSearch.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true})); await settle();
  liveCanvas().dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true})); await settle();
  check("keyboard activation opens the root Swift file", () => textOf('[data-testid="opened-file"]') === "main.swift");
  const direction = document.querySelector<HTMLSelectElement>('select[aria-label="Trace direction"]');
  check("the connections control mounts", () => !!direction);
  if (!direction) return "";
  direction.value = "outgoing"; direction.dispatchEvent(new Event("change",{bubbles:true})); await settle();
  check("outgoing connections list dependencies and no callers", () => textOf(".neighbor-list").includes("Dependency") && !textOf(".neighbor-list").includes("Caller"));
  direction.value = "incoming"; direction.dispatchEvent(new Event("change",{bubbles:true})); await settle();
  check("incoming connections list callers and no dependencies", () => textOf(".neighbor-list").includes("Caller") && !textOf(".neighbor-list").includes("Dependency"));
  await click('.neighbor-list button');
  check("activating a neighbour selects it", () => selection().includes("Caller"));
  setLoad({...explorer,payload:{nodes:fixtureNodes.filter(n=>n.id!=="caller"),links:[]}}); await settle();
  check("a removed selection leaves no stale inspector", () => !document.querySelector(".map-neighbors"));
  const notes: GraphVizLoad = {available:true,kind:"code_graph",path:"/fixture/notes",payload:{nodes:[
    {id:"code",name:"Code",path:"notes/editor.ts",documentation:false},
    {id:"note",name:"Note",path:"README.md",documentation:true},
    {id:"doc",name:"Doc",path:"guide.mdx",documentation:true},
  ],links:[{source:"code",target:"note"}]}};
  setLoad(notes); await settle();
  await openFilters();
  await click('input[aria-label="Hide notes & Markdown"]');
  check("the notes filter hides documentation", () => filterCount() === "1 of 3 match filters");
  await click('[data-testid="map-nodes-toggle"]');
  check("hidden Markdown is absent from the node browser", () => !textOf(".map-results").includes("README.md"));
  await click('.map-results button');
  check("a hidden note is absent from the connections", () => !textOf(".neighbor-list").includes("Note"));
  setLoad({...notes,payload:{...notes.payload!,nodes:notes.payload!.nodes.map(n=>({...n,documentation:true}))}}); await settle();
  check("a refresh drops a selection the filter now hides", () => !document.querySelector(".map-neighbors"));
  check("an all-documentation filter explains the blank canvas", () => textOf(".map-empty").includes("No nodes match"));
  await click('[data-testid="map-clear-filters"]');
  check("clearing the filters restores the documents", () => filterCount() === "3 of 3 match filters");
  await openFilters();
  await click('input[aria-label="Hide notes & Markdown"]');
  setLoad({...notes,path:"/fixture/notes-other"}); await settle();
  // The scope change closes the popover; reopen it so the box is read, not absent.
  await openFilters();
  check("a repository switch resets the notes filter", () => document.querySelector<HTMLInputElement>('input[aria-label="Hide notes & Markdown"]')?.checked === false);
  setLoad({available: true, kind: "code_graph", payload: JSON.parse('{"nodes":[null,{"id":"a","name":12,"language":{}},{"id":"b","name":"B"},{"id":"bad"},{"id":"bad"}],"links":[null,{"source":"a","target":"b"},{"source":"bad","target":"b"}]}')});
  await settle();
  check("a malformed payload keeps an honest rendered-node count", () => textOf('[data-testid="code-graph-nodes-label"]').includes("2 of 5"));
  check("a malformed payload explains what it omitted", () => textOf(".code-map").includes("invalid or ambiguous"));
  setLoad({available: true, kind: "code_graph", payload: {nodes: [], links: []}});
  await settle();
  check("an empty payload has a visible explanation", () => !!document.querySelector(".map-empty"));
  setLoad({available: false, kind: "code_graph", reason: "Fixture index unavailable"});
  await settle();
  check("an unavailable payload keeps its reason", () => textOf('[data-testid="code-graph-legend"]').includes("Fixture index unavailable"));
  // The panel's order on a view switch: the scope changes while the model is
  // still empty (load cleared), and the payload lands later under that scope.
  // The first payload must be fitted without the user pressing Fit.
  setLoad({available:true,kind:"code_graph",path:"/fixture/before-fit",payload});
  await settle();
  setLoad(null);
  await settle();
  setLoad({available:true,kind:"code_graph",payload:{nodes:[{id:"first-fit",name:"First fit",kind:"file",x:10000,y:10000}],links:[]}});
  await settle();
  // The payload also changes the toolbar (legend, groups and the filter count),
  // so the stage is resized after the fit; the fitted node must still be centred.
  await clickCentre();
  check("the first payload after a scope change is fitted", () => selection().includes("First fit"));
  // Duplicate names: repeated file names read as their shortest unique path suffix.
  setLoad({available:true,kind:"code_graph",path:"/fixture/dupes",payload:{level:"file",nodes:[
    {id:"src/a/mod.rs",name:"mod.rs",kind:"file",path:"src/a/mod.rs",x:100,y:100},
    {id:"src/b/mod.rs",name:"mod.rs",kind:"file",path:"src/b/mod.rs",x:200,y:100},
    {id:"unique.rs",name:"unique.rs",kind:"file",path:"unique.rs",x:300,y:100},
  ],links:[]}});
  await settle();
  await click('[data-testid="map-nodes-toggle"]');
  const browser = textOf(".map-results");
  check(`repeated names are told apart in the node browser (${browser})`, () => browser.includes("a/mod.rs") && browser.includes("b/mod.rs") && browser.includes("unique.rs"));
  await click('[data-testid="map-nodes-toggle"]');
  // Globe: only the file and symbol graphs offer it; it is opt-in and flat by default.
  const globeNodes = Array.from({length:60},(_,i) => ({id:`pkg-${i % 6}/file-${i}.ts`,name:`file-${i}.ts`,kind:"file",path:`pkg-${i % 6}/file-${i}.ts`,community:`pkg-${i % 6}`,degree:i % 5}));
  const globeLinks = globeNodes.slice(1).map(n => ({source:globeNodes[0].id,target:n.id,kind:"imports"}));
  const layout = () => document.querySelector('[data-testid="map-layout"]');
  setLoad({available:true,kind:"code_graph",path:"/fixture/globe",payload:{nodes:globeNodes,links:globeLinks}});
  await settle();
  check("the globe is not offered for a graph with no file or symbol level", () => !layout());
  setLoad({available:true,kind:"code_graph",path:"/fixture/globe",payload:{level:"file",nodes:globeNodes,links:globeLinks}});
  await settle();
  check("the globe toggle is offered on the file graph", () => !!layout());
  const toggle = (name: string) => [...document.querySelectorAll<HTMLButtonElement>('[data-testid="map-layout"] button')].find(b => b.textContent?.trim() === name);
  check("the globe is opt-in: the default is Flat", () => toggle("Flat")?.getAttribute("aria-pressed") === "true");
  const flatPixels = liveCanvas().toDataURL();
  toggle("Globe")?.click(); await settle();
  check("the Globe toggle engages", () => toggle("Globe")?.getAttribute("aria-pressed") === "true");
  check("the canvas label describes rotation in globe mode", () => liveCanvas().getAttribute("aria-label")?.includes("rotate"));
  const globePixels = liveCanvas().toDataURL();
  check("the globe repaints the canvas", () => globePixels !== flatPixels);
  liveCanvas().dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowLeft',bubbles:true})); await settle();
  check("arrow keys rotate the globe", () => liveCanvas().toDataURL() !== globePixels);
  // Focus request: selects the node and, on the globe, turns it to face front.
  setFocus("pkg-3/file-3.ts"); await settle();
  await new Promise<void>(resolve => setTimeout(resolve, 450)); await settle();
  check("focusNodeId selects the node on the globe", () => selection().includes("file-3.ts"));
  toggle("Flat")?.click(); await settle();
  setFocus(null); await settle();
  setFocus("pkg-4/file-4.ts"); await settle();
  check("focusNodeId selects the node in the flat view", () => selection().includes("file-4.ts"));
  setFocus(null); await settle();
  return "PASS: repaint, refresh and repository isolation, fit supplied coordinates, first payload fitted after a scope change, combined filters, repeated-name labels, globe toggle and rotation, focus requests, paged browsing, directed neighbors, keyboard file activation, removed selection, malformed/empty/unavailable states";
}

/** Measure mounted Svelte + canvas work, including actual frame completion. */
export async function stressDevmap(setLoad: (load: GraphVizLoad) => void, check: DevmapCheck): Promise<string> {
  const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  const results: string[] = [];
  const map = document.querySelector<HTMLElement>('.code-map');
  check("the map mounts", () => !!map);
  if (!map) return "";
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
        check(`${shape} ${width}px: the canvas stays mounted`, () => !!canvas);
        if (!canvas) return "";
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
        check(`${shape} ${width}px: load within 4 s and frames within 500 ms (load ${Math.round(loadMs)}ms, frame max ${Math.round(worst)}ms)`, () => loadMs <= 4000 && worst <= 500);
        const groupControls = map.querySelectorAll('[data-map-group]').length;
        check(`${shape} ${width}px: community controls stay bounded (${groupControls} of at most 41)`, () => groupControls <= 41);
      }
    }
  } finally { map.style.width = originalWidth; }
  return `PASS: 5,000-node browser stress (0 / 5,000 / 50,000 links). ${results.join("; ")}`;
}
