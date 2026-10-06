<script lang="ts">
  import TaskAgentPanel from "../src/lib/components/TaskAgentPanel.svelte";
  import TerminalDock from "../src/lib/components/TerminalDock.svelte";
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { bindAttention } from "../src/lib/terminal/sessionActivity";
  import { repoStore } from "../src/lib/stores/repoStore";
  import type { OpenTabRef } from "../src/lib/workbench/openMembership";
  import type { Repository, Task } from "../src/lib/workbench/client";
  let { task, repositories, openTabs = [] }: { task: Task; repositories: Repository[]; openTabs?: OpenTabRef[] } = $props();
  let active = $state(true);
  let working = $state(0);
  let asking = $state(0);
  // As SessionNotificationBridge does in the app: announcements feed the pane.
  onMount(() => { let stop: (() => void) | null = null; void bindAttention(listen).then((unlisten) => { stop = unlisten; }); return () => stop?.(); });
  const load = () => import("../src/lib/components/TerminalPanel.svelte");
</script>

<div class="h-screen flex gap-4 p-4 bg-background text-textPrimary">
  <aside class="w-[360px] shrink-0 overflow-auto">
    <h2>Preserve the task’s repository and permissions</h2>
    <p>Disposable browser fixture. Native process and database transport are simulated.</p>
    <button class="gp-btn" onclick={() => active = !active}>{active ? "Suspend run updates" : "Resume run updates"}</button>
    <span data-testid="host-working" data-count={working} data-asking={asking}>Agents badge: {working} ({asking} asking)</span>
    <TaskAgentPanel {task} {repositories} {openTabs} {active} onCount={(count) => { working = count; }} onAttention={(count) => { asking = count; }} />
  </aside>
  <div class="flex flex-col flex-1 min-w-0 min-h-0">
    <div class="flex-1 p-4 text-textMuted">The selected task’s terminal opens here.</div>
    <!-- As in App.svelte: the dock lives in the repository view, which exists
         only while some repository is current. -->
    {#if $repoStore.currentPath}
      <TerminalDock open={$repoStore.terminalOpen} onClose={() => repoStore.setTerminalOpen(false)} {load} />
    {/if}
  </div>
</div>
