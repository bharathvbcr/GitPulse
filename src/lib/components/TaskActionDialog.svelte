<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { Trash2, X } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { TaskBatch, type TaskAction } from "../workbench/taskActions";
  import type { TaskCard } from "../workbench/client";
  let { tasks, action, onClose, onChanged }: { tasks: TaskCard[]; action: TaskAction; onClose: () => void; onChanged: (ids: string[]) => void } = $props();
  const batch = new TaskBatch(untrack(() => tasks), untrack(() => action));
  let rows = $state(batch.snapshot()), busy = $state(false), started = $state(false), stopped = $state(false);
  let cancelButton: HTMLButtonElement;
  let disposed = false;
  const deleting = $derived(action.kind === "delete");
  const done = $derived(rows.filter(row => row.state === "done").length);
  const uncertain = $derived(rows.some(row => row.state === "uncertain"));
  const waiting = $derived(rows.some(row => row.state === "waiting"));
  const failures = $derived(rows.filter(row => row.state === "failed").length);
  onDestroy(() => { disposed = true; batch.stop(); });
  onMount(() => { void tick().then(() => { if(!disposed) cancelButton.focus(); }); });
  async function run() {
    if (busy) return;
    busy = true; started = true; stopped = false;
    await batch.run(() => { if (!disposed) rows = batch.snapshot(); });
    if (!disposed) { busy = false; onChanged(rows.filter(row => row.state === "done").map(row => row.id)); }
  }
  function close() { if (!busy && !uncertain) onClose(); }
  function key(event: KeyboardEvent) {
    event.stopPropagation();
    if (event.key === "Escape") { event.preventDefault(); close(); }
  }
</script>

<div use:portal class="task-action-backdrop gp-scrim" role="presentation" style="z-index:{LAYERS.PROMPT}">
  <div class="task-action-dialog gp-card gp-glass shadow-float" role="dialog" aria-modal="true" aria-label={deleting ? "Delete tasks" : "Update tasks"} tabindex="-1" onkeydown={key} use:trapFocus={{initial: () => cancelButton}}>
    <header><h2>{deleting ? "Delete tasks" : "Update tasks"}</h2><button class="gp-icon-btn" aria-label="Close task action" disabled={busy || uncertain} onclick={close}><X size={16} /></button></header>
    <div class="body">
      {#if !started}<p>{deleting ? "Delete these tasks from every linked repository and workspace? History is retained, but deletion cannot be undone here. Running agents continue." : "Apply this change to the selected tasks? Existing descriptions, links and other fields are preserved."}</p>{/if}
      <ul>{#each rows as row (row.id)}<li><span>{row.title}</span>{#if started}<small>{row.state === "done" ? deleting ? "Deleted" : "Updated" : row.state === "uncertain" ? "Needs confirmation" : row.state === "failed" ? "Not changed" : row.state === "running" ? "Working…" : "Waiting"}</small>{/if}{#if row.error}<p class="error">{row.error}</p>{/if}</li>{/each}</ul>
      {#if started}<p role="status">{done} of {rows.length} {deleting ? "deleted" : "updated"}{failures ? ` · ${failures} not changed` : ""}{stopped ? " · Paused" : ""}.</p>{/if}
      {#if failures}<p>Refresh tasks and review changed items before trying them again.</p>{/if}
      {#if uncertain}<p role="alert">Confirmation was lost. Retry the same action to recover its result before closing.</p>{/if}
    </div>
    <footer>
      <button bind:this={cancelButton} class="gp-btn" disabled={busy || uncertain} onclick={close}>{started ? "Done" : "Cancel"}</button>
      {#if busy}<button class="gp-btn" disabled={stopped} onclick={() => { stopped = true; batch.stop(); }}>Pause after current task</button>
      {:else if !started || uncertain || waiting}<button class:danger={deleting} class="gp-btn-primary" onclick={run}>{#if deleting}<Trash2 size={13} />{/if}{uncertain ? deleting ? "Retry deletion" : "Retry update" : started ? "Continue remaining tasks" : deleting ? `Delete ${rows.length} ${rows.length === 1 ? "task" : "tasks"}` : `Update ${rows.length} ${rows.length === 1 ? "task" : "tasks"}`}</button>{/if}
    </footer>
  </div>
</div>
<style>
  .task-action-backdrop{position:fixed;inset:0;display:grid;place-items:center;padding:20px;background:#0005;color:rgb(var(--c-text))}.task-action-dialog{width:min(480px,100%);max-height:calc(100dvh - 40px);display:flex;flex-direction:column;border-radius:16px;overflow:hidden}header,footer{display:flex;align-items:center;justify-content:space-between;gap:10px;padding:16px 20px;flex-shrink:0}header{border-bottom:1px solid rgb(var(--c-border)/.5)}h2{font-size:15px;font-weight:650;margin:0}.body{padding:0 20px;overflow:auto;font-size:12px}p{line-height:1.6;margin:14px 0;color:rgb(var(--c-text-muted))}ul{list-style:none;padding:0;margin:0;max-height:260px;overflow:auto}li{padding:10px 0;border-bottom:1px solid rgb(var(--c-border)/.4);display:flex;flex-wrap:wrap;gap:4px 12px;justify-content:space-between}li span{overflow-wrap:anywhere;flex:1;min-width:160px}li p{flex-basis:100%;margin:4px 0}.error{color:#dc6565}small{color:rgb(var(--c-text-muted))}footer{justify-content:flex-end}.danger{background:#be3939;color:white}
</style>
