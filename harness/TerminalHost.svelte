<script lang="ts">
  import TerminalDock from "../src/lib/components/TerminalDock.svelte";
  import { repoStore } from "../src/lib/stores/repoStore";
  import { interfaceStore } from "../src/lib/stores/interfaceStore";
  import { themeStore } from "../src/lib/stores/themeStore";
  const load = () => import("../src/lib/components/TerminalPanel.svelte");
  let narrow = $state(false);
</script>

<div class="flex flex-col h-screen p-4 gap-3 bg-background text-textPrimary" style:max-width={narrow ? "460px" : "100%"}>
  <div class="flex gap-2 flex-wrap shrink-0">
    <button class="gp-btn" onclick={() => (narrow = !narrow)}>Toggle narrow layout</button>
    <button class="gp-btn" onclick={() => themeStore.setTheme($themeStore === "dark" ? "light" : "dark")}>Toggle theme</button>
    <button class="gp-btn" onclick={() => interfaceStore.setTerminalDockOpen(true)}>Show terminal</button>
    {#each $repoStore.openTabs as tab (tab.id)}
      <button class="gp-btn" onclick={() => repoStore.activateTab(tab.id)}>Repository {tab.name}</button>
    {/each}
  </div>
  <div class="flex-1 min-h-0 flex flex-col border border-border rounded-lg overflow-hidden">
    <div class="flex-1 min-h-0 overflow-hidden p-4 text-textMuted">Terminal verification · Simulated PTY transport, real components and xterm. No commands execute.</div>
    <TerminalDock open={$interfaceStore.terminalDockOpen} onClose={() => interfaceStore.setTerminalDockOpen(false)} {load} />
  </div>
</div>
