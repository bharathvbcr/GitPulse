<script lang="ts">
  import TerminalDock from "../src/lib/components/TerminalDock.svelte";
  import CodeViewer from "../src/lib/components/files/CodeViewer.svelte";
  import ScanProposals from "../src/lib/components/ScanProposals.svelte";
  import { repoStore } from "../src/lib/stores/repoStore";
  import { themeStore } from "../src/lib/stores/themeStore";
  const load = () => import("../src/lib/components/TerminalPanel.svelte");
  let narrow = $state(false);
  // App's rule: an open, expanded dock covers the view above it.
  let expanded = $state(false);
  const covered = $derived($repoStore.terminalOpen && expanded);

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
    <button class="gp-btn" onclick={() => repoStore.setTerminalOpen(true)}>Show terminal</button>
    {#each $repoStore.openTabs as tab (tab.id)}
      <button class="gp-btn" onclick={() => repoStore.activateTab(tab.id)}>Repository {tab.name}</button>
    {/each}
  </div>
  <div class="flex-1 min-h-0 flex flex-col border border-border rounded-lg overflow-hidden">
    <!-- The view above the dock, in the shape App gives it: `.gp-view` around
         Work's 36px section bar and the tasks surface, whose Proposals pill
         floats over the board's corner. An expanded dock leaves this no
         room, and what is positioned inside it must not show over the dock. -->
    <div class="flex-1 min-h-0 flex flex-col" data-view-host style:visibility={covered ? "hidden" : null} inert={covered}>
      <div class="h-9 shrink-0 border-b border-border/60"></div>
      <div class="relative flex-1 min-h-0 flex flex-col">
        {#if selectedPath}
          <div class="flex-1 min-h-0 overflow-hidden" data-code-host>
            <CodeViewer filePath={selectedPath} content={fixtureContent} readOnly />
          </div>
        {:else}
          <div class="flex-1 min-h-0 overflow-hidden p-4 text-textMuted">Terminal verification · Simulated PTY transport, real components and xterm. No commands execute.</div>
        {/if}
        <ScanProposals repositoryPath={$repoStore.currentPath ?? ""} />
      </div>
    </div>
    <TerminalDock bind:expanded open={$repoStore.terminalOpen} onClose={() => repoStore.setTerminalOpen(false)} {load} />
  </div>
</div>
