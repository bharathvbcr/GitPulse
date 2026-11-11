<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { Trash2, X } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { trapFocus } from "../ui/focusTrap";
  import { scrollEdgeFade } from "../ui/scrollEdgeFade";
  import { LAYERS } from "../ui/layers";
  import { fade, scale } from "svelte/transition";
  import { backdropFade, backdropFadeOut, cardScale, cardScaleOut } from "../ui/transitions";
  import { DEFERRED_DELETE_MS, TaskBatch, type TaskAction } from "../workbench/taskActions";
  import type { TaskCard } from "../workbench/client";
  let { tasks, action, batch: existing, onDefer, onClose, onChanged }: {
    tasks: TaskCard[];
    action: TaskAction;
    /** A batch that already ran (a deferred deletion): show its receipts and retry it. */
    batch?: TaskBatch;
    /**
     * Hand a confirmed deletion back instead of running it, so the board can
     * hold it for its undo window (`DEFERRED_DELETE_MS`).
     */
    onDefer?: (batch: TaskBatch) => void;
    onClose: () => void;
    onChanged: (ids: string[]) => void;
  } = $props();
  const batch = untrack(() => existing) ?? new TaskBatch(untrack(() => tasks), untrack(() => action));
  let rows = $state(batch.snapshot()), busy = $state(false), started = $state(untrack(() => existing) !== undefined), stopped = $state(false);
  const deferring = $derived(onDefer !== undefined && action.kind === "delete" && !started);
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
    if (deferring && onDefer) { onDefer(batch); return; }
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

<div use:portal class="task-action-backdrop gp-scrim bg-black/50" role="presentation" style="z-index:{LAYERS.PROMPT}" in:fade={backdropFade()} out:fade={backdropFadeOut()}>
  <div class="task-action-dialog gp-card gp-glass shadow-float" in:scale={cardScale()} out:scale={cardScaleOut()} role="dialog" aria-modal="true" aria-label={deleting ? "Delete tasks" : "Update tasks"} tabindex="-1" onkeydown={key} use:trapFocus={{initial: () => cancelButton}}>
    <header><h2>{deleting ? `Delete ${rows.length === 1 ? "task" : `${rows.length} tasks`}?` : "Update tasks"}</h2><button type="button" class="gp-icon-btn" data-size="md" aria-label="Close task action" disabled={busy || uncertain} onclick={close}><X size={16} /></button></header>
    <div class="body" data-dialog-body use:scrollEdgeFade>
      {#if !started}<p>{deleting ? deferring ? `Delete these tasks from every linked repository and workspace? Nothing is deleted for ${DEFERRED_DELETE_MS / 1000} seconds, and Undo on the board keeps them. After that, deletion cannot be undone here. History is retained. Running agents continue.` : "Delete these tasks from every linked repository and workspace? History is retained, but deletion cannot be undone here. Running agents continue." : "Apply this change to the selected tasks? Existing descriptions, links and other fields are preserved."}</p>{/if}
      <ul>{#each rows as row (row.id)}<li><span>{row.title}</span>{#if started}<small>{row.state === "done" ? deleting ? "Deleted" : "Updated" : row.state === "uncertain" ? "Needs confirmation" : row.state === "failed" ? "Not changed" : row.state === "running" ? "Working…" : "Waiting"}</small>{/if}{#if row.error}<p class="error">{row.error}</p>{/if}</li>{/each}</ul>
      {#if started}<p role="status">{done} of {rows.length} {deleting ? "deleted" : "updated"}{failures ? ` · ${failures} not changed` : ""}{stopped ? " · Paused" : ""}.</p>{/if}
      {#if failures}<p>Refresh tasks and review changed items before trying them again.</p>{/if}
      {#if uncertain}<p role="alert">Confirmation was lost. Try the same action again to recover its result before closing.</p>{/if}
    </div>
    <footer>
      <button bind:this={cancelButton} type="button" class="gp-btn" data-variant="secondary" disabled={busy || uncertain} onclick={close}>{started ? "Done" : "Cancel"}</button>
      {#if busy}<button type="button" class="gp-btn" data-variant="secondary" disabled={stopped} onclick={() => { stopped = true; batch.stop(); }}>Pause after current task</button>
      {:else if !started || uncertain || waiting}<button type="button" class="gp-btn" data-variant={deleting ? "danger" : "primary"} onclick={run}>{#if deleting}<Trash2 size={13} />{/if}{uncertain ? "Try again" : started ? "Continue remaining tasks" : deleting ? `Delete ${rows.length} ${rows.length === 1 ? "task" : "tasks"}` : `Update ${rows.length} ${rows.length === 1 ? "task" : "tasks"}`}</button>{/if}
    </footer>
  </div>
</div>
<style>
  .task-action-backdrop{position:fixed;inset:0;display:grid;place-items:center;padding:20px;color:rgb(var(--c-text))}
  .task-action-dialog{width:min(480px,100%);max-height:calc(100dvh - 40px);display:flex;flex-direction:column;border-radius:var(--gp-r-xl);overflow:hidden}
  header,footer{display:flex;align-items:center;justify-content:space-between;gap:10px;padding:16px 20px;flex-shrink:0}
  h2{font-size:var(--text-base);font-weight:600;margin:0}
  .body{padding:0 20px;overflow:auto;scroll-padding-block:2rem;font-size:var(--text-xs)}
  p{line-height:1.6;margin:14px 0;color:rgb(var(--c-text-muted))}
  ul{list-style:none;padding:0;margin:0;max-height:260px;overflow:auto;display:flex;flex-direction:column;gap:4px}
  li{padding:8px 0;display:flex;flex-wrap:wrap;gap:4px 12px;justify-content:space-between}
  li span{overflow-wrap:anywhere;flex:1;min-width:160px}
  li p{flex-basis:100%;margin:4px 0}
  .error{color:rgb(var(--c-danger))}
  small{color:rgb(var(--c-text-muted))}
  footer{justify-content:flex-end}
</style>
