<script lang="ts">
  import PromptModal from "../src/lib/components/PromptModal.svelte";
  import TaskBoard from "../src/lib/components/TaskBoard.svelte";
  import ScanProposals from "../src/lib/components/ScanProposals.svelte";
  let repositoryPath = $state<string | null>(null);
</script>

<div class="fixture-controls" aria-label="Fixture repository tabs">
  <span>Task UI fixture · Simulated storage</span>
  <button onclick={() => { repositoryPath = null; }}>Global fixture</button>
  <button onclick={() => { repositoryPath = "/fixture/GitPulse"; }}>GitPulse fixture</button>
  <button onclick={() => { repositoryPath = "/fixture/Manvi"; }}>Manvi fixture</button>
</div>
<!-- The same pane wrapper App.svelte gives the board. `.gp-pane` contains
     layout and paint, which makes it the containing block of every
     `position: fixed` descendant; a board mounted bare here hid that. -->
<div class="gp-workspace flex-1 flex flex-col min-h-0 bg-background gp-pane">
  <!-- As RepositoryTasks hosts them in the app: the proposals dock sits over the board's corner. -->
  <div class="repo-tasks">
    <TaskBoard {repositoryPath} acceptsOpenRequests={repositoryPath === null} />
    {#if repositoryPath}<ScanProposals {repositoryPath} />{/if}
  </div>
</div>
<PromptModal />

<style>
  .fixture-controls{display:flex;gap:12px;align-items:center;padding:8px 16px;font:11px system-ui;color:rgb(var(--c-text-muted));border-bottom:1px solid rgb(var(--c-border))}
  .repo-tasks{position:relative;display:flex;flex:1;min-height:0;min-width:0}
  button{padding:4px 8px;border:1px solid rgb(var(--c-border));border-radius:6px}
</style>
