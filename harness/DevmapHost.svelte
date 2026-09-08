<script lang="ts">
  import { runDevmapChecks, stressDevmap } from "./devmapChecks";
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
  let load: GraphVizLoad = $state({ available: true, kind: "code_graph", payload: fixture });
  let opened = $state("");
  let show = $state(true);
  let verdict = $state("");
  let checking = $state(false);
  async function runChecks(stress = false) {
    checking = true;
    const errors: string[] = [];
    const onError = (event: ErrorEvent) => errors.push(event.message);
    const onRejection = (event: PromiseRejectionEvent) => errors.push(String(event.reason));
    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onRejection);
    try {
      const result = await (stress ? stressDevmap : runDevmapChecks)(next => load = next);
      for (let i=0;i<3;i++) await new Promise<void>(resolve=>requestAnimationFrame(()=>resolve()));
      if (errors.length) throw new Error(`Browser runtime errors: ${errors.join("; ")}`);
      verdict = result;
    }
    catch (error) { verdict = `FAIL: ${String(error)}`; }
    finally {
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onRejection);
      checking = false;
    }
  }
  onMount(async () => {
    themeStore.setTheme(new URLSearchParams(location.search).get("theme") === "light" ? "light" : "dark");
    if (new URLSearchParams(location.search).has("real")) {
      const response = await fetch("./.devmap-preview.json");
      if (!response.ok) throw new Error(`Preview fixture: ${response.status}`);
      load = {available: true, kind: "code_graph", payload: await response.json()};
    }
  });
</script>
<div style="display:flex;gap:8px;align-items:center;padding:24px 20px 8px;background:#10151f;color:#9baac0;font:11px system-ui">
  <span style="flex:1">DevMap · browser verification</span>
  <button onclick={() => runChecks()} disabled={checking || !show}>Run runtime checks</button>
  <button onclick={() => runChecks(true)} disabled={checking || !show}>Run stress checks</button>
  <span role="status" data-testid="runtime-verdict">{verdict}</span>
  <button onclick={() => themeStore.toggle()}>Toggle theme</button>
  <button onclick={() => { load = {available: true, kind: "code_graph", payload: {...fixture, nodes: fixture.nodes.map(n => ({...n, name: `Updated ${n.name}`}))}}; }}>Replace same-count payload</button>
  <button onclick={() => show = !show}>Toggle mount</button>
  <span data-testid="opened-file">{opened}</span>
</div>
{#if show}<CodeGraphCanvas {load} onOpenNode={(path) => opened = path} />{/if}
