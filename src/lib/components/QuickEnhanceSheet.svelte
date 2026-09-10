<script lang="ts">
  import { onMount } from "svelte";
  import { Sparkles, X } from "@lucide/svelte";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { cardScale, backdropFade, backdropFadeOut } from "../ui/transitions";
  import { fade, scale } from "svelte/transition";
  import { getTask, explainError, type Task } from "../workbench/client";
  import { hiddenTaskDetails, visibleHiddenDetails } from "../workbench/taskOrganize";
  import { bounded } from "../workbench/taskActions";
  import TaskEnhancements from "./TaskEnhancements.svelte";
  import SettingToggle from "./SettingToggle.svelte";

  let {
    taskId,
    repoName,
    onClose,
    onApplied,
    onOpenEditor,
  }: {
    taskId: string;
    repoName: (id: string) => string | undefined;
    onClose: () => void;
    onApplied: (task: Task) => void;
    onOpenEditor: (task: Task) => void;
  } = $props();

  let task = $state<Task | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state("");
  let showEmpty = $state(false);
  let disposed = false;
  const details = $derived(task ? hiddenTaskDetails(task, repoName) : []);
  const visibleDetails = $derived(visibleHiddenDetails(details, showEmpty));
  onMount(() => { void load(); return () => { disposed = true; }; });
  async function load() {
    loading = true; error = "";
    try {
      const loaded = await bounded(getTask(taskId));
      if (!disposed) task = loaded;
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) loading = false; }
  }

  function onKey(event: KeyboardEvent) {
    if (event.key === "Escape" && !busy) {
      event.preventDefault();
      onClose();
    }
  }
</script>

<div
  class="gp-scrim bg-black/40 flex justify-end gp-gpu"
  style="z-index: {LAYERS.MODAL}"
  role="presentation"
  onclick={(e) => { if (e.target === e.currentTarget && !busy) onClose(); }}
  onkeydown={onKey}
  in:fade={backdropFade()}
  out:fade={backdropFadeOut()}
>
  <div
    use:trapFocus={{ autofocus: true }}
    in:scale={cardScale()}
    class="gp-card shadow-float h-full w-[min(440px,96vw)] flex flex-col overflow-hidden text-xs text-textPrimary gp-gpu"
    role="dialog"
    aria-modal="true"
    aria-labelledby="quick-enhance-title"
  >
    <header class="p-4 border-b border-border/60 gp-section-edge flex items-start justify-between gap-3">
      <div class="min-w-0">
        <p id="quick-enhance-title" class="text-sm font-semibold flex items-center gap-1.5">
          <Sparkles size={14} class="text-accent shrink-0" />
          Quick Enhance
        </p>
        <p class="text-[11px] text-textMuted mt-1 leading-relaxed">
          Surface the fields the board hides, then let Manvi rewrite title and description. Acceptance still changes only the fields you select.
        </p>
      </div>
      <button type="button" class="gp-icon-btn" aria-label="Close Quick Enhance" onclick={onClose} disabled={busy}><X size={14} /></button>
    </header>

    <div class="flex-1 overflow-auto p-4 space-y-4">
      {#if loading}<p role="status" class="text-textMuted">Loading task…</p>{/if}
      {#if error}<p role="alert" class="text-rose-400">{error}</p>{/if}
      {#if task}
        <div>
          <h3 class="text-[11px] font-semibold uppercase tracking-wide text-textMuted mb-1">Task</h3>
          <p class="font-medium text-textPrimary wrap-break-word">{task.title}</p>
          <p class="text-[11px] text-textMuted mt-0.5">Primary repository: {repoName(task.primary_repository_id) ?? task.primary_repository_id}</p>
          <p class="text-[11px] text-textMuted mt-0.5">Revision {task.revision} · {task.status.replace("_", " ")}</p>
        </div>
        <div class="space-y-2">
          <div class="flex items-center justify-between gap-2">
            <h3 class="text-[11px] font-semibold uppercase tracking-wide text-textMuted">Hidden on the board</h3>
          </div>
          <SettingToggle
            label="Show empty fields"
            description="Owner, due date, labels, locks, and extra repositories stay off until they have a value."
            checked={showEmpty}
            onchange={(next) => { showEmpty = next; }}
          />
          {#each visibleDetails as row (row.key)}
            <div>
              <p class="text-[11px] text-textMuted">{row.label}</p>
              <pre class="mt-0.5 whitespace-pre-wrap wrap-break-word max-h-32 overflow-auto rounded-lg border border-border/60 bg-background/50 px-2 py-1.5 {row.empty ? 'text-textMuted' : ''}">{row.value}</pre>
            </div>
          {/each}
          {#if visibleDetails.length === 0}<p class="text-textMuted">No extra fields are filled in. Toggle empty fields to inspect them.</p>{/if}
        </div>
        <TaskEnhancements {task} disabled={loading} quick onApplied={(saved) => { task = saved; onApplied(saved); }} onBusy={(value) => { busy = value; }} />
      {/if}
    </div>
    <footer class="p-3 border-t border-border/60 gp-section-edge flex justify-between gap-2">
      <button type="button" class="gp-btn" disabled={!task || busy} onclick={() => { if (task) onOpenEditor(task); }}>Open full editor</button>

    </footer>
  </div>
</div>
