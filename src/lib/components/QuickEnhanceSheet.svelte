<script lang="ts">
  import { onMount } from "svelte";
  import { Sparkles, X } from "@lucide/svelte";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { cardScale, backdropFade, backdropFadeOut } from "../ui/transitions";
  import { fade, scale } from "svelte/transition";
  import {
    changeEnhancement,
    enhancementConfiguration,
    explainError,
    getEnhancement,
    getTask,
    listEnhancements,
    newID,
    WorkbenchError,
    type Enhancement,
    type EnhancementConfiguration,
    type EnhancementField,
    type EnhancementSummary,
    type Task,
  } from "../workbench/client";
  import { canQuickEnhance, hiddenTaskDetails, visibleHiddenDetails } from "../workbench/taskOrganize";
  import { startQuickEnhance } from "../workbench/taskEnhance";
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
  let configuration = $state<EnhancementConfiguration | null>(null);
  let configurationError = $state<string | null>(null);
  let proposal = $state<Enhancement | null>(null);
  let entries = $state<EnhancementSummary[]>([]);
  let requested = $state<EnhancementField[]>(["title", "description"]);
  let selected = $state<EnhancementField[]>([]);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state("");
  let note = $state("");
  let now = $state(Date.now() / 1000);
  let showEmpty = $state(false);
  let disposed = false;
  let polling = false;

  const details = $derived(task ? hiddenTaskDetails(task, repoName) : []);
  const visibleDetails = $derived(visibleHiddenDetails(details, showEmpty));
  const gate = $derived(task ? canQuickEnhance(task, configuration, configurationError) : { ok: false as const, reason: "Loading task…" });
  const available = $derived.by(() => {
    const current = task;
    if (!current) return [];
    return requested.filter((field) => !(current.locked_fields ?? []).includes(field));
  });

  onMount(() => {
    void load();
    const tick = window.setInterval(() => { now = Date.now() / 1000; }, 1000);
    return () => { disposed = true; window.clearInterval(tick); };
  });

  $effect(() => {
    if (!proposal || !["pending", "running", "cancel_requested"].includes(proposal.state)) return;
    const id = proposal.id;
    const timer = window.setInterval(() => { void poll(id); }, 1000);
    return () => window.clearInterval(timer);
  });

  async function load() {
    loading = true; error = "";
    try {
      const [loaded, config] = await Promise.all([
        getTask(taskId),
        enhancementConfiguration().catch((cause) => {
          configurationError = explainError(cause);
          return null;
        }),
      ]);
      if (disposed) return;
      task = loaded;
      if (config) { configuration = config; configurationError = null; }
      requested = (["title", "description"] as EnhancementField[]).filter((field) => !(loaded.locked_fields ?? []).includes(field));
      const page = await listEnhancements(taskId);
      if (disposed) return;
      entries = page.items;
      if (page.items[0]) {
        proposal = await getEnhancement(page.items[0].id);
        if (proposal) selected = proposal.fields.filter((field) => !(loaded.locked_fields ?? []).includes(field));
      }
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      if (!disposed) loading = false;
    }
  }

  async function poll(id: string) {
    if (polling || busy || disposed) return;
    polling = true;
    try {
      const page = await listEnhancements(taskId);
      if (disposed || proposal?.id !== id) return;
      entries = page.items;
      const summary = page.items.find((item) => item.id === id);
      if (summary && summary.revision !== proposal.revision) {
        proposal = await getEnhancement(id);
        const current = task;
        if (proposal && current) selected = proposal.fields.filter((field) => !(current.locked_fields ?? []).includes(field));
      }
    } catch (cause) {
      if (!disposed) error = `Status refresh failed: ${explainError(cause)}`;
    } finally {
      polling = false;
    }
  }

  async function generate() {
    const current = task;
    if (!current || !configuration || busy || !gate.ok) return;
    busy = true; error = ""; note = "";
    try {
      const started = await startQuickEnhance(current, available, configuration);
      if (disposed) return;
      proposal = started.proposal;
      selected = started.proposal.fields.filter((field) => !(current.locked_fields ?? []).includes(field));
      note = started.proposal.state === "ready" ? "Ready for review" : "Generating";
      const page = await listEnhancements(taskId);
      if (!disposed) entries = page.items;
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      if (!disposed) busy = false;
    }
  }

  async function accept() {
    if (!task || !proposal || proposal.state !== "ready" || !selected.length || busy) return;
    busy = true; error = "";
    const input = { id: proposal.id, request_id: newID(), expected_revision: proposal.revision, expected_task_revision: task.revision, fields: [...selected] };
    try {
      proposal = await changeEnhancement("enhancements.accept", input);
      const saved = await getTask(task.id);
      if (disposed) return;
      task = saved;
      onApplied(saved);
      note = "Accepted into the saved task.";
    } catch (cause) {
      if (!disposed) {
        error = explainError(cause);
        if (cause instanceof WorkbenchError && ["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) {
          note = "The result needs reconciliation. Retry accept from the full editor if this stays uncertain.";
        }
      }
    } finally {
      if (!disposed) busy = false;
    }
  }

  async function dismiss() {
    if (!proposal || busy) return;
    busy = true; error = "";
    try {
      proposal = await changeEnhancement("enhancements.dismiss", { id: proposal.id, request_id: newID(), expected_revision: proposal.revision });
      note = proposal.state === "running" || proposal.state === "cancel_requested" ? "Cancellation requested." : "Dismissed.";
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      if (!disposed) busy = false;
    }
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
        <div class="space-y-2">
          <h3 class="text-[11px] font-semibold uppercase tracking-wide text-textMuted">Manvi</h3>
          {#if !gate.ok}<p class="text-textMuted">{gate.reason}</p>{/if}
          {#if configuration}<p class="text-textMuted">{configuration.provider} / {configuration.model}</p>{/if}
          {#each ["title", "description"] as field}
            {@const target: EnhancementField = field === "title" ? "title" : "description"}
            {@const locked = (task.locked_fields ?? []).includes(target)}
            <SettingToggle
              label={`Rewrite ${field}`}
              description={locked ? "Locked on this task. Unlock it in the editor to include it." : `Manvi may replace the saved ${field}.`}
              checked={available.includes(target)}
              ariaLabel={`Rewrite ${field}`}
              onchange={(next) => {
                if (busy || locked) return;
                requested = next ? [...new Set([...requested, target])] : requested.filter((value) => value !== target);
              }}
            />
          {/each}
          <button type="button" class="gp-btn-primary" disabled={busy || !gate.ok || available.length === 0} onclick={() => void generate()}>
            {busy ? "Working…" : "Generate suggestion"}
          </button>
        </div>
        {#if proposal}
          <article class="space-y-2" aria-label="Enhancement review">
            <h3 class="font-semibold">{proposal.state.replace("_", " ")}</h3>
            {#if proposal.failure}<p class="text-rose-400">{proposal.failure}</p>{/if}
            {#if proposal.rationale}<p class="text-textMuted leading-relaxed">{proposal.rationale}</p>{/if}
            {#if proposal.state === "ready"}
              {#each proposal.fields as field}
                <SettingToggle
                  label={field === "title" ? "Accept title" : "Accept description"}
                  checked={selected.includes(field)}
                  onchange={(next) => {
                    if (busy) return;
                    selected = next ? [...new Set([...selected, field])] : selected.filter((value) => value !== field);
                  }}
                />
                <p class="text-[11px] text-textMuted">Original</p>
                <pre class="whitespace-pre-wrap wrap-break-word max-h-28 overflow-auto rounded-lg border border-border/60 bg-background/50 px-2 py-1.5">{proposal.source[field]}</pre>
                <p class="text-[11px] text-textMuted">Suggestion</p>
                <pre class="whitespace-pre-wrap wrap-break-word max-h-28 overflow-auto rounded-lg border border-accent/40 bg-background/50 px-2 py-1.5">{proposal.proposed[field]}</pre>
              {/each}
              <div class="flex flex-wrap gap-2">
                <button type="button" class="gp-btn-primary" disabled={busy || !selected.length} onclick={() => void accept()}>Accept selected</button>
                <button type="button" class="gp-btn" disabled={busy} onclick={() => void dismiss()}>Dismiss</button>
              </div>
            {:else if ["pending", "running", "failed"].includes(proposal.state)}
              <button type="button" class="gp-btn" disabled={busy} onclick={() => void dismiss()}>{proposal.state === "running" ? "Request cancellation" : "Dismiss"}</button>
              {#if ["running", "cancel_requested"].includes(proposal.state) && now >= proposal.expires_at}
                <p class="text-textMuted">The deadline passed. Provider termination has not been confirmed.</p>
              {/if}
            {/if}
          </article>
        {:else if !loading}
          <p class="text-textMuted">No suggestions for this task yet.</p>
        {/if}
        {#if note}<p role="status" class="text-textMuted">{note}</p>{/if}
      {/if}
    </div>
    <footer class="p-3 border-t border-border/60 gp-section-edge flex justify-between gap-2">
      <button type="button" class="gp-btn" disabled={!task || busy} onclick={() => { if (task) onOpenEditor(task); }}>Open full editor</button>
      <span class="text-[11px] text-textMuted self-center">{entries.length ? `${entries.length} saved suggestion${entries.length === 1 ? "" : "s"}` : ""}</span>
    </footer>
  </div>
</div>
