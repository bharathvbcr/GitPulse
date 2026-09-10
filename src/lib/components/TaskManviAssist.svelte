<script lang="ts">
  import { onMount } from "svelte";
  import { Sparkles } from "@lucide/svelte";
  import {
    enhancementConfiguration,
    explainError,
    getEnhancement,
    listEnhancements,
    newID,
    type Enhancement,
    type EnhancementConfiguration,
    type EnhancementField,
    type Task,
  } from "../workbench/client";
  import { acceptEnhancementInput, startQuickEnhance, EnhancementAction, liveEnhancement } from "../workbench/taskEnhance";
  import { bounded } from "../workbench/taskActions";
  import { canAskManvi, suggestionDiffers } from "../workbench/taskCompose";
  import { canQuickEnhance } from "../workbench/taskOrganize";

  let {
    task,
    notes,
    onNotes,
    title = $bindable(""),
    description = $bindable(""),
    repositoryIds,
    lockedFields = [],
    prepareTask,
    onApplied,
    onBusy,
    disabled = false,
    dirty = false,
    active = true,
    autofocus = false,
  }: {
    task: Task | null;
    notes: string;
    onNotes: (value: string) => void;
    title: string;
    description: string;
    repositoryIds: readonly string[];
    lockedFields?: readonly EnhancementField[];
    prepareTask: () => Promise<Task | null>;
    onApplied: (task: Task) => void;
    onBusy: (busy: boolean) => void;
    disabled?: boolean;
    dirty?: boolean;
    active?: boolean;
    autofocus?: boolean;
  } = $props();

  let configuration = $state<EnhancementConfiguration | null>(null);
  let configurationError = $state<string | null>(null);
  let proposal = $state<Enhancement | null>(null);
  let requested = $state<EnhancementField[]>(["title", "description"]);
  let busy = $state(false);
  let error = $state("");
  let note = $state("");
  let now = $state(Date.now() / 1000);
  let disposed = false;
  let polling = false;
  let epoch = 0;
  const action = new EnhancementAction();
  let needsReconcile = $state(false);
  const acting = $derived(busy || needsReconcile);
  const liveAttempt = $derived(liveEnhancement(proposal));
  const stale = $derived(Boolean(proposal && task && proposal.source_revision !== task.revision));
  const acceptDisabled = $derived(acting || disabled || dirty || stale);
  let flash = $state<EnhancementField[]>([]);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  let notesEl: HTMLTextAreaElement | undefined = $state();
  const ready = $derived(proposal?.state === "ready");
  const titleSuggestion = $derived(ready && proposal ? proposal.proposed.title : "");
  const descriptionSuggestion = $derived(ready && proposal ? proposal.proposed.description : "");
  const showTitleSuggestion = $derived(Boolean(ready && proposal?.fields.includes("title") && suggestionDiffers(title, titleSuggestion)));
  const showDescriptionSuggestion = $derived(Boolean(ready && proposal?.fields.includes("description") && suggestionDiffers(description, descriptionSuggestion)));

  const gate = $derived(canAskManvi({ title, description, repository_ids: repositoryIds }, notes));
  const manviGate = $derived(task ? canQuickEnhance(task, configuration, configurationError) : { ok: false as const, reason: configurationError ?? "Save a draft for Manvi to read." });
  const available = $derived(requested.filter((field) => !lockedFields.includes(field)));
  const askLabel = $derived(busy ? "Asking Manvi…" : notes.trim() ? "Draft with Manvi" : "Improve with Manvi");
  const manviReady = $derived(Boolean(configuration?.provider.trim() && configuration?.model.trim()) && !configurationError);
  let configPending = $state(false);
  const fieldReason = $derived(
    lockedFields.includes("title") && lockedFields.includes("description")
      ? "Title and description are locked"
      : available.length === 0
        ? "Choose title, description, or both"
        : !manviReady
          ? (configurationError ?? "Manvi has no provider and model selected.")
          : undefined,
  );
  const askDisabled = $derived(disabled || acting || liveAttempt || configPending || available.length === 0 || Boolean(gate) || !manviReady);

  function toggleField(field: EnhancementField, on: boolean) {
    if (acting || disabled || lockedFields.includes(field)) return;
    requested = on ? [...new Set([...requested, field])] : requested.filter((value) => value !== field);
  }

  onMount(() => {
    void loadConfig();
    if (autofocus) queueMicrotask(() => notesEl?.focus());
    const tick = window.setInterval(() => { now = Date.now() / 1000; }, 1000);
    return () => { disposed = true; window.clearInterval(tick); if (flashTimer) clearTimeout(flashTimer); };
  });

  $effect(() => { onBusy(acting); });
  $effect(() => {
    if (!active || !proposal || !liveEnhancement(proposal)) return;
    const id = proposal.id;
    const timer = window.setInterval(() => { void poll(id); }, 1000);
    return () => window.clearInterval(timer);
  });

  async function loadConfig() {
    if (configPending || acting || disabled) return;
    configPending = true;
    try {
      const config = await bounded(enhancementConfiguration());
      if (disposed) return;
      configuration = config;
      configurationError = null;
    } catch (cause) {
      if (!disposed) configurationError = explainError(cause);
    } finally { if (!disposed) configPending = false; }
  }

  async function poll(id: string) {
    if (polling || acting || disposed || !active || document.visibilityState === "hidden") return;
    polling = true;
    const ticket = epoch;
    try {
      const next = await bounded(getEnhancement(id));
      if (disposed || ticket !== epoch || acting || proposal?.id !== id || next.revision < proposal.revision) return;
      if (next.id !== id || next.task_id !== task?.id) throw new Error("Suggestion does not belong to this task.");
      proposal = next;
      error = "";
    } catch (cause) {
      if (!disposed && ticket === epoch) error = `Status refresh failed: ${explainError(cause)}`;
    } finally { polling = false; }
  }

  async function retry() {
    const pending = action.pending;
    if (!pending || busy || disabled) return;
    busy = true; epoch++; error = "";
    try {
      const result = await action.run(pending.method, pending.input, pending.taskID);
      if (disposed) return;
      proposal = result.proposal;
      if (result.task) onApplied(result.task);
      note = "Enhancement action confirmed.";
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) { needsReconcile = action.pending !== null; busy = false; } }
  }

  async function generate() {
    if (askDisabled) return;
    busy = true; epoch++; error = ""; note = "";
    try {
      const saved = await prepareTask();
      if (disposed) return;
      if (!saved) {
        if (!error) error = gate ?? "Could not save a draft for Manvi.";
        return;
      }
      if (!configuration) {
        error = configurationError ?? "Manvi configuration has not been loaded.";
        return;
      }
      // Inspect saved history before spending another model call, including
      // proposals started from another enhancement surface.
      const page = await bounded(listEnhancements(saved.id));
      if (disposed) return;
      const existing = page.items.find(liveEnhancement);
      if (existing) {
        const current = await bounded(getEnhancement(existing.id));
        if (current.id !== existing.id || current.task_id !== saved.id) throw new Error("Suggestion does not belong to this task.");
        if (!disposed) { proposal = current; note = "A suggestion is already in progress."; }
        return;
      }
      const started = await startQuickEnhance(saved, available, configuration, action);
      if (disposed) return;
      proposal = started.proposal;
      const changed = started.proposal.state === "ready" && (
        suggestionDiffers(title, started.proposal.proposed.title) ||
        suggestionDiffers(description, started.proposal.proposed.description)
      );
      note = started.proposal.state === "ready"
        ? (changed ? "Suggestions ready under the fields they change." : "Manvi kept your wording.")
        : "Manvi is drafting title and description.";
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      if (!disposed) { needsReconcile = action.pending !== null; busy = false; }
    }
  }

  async function accept(fields: EnhancementField[]) {
    if (!task || !proposal || acceptDisabled) return;
    const input = acceptEnhancementInput(proposal, task, fields, newID());
    if (!input) { error = "Save or reload the task, then request a fresh suggestion."; return; }
    busy = true; epoch++; error = "";
    try {
      const result = await action.run("enhancements.accept", input, task.id);
      if (disposed) return;
      proposal = result.proposal;
      if (result.task) onApplied(result.task);
      note = fields.length === 1 ? `Saved the suggested ${fields[0]}.` : "Saved the suggested title and description.";
      flash = [...fields];
      if (flashTimer) clearTimeout(flashTimer);
      flashTimer = setTimeout(() => { flash = []; }, 1600);
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) { needsReconcile = action.pending !== null; busy = false; } }
  }

</script>

<section class="manvi-assist" aria-label="Manvi task assist">
  <div class="assist-head">
    <Sparkles size={13} class="text-accent shrink-0" />
    <div class="min-w-0">
      <p class="assist-title">What do you need?</p>
      <p class="assist-hint">Dump the goal and evidence here. Manvi turns it into a title and description you accept below.</p>
    </div>
  </div>
  <label class="notes-label">
    <span class="sr-only">What do you need?</span>
    <textarea
      bind:this={notesEl}
      class="gp-field"
      value={notes}
      maxlength="65536"
      rows="4"
      placeholder="Keep the original E42 across both repository links, and say how to reproduce it."
      disabled={disabled || acting}
      oninput={(event) => onNotes(event.currentTarget.value)}
      onkeydown={(event) => {
        if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
          event.preventDefault();
          void generate();
        }
      }}
    ></textarea>
  </label>
  <div class="gp-segmented field-picks" role="group" aria-label="Fields Manvi may change">
    <button
      type="button"
      class="gp-seg-btn"
      data-active={available.includes("title")}
      aria-pressed={available.includes("title")}
      disabled={disabled || acting || lockedFields.includes("title")}
      title={lockedFields.includes("title") ? "Title is locked against enhancement" : "Include title"}
      onclick={() => toggleField("title", !available.includes("title"))}
    >Title</button>
    <button
      type="button"
      class="gp-seg-btn"
      data-active={available.includes("description")}
      aria-pressed={available.includes("description")}
      disabled={disabled || acting || lockedFields.includes("description")}
      title={lockedFields.includes("description") ? "Description is locked against enhancement" : "Include description"}
      onclick={() => toggleField("description", !available.includes("description"))}
    >Description</button>
  </div>
  <details class="model-settings" open={!manviReady && !configPending}>
    <summary>Task model settings</summary>
    {#if configuration}
      <label>Provider<select class="gp-field" bind:value={configuration.provider} disabled={disabled || acting || configPending}>{#each configuration.providers as provider}<option value={provider}>{provider}</option>{/each}</select></label>
      <label>Model<input class="gp-field" bind:value={configuration.model} disabled={disabled || acting || configPending} maxlength="512" placeholder="Model name served by this provider" /></label>
      <p class="meta">Task suggestions use Manvi’s provider configuration. Choose a model here when none is configured.</p>
    {/if}
    <button type="button" class="gp-btn" disabled={disabled || acting || configPending} onclick={() => void loadConfig()}>{configPending ? "Loading Manvi configuration…" : "Reload Manvi configuration"}</button>
  </details>
  {#if configuration?.provider.trim() && configuration?.model.trim()}<p class="meta">{configuration.provider} / {configuration.model}</p>{/if}
  {#if configurationError}<p class="warn">{configurationError}</p>{/if}
  {#if !manviReady && !configPending && !configurationError}<p class="warn">Manvi has no provider and model selected.</p>{/if}
  {#if gate && (notes.trim() || title.trim() || task)}<p class="meta">{gate}</p>{/if}
  {#if task && !manviGate.ok && manviReady && !gate}<p class="warn">{manviGate.reason}</p>{/if}
  <button
    type="button"
    class="gp-btn-primary ask"
    disabled={askDisabled}
    title={gate ?? fieldReason}
    onclick={() => void generate()}
  >
    <Sparkles size={12} />
    {askLabel}
  </button>
  {#if askDisabled && !notes.trim() && !title.trim() && !task}
    <p class="meta">Type a few sentences, then draft. Or fill a title to improve an existing one.</p>
  {/if}
  {#if needsReconcile}<p class="warn">The result is uncertain. Retry the same action before editing or closing.</p><button type="button" class="gp-btn" disabled={busy || disabled} onclick={() => void retry()}>Retry pending action</button>{/if}
  {#if ready && (dirty || stale)}<p class="warn">{dirty ? "Save or reload your edits before accepting a suggestion." : "This suggestion is for an older task revision. Request a fresh suggestion."}</p>{/if}
  {#if error}<p role="alert" class="error">{error}</p>{/if}
  {#if note}<p role="status" class="meta">{note}</p>{/if}
  {#if proposal && ["pending", "running", "cancel_requested"].includes(proposal.state)}
    <p class="meta" role="status">{proposal.state === "running" ? "Manvi is drafting…" : "Waiting to start."}{#if now >= proposal.expires_at} The deadline passed; termination is unconfirmed.{/if}</p>
  {/if}
  {#if proposal?.failure}<p class="error">{proposal.failure}</p>{/if}
  {#if proposal?.rationale && ready}<p class="meta">{proposal.rationale}</p>{/if}

  <label class:flash={flash.includes("title")}>Title
    <input class="gp-field" name="task-title" bind:value={title} disabled={disabled || acting} required maxlength="300" placeholder="Or let Manvi draft this from your notes" />
  </label>
  {#if showTitleSuggestion}
    <div class="inline-suggestion">
      <p class="meta">Manvi title</p>
      <p class="suggestion-body suggested">{titleSuggestion}</p>
      <button type="button" class="gp-btn" disabled={acceptDisabled} onclick={() => void accept(["title"])}>Use this title</button>
    </div>
  {/if}

  <label class:flash={flash.includes("description")}>Description
    <textarea class="gp-field" bind:value={description} disabled={disabled || acting} rows="6" maxlength="65536" placeholder="Or let Manvi draft this from your notes"></textarea>
  </label>
  {#if showDescriptionSuggestion}
    <div class="inline-suggestion">
      <p class="meta">Manvi description</p>
      <pre class="suggestion-body suggested">{descriptionSuggestion}</pre>
      <button type="button" class="gp-btn" disabled={acceptDisabled} onclick={() => void accept(["description"])}>Use this description</button>
    </div>
  {/if}
  {#if showTitleSuggestion || showDescriptionSuggestion}
    <div class="review-actions">
      {#if showTitleSuggestion && showDescriptionSuggestion}
        <button type="button" class="gp-btn-primary" disabled={acceptDisabled} onclick={() => void accept(["title", "description"])}>Use both</button>
      {/if}
      <button type="button" class="gp-btn" disabled={acting || disabled} onclick={() => { epoch++; proposal = null; note = "Suggestion hidden. It stays in Manvi history."; }}>Not now</button>
    </div>
  {/if}
</section>

<style>
  .manvi-assist{margin:0 0 16px;padding:12px;border:1px solid rgb(var(--c-border) / 0.65);border-radius:12px}
  .assist-head{display:flex;gap:8px;align-items:flex-start;margin-bottom:8px}
  .assist-title{margin:0;font-size:12px;font-weight:650}
  .assist-hint,.meta{margin:4px 0 0;font-size:11px;color:rgb(var(--c-text-muted));line-height:1.45}
  .notes-label{display:block;margin:8px 0}
  textarea,pre.suggestion-body{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}
  textarea{resize:vertical}
  .field-picks{margin:0 0 8px}
  .ask{margin-top:8px}
  label{display:flex;flex-direction:column;gap:6px;margin:12px 0 8px;font-size:12px}
  input,textarea{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}
  .inline-suggestion{margin:-4px 0 12px;padding:8px;border:1px solid rgb(var(--c-accent) / 0.4);border-radius:8px}
  .suggestion-body{margin:4px 0 8px;white-space:pre-wrap;overflow-wrap:anywhere;max-height:7rem;overflow:auto;font:12px/1.45 inherit}
  .suggested{border:1px solid rgb(var(--c-accent) / 0.45);border-radius:7px;padding:8px;background:rgb(var(--c-bg) / 0.45)}
  .review-actions{display:flex;flex-wrap:wrap;gap:6px;margin-bottom:4px}
  .flash :is(input,textarea){outline:2px solid rgb(var(--c-accent));outline-offset:1px}
  .error{color:#dc6565}
  .warn{color:#d4a017;font-size:11px;margin:4px 0}
</style>
