<script lang="ts">
  import TaskRuns from "../src/lib/components/TaskRuns.svelte";
  import TerminalDock from "../src/lib/components/TerminalDock.svelte";
  import { interfaceStore } from "../src/lib/stores/interfaceStore";
  import type { Repository, Task } from "../src/lib/workbench/client";
  let { task, repositories }: { task: Task; repositories: Repository[] } = $props();
  let active = $state(true);
  const load = () => import("../src/lib/components/TerminalPanel.svelte");
</script>

<div class="h-screen flex gap-4 p-4 bg-background text-textPrimary">
  <aside class="w-[360px] shrink-0 overflow-auto">
    <h2>Preserve the task’s repository and permissions</h2>
    <p>Disposable browser fixture. Native process and database transport are simulated.</p>
    <button class="gp-btn" onclick={() => active = !active}>{active ? "Suspend run updates" : "Resume run updates"}</button>
    <TaskRuns {task} {repositories} {active} />
  </aside>
  <div class="flex flex-col flex-1 min-w-0 min-h-0">
    <div class="flex-1 p-4 text-textMuted">The selected task’s terminal opens here.</div>
    <TerminalDock open={$interfaceStore.terminalDockOpen} onClose={() => interfaceStore.setTerminalDockOpen(false)} {load} />
  </div>
</div>
