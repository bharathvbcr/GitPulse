<script lang="ts">
  import PromptModal from "../src/lib/components/PromptModal.svelte";
  import TaskBoard from "../src/lib/components/TaskBoard.svelte";
  let repositoryPath = $state<string | null>(null);
</script>

<div class="fixture-controls" aria-label="Fixture repository tabs">
  <span>Task UI fixture · Simulated storage</span>
  <button onclick={() => { repositoryPath = null; }}>Global fixture</button>
  <button onclick={() => { repositoryPath = "/fixture/GitPulse"; }}>GitPulse fixture</button>
  <button onclick={() => { repositoryPath = "/fixture/Manvi"; }}>Manvi fixture</button>
</div>
<TaskBoard {repositoryPath} />
<PromptModal />

<style>
  .fixture-controls{display:flex;gap:12px;align-items:center;padding:8px 16px;font:11px system-ui;color:rgb(var(--c-text-muted));border-bottom:1px solid rgb(var(--c-border))}
  button{padding:4px 8px;border:1px solid rgb(var(--c-border));border-radius:6px}
</style>
