<script lang="ts">
  import { runDevmapChecks, stressDevmap } from "./devmapChecks";
  import { createChecks, type CheckRow } from "./checks";
  import { onMount } from "svelte";
  import CodeGraphCanvas from "../src/lib/components/CodeGraphCanvas.svelte";
  import type { GraphVizLoad, GraphVizPayload } from "../src/lib/codeintel/types";
  import { themeStore } from "../src/lib/stores/themeStore";

  const groups = [83, 42, 35, 34, 31, 23, 12, 9, ...Array.from({length: 19}, () => 1)];
  const nodes = groups.flatMap((count, group) => Array.from({length: count}, (_, index) => ({
    id: `src/area-${group}/Module${index}.ts`, path: `src/area-${group}/Module${index}.ts`,
    name: `Module${index}.ts`, kind: "file", community: `community-${group}`, degree: index < 4 ? 14 - index * 3 : 0,
  })));
  const fixture: GraphVizPayload = {
    level: "file", generation_id: 1, nodes,
    links: nodes.filter(n => n.degree > 0).slice(1).map(n => ({source: nodes[0].id, target: n.id, kind: "imports"})),
    counts: { nodes_shown: nodes.length, nodes_total: 400, nodes_truncated: true, max_nodes: nodes.length },
  };
  let load: GraphVizLoad | null = $state({ available: true, kind: "code_graph", payload: fixture });
  let opened = $state("");
  let focusId = $state<string | null>(null);
  let show = $state(true);
  let verdict = $state("");
  let checking = $state(false);
  /** Run the runtime checks (or the stress pass) and return the rows. A failed
   * check ends the run with a row naming where it stopped, so the verdict never
   * reads as a shorter list of passes. */
  async function runChecks(stress = false): Promise<{ rows: CheckRow[]; errors: string[] }> {
    checking = true;
    const errors: string[] = [];
    const onError = (event: ErrorEvent) => errors.push(event.message);
    const onRejection = (event: PromiseRejectionEvent) => errors.push(String(event.reason));
    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onRejection);
    const { results, check, stopped } = createChecks();
    try {
      const summary = stress
        ? await stressDevmap(next => load = next, check)
        : await runDevmapChecks(next => load = next, check, id => focusId = id);
      for (let i=0;i<3;i++) await new Promise<void>(resolve=>requestAnimationFrame(()=>resolve()));
      check("no unhandled runtime errors", () => errors.length === 0);
      verdict = summary;
    }
    catch (error) { verdict = `FAIL: ${stopped(error)}`; }
    finally {
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onRejection);
      checking = false;
    }
    return { rows: results, errors };
  }
  /** `?check=1&report=…` is how the regression gate drives a page: run both
   * suites (runtime, then the 5,000-node stress pass), publish every check of
   * each as its own row on the document element, and POST the page back. A
   * suite that stopped early carries the row that says where. */
  async function reportToGate(report: string) {
    const results: CheckRow[] = [];
    const errors: string[] = [];
    const verdicts: string[] = [];
    for (const [suite, stress] of [["runtime checks", false], ["stress checks", true]] as const) {
      const { rows, errors: suiteErrors } = await runChecks(stress);
      for (const row of rows) results.push({ name: `${suite}: ${row.name}`, pass: row.pass });
      for (const message of suiteErrors) errors.push(`${suite}: ${message}`);
      verdicts.push(`${suite}: ${verdict}`);
    }
    document.documentElement.dataset.gpResult = encodeURIComponent(JSON.stringify({ results, errors, diagnostics: JSON.stringify(verdicts) }));
    if (report.startsWith("/__gp_result/")) await fetch(report, { method: "POST", body: document.documentElement.outerHTML });
  }
  onMount(async () => {
    const params = new URLSearchParams(location.search);
    themeStore.setTheme(params.get("theme") === "light" ? "light" : "dark");
    if (params.has("real")) {
      const response = await fetch("./.devmap-preview.json");
      if (!response.ok) throw new Error(`Preview fixture: ${response.status}`);
      load = {available: true, kind: "code_graph", payload: await response.json()};
    }
    if (params.get("check") === "1") await reportToGate(params.get("report") ?? "");
  });
</script>
<div style="display:flex;gap:8px;align-items:center;padding:24px 20px 8px;background:#10151f;color:#9baac0;font:11px system-ui">
  <span style="flex:1">DevMap · browser verification</span>
  <button onclick={() => void runChecks()} disabled={checking || !show}>Run runtime checks</button>
  <button onclick={() => void runChecks(true)} disabled={checking || !show}>Run stress checks</button>
  <span role="status" data-testid="runtime-verdict">{verdict}</span>
  <button onclick={() => themeStore.toggle()}>Toggle theme</button>
  <button onclick={() => { load = {available: true, kind: "code_graph", payload: {...fixture, nodes: fixture.nodes.map(n => ({...n, name: `Updated ${n.name}`}))}}; }}>Replace same-count payload</button>
  <button onclick={() => show = !show}>Toggle mount</button>
  <span data-testid="opened-file">{opened}</span>
</div>
{#if show}<CodeGraphCanvas {load} focusNodeId={focusId} onOpenNode={(path) => opened = path} />{/if}
