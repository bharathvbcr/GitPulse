<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { automaticUpdates, changeEnhancement, enhancementConfiguration, explainError, getEnhancement, getTask, listEnhancements, newID, WorkbenchError, type Enhancement, type EnhancementField, type EnhancementMutation, type EnhancementSummary, type Task } from "../workbench/client";

  let { task, disabled, onApplied, onBusy }: { task: Task; disabled: boolean; onApplied: (task: Task) => void; onBusy: (busy: boolean) => void } = $props();
  let opened = $state(false), loading = $state(false), busy = $state(false);
  let error = $state(""), configurationError = $state(""), note = $state("");
  let provider = $state(""), model = $state(""), providers = $state<string[]>([]);
  let configurationEdited = false;
  let requested = $state<EnhancementField[]>(["title", "description"]);
  let selected = $state<EnhancementField[]>([]);
  let entries = $state<EnhancementSummary[]>([]), total = $state(0), cursor = $state<string | null>(null);
  let proposal = $state<Enhancement | null>(null);
  let editing = $state(false);
  let revisionDraft = $state<Partial<Record<EnhancementField, string>>>({});
  type Pending = { method: EnhancementMutation; input: Record<string, unknown>; result: Enhancement | null };
  let pending = $state<Pending | null>(null);
  let visible = $state(true), now = $state(Date.now() / 1000);
  let disposed = false, selecting = 0, polling = false;
  const labels = { pending: "Waiting to start", running: "Generating", cancel_requested: "Cancellation requested", ready: "Ready for review", failed: "Generation failed", cancelled: "Cancelled", interrupted: "Outcome uncertain", dismissed: "Dismissed", accepted: "Accepted", undone: "Undone" };
  const availableFields = $derived(requested.filter((field) => !(task.locked_fields ?? []).includes(field)));
  const acting = $derived(busy || pending !== null);
  const controlsLocked = $derived(acting || editing);
  const revisionDirty = $derived(proposal?.fields.some((field) => revisionDraft[field] !== proposal?.proposed[field]) ?? false);

  onMount(() => {
    const update = () => { visible = document.visibilityState === "visible"; now = Date.now() / 1000; };
    update(); document.addEventListener("visibilitychange", update);
    return () => { disposed = true; selecting++; document.removeEventListener("visibilitychange", update); };
  });
  $effect(() => { onBusy(controlsLocked); });
  $effect(() => {
    const worker = $automaticUpdates.status;
    if (opened && visible && !acting && !editing && worker) untrack(() => { void history(); });
  });
  $effect(() => {
    if (!opened || !visible || !proposal || !["pending", "running", "cancel_requested"].includes(proposal.state)) return;
    const id = proposal.id;
    const timer = setInterval(() => { now = Date.now() / 1000; void poll(id); }, 1000);
    return () => clearInterval(timer);
  });

  async function configure() {
    try {
      const configuration = await enhancementConfiguration();
      if (disposed) return;
      providers = configuration.providers;
      if (!configurationEdited) { provider = configuration.provider; model = configuration.model; }
      configurationError = "";
    } catch (cause) { if (!disposed) configurationError = explainError(cause); }
  }
  async function choose(id: string) {
    if (editing) return;
    const ticket = ++selecting;
    try {
      const next = await getEnhancement(id);
      if (disposed || ticket !== selecting) return;
      proposal = next; selected = next.fields.filter((field) => !(task.locked_fields ?? []).includes(field)); error = "";
    } catch (cause) { if (!disposed && ticket === selecting) error = explainError(cause); }
  }
  async function history(append = false) {
    if (loading || editing) return;
    loading = true;
    try {
      const result = await listEnhancements(task.id, append ? cursor ?? undefined : undefined);
      if (disposed) return;
      entries = append ? [...entries, ...result.items.filter((entry) => !entries.some((old) => old.id === entry.id))] : result.items;
      total = result.total; cursor = result.next_cursor;
      if (!proposal && result.items[0]) await choose(result.items[0].id);
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) loading = false; }
  }
  async function poll(id: string) {
    if (polling || acting || disposed) return;
    polling = true;
    try {
      const result = await listEnhancements(task.id);
      if (disposed || proposal?.id !== id) return;
      const summary = result.items.find((item) => item.id === id);
      entries = [...result.items, ...entries.filter((entry) => !result.items.some((fresh) => fresh.id === entry.id))]; total = result.total;
      if (summary && summary.revision !== proposal.revision) await choose(id);
    } catch (cause) { if (!disposed) error = `Status refresh failed: ${explainError(cause)}`; }
    finally { polling = false; }
  }
  function toggle(field: EnhancementField, target: "requested" | "selected", checked: boolean) {
    const old = target === "requested" ? requested : selected;
    const next = checked ? [...new Set([...old, field])] : old.filter((value) => value !== field);
    if (target === "requested") requested = next; else selected = next;
  }
  async function mutate(method: EnhancementMutation, input: Record<string, unknown>) {
    if (busy || disabled) return;
    if (!pending) pending = { method, input, result: null };
    busy = true; error = ""; note = "";
    try {
      // A confirmed receipt followed by a failed refresh needs only a read.
      // Uncertain transport retries retain the exact original mutation identity.
      const action = pending;
      const result = action.result ?? await changeEnhancement(action.method, action.input);
      action.result = result;
      if (disposed) return;
      selecting++; proposal = result; selected = result.fields.filter((field) => !(task.locked_fields ?? []).includes(field));
      if (action.method === "enhancements.revise") { editing = false; revisionDraft = {}; }
      if (action.method === "enhancements.accept" || action.method === "enhancements.undo") {
        const saved = await getTask(task.id);
        if (disposed) return;
        onApplied(saved);
      }
      pending = null;
      if (action.method === "enhancements.create" && result.state === "pending") {
        const next = { id: result.id, request_id: newID(), expected_revision: result.revision };
        pending = { method: "enhancements.generate", input: next, result: null };
        const running = await changeEnhancement("enhancements.generate", next);
        if (disposed) return;
        proposal = running; pending = null;
      }
      note = action.method === "enhancements.revise" ? "Suggestion saved. The task has not changed." : proposal ? labels[proposal.state] : "Saved";
      await history();
    } catch (cause) {
      if (disposed) return;
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) pending = null;
    } finally { if (!disposed) busy = false; }
  }
  function generate() {
    if (!provider.trim() || !model.trim() || !availableFields.length) return;
    void mutate("enhancements.create", { id: newID(), request_id: newID(), expected_revision: 0, task_id: task.id, source_revision: task.revision, fields: [...availableFields], provider: provider.trim(), model: model.trim() });
  }
  function editSuggestion() {
    if (!proposal || proposal.state !== "ready" || disabled || acting) return;
    selecting++; revisionDraft = { ...proposal.proposed }; editing = true;
  }
  function saveSuggestion() {
    if (!proposal || !revisionDirty) return;
    const input: Record<string, unknown> = { id: proposal.id, request_id: newID(), expected_revision: proposal.revision };
    for (const field of proposal.fields) {
      if (revisionDraft[field] !== proposal.proposed[field]) input[field] = revisionDraft[field];
    }
    void mutate("enhancements.revise", input);
  }
  function act(method: EnhancementMutation) {
    if (!proposal) return;
    const input: Record<string, unknown> = { id: proposal.id, request_id: newID(), expected_revision: proposal.revision };
    if (method === "enhancements.accept" || method === "enhancements.undo") input.expected_task_revision = task.revision;
    if (method === "enhancements.accept") input.fields = [...selected];
    if (method === "enhancements.recover") {
      if (!window.confirm("Release this expired attempt? Its provider outcome is unknown. Starting again may create another model call.")) return;
      input.worker_id = proposal.worker_id; input.acknowledge_uncertain = true;
    }
    void mutate(method, input);
  }
  function open() { opened = !opened; if (opened) { void configure(); void history(); } }
</script>

<section class="enhancements" aria-label="Manvi task enhancements">
  <button type="button" class="heading" aria-expanded={opened} disabled={editing} onclick={open}>Manvi enhancements <span>{opened ? "−" : "+"}</span></button>
  {#if opened}
    <p>Rewrite saved task fields with a reviewable suggestion. Acceptance changes only the fields you select.</p>
    {#if disabled}<p class="hint">Save or discard task edits before generating or accepting suggestions.</p>{/if}
    <fieldset disabled={disabled || controlsLocked}>
      <div class="pair"><label>Provider<input bind:value={provider} maxlength="128" list="enhancement-providers" oninput={() => { configurationEdited = true; }} /></label><label>Model<input bind:value={model} maxlength="512" placeholder="Choose a model" oninput={() => { configurationEdited = true; }} /></label></div>
      <datalist id="enhancement-providers">{#each providers as name}<option value={name}></option>{/each}</datalist>
      {#each ["title", "description"] as field}
        {@const target: EnhancementField = field === "title" ? "title" : "description"}
        <label class="check"><input type="checkbox" checked={availableFields.includes(target)} disabled={(task.locked_fields ?? []).includes(target)} onchange={(event) => toggle(target, "requested", event.currentTarget.checked)} />Rewrite {field}{(task.locked_fields ?? []).includes(target) ? " (locked)" : ""}</label>
      {/each}
      <button class="primary" type="button" onclick={generate} disabled={!provider.trim() || !model.trim() || !availableFields.length}>Generate suggestion</button>
    </fieldset>
    {#if configurationError}<p role="alert" class="error">Manvi configuration: {configurationError}</p>{/if}
    {#if error}<p role="alert" class="error">{error}</p>{/if}
    {#if pending}<p>The result needs reconciliation. Task editing is paused to preserve your saved revision.</p><button type="button" disabled={busy} onclick={() => { if (pending) void mutate(pending.method, pending.input); }}>Retry pending action</button>{/if}
    <div class="history-heading"><strong>Suggestions</strong><button type="button" disabled={loading || controlsLocked} onclick={() => history()}>Refresh</button></div>
    {#if loading && !entries.length}<p role="status">Loading suggestions…</p>{:else if !entries.length}<p>No suggestions for this task yet.</p>{/if}
    <div class="history">{#each entries as entry (entry.id)}<button type="button" class:selected={proposal?.id === entry.id} disabled={controlsLocked} onclick={() => choose(entry.id)}>{labels[entry.state]} · {entry.model}<small>Task revision {entry.source_revision}{entry.automatic ? " · Automatic suggestion" : ""}{entry.edited_fields.length ? " · Edited suggestion" : ""}</small></button>{/each}</div>
    {#if cursor}<button type="button" disabled={loading || controlsLocked} onclick={() => history(true)}>Load more ({entries.length} of {total})</button>{/if}
    {#if proposal}
      <article aria-label="Enhancement review">
        <h3>{labels[proposal.state]}</h3><small>{proposal.provider} / {proposal.model} · source revision {proposal.source_revision}</small>
        {#if proposal.failure}<p class="error">{proposal.failure}</p>{/if}
        {#if proposal.rationale}<p>{proposal.edited_fields.length ? "Original rationale: " : ""}{proposal.rationale}</p>{/if}
        {#if proposal.state === "ready" || proposal.state === "accepted" || proposal.state === "undone"}
          {#each proposal.fields as field}
            <div class="field-review"><label class="check"><input type="checkbox" checked={proposal.state === "ready" ? selected.includes(field) : proposal.accepted_fields.includes(field)} disabled={disabled || controlsLocked || proposal.state !== "ready" || (task.locked_fields ?? []).includes(field)} onchange={(event) => toggle(field, "selected", event.currentTarget.checked)} />{field === "title" ? "Title" : "Description"}</label>
              <small>Original task</small><pre>{proposal.source[field]}</pre>
              {#if proposal.edited_fields.includes(field)}<small>Original suggestion</small><pre>{proposal.original_proposed?.[field]}</pre>{/if}
              <small>{proposal.edited_fields.includes(field) ? "Edited suggestion" : "Suggestion"}</small><pre class="suggestion">{proposal.proposed[field]}</pre>
              {#if editing}<label>Revised {field}<textarea bind:value={revisionDraft[field]} rows={field === "title" ? 2 : 5} maxlength={field === "title" ? 300 : 65536} disabled={acting || (task.locked_fields ?? []).includes(field)}></textarea></label>{/if}
            </div>
          {/each}
        {/if}
        <div class="actions">
          {#if editing}<button class="primary" type="button" disabled={acting || !revisionDirty || (proposal.fields.includes("title") && !revisionDraft.title?.trim())} onclick={saveSuggestion}>Save suggestion edits</button><button type="button" disabled={acting} onclick={() => { editing = false; revisionDraft = {}; }}>Discard suggestion edits</button>{/if}
          {#if proposal.state === "ready"}<button class="primary" type="button" disabled={disabled || controlsLocked || !selected.length} onclick={() => act("enhancements.accept")}>Accept selected fields</button><button type="button" disabled={disabled || controlsLocked} onclick={editSuggestion}>Edit suggestion</button>{/if}
          {#if proposal.state === "accepted"}<button type="button" disabled={disabled || acting} onclick={() => act("enhancements.undo")}>Undo accepted fields</button>{/if}
          {#if proposal.state === "pending" && now < proposal.expires_at}<button type="button" disabled={disabled || acting} onclick={() => act("enhancements.generate")}>Start generation</button>{/if}
          {#if ["pending", "ready", "failed", "running"].includes(proposal.state)}<button type="button" disabled={disabled || controlsLocked} onclick={() => act("enhancements.dismiss")}>{proposal.state === "running" ? "Request cancellation" : "Dismiss"}</button>{/if}
          {#if ["running", "cancel_requested"].includes(proposal.state) && now >= proposal.expires_at}<button type="button" disabled={disabled || acting} onclick={() => act("enhancements.recover")}>Resolve uncertain attempt</button>{/if}
        </div>
        {#if proposal.state === "cancel_requested"}<p>Waiting for the worker to acknowledge cancellation.</p>{/if}
        {#if ["running", "cancel_requested"].includes(proposal.state) && now >= proposal.expires_at}<p>The deadline passed. Provider termination has not been confirmed.</p>{/if}
      </article>
    {/if}
    <p role="status">{busy ? "Saving enhancement action…" : note}</p>
  {/if}
</section>

<style>
  textarea{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:6px;background:rgb(var(--c-bg));color:inherit;resize:vertical}
  .enhancements{border-top:1px solid rgb(var(--c-border));margin-top:18px;padding-top:12px;font-size:12px}.heading{display:flex;width:100%;justify-content:space-between;text-align:left;font-weight:650;padding:8px 0}.pair{display:flex;gap:10px}label{display:flex;flex:1;flex-direction:column;gap:5px;margin:10px 0;min-width:0}fieldset{border:0;margin:0;padding:0}.check{flex-direction:row;align-items:center;gap:7px}.check input{width:auto}input{width:100%;min-width:0;padding:7px;border:1px solid rgb(var(--c-border));border-radius:6px;background:rgb(var(--c-bg));color:inherit}button{padding:6px 9px;border:1px solid rgb(var(--c-border));border-radius:6px}button:disabled{opacity:.5}button:hover:enabled{background:rgb(var(--c-surface-hover))}.primary{background:rgb(var(--c-accent));color:white}.actions,.history-heading{display:flex;align-items:center;gap:7px;flex-wrap:wrap}.history-heading{justify-content:space-between;margin:16px 0 8px}.history{max-height:150px;overflow:auto;display:flex;flex-direction:column;gap:5px}.history button{text-align:left}.history small{display:block}.history .selected{border-color:rgb(var(--c-accent))}small,.hint{color:rgb(var(--c-text-muted));font-size:11px}p{margin:9px 0;line-height:1.5}.error{color:#dc6565}h3{font-size:13px;margin:14px 0 4px}.field-review{margin:12px 0}pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:170px;overflow:auto;font:12px/1.5 inherit;background:rgb(var(--c-bg));padding:9px;border:1px solid rgb(var(--c-border));border-radius:6px;margin:4px 0 8px}.suggestion{border-left:3px solid rgb(var(--c-accent))}
</style>
