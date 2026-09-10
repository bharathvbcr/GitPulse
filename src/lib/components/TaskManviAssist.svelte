<script lang="ts">
  import { onMount } from "svelte";
  import { Sparkles } from "@lucide/svelte";
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
    type Task,
  } from "../workbench/client";
  import { acceptEnhancementInput, startQuickEnhance } from "../workbench/taskEnhance";
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
    autofocus?: boolean;
  } = $props();

  let configuration = $state<EnhancementConfiguration | null>(null);
  let configurationError = $state<string | null>(null);
  let proposal = $state<Enhancement | null>(null);
  let requested = $state<EnhancementField[]>(["title", "description"]);
  let selected = $state<EnhancementField[]>([]);
  let busy = $state(false);
  let error = $state("");
  let note = $state("");
  let now = $state(Date.now() / 1000);
  let disposed = false;
  let polling = false;
  let flash = $state<EnhancementField[]>([]);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  let notesEl: HTMLTextAreaElement | undefined = $state();
  const titleSuggestion = $derived(ready && proposal ? proposal.proposed.title : "");
  const descriptionSuggestion = $derived(ready && proposal ? proposal.proposed.description : "");
  const showTitleSuggestion = $derived(Boolean(ready && proposal?.fields.includes("title") && suggestionDiffers(title, titleSuggestion)));
  const showDescriptionSuggestion = $derived(Boolean(ready && proposal?.fields.includes("description") && suggestionDiffers(description, descriptionSuggestion)));

  const gate = $derived(canAskManvi({ title, description, repository_ids: repositoryIds }, notes));
  const manviGate = $derived(task ? canQuickEnhance(task, configuration, configurationError) : { ok: false as const, reason: configurationError ?? "Save a draft for Manvi to read." });
  const available = $derived(requested.filter((field) => !lockedFields.includes(field)));
  const ready = $derived(proposal?.state === "ready");
  const askLabel = $derived(busy ? "Asking Manvi…" : notes.trim() ? "Draft with Manvi" : "Improve with Manvi");
  const askDisabled = $derived(disabled || busy || available.length === 0 || Boolean(gate));

  function toggleField(field: EnhancementField, on: boolean) {
    if (busy || lockedFields.includes(field)) return;
    requested = on ? [...new Set([...requested, field])] : requested.filter((value) => value !== field);
  }

  onMount(() => {
    void loadConfig();
    if (autofocus) queueMicrotask(() => notesEl?.focus());
    const tick = window.setInterval(() => { now = Date.now() / 1000; }, 1000);
    return () => { disposed = true; window.clearInterval(tick); if (flashTimer) clearTimeout(flashTimer); };
  });

  $effect(() => { onBusy(busy); });
  $effect(() => {
    if (!proposal || !["pending", "running", "cancel_requested"].includes(proposal.state)) return;
    const id = proposal.id;
    const timer = window.setInterval(() => { void poll(id); }, 1000);
    return () => window.clearInterval(timer);
  });

  async function loadConfig() {
    try {
      const config = await enhancementConfiguration();
      if (disposed) return;
      configuration = config;
      configurationError = null;
    } catch (cause) {
      if (!disposed) configurationError = explainError(cause);
    }
  }

  async function poll(id: string) {
    if (polling || busy || disposed || !task) return;
    polling = true;
    try {
      const page = await listEnhancements(task.id);
      if (disposed || proposal?.id !== id) return;
      const summary = page.items.find((item) => item.id === id);
      if (summary && summary.revision !== proposal.revision) {
        proposal = await getEnhancement(id);
        if (proposal) selected = proposal.fields.filter((field) => !lockedFields.includes(field));
      }
    } catch (cause) {
      if (!disposed) error = `Status refresh failed: ${explainError(cause)}`;
    } finally {
      polling = false;
    }
  }

  async function generate() {
    if (busy || disabled) return;
    busy = true; error = ""; note = "";
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
      const started = await startQuickEnhance(saved, available, configuration);
      if (disposed) return;
      proposal = started.proposal;
      selected = started.proposal.fields.filter((field) => !lockedFields.includes(field));
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
      if (!disposed) busy = false;
    }
  }

  async function accept(fields: EnhancementField[]) {
    if (!task || !proposal || busy || disabled) return;
    const input = acceptEnhancementInput(proposal, task, fields, newID());
    if (!input) return;
    busy = true; error = "";
    try {
      proposal = await changeEnhancement("enhancements.accept", input);
      const saved = await getTask(task.id);
      if (disposed) return;
      onApplied(saved);
      note = fields.length === 1
        ? `Saved the suggested ${fields[0]}.`
        : "Saved the suggested title and description.";
      flash = [...fields];
      if (flashTimer) clearTimeout(flashTimer);
      flashTimer = setTimeout(() => { flash = []; }, 1600);
    } catch (cause) {
      if (!disposed) {
        error = explainError(cause);
        if (cause instanceof WorkbenchError && ["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) {
          note = "The result needs reconciliation. Retry accept if this stays uncertain.";
        }
      }
    } finally {
      if (!disposed) busy = false;
    }
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
      disabled={disabled || busy}
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
      disabled={busy || lockedFields.includes("title")}
      title={lockedFields.includes("title") ? "Title is locked against enhancement" : "Include title"}
      onclick={() => toggleField("title", !available.includes("title"))}
    >Title</button>
    <button
      type="button"
      class="gp-seg-btn"
      data-active={available.includes("description")}
      aria-pressed={available.includes("description")}
      disabled={busy || lockedFields.includes("description")}
      title={lockedFields.includes("description") ? "Description is locked against enhancement" : "Include description"}
      onclick={() => toggleField("description", !available.includes("description"))}
    >Description</button>
  </div>
  {#if configuration}<p class="meta">{configuration.provider} / {configuration.model}</p>{/if}
  {#if configurationError}<p class="warn">{configurationError}</p>{/if}
  {#if gate && (notes.trim() || title.trim() || task)}<p class="meta">{gate}</p>{/if}
  {#if task && !manviGate.ok && !gate}<p class="warn">{manviGate.reason}</p>{/if}
  <button
    type="button"
    class="gp-btn-primary ask"
    disabled={askDisabled}
    title={gate ?? (available.length === 0 ? "Title and description are locked" : undefined)}
    onclick={() => void generate()}
  >
    <Sparkles size={12} />
    {askLabel}
  </button>
  {#if askDisabled && !notes.trim() && !title.trim() && !task}
    <p class="meta">Type a few sentences, then draft. Or fill a title to improve an existing one.</p>
  {/if}
  {#if error}<p role="alert" class="error">{error}</p>{/if}
  {#if note}<p role="status" class="meta">{note}</p>{/if}
  {#if proposal && ["pending", "running", "cancel_requested"].includes(proposal.state)}
    <p class="meta" role="status">{proposal.state === "running" ? "Manvi is drafting…" : "Waiting to start."}{#if now >= proposal.expires_at} The deadline passed; termination is unconfirmed.{/if}</p>
  {/if}
  {#if proposal?.failure}<p class="error">{proposal.failure}</p>{/if}
  {#if proposal?.rationale && ready}<p class="meta">{proposal.rationale}</p>{/if}

  <label class:flash={flash.includes("title")}>Title
    <input class="gp-field" bind:value={title} required maxlength="300" placeholder="Or let Manvi draft this from your notes" />
  </label>
  {#if showTitleSuggestion}
    <div class="inline-suggestion">
      <p class="meta">Manvi title</p>
      <p class="suggestion-body suggested">{titleSuggestion}</p>
      <button type="button" class="gp-btn" disabled={busy || disabled} onclick={() => void accept(["title"])}>Use this title</button>
    </div>
  {/if}

  <label class:flash={flash.includes("description")}>Description
    <textarea class="gp-field" bind:value={description} rows="6" maxlength="65536" placeholder="Or let Manvi draft this from your notes"></textarea>
  </label>
  {#if showDescriptionSuggestion}
    <div class="inline-suggestion">
      <p class="meta">Manvi description</p>
      <pre class="suggestion-body suggested">{descriptionSuggestion}</pre>
      <button type="button" class="gp-btn" disabled={busy || disabled} onclick={() => void accept(["description"])}>Use this description</button>
    </div>
  {/if}
  {#if showTitleSuggestion || showDescriptionSuggestion}
    <div class="review-actions">
      {#if showTitleSuggestion && showDescriptionSuggestion}
        <button type="button" class="gp-btn-primary" disabled={busy || disabled} onclick={() => void accept(["title", "description"])}>Use both</button>
      {/if}
      <button type="button" class="gp-btn" disabled={busy} onclick={() => { proposal = null; note = "Suggestion hidden. It stays in Manvi history."; }}>Not now</button>
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
