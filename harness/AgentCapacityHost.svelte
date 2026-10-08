<script lang="ts">
  import TerminalDock from "../src/lib/components/TerminalDock.svelte";
  import { repoStore } from "../src/lib/stores/repoStore";
  const load = () => import("../src/lib/components/TerminalPanel.svelte");
</script>

<!-- As in App.svelte: the dock lives in the repository view, which exists
     only while some repository is current. -->
<div class="h-screen flex flex-col">
  {#if $repoStore.currentPath}
    <TerminalDock open={$repoStore.terminalOpen} onClose={() => repoStore.setTerminalOpen(false)} {load} />
  {/if}
</div>
