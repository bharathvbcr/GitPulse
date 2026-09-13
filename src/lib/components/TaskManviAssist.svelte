<script lang="ts">
  /**
   * One Manvi section for the task sheet and Quick Enhance.
   * Model choice lives in Local model servers; this surface only summarizes it.
   */
  import { onMount, untrack } from "svelte";
  import { Sparkles } from "@lucide/svelte";
  import { get } from "svelte/store";
  import {
    automaticUpdates,
    enhancementConfiguration,
    explainError,
    getEnhancement,
    listEnhancements,
    newID,
    type Enhancement,
    type EnhancementConfiguration,
    type EnhancementField,
    type EnhancementMutation,
    type EnhancementSummary,
    type Task,
  } from "../workbench/client";
  import { acceptEnhancementInput, assistEngineName, runAppleEnhancement, startQuickEnhance, DEFAULT_ASSIST_ENGINE, EnhancementAction, liveEnhancement, type AssistEngine } from "../workbench/taskEnhance";
  import {
    appleBadge,
    appleContext,
    appleGate,
    appleIntelligenceStatus,
    appleReady,
    type AppleIntelligenceStatus,
  } from "../ai/appleIntelligence";
  import { bounded } from "../workbench/taskActions";
  import { canAskManvi, suggestionDiffers } from "../workbench/taskCompose";
  import { canQuickEnhance } from "../workbench/taskOrganize";
  import {
    describeSelection,
    effectiveSelection,
    explainEnhancementFailure,
  } from "../workbench/taskModel";
  import { harnessStore } from "../stores/harnessStore";
  import { requestManviFocus } from "../ui/manviFocus";
  import { askConfirm } from "../stores/modalStore";
  import SettingToggle from "./SettingToggle.svelte";

  let {
    task,
    notes = "",
    onNotes = (_value: string) => {},
    title = $bindable(""),
    description = $bindable(""),
    repositoryIds = [],
    repositoryNames = [],
    lockedFields = $bindable<EnhancementField[]>([]),
    prepareTask = async (): Promise<Task | null> => task,
    onApplied,
    onBusy,
    disabled = false,
    dirty = false,
    active = true,
    autofocus = false,
    quick = false,
    startRequest = 0,
    onSuggestion = (_state: AssistSuggestion) => {},
    onCount = (_total: number) => {},
    onEngine = (_name: string) => {},
  }: {
    task: Task | null;
    notes?: string;
    onNotes?: (value: string) => void;
    title?: string;
    description?: string;
    repositoryIds?: readonly string[];
    /**
     * Repository names, for the on-device model's context line.
     *
     * Names, never ids: "GitPulse" tells a model something, and "repo-0" is
     * noise that a model asked to be specific will happily build a sentence
     * around. A caller that has no names passes none and the line is omitted.
     */
    repositoryNames?: readonly string[];
    lockedFields?: EnhancementField[];
    prepareTask?: () => Promise<Task | null>;
    onApplied: (task: Task) => void;
    onBusy: (busy: boolean) => void;
    disabled?: boolean;
    dirty?: boolean;
    active?: boolean;
    autofocus?: boolean;
    quick?: boolean;
    startRequest?: number;
    /**
     * Reports the reviewable suggestion outward.
     *
     * The title and description inputs live in the task sheet, above this
     * section, because that is where a reader writes them. This component
     * still owns the enhancement lifecycle — creating, polling, accepting,
     * undoing — and hands the sheet just enough to draw the two "use this"
     * affordances beside the fields they would change.
     */
    onSuggestion?: (state: AssistSuggestion) => void;
    /** How many proposals this task has, for the sheet's tab badge. */
    onCount?: (total: number) => void;
    /**
     * Which engine the ask button would actually use, by display name.
     *
     * The picker lives here, but the sheet around this section writes about
     * the same engine — placeholders, and the message that refuses a shortcut
     * mid-generation. It reads the name from here rather than deriving its
     * own, so the two cannot disagree about which one is running.
     */
    onEngine?: (name: string) => void;
  } = $props();

  /** What the sheet needs to render an inline suggestion beside a field. */
  export interface AssistSuggestion {
    title: string;
    description: string;
    /** True when a suggestion is ready and differs from what is typed. */
    showTitle: boolean;
    showDescription: boolean;
    /** Why accepting is refused right now, or "" when it is allowed. */
    blocked: string;
    /** Fields just accepted, for the sheet to flash. */
    flash: EnhancementField[];
  }

  /** Accept one or both suggested fields. Called by the sheet's buttons. */
  export function acceptFields(fields: EnhancementField[]) {
    void accept(fields);
  }

  /** Hide the current suggestion without dismissing it in Manvi's history. */
  export function hideSuggestion() {
    epoch++;
    proposal = null;
    note = "Suggestion hidden. It stays in Manvi history.";
  }

  const labels: Record<Enhancement["state"], string> = {
    pending: "Waiting to start", running: "Generating", cancel_requested: "Cancellation requested",
    ready: "Ready for review", failed: "Generation failed", cancelled: "Cancelled",
    interrupted: "Outcome uncertain", dismissed: "Dismissed", accepted: "Accepted", undone: "Undone",
  };

  let historyOpen = $state(false);
  $effect(() => { if (quick) historyOpen = true; });
  let configuration = $state<EnhancementConfiguration | null>(null);
  let configurationError = $state<string | null>(null);
  let proposal = $state<Enhancement | null>(null);
  let requested = $state<EnhancementField[]>(["title", "description"]);
  let selected = $state<EnhancementField[]>([]);
  let entries = $state<EnhancementSummary[]>([]);
  let total = $state(0);
  let cursor = $state<string | null>(null);
  let historyLoading = $state(false);
  let busy = $state(false);
  let preparing = $state(false);
  let error = $state("");
  let note = $state("");
  let now = $state(Date.now() / 1000);
  let disposed = false;
  let polling = false;
  let epoch = 0;
  let lastStart = 0;
  let editing = $state(false);
  let chooseFields = $state(false);
  let revisionDraft = $state<Partial<Record<EnhancementField, string>>>({});
  let selecting = 0;
  let needsReconcile = $state(false);
  let configPending = $state(false);
  let flash = $state<EnhancementField[]>([]);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  let notesEl: HTMLTextAreaElement | undefined = $state();
  let visible = $state(true);
  /**
   * Which engine writes the text.
   *
   * Not a preference that survives the sheet: the two engines fail in
   * different ways and on different machines, and a remembered choice would
   * mean a reader who once tried Apple Intelligence on a Mac that later turned
   * it off finds a disabled button with no idea why. `engine` falls back to
   * Manvi the moment Apple stops being available.
   */
  let requestedEngine = $state<AssistEngine>(DEFAULT_ASSIST_ENGINE);
  let apple = $state<AppleIntelligenceStatus | null>(null);

  // Re-subscribe so preferred / ai.selected updates re-render.
  let harnessTick = $state(0);
  const liveSelection = $derived.by(() => {
    harnessTick;
    return effectiveSelection(get(harnessStore));
  });
  const action = new EnhancementAction(undefined, () => liveSelection);
  const acting = $derived(busy || needsReconcile || preparing);
  const controlsLocked = $derived(acting || editing);
  const liveAttempt = $derived(liveEnhancement(proposal) || entries.some((entry) => liveEnhancement(entry)));
  const stale = $derived(Boolean(proposal && task && proposal.source_revision !== task.revision));
  const acceptDisabled = $derived(acting || disabled || dirty || stale);
  const ready = $derived(proposal?.state === "ready");
  const titleSuggestion = $derived(ready && proposal ? proposal.proposed.title ?? "" : "");
  const descriptionSuggestion = $derived(ready && proposal ? proposal.proposed.description ?? "" : "");
  const showTitleSuggestion = $derived(Boolean(!quick && ready && proposal?.fields.includes("title") && suggestionDiffers(title, titleSuggestion)));
  const showDescriptionSuggestion = $derived(Boolean(!quick && ready && proposal?.fields.includes("description") && suggestionDiffers(description, descriptionSuggestion)));
  const gate = $derived(canAskManvi({ title, description, repository_ids: repositoryIds }, notes));
  const manviGate = $derived(canQuickEnhance({ locked_fields: lockedFields }, configuration, configurationError));
  const available = $derived(requested.filter((field) => !lockedFields.includes(field)));
  // Offered only where it could ever work: a build with the bridge linked in.
  // A picker whose second option is permanently "not in this build" teaches
  // the reader to ignore the picker.
  const appleOffered = $derived(Boolean(apple?.compiled));
  const engine = $derived<AssistEngine>(requestedEngine === "apple" && appleReady(apple) ? "apple" : DEFAULT_ASSIST_ENGINE);
  const engineName = $derived(assistEngineName(engine));
  const appleRequest = $derived({
    fields: available as string[],
    notes,
    title,
    description,
    context: appleContext({ kind: task?.kind, repositories: [...repositoryNames], labels: task?.labels ?? [] }),
  });
  const appleAsk = $derived(appleGate(apple, appleRequest));
  const askLabel = $derived(busy || preparing
    ? (quick ? "Enhancing…" : `Asking ${engineName}…`)
    : notes.trim() ? `Draft with ${engineName}` : quick ? `Enhance with ${engineName}` : `Improve with ${engineName}`);
  const selectionSummary = $derived(liveSelection ? describeSelection(liveSelection) : "");
  const confirmedModel = $derived(configuration?.model_source === "env" && configuration.model.trim()
    ? (liveSelection ? describeSelection({ base_url: liveSelection.base_url, model: configuration.model }) : `${configuration.provider} / ${configuration.model}`)
    : selectionSummary);
  const manviReady = $derived(!configPending && Boolean(liveSelection) && Boolean(configuration?.provider.trim() && configuration?.model.trim()) && !configurationError);
  const fieldReason = $derived(
    lockedFields.includes("title") && lockedFields.includes("description")
      ? "Title and description are locked"
      : available.length === 0
        ? "Choose title, description, or both"
        : !liveSelection
          ? "Pick a local model in Local model servers"
          : !manviReady
            ? (configurationError ?? "Manvi has no provider and model selected.")
            : !manviGate.ok
              ? manviGate.reason
              : undefined,
  );
  const askDisabled = $derived(
    engine === "apple"
      ? disabled || acting || liveAttempt || available.length === 0 || Boolean(gate) || !appleAsk.ok || !manviGate.ok
      : disabled || acting || liveAttempt || configPending || available.length === 0 || Boolean(gate) || !manviReady || !manviGate.ok,
  );
  const revisionDirty = $derived(proposal?.fields.some((field) => revisionDraft[field] !== proposal?.proposed[field]) ?? false);
  const failureAdvice = $derived(proposal?.failure ? explainEnhancementFailure(proposal.failure) : null);
  const expiredLive = $derived(Boolean(proposal && liveEnhancement(proposal) && now >= proposal.expires_at));

  function toggleField(field: EnhancementField, on: boolean) {
    if (acting || disabled || lockedFields.includes(field)) return;
    requested = on ? [...new Set([...requested, field])] : requested.filter((value) => value !== field);
  }
  function setLock(field: EnhancementField, on: boolean) {
    if (acting || disabled) return;
    lockedFields = on ? [...new Set([...lockedFields, field])] : lockedFields.filter((value) => value !== field);
  }
  function toggleSelected(field: EnhancementField, checked: boolean) {
    selected = checked ? [...new Set([...selected, field])] : selected.filter((value) => value !== field);
  }

  onMount(() => {
    const unsub = harnessStore.subscribe(() => { harnessTick += 1; });
    const update = () => { visible = document.visibilityState === "visible"; now = Date.now() / 1000; };
    update();
    document.addEventListener("visibilitychange", update);
    void loadConfig();
    void appleIntelligenceStatus().then((status) => { if (!disposed) apple = status; });
    if (task) void history();
    if (autofocus && !quick) queueMicrotask(() => notesEl?.focus());
    const tick = window.setInterval(() => { now = Date.now() / 1000; }, 1000);
    return () => {
      disposed = true;
      unsub();
      selecting++;
      document.removeEventListener("visibilitychange", update);
      window.clearInterval(tick);
      if (flashTimer) clearTimeout(flashTimer);
    };
  });

  $effect(() => { onBusy(controlsLocked); });
  $effect(() => { onCount(total); });
  $effect(() => { onEngine(engineName); });
  $effect(() => {
    onSuggestion({
      title: titleSuggestion,
      description: descriptionSuggestion,
      showTitle: showTitleSuggestion,
      showDescription: showDescriptionSuggestion,
      blocked: acceptDisabled ? (dirty ? "Save or reload your edits before accepting a suggestion." : stale ? "This suggestion is for an older task revision. Request a fresh suggestion." : "") : "",
      flash: [...flash],
    });
  });
  $effect(() => {
    // Configuration belongs to the shared model selection. A picker change
    // must recover this editor without closing it or losing the draft.
    void liveSelection?.base_url;
    void liveSelection?.model;
    untrack(() => { void loadConfig(); });
  });
  $effect(() => {
    if (quick && active) untrack(() => { if (!startRequest) { void loadConfig(); void history(); } });
  });
  $effect(() => {
    if (active && startRequest > lastStart) {
      lastStart = startRequest;
      untrack(() => { void startEnhancement(); });
    }
  });
  $effect(() => {
    const worker = $automaticUpdates.status;
    if (active && visible && !acting && !editing && worker && task) untrack(() => { void history(); });
  });
  $effect(() => {
    if (!active || !proposal || !liveEnhancement(proposal)) return;
    const id = proposal.id;
    const timer = window.setInterval(() => { void poll(id); }, 1000);
    return () => window.clearInterval(timer);
  });
  async function loadConfig() {
    if (configPending || acting || disabled) return;
    const selectionKey = JSON.stringify(liveSelection);
    configPending = true;
    try {
      const config = await bounded(enhancementConfiguration(liveSelection));
      if (disposed || selectionKey !== JSON.stringify(liveSelection)) return;
      configuration = config;
      configurationError = null;
    } catch (cause) {
      if (!disposed && selectionKey === JSON.stringify(liveSelection)) configurationError = explainError(cause);
    } finally {
      if (!disposed) {
        configPending = false;
        // Coalesce rapid selection changes into one follow-up request.
        if (selectionKey !== JSON.stringify(liveSelection)) void loadConfig();
      }
    }
  }

  async function history(append = false) {
    if (!task || historyLoading || editing) return;
    historyLoading = true;
    try {
      const result = await bounded(listEnhancements(task.id, append ? cursor ?? undefined : undefined));
      if (disposed) return;
      entries = append ? [...entries, ...result.items.filter((entry) => !entries.some((old) => old.id === entry.id))] : result.items;
      total = result.total;
      cursor = result.next_cursor;
      if (!proposal && result.items[0]) await choose(result.items[0].id);
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) historyLoading = false; }
  }

  async function choose(id: string) {
    if (editing || acting) return;
    const ticket = ++selecting;
    try {
      const next = await bounded(getEnhancement(id));
      if (disposed || ticket !== selecting) return;
      proposal = next;
      selected = next.fields.filter((field) => !lockedFields.includes(field));
      error = "";
    } catch (cause) { if (!disposed && ticket === selecting) error = explainError(cause); }
  }

  async function poll(id: string) {
    if (polling || acting || disposed || !active || document.visibilityState === "hidden") return;
    polling = true;
    const ticket = epoch;
    try {
      const next = await bounded(getEnhancement(id));
      if (disposed || ticket !== epoch || acting || proposal?.id !== id || next.revision < (proposal?.revision ?? 0)) return;
      proposal = next;
      error = "";
      if (task) void history();
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
      if (task) await history();
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) { needsReconcile = action.pending !== null; busy = false; } }
  }

  async function startEnhancement() {
    if (preparing || disabled || acting || editing) return;
    preparing = true;
    let readyConfig = false;
    try {
      await loadConfig();
      if (task) await history();
      readyConfig = Boolean(configuration && !configurationError);
    } finally { preparing = false; }
    if (readyConfig && !disposed && active && !disabled && !liveAttempt) void generate();
  }

  async function generate() {
    if (askDisabled && !quick) {
      error = gate ?? (engine === "apple" ? appleAsk.reason : fieldReason)
        ?? (liveAttempt ? "A suggestion is already in progress." : `${engineName} cannot draft this task yet.`);
      return;
    }
    if (quick && (liveAttempt || disabled || controlsLocked || !available.length || !task)) return;
    if (quick && engine !== "apple" && !manviReady) return;
    busy = true; epoch++; error = ""; note = "";
    try {
      const saved = quick ? task : await prepareTask();
      if (disposed) return;
      if (!saved) {
        if (!error) error = gate ?? `Could not save a draft for ${engineName}.`;
        return;
      }
      if (engine !== "apple") {
        if (!configuration) {
          error = configurationError ?? "Manvi configuration has not been loaded.";
          return;
        }
        if (!liveSelection) {
          error = "Pick a local model in Local model servers.";
          return;
        }
      }
      // Both engines share the store's "one live attempt per task" rule, so
      // this check belongs to neither of them in particular.
      const page = await bounded(listEnhancements(saved.id));
      if (disposed) return;
      const existing = page.items.find(liveEnhancement);
      if (existing) {
        const current = await bounded(getEnhancement(existing.id));
        if (!disposed) { proposal = current; note = "A suggestion is already in progress."; await history(); }
        return;
      }
      const result = engine === "apple"
        ? await runAppleEnhancement(saved, available, {
            kind: notes.trim() ? "extract" : title.trim() || description.trim() ? "improve" : "draft",
            notes,
            title,
            description,
            context: appleContext({ kind: saved.kind, repositories: [...repositoryNames], labels: saved.labels }),
          })
        : (await startQuickEnhance(saved, available, configuration!, action)).proposal;
      if (disposed) return;
      proposal = result;
      selected = result.fields.filter((field) => !lockedFields.includes(field));
      const changed = result.state === "ready" && (
        suggestionDiffers(title, result.proposed.title ?? "") ||
        suggestionDiffers(description, result.proposed.description ?? "")
      );
      note = result.state === "ready"
        ? (changed ? "Suggestions ready under the fields they change." : `${engineName} kept your wording.`)
        : `${engineName} is drafting title and description.`;
      await history();
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
      await history();
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) { needsReconcile = action.pending !== null; busy = false; } }
  }

  function editSuggestion() {
    if (!proposal || proposal.state !== "ready" || disabled || acting) return;
    selecting++; revisionDraft = { ...proposal.proposed }; editing = true;
  }

  async function saveSuggestion() {
    if (!proposal || !revisionDirty) return;
    const input: Record<string, unknown> = { id: proposal.id, request_id: newID(), expected_revision: proposal.revision };
    for (const field of proposal.fields) {
      if (revisionDraft[field] !== proposal.proposed[field]) input[field] = revisionDraft[field];
    }
    await mutate("enhancements.revise", input);
  }

  async function mutate(method: EnhancementMutation, input: Record<string, unknown>) {
    if (busy || disabled || !task) return;
    selecting++; busy = true; error = ""; note = "";
    try {
      const result = await action.run(method, input, task.id);
      if (disposed) return;
      selecting++; proposal = result.proposal;
      selected = proposal.fields.filter((field) => !lockedFields.includes(field));
      if (method === "enhancements.revise") { editing = false; revisionDraft = {}; }
      if (result.task) onApplied(result.task);
      needsReconcile = false;
      note = method === "enhancements.revise" ? "Suggestion saved. The task has not changed." : labels[proposal.state];
      await history();
    } catch (cause) { if (!disposed) { error = explainError(cause); needsReconcile = action.pending !== null; } }
    finally { if (!disposed) busy = false; }
  }

  async function act(method: EnhancementMutation) {
    if (!proposal || !task) return;
    const input: Record<string, unknown> = { id: proposal.id, request_id: newID(), expected_revision: proposal.revision };
    if (method === "enhancements.accept" || method === "enhancements.undo") input.expected_task_revision = task.revision;
    if (method === "enhancements.accept") {
      const accepted = acceptEnhancementInput(proposal, task, selected, String(input.request_id));
      if (!accepted) { error = "This suggestion no longer matches the saved task. Request a fresh suggestion."; return; }
      Object.assign(input, accepted);
    }
    if (method === "enhancements.recover") {
      if (!await askConfirm({
        title: "Release this expired attempt?",
        message: "Its provider outcome is unknown. Starting again may create another model call.",
        confirmLabel: "Release",
        cancelLabel: "Keep waiting",
        destructive: true,
      })) return;
      input.worker_id = proposal.worker_id;
      input.acknowledge_uncertain = true;
    }
    void mutate(method, input);
  }

  function clearExpired() {
    if (!proposal) return;
    epoch++;
    proposal = null;
    note = "Suggestion request expired; start again.";
  }
</script>

<section class="manvi-assist" class:quick aria-label="Manvi task assist">
  {#if !quick}
    <div class="assist-head">
      <Sparkles size={13} class="text-accent shrink-0" />
      <div class="min-w-0">
        <p class="assist-title">What do you need?</p>
        <p class="assist-hint">Notes become a title and description you accept below.</p>
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
  {/if}

  {#if appleOffered}
    <div class="gp-segmented engine-picks" role="group" aria-label="Which model writes this" data-testid="task-assist-engine">
      <button type="button" class="gp-seg-btn" data-active={engine === "manvi"} aria-pressed={engine === "manvi"} disabled={disabled || acting} onclick={() => { requestedEngine = "manvi"; }}>{assistEngineName("manvi")}</button>
      <button
        type="button"
        class="gp-seg-btn"
        data-active={engine === "apple"}
        aria-pressed={engine === "apple"}
        disabled={disabled || acting || !appleReady(apple)}
        title={apple?.detail ?? ""}
        onclick={() => { requestedEngine = "apple"; }}
      >{assistEngineName("apple")}<span class="engine-badge">{appleBadge(apple)}</span></button>
    </div>
  {/if}

  {#if engine === "apple"}
    <p class="meta">{apple?.detail ?? "Runs on this device; the text never leaves it."}</p>
  {:else}
    <div class="model-row">
      {#if confirmedModel}
        <p class="meta">Using {confirmedModel}</p>
      {:else}
        <p class="warn">Pick a local model in Local model servers</p>
      {/if}
      <button type="button" class="change-link" onclick={() => requestManviFocus("model")}>Change</button>
    </div>
    {#if configurationError}
      <p class="warn">{configurationError}</p>
      <button type="button" class="gp-btn" disabled={configPending || acting || disabled} onclick={() => void loadConfig()}>Retry Manvi configuration</button>
    {/if}
  {/if}
  {#if appleOffered && requestedEngine === "apple" && !appleReady(apple)}
    <p class="warn" role="status">{apple?.detail}</p>
  {/if}

  <div class="locks">
    <SettingToggle label="Keep title" checked={lockedFields.includes("title")} disabled={disabled || acting} onchange={(next) => setLock("title", next)} />
    <SettingToggle label="Keep description" checked={lockedFields.includes("description")} disabled={disabled || acting} onchange={(next) => setLock("description", next)} />
  </div>

  <div class="gp-segmented field-picks" role="group" aria-label="Fields {engineName} may change">
    <button type="button" class="gp-seg-btn" data-active={available.includes("title")} aria-pressed={available.includes("title")} disabled={disabled || acting || lockedFields.includes("title")} title={lockedFields.includes("title") ? "Title is locked against enhancement" : "Include title"} onclick={() => toggleField("title", !available.includes("title"))}>Title</button>
    <button type="button" class="gp-seg-btn" data-active={available.includes("description")} aria-pressed={available.includes("description")} disabled={disabled || acting || lockedFields.includes("description")} title={lockedFields.includes("description") ? "Description is locked against enhancement" : "Include description"} onclick={() => toggleField("description", !available.includes("description"))}>Description</button>
  </div>

  {#if gate && (notes.trim() || title.trim() || task)}<p class="meta">{gate}</p>{/if}
  {#if !manviGate.ok && (manviReady || engine === "apple") && !gate}<p class="warn">{manviGate.reason}</p>{/if}
  {#if engine === "apple" && !appleAsk.ok && !gate && appleReady(apple)}<p class="warn">{appleAsk.reason}</p>{/if}

  <button type="button" class="gp-btn-primary ask" disabled={askDisabled} title={gate ?? (engine === "apple" ? appleAsk.reason : fieldReason)} onclick={() => void generate()}>
    <Sparkles size={12} />
    {askLabel}
  </button>
  {#if askDisabled && !notes.trim() && !title.trim() && !task && !quick}
    <p class="meta">Type a few sentences, then draft. Or fill a title to improve an existing one.</p>
  {/if}
  {#if needsReconcile}<p class="warn" role="status">The result is uncertain. Retry the same action before editing or closing.</p><button type="button" class="gp-btn" disabled={busy || disabled} onclick={() => void retry()}>Retry pending action</button>{/if}
  {#if ready && (dirty || stale)}<p class="warn">{dirty ? "Save or reload your edits before accepting a suggestion." : "This suggestion is for an older task revision. Request a fresh suggestion."}</p>{/if}
  {#if error}<p role="alert" class="error">{error}</p>{/if}
  {#if note}<p role="status" class="meta">{note}</p>{/if}
  {#if proposal && liveEnhancement(proposal)}
    <p class="meta" role="status">
      {proposal.state === "running" ? "Manvi is drafting…" : proposal.state === "cancel_requested" ? "Waiting for cancellation…" : "Waiting to start."}
      {#if expiredLive} The deadline passed; termination is unconfirmed.{/if}
    </p>
  {/if}
  {#if proposal?.state === "failed" || proposal?.state === "cancelled" || proposal?.failure}
    <p class="error" role="alert">{failureAdvice?.guidance ?? proposal?.failure}</p>
    {#if /expired/i.test(proposal?.failure ?? "") || proposal?.state === "failed"}
      <button type="button" class="gp-btn" disabled={acting || disabled} onclick={clearExpired}>Start again</button>
    {/if}
  {/if}
  {#if proposal?.rationale && ready}<p class="meta">{proposal.rationale}</p>{/if}

  {#if task}
    <details class="history-drawer" bind:open={historyOpen}>
      <summary>Manvi history{#if entries.length} · {entries.length}{/if}</summary>
      <div class="history-heading">
        <strong>Suggestions</strong>
        <button type="button" class="gp-btn" disabled={historyLoading || controlsLocked} onclick={() => history()}>Refresh</button>
      </div>
      {#if historyLoading && !entries.length}<p role="status" class="meta">Loading suggestions…</p>
      {:else if !entries.length}<p class="meta">No suggestions for this task yet.</p>{/if}
      <div class="history">
        {#each entries as entry (entry.id)}
          <button type="button" class:selected={proposal?.id === entry.id} disabled={controlsLocked} onclick={() => choose(entry.id)}>
            {labels[entry.state]} · {entry.model}
            <small>Task revision {entry.source_revision}{entry.automatic ? " · Automatic" : ""}{entry.edited_fields.length ? " · Edited" : ""}</small>
          </button>
        {/each}
      </div>
      {#if cursor}<button type="button" class="gp-btn" disabled={historyLoading || controlsLocked} onclick={() => history(true)}>Load more ({entries.length} of {total})</button>{/if}

      {#if proposal && (quick || historyOpen || proposal.state !== "ready" || (!showTitleSuggestion && !showDescriptionSuggestion))}
        <article aria-label="Enhancement review">
          <h3>{labels[proposal.state]}</h3>
          <small>{proposal.provider} / {proposal.model} · source revision {proposal.source_revision}</small>
          {#if proposal.state === "ready" || proposal.state === "accepted" || proposal.state === "undone"}
            {#each proposal.fields as field}
              <div class="field-review">
                <label class="check" class:advanced-hidden={quick && !chooseFields}>
                  <input class="gp-field" type="checkbox" checked={proposal.state === "ready" ? selected.includes(field) : proposal.accepted_fields.includes(field)} disabled={disabled || controlsLocked || proposal.state !== "ready" || lockedFields.includes(field)} onchange={(event) => toggleSelected(field, event.currentTarget.checked)} />
                  {field === "title" ? "Title" : "Description"}
                </label>
                {#if quick && !chooseFields}<h4>{field === "title" ? "Title" : "Description"}</h4>{/if}
                <details open={!quick}><summary>Original task</summary><pre>{proposal.source[field]}</pre></details>
                {#if proposal.edited_fields.includes(field)}<small>Original suggestion</small><pre>{proposal.original_proposed?.[field]}</pre>{/if}
                <small>{proposal.edited_fields.includes(field) ? "Edited suggestion" : "Suggestion"}</small>
                <pre class="suggestion">{proposal.proposed[field]}</pre>
                {#if editing}<label>Revised {field}<textarea class="gp-field" bind:value={revisionDraft[field]} rows={field === "title" ? 2 : 5} maxlength={field === "title" ? 300 : 65536} disabled={acting || lockedFields.includes(field)}></textarea></label>{/if}
              </div>
            {/each}
          {/if}
          <div class="actions">
            {#if editing}
              <button class="gp-btn-primary" type="button" disabled={acting || !revisionDirty || (proposal.fields.includes("title") && !revisionDraft.title?.trim())} onclick={() => void saveSuggestion()}>Save suggestion edits</button>
              <button type="button" class="gp-btn" disabled={acting} onclick={() => { editing = false; revisionDraft = {}; }}>Discard suggestion edits</button>
            {/if}
            {#if proposal.state === "ready"}
              <button class="gp-btn-primary" type="button" disabled={disabled || controlsLocked || !selected.length} onclick={() => act("enhancements.accept")}>{quick ? "Apply enhancement" : "Accept selected fields"}</button>
              {#if quick}<button type="button" class="gp-btn" disabled={disabled || controlsLocked} onclick={() => { chooseFields = !chooseFields; }}>{chooseFields ? "Hide field choices" : "Choose fields"}</button>{/if}
              <button type="button" class="gp-btn" disabled={disabled || controlsLocked} onclick={editSuggestion}>Edit suggestion</button>
            {/if}
            {#if proposal.state === "accepted"}<button type="button" class="gp-btn" disabled={disabled || acting} onclick={() => act("enhancements.undo")}>Undo accepted fields</button>{/if}
            {#if proposal.state === "pending" && now < proposal.expires_at}<button type="button" class="gp-btn" disabled={disabled || acting} onclick={() => act("enhancements.generate")}>Start generation</button>{/if}
            {#if ["pending", "ready", "failed", "running", "interrupted"].includes(proposal.state)}
              <button type="button" class="gp-btn" disabled={disabled || controlsLocked} onclick={() => act("enhancements.dismiss")}>{proposal.state === "running" ? "Cancel" : "Dismiss"}</button>
            {/if}
            {#if ["running", "cancel_requested"].includes(proposal.state) && now >= proposal.expires_at}
              <button type="button" class="gp-btn" disabled={disabled || acting} onclick={() => act("enhancements.recover")}>Resolve uncertain attempt</button>
            {/if}
          </div>
        </article>
      {/if}
    </details>
  {/if}
</section>

<style>
  .manvi-assist{margin:0 0 16px;padding:12px;border:1px solid rgb(var(--c-border) / 0.65);border-radius:12px}
  .manvi-assist.quick{margin-top:8px;padding-top:8px;border:0;border-radius:0;padding-left:0;padding-right:0}
  .assist-head{display:flex;gap:8px;align-items:flex-start;margin-bottom:8px}
  .assist-title{margin:0;font-size:12px;font-weight:650}
  .assist-hint,.meta{margin:4px 0 0;font-size:11px;color:rgb(var(--c-text-muted));line-height:1.45}
  .notes-label{display:block;margin:8px 0}
  textarea,pre{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}
  textarea{resize:vertical}
  .field-picks{margin:0 0 8px}
  .engine-picks{margin:0 0 8px}
  .engine-badge{margin-left:5px;font-size:9px;opacity:.75}
  .ask{margin-top:8px}
  label{display:flex;flex-direction:column;gap:6px;margin:12px 0 8px;font-size:12px}
  input,textarea{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}
  .actions,.history-heading,.model-row{display:flex;flex-wrap:wrap;gap:6px;align-items:center}
  .model-row{justify-content:space-between;margin:4px 0 8px}
  .change-link{background:none;border:0;color:rgb(var(--c-accent));font-size:11px;padding:0;cursor:pointer;text-decoration:underline}
  .locks{margin:4px 0 8px}
  .error{color:#dc6565}
  .warn{color:#d4a017;font-size:11px;margin:4px 0}
  .history-drawer{margin-top:12px;border-top:1px solid rgb(var(--c-border) / 0.55);padding-top:8px}
  .history-drawer summary{cursor:pointer;font-size:11px;color:rgb(var(--c-text-muted));padding:4px 0}
  .history-heading{justify-content:space-between;margin:8px 0}
  .history{max-height:150px;overflow:auto;display:flex;flex-direction:column;gap:5px}
  .history button{text-align:left;padding:6px 9px;border:1px solid rgb(var(--c-border));border-radius:6px;background:transparent;color:inherit}
  .history small{display:block;color:rgb(var(--c-text-muted));font-size:11px}
  .history .selected{border-color:rgb(var(--c-accent))}
  .field-review{margin:12px 0}
  .check{flex-direction:row;align-items:center;gap:7px}
  .check input{width:auto}
  .advanced-hidden{display:none}
  h3{font-size:13px;margin:14px 0 4px}
  h4{font-size:12px;margin:10px 0 6px}
  pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:170px;overflow:auto;font:12px/1.5 inherit;margin:4px 0 8px}
  .suggestion{border-left:3px solid rgb(var(--c-accent))}
  .actions button{padding:6px 9px;border:1px solid rgb(var(--c-border));border-radius:6px}
</style>
