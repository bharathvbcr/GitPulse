<script lang="ts">
  import StatusPopover from "../src/lib/desktop/StatusPopover.svelte";
  import { statusFixture } from "./statusFixtures";
  let scenario = $state("changes");
  let theme = $state("light");
  let material = $state<"preview" | "opaque">("preview");
  let lastAction = $state("None");
  let repo = $state("GitPulse");
  $effect(() => {
    document.documentElement.classList.toggle("light", theme === "light");
    document.documentElement.classList.toggle("dark", theme === "dark");
  });
  const snapshot = $derived.by(() => {
    const value = statusFixture(scenario);
    value.status.repository = scenario === "long names" ? value.status.repository : repo;
    value.activePath = scenario === "empty" ? null : `/Projects/${repo}`;
    value.repositories = value.repositories.map((entry) => ({ ...entry, active: entry.label === repo }));
    return value;
  });
  function action(id: string) { lastAction = id; if (id.startsWith("activate-repo:")) repo = id.endsWith("ScholarLM") ? "ScholarLM" : "GitPulse"; }
</script>
<svelte:head><title>GitPulse status popover preview</title></svelte:head>
<main class:dark={theme === "dark"}>
  <div class="controls"><span>UI preview · sample data · simulated backdrop</span><label>Appearance <select bind:value={theme}><option>light</option><option>dark</option></select></label><label>State <select bind:value={scenario}>{#each ["changes","conflicts","clean","loading","unavailable","empty","long names","busy","operation"] as name}<option>{name}</option>{/each}</select></label><label>Material <select bind:value={material}><option value="preview">Glass</option><option value="opaque">Opaque</option></select></label></div>
  <div class="stage"><StatusPopover {snapshot} {material} onaction={action} /></div>
  <p class="action" aria-live="polite">Last action: {lastAction}</p>
</main>
<style>
  :global(body) { margin:0; }
  :global(*) { box-sizing:border-box; }
  main { min-height:100vh; background:radial-gradient(ellipse at 24% 26%,#95c0eb 0,transparent 44%),radial-gradient(ellipse at 72% 55%,#cab2de 0,transparent 40%),linear-gradient(130deg,#e9ecf1,#c2d9d5); padding:25px; font-family:-apple-system,BlinkMacSystemFont,sans-serif; }
  main.dark { background:radial-gradient(ellipse at 24% 26%,#244466 0,transparent 44%),radial-gradient(ellipse at 72% 55%,#463459 0,transparent 40%),linear-gradient(130deg,#101217,#1f3636); }
  .controls { display:flex; justify-content:center; align-items:center; flex-wrap:wrap; gap:20px; font-size:11px; color:#727b88; }
  .controls span { font-weight:550; }
  label { display:flex; align-items:center; gap:6px; }
  select { font:inherit; padding:4px; border:1px solid #a1a9b644; border-radius:5px; background:#ffffff50; color:inherit; }
  .stage { width:376px; max-width:100%; margin:58px auto 0; }
  .action { text-align:center; color:#727b88; font-size:11px; margin-top:25px; }
</style>
