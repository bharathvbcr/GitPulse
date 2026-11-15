import { tick } from "svelte";
import { codeGraphHitTestCount, codeGraphLayoutCount } from "../src/lib/codeintel/codeMap";
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
  // A lost context comes back blank; the restore event must re-acquire, re-tune
  // and repaint it — and nothing may repaint while the context is still lost.
  const painted = canvas.toDataURL();
  const lost = new Event("contextlost", { cancelable: true });
  canvas.dispatchEvent(lost);
  check("contextlost is claimed, so the browser can restore the context", () => lost.defaultPrevented);
  await settle();
  const raw2d = canvas.getContext("2d");
  if (raw2d) {
    raw2d.save();
    raw2d.setTransform(1, 0, 0, 1, 0, 0);
    raw2d.clearRect(0, 0, canvas.width, canvas.height);
    raw2d.restore();
    raw2d.imageSmoothingEnabled = false;
  }
  await settle();
  check("the blanked canvas stays blank until contextrestored", () => canvas.toDataURL() !== painted);
  canvas.dispatchEvent(new Event("contextrestored"));
  await settle();
  check("contextrestored re-acquires and tunes the 2D context", () => canvas.getContext("2d")?.imageSmoothingEnabled === true);
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
  const mapEl = document.querySelector<HTMLElement>(".code-map");
  check("the map container is mounted for resize checks", () => !!mapEl);
  if (!mapEl) return "";
  const savedMapWidth = mapEl.style.width;
  try {
    const layoutsBeforeLoad = codeGraphLayoutCount();
    setLoad({
      available: true,
      kind: "code_graph",
      path: "/fixture/resize-no-relayout",
      payload: {
        level: "file",
        nodes: [
          { id: "r1", name: "ResizeOne", kind: "file", path: "src/r1.ts" },
          { id: "r2", name: "ResizeTwo", kind: "file", path: "src/r2.ts" },
        ],
        links: [{ source: "r1", target: "r2", kind: "imports" }],
      },
    });
    await settle();
    const layoutsAfterLoad = codeGraphLayoutCount();
    check("loading a new graph payload runs layout once", () => layoutsAfterLoad === layoutsBeforeLoad + 1);
    mapEl.style.width = "480px";
    await settle();
    check("resizing the fitted stage updates the canvas width", () => liveCanvas().style.width === "480px");
    mapEl.style.width = "720px";
    await settle();
    check("resizing the fitted stage again updates the canvas width", () => liveCanvas().style.width === "720px");
    check("resizing while fitted does not call layoutNodes", () => codeGraphLayoutCount() === layoutsAfterLoad);
    liveCanvas().dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    await settle();
    mapEl.style.width = "560px";
    await settle();
    check("resizing after panning updates the canvas width without calling layoutNodes", () =>
      liveCanvas().style.width === "560px" && codeGraphLayoutCount() === layoutsAfterLoad);
  } finally {
    mapEl.style.width = savedMapWidth;
    await settle();
  }
  setLoad({
    available: true,
    kind: "code_graph",
    path: "/fixture/hover-coalesce",
    payload: {
      level: "file",
      nodes: [{ id: "hover-node", name: "Hover target", kind: "file", path: "src/hover.ts", x: 400, y: 280 }],
      links: [],
    },
  });
  await settle();
  const hoverCanvas = liveCanvas();
  const hoverBounds = hoverCanvas.getBoundingClientRect();
  const centreX = hoverBounds.left + hoverBounds.width / 2;
  const centreY = hoverBounds.top + hoverBounds.height / 2;
  const cornerX = hoverBounds.left + 6;
  const cornerY = hoverBounds.top + 6;
  const hitsBeforeBurst = codeGraphHitTestCount();
  for (let i = 0; i < 9; i++) {
    hoverCanvas.dispatchEvent(new PointerEvent("pointermove", { clientX: cornerX + i, clientY: cornerY, bubbles: true }));
  }
  hoverCanvas.dispatchEvent(new PointerEvent("pointermove", { clientX: centreX, clientY: centreY, bubbles: true }));
  const syncHitDelta = codeGraphHitTestCount() - hitsBeforeBurst;
  check("pointermove does not run hitTestCodeGraph synchronously", () => syncHitDelta === 0);
  await settle();
  const burstHitDelta = codeGraphHitTestCount() - hitsBeforeBurst;
  check(`a 10-event pointermove burst runs at most one hitTestCodeGraph per frame (ran ${burstHitDelta})`, () => burstHitDelta === 1);
  check("coalesced hover hit-test uses the newest pointer position", () => selection().includes("Hover target"));
  hoverCanvas.dispatchEvent(new PointerEvent("pointermove", { clientX: cornerX, clientY: cornerY, bubbles: true }));
  await settle();
  check("moving off the node clears the hover inspector", () => !selection().includes("Hover target"));
  const hitsBeforeLeave = codeGraphHitTestCount();
  hoverCanvas.dispatchEvent(new PointerEvent("pointermove", { clientX: centreX, clientY: centreY, bubbles: true }));
  hoverCanvas.dispatchEvent(new PointerEvent("pointerleave", { bubbles: false }));
  await settle();
  check("pointerleave cancels a queued hover hit-test in the same frame", () =>
    codeGraphHitTestCount() === hitsBeforeLeave && !selection().includes("Hover target"));
  return "PASS: repaint, resize without re-layout, coalesced hover hit-test, refresh and repository isolation, fit supplied coordinates, first payload fitted after a scope change, combined filters, repeated-name labels, globe toggle and rotation, focus requests, paged browsing, directed neighbors, keyboard file activation, removed selection, malformed/empty/unavailable states";
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

/**
 * Real-DOM checks of the Code → Map panel: the first-five-rows preview on
 * every long list, the reset when the subsystem changes, and what each state
 * of the index says. The panel is mounted in `stage` over a mocked IPC
 * (`devmapPanelFixture.ts`) and every query is scoped to it, so these checks
 * and the canvas checks above cannot see each other's DOM.
 */
export async function runPanelChecks(stage: HTMLElement, check: DevmapCheck): Promise<string> {
  const { mount, unmount } = await import("svelte");
  const fixture = await import("./devmapPanelFixture");
  const { holdForeground } = await import("./foreground");
  const { repoStore } = await import("../src/lib/stores/repoStore");
  const { default: RepoMapPanel } = await import("../src/lib/components/RepoMapPanel.svelte");

  holdForeground();
  fixture.installPanelIpc();
  await repoStore.openRepo(fixture.FIXTURE_REPO);

  const wait = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
  const settle = async () => { await tick(); await wait(40); await tick(); };
  const until = async (what: string, test: () => unknown) => {
    const end = Date.now() + 8000;
    while (!test() && Date.now() < end) await wait(40);
    if (!test()) throw new Error(`Timed out waiting for ${what}`);
  };
  const flat = (node: Element | null | undefined) => (node?.textContent ?? "").replace(/\s+/g, " ").trim();
  const qa = (selector: string) => [...stage.querySelectorAll<HTMLElement>(selector)];
  const q = (selector: string) => stage.querySelector<HTMLElement>(selector);
  /** The data rows of a section, whatever the list draws them as. */
  const rows = (section: string) => qa(`section[aria-labelledby="${section}"] li`);
  const toggleOf = (section: string) => q(`section[aria-labelledby="${section}"] [data-list-preview-toggle]`);
  const toggleLabel = (section: string) => flat(toggleOf(section));
  const click = async (element: HTMLElement | null | undefined, what: string) => {
    if (!element) throw new Error(`Missing control: ${what}`);
    element.click();
    await settle();
  };
  const treeRow = (area: string) => qa('[role="treeitem"]').find((row) => row.getAttribute("title") === area) ?? null;
  const tab = (label: string) => qa('[role="tab"]').find((t) => flat(t) === label) ?? null;
  /** The unwired / dead-symbol lists are repository-wide, under one heading. */
  const wideList = (heading: string) =>
    qa("h3").find((h) => flat(h).startsWith(heading))?.parentElement ?? null;

  let app: ReturnType<typeof mount> | null = null;
  const open = async (scenario: typeof fixture.panel.scenario, ready: () => unknown) => {
    if (app) await unmount(app);
    stage.replaceChildren();
    fixture.panel.scenario = scenario;
    fixture.panel.releaseStatus = null;
    app = mount(RepoMapPanel, { target: stage });
    await until("the panel to settle", ready);
    await settle();
  };
  const detailReady = () => !!q('[data-testid="map-overview"]');

  stage.style.width = "1100px";
  await open({ map: fixture.mapLoad(fixture.mapDocument()), status: "fresh" }, detailReady);

  // Overview: what the page answers first.
  const overview = q('[data-testid="map-overview"]');
  check("the page opens on an overview of the code map", () => !!overview && flat(overview).includes("Overview"));
  check("the overview states size, caps and languages",
    () => flat(overview).includes("Subsystems 8") && flat(overview).includes("Unwired candidates 9 of 30")
      && flat(overview).includes("TypeScript, Rust, Svelte"));
  check("the overview sits above the selected subsystem", () => {
    const heading = q("h2.font-mono");
    return !!overview && !!heading && !!(overview.compareDocumentPosition(heading) & Node.DOCUMENT_POSITION_FOLLOWING);
  });
  check("a fresh, complete index shows no warning notice",
    () => !q('[data-testid="map-graph-degraded"]') && !q('[data-testid="map-index-stale"]') && !q('[data-testid="map-unavailable"]'));
  check("the strip reports the fresh index", () => flat(q('[role="status"]')).includes("Fresh"));

  // The subsystem navigator is never previewed: every row stays, scrollable.
  const treeRows = qa('[role="treeitem"]');
  check("the navigator lists every area (more than five rows, no expander)",
    () => treeRows.length > 5 && !q('nav[aria-label="Subsystems"] [data-list-preview-toggle]'));

  // Long path names truncate, with the full path in the tooltip.
  const longButton = qa('[data-testid="map-overview"] button').find((b) => b.getAttribute("title") === fixture.LONG_PATH);
  check("a long path is truncated and keeps its full text in the tooltip", () => {
    const label = longButton?.querySelector<HTMLElement>(".truncate");
    return !!longButton && !!label && label.scrollWidth > label.clientWidth;
  });
  const detail = q('[data-testid="map-detail"]');
  check("a long path does not widen the pane", () => !!detail && detail.scrollWidth <= detail.clientWidth + 1);

  // Important files: seven rows, first five, then all.
  check("seven important files open as five rows", () => rows("map-overview").length === 5);
  check("the toggle names the rows it reveals", () => toggleLabel("map-overview") === "Show all 7 files");

  // Entry points: eight rows for src/big.
  check("eight entry points open as five rows", () => rows("map-entry-points").length === 5);
  check("the toggle says Show all 8 entry points", () => toggleLabel("map-entry-points") === "Show all 8 entry points");
  check("the toggle is collapsed", () => toggleOf("map-entry-points")?.getAttribute("aria-expanded") === "false");
  await click(toggleOf("map-entry-points"), "entry points toggle");
  check("expanding shows all eight entry points", () => rows("map-entry-points").length === 8);
  check("the expanded toggle offers Show fewer", () => toggleLabel("map-entry-points") === "Show fewer"
    && toggleOf("map-entry-points")?.getAttribute("aria-expanded") === "true");
  await click(toggleOf("map-entry-points"), "entry points toggle");
  check("Show fewer collapses to five again", () => rows("map-entry-points").length === 5);

  // Critical files: six rows.
  check("six critical files open as five rows with a Show all 6 toggle",
    () => rows("map-critical").length === 5 && toggleLabel("map-critical") === "Show all 6 critical files");

  // Tests: a capped sample of twelve of twenty. The cap notice stays visible while collapsed.
  check("tests keep their 'N of M' count", () => flat(q("#map-tests")).includes("12 of 20"));
  check("twelve sampled tests open as five rows", () => rows("map-tests").length === 5);
  check("the toggle counts the sample it can reveal, not the index total", () => toggleLabel("map-tests") === "Show all 12 test files");
  check("the sample notice is visible while the list is collapsed",
    () => flat(q('section[aria-labelledby="map-tests"]')).includes("Sample of 12; 20 total for this area."));

  // Neighbors: seven chips.
  check("seven neighbors open as five chips with a toggle",
    () => rows("map-neighbors").length === 5 && toggleLabel("map-neighbors") === "Show all 7 neighbors");

  // Repository-wide lists.
  const unwired = () => wideList("Unwired candidates");
  const deadSymbols = () => wideList("Dead symbol candidates");
  check("nine unwired candidates open as five rows with the cap still stated",
    () => (unwired()?.querySelectorAll("li").length ?? 0) === 5 && flat(unwired()).includes("9 of 30")
      && flat(unwired()?.querySelector("[data-list-preview-toggle]")) === "Show all 9 unwired files");
  check("twelve dead symbols open as five rows",
    () => (deadSymbols()?.querySelectorAll("li").length ?? 0) === 5
      && flat(deadSymbols()?.querySelector("[data-list-preview-toggle]")) === "Show all 12 dead symbols");
  check("six unreachable files open as five rows",
    () => (wideList("Unreachable files")?.querySelectorAll("li").length ?? 0) === 5);
  check("no list scrolls inside the page", () => !qa("ul").some((list) => getComputedStyle(list).overflowY === "auto"));

  // Reset: expand several, switch subsystem, everything collapses; coming back is collapsed too.
  await click(toggleOf("map-neighbors"), "neighbors toggle");
  await click(unwired()?.querySelector<HTMLElement>("[data-list-preview-toggle]"), "unwired toggle");
  check("a second list expands independently", () => rows("map-neighbors").length === 7 && (unwired()?.querySelectorAll("li").length ?? 0) === 9);
  check("expanding one list leaves the entry points collapsed", () => rows("map-entry-points").length === 5);
  await click(toggleOf("map-entry-points"), "entry points toggle");
  check("entry points expanded before the switch", () => rows("map-entry-points").length === 8);
  // A background index refresh rebuilds the map; the same subsystem stays selected, and what was opened stays open.
  const readsBefore = fixture.panel.mapReads;
  const { liveIndex } = await import("../src/lib/codeintel/liveIndex");
  liveIndex.snapshots.set({ [fixture.FIXTURE_REPO]: { phase: "ready", decision: "refresh", reason: null, updatedAt: 1, refreshing: false, revision: 7, stage: null } });
  await until("the map to be read again", () => fixture.panel.mapReads > readsBefore);
  await settle();
  check("a background refresh keeps the same subsystem selected", () => flat(q("h2.font-mono")) === "src/big");
  check("a background refresh does not close an expanded list",
    () => rows("map-entry-points").length === 8 && toggleLabel("map-entry-points") === "Show fewer");
  await click(treeRow("src/small"), "src/small row");
  check("the subsystem switched", () => flat(q("h2.font-mono")) === "src/small");
  check("a subsystem with three entry points shows three rows and no toggle",
    () => rows("map-entry-points").length === 3 && !toggleOf("map-entry-points"));
  check("exactly five critical files show five rows and no toggle",
    () => rows("map-critical").length === 5 && !toggleOf("map-critical"));
  check("a subsystem with no test files says so", () => flat(q('section[aria-labelledby="map-tests"]')).includes("No test role files in this area."));
  check("two neighbors show two chips and no toggle", () => rows("map-neighbors").length === 2 && !toggleOf("map-neighbors"));
  check("repository-wide lists collapse on the switch", () => (unwired()?.querySelectorAll("li").length ?? 0) === 5);
  await click(treeRow("src/big"), "src/big row");
  check("returning to the first subsystem finds its lists collapsed",
    () => rows("map-entry-points").length === 5 && rows("map-neighbors").length === 5
      && toggleLabel("map-entry-points") === "Show all 8 entry points");

  // A neighbor button selects that subsystem (navigation still works through the preview).
  await click(qa('section[aria-labelledby="map-neighbors"] button').find((b) => flat(b) === "src/small"), "neighbor src/small");
  check("a neighbor button selects its subsystem", () => flat(q("h2.font-mono")) === "src/small");

  // The pane's width, not the window's, sets the layout.
  const columns = () => {
    const grid = q('[data-testid="map-overview"] dl');
    if (!grid) throw new Error("Missing the overview grid");
    return getComputedStyle(grid).gridTemplateColumns.split(" ").length;
  };
  check("a wide pane lays the overview out in three columns", () => columns() === 3);
  stage.style.width = "560px";
  await settle();
  const card = q("[data-repomap-subsystems-card]");
  check("a narrow pane lays the overview out in two columns", () => columns() === 2);
  check("a narrow pane shrinks the subsystem card to at most 40%", () => !!card && card.getBoundingClientRect().width <= 560 * 0.4 + 1);
  const narrowDetail = q('[data-testid="map-detail"]');
  check("a narrow pane does not overflow sideways", () => !!narrowDetail && narrowDetail.scrollWidth <= narrowDetail.clientWidth + 1);
  stage.style.width = "1100px";
  await settle();

  // Docs: eight hits and nine broken links, first five, then all; a new search closes the hits.
  await click(tab("Docs"), "Docs tab");
  await until("the docs view", () => q('[data-testid="docs-panel"]') && rows("docs-broken-heading").length > 0);
  check("nine broken links open as five rows with a Show all 9 broken links toggle",
    () => rows("docs-broken-heading").length === 5 && toggleLabel("docs-broken-heading") === "Show all 9 broken links");
  await click(toggleOf("docs-broken-heading"), "broken links toggle");
  check("expanding shows all nine broken links, with no inner scroller",
    () => rows("docs-broken-heading").length === 9
      && !qa('section[aria-labelledby="docs-broken-heading"] ul').some((list) => getComputedStyle(list).overflowY === "auto"));
  const search = stage.querySelector<HTMLInputElement>('input[aria-label="Search repository markdown"]');
  const submitSearch = async (query: string) => {
    if (!search) throw new Error("Missing the docs search field");
    search.value = query;
    search.dispatchEvent(new Event("input", { bubbles: true }));
    await settle();
    await click(qa("form button").find((b) => flat(b) === "Search"), "Search");
    await until("hits", () => rows("docs-search-heading").length > 0);
  };
  await submitSearch("alpha");
  check("eight docs hits open as five rows with a Show all 8 hits toggle",
    () => rows("docs-search-heading").length === 5 && toggleLabel("docs-search-heading") === "Show all 8 hits");
  await click(toggleOf("docs-search-heading"), "hits toggle");
  check("expanding shows all eight hits", () => rows("docs-search-heading").length === 8);
  await submitSearch("beta");
  await until("the new answer", () => flat(q('section[aria-labelledby="docs-search-heading"]')).includes("beta hit 1"));
  check("a new search answer starts collapsed", () => rows("docs-search-heading").length === 5);

  // Links: seven candidates.
  await click(tab("Links"), "Links tab");
  await until("link candidates", () => qa('[data-testid="link-candidates-panel"] li').length > 0);
  check("seven link candidates open as five rows with a Show all 7 link candidates toggle",
    () => qa('[data-testid="link-candidates-panel"] li').length === 5
      && flat(q('[data-testid="link-candidates-panel"] [data-list-preview-toggle]')) === "Show all 7 link candidates");
  await click(tab("Navigator"), "Navigator tab");

  // States the index can be in.
  const degraded = fixture.mapDocument({
    graph_degraded: true, graph_degraded_reason: "170746 of 331397 call sites were not resolved",
    liveness_meta: { ...fixture.mapDocument().liveness_meta, unavailable: { unreachable_files: "the component pass could not answer reachability" } },
  });
  await open({ map: fixture.mapLoad(degraded), status: "stale" }, () => q('[data-testid="map-graph-degraded"]'));
  check("a degraded graph says so, with its reason",
    () => flat(q('[data-testid="map-graph-degraded"]')).includes("The code map is degraded.")
      && flat(q('[data-testid="map-graph-degraded"]')).includes("170746 of 331397 call sites were not resolved"));
  check("a stale index says it is out of date, what that means and why",
    () => flat(q('[data-testid="map-index-stale"]')).includes("The index is out of date.")
      && flat(q('[data-testid="map-index-stale"]')).includes("3 pending updates are not indexed yet.")
      && flat(q('[data-testid="map-index-stale"]')).includes("stored extraction payload is obsolete"));
  check("an analysis that could not run is named, with its reason, instead of reading as none",
    () => flat(q('[data-testid="map-unavailable"]')).includes("Couldn't check unreachable files.")
      && flat(q('[data-testid="map-unavailable"]')).includes("the component pass could not answer reachability"));
  check("the strip says Stale beside the notice", () => flat(q('[role="status"]')).includes("Stale"));

  await open({ map: fixture.mapLoad(fixture.mapDocument()), status: "degraded" }, () => q('[data-testid="map-index-degraded"]'));
  check("a current index that reports a reason is called degraded, not shown clean",
    () => flat(q('[data-testid="map-index-degraded"]')).includes("The index is degraded.")
      && flat(q('[data-testid="map-index-reason"]')).includes("stored extraction payload is obsolete") && !q('[data-testid="map-index-stale"]'));
  const suppressed = fixture.mapDocument({
    liveness_unreachable_unreliable: true,
    liveness_meta: { ...fixture.mapDocument().liveness_meta, unavailable: { unreachable_files: "the component pass could not answer reachability" } },
  });
  await open({ map: fixture.mapLoad(suppressed), status: "fresh" }, detailReady);
  check("the unreachable list's own explanation is not repeated in a second notice",
    () => !q('[data-testid="map-unavailable"]') && flat(stage).includes("Unreachable files are hidden because the index marks that list as unreliable."));

  await open({ map: fixture.mapLoad(fixture.mapDocument({ subsystems: [] })), status: "fresh" }, () => q('[data-testid="map-no-subsystems"]'));
  check("a map with no subsystems says it lists none instead of 'No areas match'",
    () => flat(q('[data-testid="map-no-subsystems"]')).includes("This map lists no subsystems") && !flat(stage).includes("No areas match"));

  await open({ map: fixture.mapLoad(fixture.mapDocument()), status: "pending" }, () => q('[data-testid="map-status-pending"]'));
  check("while the status read is in flight the strip says it is checking, not unknown",
    () => flat(q('[data-testid="map-status-pending"]')) === "Checking index status…" && !q('[data-testid="map-status-unknown"]'));
  check("the map is already usable while the status is pending", () => !!q('[data-testid="map-overview"]'));
  fixture.panel.releaseStatus?.();
  await until("the status to arrive", () => !q('[data-testid="map-status-pending"]'));
  check("an arrived status replaces the pending text", () => flat(q('[role="status"]')).includes("Fresh"));

  await open({ map: fixture.mapLoad(fixture.mapDocument()), status: "failed" }, () => q('[data-testid="map-status-unknown"]'));
  check("a status read that failed says it could not be read",
    () => flat(q('[data-testid="map-status-unknown"]')) === "Couldn't read the index status.");

  if (app) await unmount(app);
  stage.replaceChildren();
  check(`every command the panel invoked has a fixture (${[...fixture.panel.unexpected].join(", ") || "none unanswered"})`, () => fixture.panel.unexpected.size === 0);
  return "PASS: Code → Map panel: five-row previews and expanders on every long list, collapse on subsystem and search change, no previewing of the navigator, overview, degraded/stale/unavailable/pending/failed/empty states, pane-width layout";
}
