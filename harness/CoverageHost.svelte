<script lang="ts">
  import CoverageViewer from "../src/lib/components/CoverageViewer.svelte";
  import TerminalDock from "../src/lib/components/TerminalDock.svelte";
  import { repoStore } from "../src/lib/stores/repoStore";
  import { interfaceStore } from "../src/lib/stores/interfaceStore";
  import { themeStore } from "../src/lib/stores/themeStore";
  let { setMode }: { setMode: (mode: string) => void } = $props();
  const load = () => import("../src/lib/components/TerminalPanel.svelte");
  let narrow = $state(0);
</script>

<main class="flex flex-col h-screen gap-2 p-3 bg-background text-textPrimary" style:max-width={narrow === 1 ? "680px" : narrow === 2 ? "420px" : "100%"}>
  <div class="flex flex-wrap gap-2 shrink-0">
    <button class="gp-btn" onclick={() => setMode("empty")}>Empty coverage</button>
    <button class="gp-btn" onclick={() => setMode("measured")}>Measured coverage</button>
    <button class="gp-btn" onclick={() => setMode("failed")}>Failed scan</button>
    <button class="gp-btn" onclick={() => setMode("spawn-error")}>Fail next agent launch</button>
    <button class="gp-btn" onclick={() => (narrow = (narrow + 1) % 3)}>Toggle narrow layout</button>
    <button class="gp-btn" onclick={() => themeStore.setTheme($themeStore === "dark" ? "light" : "dark")}>Toggle theme</button>
    {#each $repoStore.openTabs as tab (tab.id)}
      <button class="gp-btn" onclick={() => repoStore.activateTab(tab.id)}>Repository {tab.name}</button>
    {/each}
  </div>
  <div class="min-h-0 flex-1 flex flex-col border border-border rounded overflow-hidden">
    {#key $repoStore.currentPath}<CoverageViewer />{/key}
    <TerminalDock open={$interfaceStore.terminalDockOpen} onClose={() => interfaceStore.setTerminalDockOpen(false)} {load} />
  </div>
</main>
