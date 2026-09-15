<script lang="ts">
  import { onMount } from "svelte";
  import { Sparkles, X } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { cardScale, backdropFade, backdropFadeOut } from "../ui/transitions";
  import { fade, scale } from "svelte/transition";
  import { getTask, explainError, type EnhancementField, type Task } from "../workbench/client";
  import { hiddenTaskDetails, visibleHiddenDetails } from "../workbench/taskOrganize";
  import { bounded } from "../workbench/taskActions";
  import TaskManviAssist from "./TaskManviAssist.svelte";
  import SettingToggle from "./SettingToggle.svelte";

  let {
    taskId,
    repoName,
    startRequest = 0,
    onClose,
    onApplied,
    onOpenEditor,
  }: {
    taskId: string;
    repoName: (id: string) => string | undefined;
    /**
     * Bumped by the board to start a draft as soon as this opens.
     *
     * Quick add's drafting mode lands here: the task is already saved and on
     * the board, and this sheet is where its suggestion is reviewed. Zero
     * means the reader opened the sheet themselves and nothing runs until
     * they ask.
     */
    startRequest?: number;
    onClose: () => void;
    onApplied: (task: Task) => void;
    onOpenEditor: (task: Task) => void;
  } = $props();

  let task = $state<Task | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state("");
  let showEmpty = $state(false);
  let title = $state("");
  let description = $state("");
  let lockedFields = $state<EnhancementField[]>([]);
  let disposed = false;
  const details = $derived(task ? hiddenTaskDetails(task, repoName) : []);
  const visibleDetails = $derived(visibleHiddenDetails(details, showEmpty));
  onMount(() => { void load(); return () => { disposed = true; }; });
  async function load() {
    loading = true; error = "";
    try {
      const loaded = await bounded(getTask(taskId));
      if (!disposed) {
        task = loaded;
        title = loaded.title;
        description = loaded.description;
        lockedFields = [...(loaded.locked_fields ?? [])];
      }
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

<!--
  Portaled like the other two task dialogs. This was the one that was not, and
  a right-edge drawer rendered in place is one `contain:paint` ancestor away
  from being clipped by a pane it is supposed to float over.
-->
<div
  use:portal={"body"}
  class="gp-scrim bg-black/40 flex justify-end gp-gpu"
  style="z-index: {LAYERS.MODAL}"
  role="presentation"
  onclick={(e) => { if (e.target === e.currentTarget && !busy) onClose(); }}
  onkeydown={onKey}
  in:fade={backdropFade()}
  out:fade={backdropFadeOut()}
>
  <!-- Same material, width token and header/footer rhythm as the task sheet
       it sits beside: these two are read one after the other, and a drawer
       that looks like a different application is the tell that they were
       styled by two people. -->
  <div
    use:trapFocus={{ autofocus: true }}
    in:scale={cardScale()}
    class="gp-card gp-glass shadow-float h-full flex flex-col overflow-hidden text-xs text-textPrimary gp-gpu"
    style="width: min(var(--gp-task-sheet-w), 96vw)"
    role="dialog"
    aria-modal="true"
    aria-labelledby="quick-enhance-title"
  >
    <header class="px-[18px] pt-4 pb-2.5 border-b border-border/60 gp-section-edge flex items-start justify-between gap-3">
      <div class="min-w-0">
        <p id="quick-enhance-title" class="text-sm font-semibold flex items-center gap-1.5">
          <Sparkles size={14} class="text-accent shrink-0" />
          Quick Enhance
        </p>
        <p class="text-[11px] text-textMuted mt-1 leading-relaxed">
          {startRequest > 0
            ? "Task saved. A title and description are being drafted for you to accept or reject."
            : "Review hidden fields, then let Manvi rewrite title and description."}
        </p>
      </div>
      <button type="button" class="gp-icon-btn" aria-label="Close Quick Enhance" onclick={onClose} disabled={busy}><X size={14} /></button>
    </header>

    <div class="flex-1 overflow-auto px-[18px] py-4 space-y-4">
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
        <TaskManviAssist
          {task}
          {startRequest}
          bind:title
          bind:description
          bind:lockedFields
          repositoryIds={task.repository_ids}
          disabled={loading}
          quick
          onApplied={(saved) => { task = saved; title = saved.title; description = saved.description; lockedFields = [...(saved.locked_fields ?? [])]; onApplied(saved); }}
          onBusy={(value) => { busy = value; }}
        />
      {/if}
    </div>
    <footer class="px-[18px] pt-2.5 pb-4 border-t border-border/45 flex justify-between gap-2">
      <button type="button" class="gp-btn" disabled={!task || busy} onclick={() => { if (task) onOpenEditor(task); }}>Open full editor</button>
    </footer>
  </div>
</div>
