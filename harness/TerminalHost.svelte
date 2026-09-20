<script lang="ts">
  import TerminalDock from "../src/lib/components/TerminalDock.svelte";
  import CodeViewer from "../src/lib/components/files/CodeViewer.svelte";
  import { repoStore } from "../src/lib/stores/repoStore";
  import { interfaceStore } from "../src/lib/stores/interfaceStore";
  import { themeStore } from "../src/lib/stores/themeStore";
  const load = () => import("../src/lib/components/TerminalPanel.svelte");
  let narrow = $state(false);

  /**
   * A real CodeViewer standing in for the code view, so a terminal file link
   * can be followed all the way to the line it names.
   *
   * In the application the chain is CodeView → FileViewer → MediaViewer →
   * CodeViewer, and every link in it simply forwards `filePath`. What the
   * reveal path actually depends on is this component receiving the selected
   * path and some content, which is reproduced exactly here; mounting the
   * whole chain would add three components' worth of fixture without putting
   * anything else under test.
   */
  const selectedPath = $derived($repoStore.selectedFilePath ?? "");
  const fixtureContent = Array.from({ length: 400 }, (_, i) => `line ${i + 1} of the fixture file`).join("\n");
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
    {#if selectedPath}
      <div class="flex-1 min-h-0 overflow-hidden" data-code-host>
        <CodeViewer filePath={selectedPath} content={fixtureContent} readOnly />
      </div>
    {:else}
      <div class="flex-1 min-h-0 overflow-hidden p-4 text-textMuted">Terminal verification · Simulated PTY transport, real components and xterm. No commands execute.</div>
    {/if}
    <TerminalDock open={$interfaceStore.terminalDockOpen} onClose={() => interfaceStore.setTerminalDockOpen(false)} {load} />
  </div>
</div>
