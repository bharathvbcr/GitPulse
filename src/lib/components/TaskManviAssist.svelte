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
  import { acceptEnhancementInput, assistEngineName, enhancementApplyBlock, enhancementOptionLabel, runAppleEnhancement, startQuickEnhance, DEFAULT_ASSIST_ENGINE, ENHANCEMENT_STATE_LABELS, EnhancementAction, liveEnhancement, type AssistEngine } from "../workbench/taskEnhance";
  import { timestampFormat } from "../ui/timestampFormat";
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
    quick = false,
    startRequest = 0,
    onFlash = (_fields: EnhancementField[]) => {},
    onReview = (_ready: boolean) => {},
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
    /** Fields just accepted, so the sheet can flash the ones that changed. */
    onFlash?: (fields: EnhancementField[]) => void;
    /** A suggestion is ready to review, so the sheet can mark the Task tab. */
    onReview?: (ready: boolean) => void;
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

  const labels = ENHANCEMENT_STATE_LABELS;
  /** Two of these mount at once (the sheet and Quick Enhance), so ids collide. */
  const uid = $props.id();

  /**
   * The id the picker is displaying.
   *
   * Kept apart from `proposal.id` on purpose. A `<select>` shows whatever the
   * reader just chose the instant they choose it, while loading that proposal
   * is a round trip that can fail or be superseded. Without its own state the
   * control would sit there naming a revision the review below is not
   * rendering — the same shape of lie as a drawer that hid the review
   * entirely. On failure it snaps back to whatever is actually on screen.
   */
  let selectedId = $state("");
  /** True only between a reader's pick and the proposal landing. */
  let selectPending = $state(false);
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
  /** The configuration read currently in flight, shared by every caller. */
  let configLoad: Promise<void> | null = null;
  let flash = $state<EnhancementField[]>([]);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
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
  /**
   * Keep the picker pointing at whatever the review is rendering.
   *
   * `proposal` is replaced by five other paths besides the picker — accepting,
   * undoing, dismissing, editing, polling and the expiry reset. Syncing at
   * each of those is five chances to forget one and leave the control naming a
   * revision that is no longer on screen; this is the one place that answers
   * it. `choose` sets `selectPending` so an in-flight pick is not overwritten
   * by the proposal it is replacing.
   */
  $effect(() => {
    const id = proposal?.id ?? "";
    if (!selectPending) selectedId = id;
  });
  /** Newest first, so the ordinal counts up from the task's first attempt. */
  const historyOptions = $derived(
    entries.map((entry, index) => ({
      id: entry.id,
      label: enhancementOptionLabel(entry, {
        ordinal: Math.max(1, total - index),
        when: $timestampFormat.text(entry.created_at),
      }),
    })),
  );
  /** Why the selected proposal cannot be applied here, or "" when it can. */
  const applyBlock = $derived(enhancementApplyBlock(proposal, task));
  const liveAttempt = $derived(liveEnhancement(proposal) || entries.some((entry) => liveEnhancement(entry)));
  const stale = $derived(Boolean(proposal && task && proposal.source_revision !== task.revision));
  const ready = $derived(proposal?.state === "ready");
  /**
   * Why accepting is refused right now, or "" when it is allowed.
   *
   * A disabled button with no reason beside it is a refusal the reader cannot
   * act on, so the review prints this instead of only greying out.
   */
  const acceptBlock = $derived(
    dirty ? "Save or reload your edits before accepting a suggestion."
      : stale ? "This suggestion is for an older task revision. Ask again to apply it."
      : "",
  );
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
  $effect(() => { onEngine(engineName); });
  $effect(() => { onFlash([...flash]); });
  // Whether something is waiting to be reviewed. The sheet draws a dot on the
  // Task tab from this, so a reader sitting on Agent knows to come back.
  $effect(() => { onReview(ready); });
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
    // `disabled` is part of the condition, not just of `startEnhancement`'s own
    // guard: that guard returns silently, so a run that cannot act would still
    // consume the request and the draft would never start. Reading it here
    // leaves the request pending until the surface can honour it. No caller
    // reaches this disabled today — the one sheet that passes `startRequest`
    // only renders this component once its task has loaded — so this is the
    // seam being closed, not a bug being patched.
    if (active && !disabled && startRequest > lastStart) {
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
  /**
   * Read the model configuration, sharing one request between callers.
   *
   * Two things ask for this at once whenever a surface opens to start work
   * straight away: the selection effect on mount, and `startEnhancement`.
   * The old guard simply *returned* for the second caller, which is the wrong
   * answer to "is it loaded?" — it reported done while the request was still
   * in flight, so an auto-start read `configuration` as null and silently did
   * nothing. Awaiting the same promise keeps the single request and gives
   * every caller the real answer.
   */
  function loadConfig(): Promise<void> {
    // `busy || needsReconcile`, not `acting`. `acting` also covers
    // `preparing`, which `startEnhancement` sets *around its own call to this
    // function* — so the guard refused the one caller that most needs an
    // answer, and the auto-start could never read a configuration. What the
    // guard is actually for is not re-reading while a mutation is in flight or
    // unreconciled, and those are the two flags that say so.
    if (busy || needsReconcile || disabled) return Promise.resolve();
    configLoad ??= (async () => {
      // Coalesce rapid selection changes into one follow-up request rather
      // than one per keystroke, and bound it: a selection that somehow never
      // settles must not spin this forever.
      for (let attempt = 0; attempt < 4; attempt++) {
        const key = await readConfig();
        if (disposed || key === JSON.stringify(liveSelection)) return;
      }
    })().finally(() => { configLoad = null; });
    return configLoad;
  }
  async function readConfig(): Promise<string> {
    const selectionKey = JSON.stringify(liveSelection);
    configPending = true;
    try {
      const config = await bounded(enhancementConfiguration(liveSelection));
      if (disposed || selectionKey !== JSON.stringify(liveSelection)) return selectionKey;
      configuration = config;
      configurationError = null;
    } catch (cause) {
      if (!disposed && selectionKey === JSON.stringify(liveSelection)) configurationError = explainError(cause);
    } finally {
      if (!disposed) configPending = false;
    }
    return selectionKey;
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
    // The picker is disabled while locked, so this guard is now only reached
    // from `history()`'s auto-select. It stays because a swap mid-mutation
    // would strand the receipt the action is holding.
    if (editing || acting) { selectedId = proposal?.id ?? ""; return; }
    const ticket = ++selecting;
    selectedId = id;
    selectPending = true;
    try {
      const next = await bounded(getEnhancement(id));
      if (disposed || ticket !== selecting) return;
      proposal = next;
      selected = next.fields.filter((field) => !lockedFields.includes(field));
      error = "";
    } catch (cause) {
      if (disposed || ticket !== selecting) return;
      error = explainError(cause);
    } finally {
      // Snap back to the revision actually on screen. A picker still naming an
      // entry it failed to load is the same lie as a review that never
      // changed: the control would say one thing and the diff below another.
      if (!disposed && ticket === selecting) { selectPending = false; selectedId = proposal?.id ?? ""; }
    }
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
        ? (changed ? "Ready to review below." : `${engineName} kept your wording.`)
        : `${engineName} is drafting title and description.`;
      await history();
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      if (!disposed) { needsReconcile = action.pending !== null; busy = false; }
    }
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
      if (method === "enhancements.accept") {
        // What the store says was accepted, not what was asked for: the flash
        // is a claim about fields that actually changed, so it reads the
        // result rather than the request.
        const changed = proposal.accepted_fields.filter((field) => field === "title" || field === "description");
        note = changed.length === 1 ? `Saved the suggested ${changed[0]}.`
          : changed.length ? "Saved the suggested title and description."
          : labels[proposal.state];
        flash = [...changed];
        if (flashTimer) clearTimeout(flashTimer);
        flashTimer = setTimeout(() => { flash = []; }, 1600);
      } else {
        note = method === "enhancements.revise" ? "Suggestion saved. The task has not changed." : labels[proposal.state];
      }
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
    <!--
      A dropdown, not a disclosure. The list used to live inside a collapsed
      `<details>`, above a 150px scroller, above a review that was suppressed
      whenever a ready suggestion was already showing beside the fields — so
      the one control that could change which revision was on screen could be
      operated with no visible effect at all.

      Native `<select>` for the same reason `gp-select` keeps one everywhere
      else (app.css): this is a single choice over text, and the platform
      widget already brings type-ahead, Home/End and a popup that scrolls.
    -->
    <div class="history-row">
      <label class="history-label">
        <span class="sr-only">Suggestion</span>
        <select
          class="gp-select"
          data-testid="task-assist-history"
          value={selectedId}
          disabled={controlsLocked || (!entries.length && !selectedId)}
          title={controlsLocked
            ? "Finish the current action before changing suggestions."
            : proposal
              ? $timestampFormat.title(proposal.created_at)
              : ""}
          aria-describedby="assist-history-count-{uid}"
          onchange={(event) => void choose(event.currentTarget.value)}
        >
          {#if historyLoading && !entries.length}
            <option value="">Loading suggestions…</option>
          {:else if !entries.length}
            <option value="">No suggestions yet</option>
          {/if}
          {#each historyOptions as option (option.id)}
            <option value={option.id}>{option.label}</option>
          {/each}
        </select>
      </label>
      <button type="button" class="gp-btn" disabled={historyLoading || controlsLocked} onclick={() => history()}>Refresh</button>
      <!-- An `<option>` cannot be a button, so paging is a sibling. The count
           beside it is the store's total, not the page: a picker that looks
           finite must not imply it is complete. -->
      {#if cursor}<button type="button" class="gp-btn" disabled={historyLoading || controlsLocked} onclick={() => history(true)}>Load more</button>{/if}
    </div>
    <p id="assist-history-count-{uid}" class="meta">
      {#if selectPending}Loading suggestion…
      {:else if !entries.length}{historyLoading ? "Loading suggestions…" : "No suggestions for this task yet."}
      {:else}Showing {entries.length} of {total}{/if}
    </p>

    <!-- Always rendered for whatever the picker selects, in every state.
         Two conditions used to hide it: one suppressed the whole review
         whenever a ready suggestion was showing beside the fields, and one
         moved the accept buttons up to the sheet. Together they meant the one
         control that chooses a revision could be operated with no visible
         effect at all. Acceptance now has exactly one home — this diff — so
         the picker always changes what is on screen. -->
    {#if proposal}
        <article aria-label="Enhancement review">
          <h3>{labels[proposal.state]}</h3>
          <small>{proposal.provider} / {proposal.model} · source revision {proposal.source_revision}</small>
          {#if applyBlock}<p class="warn" role="status">{applyBlock}</p>{/if}
          <!-- A greyed-out Accept with no reason beside it is a refusal the
               reader cannot act on. -->
          {#if !applyBlock && acceptBlock && proposal.state === "ready"}<p class="warn" role="status">{acceptBlock}</p>{/if}
          {#if proposal.state === "ready" || proposal.state === "accepted" || proposal.state === "undone"}
            {#each proposal.fields as field}
              <div class="field-review">
                <label class="check" class:advanced-hidden={quick && !chooseFields}>
                  <!-- Both `input` and `change` are stopped, because a checkbox
                       click fires both and the sheet marks the task dirty from
                       either one anywhere inside its form. Which fields a
                       reader intends to accept is not an edit to the task: left
                       to bubble, ticking this box marked the draft unsaved, and
                       the unsaved-edits guard then refused the very acceptance
                       the box was selecting. -->
                  <input class="gp-field" type="checkbox" checked={proposal.state === "ready" ? selected.includes(field) : proposal.accepted_fields.includes(field)} disabled={disabled || controlsLocked || proposal.state !== "ready" || lockedFields.includes(field)} oninput={(event) => event.stopPropagation()} onchange={(event) => { event.stopPropagation(); toggleSelected(field, event.currentTarget.checked); }} />
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
              <!-- Acceptance has exactly one home, and it is here: beside the
                   diff that shows what would change. -->
              <!-- `acceptBlock`, so the refusal and its reason are one
                   expression. The dirty and stale guards used to ride on the
                   sheet's own inline buttons, and moving acceptance here
                   without them would let an accept overwrite unsaved edits —
                   but the sheet also disabled on `acting`, which is transient
                   and has no sentence to show, so that stays with
                   `controlsLocked` where it belongs. -->
              <button class="gp-btn-primary" type="button" disabled={Boolean(acceptBlock) || disabled || controlsLocked || !selected.length} onclick={() => act("enhancements.accept")}>{quick ? "Apply enhancement" : "Accept selected fields"}</button>
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
  .actions,.history-row,.model-row{display:flex;flex-wrap:wrap;gap:6px;align-items:center}
  .model-row{justify-content:space-between;margin:4px 0 8px}
  .change-link{background:none;border:0;color:rgb(var(--c-accent));font-size:11px;padding:0;cursor:pointer;text-decoration:underline}
  .locks{margin:4px 0 8px}
  .error{color:#dc6565}
  .warn{color:#d4a017;font-size:11px;margin:4px 0}
  .history-row{margin-top:12px;border-top:1px solid rgb(var(--c-border) / 0.55);padding-top:10px}
  /* The picker takes the row's slack so a long label ellipsises rather than
     pushing Refresh and Load more onto their own line. */
  .history-label{flex:1;min-width:8rem;margin:0}
  .history-label select{width:100%}
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
