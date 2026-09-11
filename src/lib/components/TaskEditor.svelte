<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { bounded } from "../workbench/taskActions";
  import { Clipboard } from "@lucide/svelte";
  import { copyText } from "../desktop/clipboard";
  import TaskRuns from "./TaskRuns.svelte";
  import NativeNotificationSettings from "./NativeNotificationSettings.svelte";
  import TaskManviAssist from "./TaskManviAssist.svelte";
  import SettingToggle from "./SettingToggle.svelte";
  import LabelInput from "./LabelInput.svelte";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { askConfirm } from "../stores/modalStore";
  import { deleteTask, explainError, getTask, getTaskBrief, newID, putTask, registerRepository, STATUSES, STATUS_LABELS, taskDraft, taskWrite, WorkbenchError, type EnhancementField, type Repository, type Task, type TaskDraft, type TaskStatus, type WorkspaceCard } from "../workbench/client";
  import { deleteAttempt, deleteConfirmCopy, isRetryableDelete } from "../workbench/taskDelete";
  import { addableOpenTabs, openMembershipCandidates, type OpenTabRef } from "../workbench/openMembership";
  import { dueInputValue, parseDueInput } from "../workbench/taskOrganize";
  import { applyNotesToDraft, canAskManvi, consumeNotes, formatDraftAgentCopy, wrapSavedBriefForAgent } from "../workbench/taskCompose";

  const KIND_OPTIONS = ["issue", "bug", "feature", "improvement", "maintenance", "research", "documentation"] as const;
  const SEVERITY_OPTIONS = [
    { value: "low", label: "Low" },
    { value: "medium", label: "Medium" },
    { value: "high", label: "High" },
    { value: "critical", label: "Critical" },
  ] as const;

  let { value, seed = null, repositories, workspaces, openTabs = [], primary = "", home = null, active = true, initialStatus = "inbox", onSaved, onClose }: {
    value: Task | null; seed?: Partial<TaskDraft> | null; repositories: Repository[]; workspaces: WorkspaceCard[]; openTabs?: OpenTabRef[];
    primary?: string; home?: string | null; active?: boolean; initialStatus?: TaskStatus;
    onSaved: (task: Task) => void; onClose: () => void;
  } = $props();
  // A mounted editor owns one snapshot. Background board updates never replace it.
  const initial = untrack(() => ({ value, seed, primary, home, initialStatus }));
  let current = $state(initial.value);
  let draft = $state<TaskDraft>((() => {
    const base = initial.value ? taskDraft(initial.value) : {
      title: "", description: "", kind: "feature", status: initial.initialStatus, priority: 2, severity: null,
      owner: null, due_at: null, labels: [], acceptance_criteria: [], repository_ids: initial.primary ? [initial.primary] : [],
      primary_repository_id: initial.primary, home_workspace_id: initial.home, position: Date.now(), locked_fields: [] as EnhancementField[],
      ...initial.seed,
    };
    return { ...base, locked_fields: base.locked_fields ?? [] };
  })());
  let criteria = $state(untrack(() => draft.acceptance_criteria.join("\n")));
  let labelChips = $state<string[]>(untrack(() => [...draft.labels]));
  let error = $state("");
  let note = $state("");
  let saving = $state(false);
  let pending = $state<Record<string, unknown> | null>(null);
  let pendingDelete = $state<{ id: string; request_id: string; expected_revision: number } | null>(null);
  let dirty = $state(false);
  let enhancementBusy = $state(false);
  let copying = $state(false);
  let extras = $state<Repository[]>([]);
  let adding = $state(false);
  let showDetails = $state(Boolean(initial.seed));
  let showOrganize = $state(true);
  let notes = $state("");
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  let confirming = $state(false);
  let reloading = $state(false);
  let shortcutBlocked = $state("");
  let customKind = $state(false);
  let sheet: HTMLElement;
  let disposed = false;
  const kindIsCustom = $derived(!KIND_OPTIONS.includes(draft.kind as typeof KIND_OPTIONS[number]));
  $effect(() => { if (kindIsCustom) customKind = true; });

  onMount(() => {
    const opener = document.activeElement;
    if (initial.value) sheet.querySelector<HTMLInputElement>('input[name="task-title"]')?.focus();
    return () => { if (opener instanceof HTMLElement && opener.isConnected) opener.focus(); };
  });
  onDestroy(() => { disposed = true; if (copiedTimer) clearTimeout(copiedTimer); });
  const copyable = $derived(Boolean(current || draft.title.trim() || draft.description.trim() || notes.trim()));
  const id = initial.value?.id ?? newID();
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const known = $derived.by(() => {
    const map = new Map(repositories.map((repo) => [repo.id, repo]));
    for (const extra of extras) map.set(extra.id, extra);
    return [...map.values()];
  });
  const addable = $derived(addableOpenTabs(openMembershipCandidates(openTabs, known, draft.repository_ids, pathOpts)));
  async function addOpenPaths(paths: string[]) {
    if (adding || pending !== null || paths.length === 0) return;
    adding = true; error = "";
    try {
      for (const path of paths) {
        const repo = await registerRepository(path);
        if (disposed) return;
        if (!extras.some((item) => item.id === repo.id)) extras = [...extras, repo];
        membership(repo.id, true);
      }
    } catch (cause) { error = explainError(cause); }
    finally { adding = false; }
  }
  export async function canLeave(): Promise<boolean> {
    if (saving || adding || reloading || confirming || enhancementBusy) return false;
    if (pending || pendingDelete) { error = "Retry the pending action before closing this task."; return false; }
    if (!dirty && !notes.trim()) return true;
    confirming = true;
    try { return await askConfirm({title:"Discard task edits?",message:"Your unsaved changes will be lost.",confirmLabel:"Discard edits",cancelLabel:"Keep editing",destructive:true}); }
    finally { confirming = false; }
  }
  async function close() {
    if (enhancementBusy) { shortcutBlocked = "Wait for Manvi to finish before closing."; return; }
    if (await canLeave()) onClose();
  }
  function membership(id: string, checked: boolean) {
    draft.repository_ids = checked ? [...new Set([...draft.repository_ids, id])] : draft.repository_ids.filter((r) => r !== id);
    if (!draft.repository_ids.includes(draft.primary_repository_id)) draft.primary_repository_id = draft.repository_ids[0] ?? "";
    dirty = true;
  }
  function applyExtractedNotes(): boolean {
    const next = consumeNotes(draft, notes);
    if (!next.extracted) return true;
    if (new TextEncoder().encode(next.description).length > 65_536) {
      error = "Description is too long. Keep it below 64 KB.";
      return false;
    }
    draft.title = next.title;
    draft.description = next.description;
    notes = next.notes;
    dirty = true;
    return true;
  }
  async function save(fromManvi = false): Promise<Task | null> {
    if (saving || adding || reloading || confirming || pendingDelete || (enhancementBusy && !fromManvi)) return null;
    if (!pending && !applyExtractedNotes()) return null;
    if (!pending && (!draft.title.trim() || !draft.kind.trim())) { error = "Add a title and task type before saving."; return null; }
    if (!pending && new TextEncoder().encode(draft.description).length > 65_536) { error = "Description is too long. Keep it below 64 KB."; return null; }
    saving = true; error = ""; note = ""; shortcutBlocked = "";
    try {
      if (!pending) {
        draft.acceptance_criteria = criteria.split("\n").map((s) => s.trim()).filter(Boolean);
        draft.labels = [...labelChips];
        pending = taskWrite(id, current?.revision ?? 0, draft);
      }
      const saved = await bounded(putTask(pending));
      if (saved.id !== id || saved.revision !== Number(pending.expected_revision) + 1) throw new WorkbenchError("protocol_error", "Task update confirmation does not match the request.");
      if (disposed) return null;
      current = saved; draft = { ...taskDraft(saved), locked_fields: saved.locked_fields ?? [] }; criteria = saved.acceptance_criteria.join("\n"); labelChips = [...saved.labels]; dirty = false; pending = null;
      note = "Saved"; onSaved(saved);
      return saved;
    } catch (cause) {
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) pending = null;
      return null;
    } finally { saving = false; }
  }
  async function prepareForManvi(): Promise<Task | null> {
    if (saving || adding || reloading || confirming || pendingDelete !== null) {
      error = "Wait for the current task action to finish.";
      throw new Error(error);
    }
    if (!applyExtractedNotes()) throw new Error(error || "Could not use these notes.");
    const blocked = canAskManvi(draft, notes);
    if (blocked) { error = blocked; throw new Error(blocked); }
    if (!current || dirty || pending) {
      const saved = await save(true);
      if (disposed) return null;
      if (!saved) throw new Error(error || "Could not save a draft for Manvi.");
      return saved;
    }
    return current;
  }
  function markCopied(message: string) {
    note = message;
    copied = true;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => { copied = false; }, 2000);
  }
  async function copyForAgent() {
    if (copying || saving || pending !== null || enhancementBusy || !copyable) return;
    copying = true; error = ""; note = "";
    try {
      if (current) {
        const revision = current.revision;
        const saved = await bounded(getTaskBrief(id, revision));
        if (disposed) return;
        if (current.revision !== revision) throw new Error("The saved task changed while loading its brief. Copy the latest revision again.");
        const packet = wrapSavedBriefForAgent(saved.markdown);
        if (packet && await copyText(packet)) markCopied(`Copied revision ${revision}${dirty ? " (unsaved edits omitted)" : ""}`);
        else error = "Clipboard unavailable";
      } else {
        const extracted = applyNotesToDraft(draft, notes);
        const packet = formatDraftAgentCopy({
          title: extracted.title,
          description: extracted.description,
          kind: draft.kind,
          status: draft.status,
          priority: draft.priority,
          owner: draft.owner,
          labels: [...labelChips],
          acceptance_criteria: criteria.split("\n").map((item) => item.trim()).filter(Boolean),
          repositoryNames: draft.repository_ids.map((repo) => known.find((item) => item.id === repo)?.name ?? repo),
        });
        if (packet && await copyText(packet)) markCopied("Copied unsaved draft for an agent");
        else error = packet ? "Clipboard unavailable" : "Add a title or description to copy";
      }
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { copying = false; }
  }
  async function reload() {
    if (!current || saving || adding || enhancementBusy || reloading || confirming || pendingDelete) return;
    reloading = true;
    confirming = true;
    try {
      if (!await askConfirm({title: "Reload saved task?", message: "Replace your unsaved edits with the latest saved revision?", confirmLabel: "Reload", cancelLabel: "Keep editing"}) || disposed) return;
      const latest = await bounded(getTask(id));
      if (disposed) return;
      if (latest.id !== id) throw new WorkbenchError("protocol_error", "Loaded task does not match this editor.");
      current = latest; draft = { ...taskDraft(latest), locked_fields: latest.locked_fields ?? [] }; criteria = latest.acceptance_criteria.join("\n"); labelChips = [...latest.labels]; notes = ""; pending = null; dirty = false; error = "";
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) { confirming = false; reloading = false; } }
  }
  async function remove() {
    if (!current || saving || adding || enhancementBusy) return;
    if (!pendingDelete) {
      const copy = deleteConfirmCopy([{ id: current.id, revision: current.revision, title: current.title }]);
      if (!await askConfirm({ title: copy.title, message: copy.message, confirmLabel: copy.confirmLabel, cancelLabel: "Keep task", destructive: true })) return;
      if (disposed) return;
      pendingDelete = deleteAttempt({ id: current.id, revision: current.revision, title: current.title }, newID());
    }
    if (!pendingDelete) return;
    saving = true; error = "";
    try {
      await bounded(deleteTask(pendingDelete.id, pendingDelete.expected_revision, pendingDelete.request_id));
      if (disposed) return;
      pendingDelete = null;
      onSaved(current);
      onClose();
    } catch (cause) {
      if (disposed) return;
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !isRetryableDelete(cause)) pendingDelete = null;
    } finally { if (!disposed) saving = false; }
  }
  function applied(saved: Task) {
    current = saved; draft = { ...taskDraft(saved), locked_fields: saved.locked_fields ?? [] }; criteria = saved.acceptance_criteria.join("\n"); labelChips = [...saved.labels];
    dirty = false; pending = null; note = `Saved revision ${saved.revision}`; onSaved(saved);
  }
  function onKindSelect(value: string) {
    if (value === "__custom__") { customKind = true; return; }
    customKind = false;
    draft.kind = value;
    dirty = true;
  }
</script>

<svelte:window onkeydown={(e) => {
    if (!active || !(e.target instanceof Node) || !sheet?.contains(e.target)) return;
    if (e.key === "Escape") {
      e.preventDefault();
      if (enhancementBusy) { shortcutBlocked = "Wait for Manvi to finish before closing."; return; }
      void close();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      if (enhancementBusy) { shortcutBlocked = "Wait for Manvi to finish before saving."; return; }
      void save();
    }
    if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "c") {
      e.preventDefault();
      void copyForAgent();
    }
  }} />

<aside bind:this={sheet} class="task-editor gp-glass bg-surface" aria-label={current ? "Task details" : "New task"} data-task-revision={current?.revision ?? ""}>
  <header class="gp-glass shadow-float">
    <div>
      <h2>{current ? "Task details" : "New task"}</h2>
      <small>{dirty ? "Unsaved" : current ? `Revision ${current.revision}` : "Describe the work, then save."}</small>
    </div>
    <div class="header-actions">
      <button type="button" class="gp-btn" onclick={() => void copyForAgent()} disabled={!copyable || copying || saving || enhancementBusy || pending !== null} aria-label="Copy task for an AI agent" title={copyable ? "Copy a packet an AI agent can paste" : "Add a title, description, or notes first"}>
        <Clipboard size={12} /> {copying ? "Copying…" : copied ? "Copied" : "Copy for agent"}
      </button>
      <button type="button" class="gp-icon-btn" onclick={close} disabled={enhancementBusy || saving || adding || reloading || confirming || pending !== null || pendingDelete !== null} aria-label="Close task details">✕</button>
    </div>
  </header>
  <div class="sheet-body">
  <form novalidate
    onsubmit={(e) => { e.preventDefault(); void save(); }}
    oninput={() => { dirty = true; }}
    onchange={() => { dirty = true; }}
  >
    <fieldset disabled={saving || reloading || pending !== null || pendingDelete !== null}>
      <TaskManviAssist
        task={current}
        {notes}
        {dirty}
        {active}
        onNotes={(value) => { notes = value; dirty = true; }}
        bind:title={draft.title}
        bind:description={draft.description}
        bind:lockedFields={draft.locked_fields!}
        repositoryIds={draft.repository_ids}
        prepareTask={prepareForManvi}
        onApplied={applied}
        onBusy={(busy) => { enhancementBusy = busy; }}
        disabled={saving || reloading || pending !== null || pendingDelete !== null}
        autofocus={!current}
      />
      <fieldset disabled={enhancementBusy}>
      <div class="pair">
        <label>Type
          {#if customKind || kindIsCustom}
            <input class="gp-field" bind:value={draft.kind} required maxlength="64" placeholder="Custom type" />
            <button type="button" class="gp-btn kind-preset" disabled={enhancementBusy} onclick={() => { customKind = false; draft.kind = "feature"; dirty = true; }}>Use preset</button>
          {:else}
            <select class="gp-select" value={draft.kind} onchange={(e) => onKindSelect(e.currentTarget.value)}>
              {#each KIND_OPTIONS as kind}<option value={kind}>{kind}</option>{/each}
              <option value="__custom__">Custom…</option>
            </select>
          {/if}
        </label>
        <label>Status<select class="gp-select" bind:value={draft.status}>{#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}</select></label>
      </div>
      <label>Acceptance criteria<textarea class="gp-field" bind:value={criteria} rows="4" maxlength="65536" placeholder="One verifiable criterion per line"></textarea></label>
      <SettingToggle
        label="Schedule and labels"
        description="Priority, due date, owner, labels, and home workspace."
        checked={showOrganize}
        onchange={(next) => { showOrganize = next; }}
      />
      {#if showOrganize}
        <div class="pair">
          <label>Priority<select class="gp-select" bind:value={draft.priority}><option value={0}>Urgent</option><option value={1}>High</option><option value={2}>Normal</option><option value={3}>Low</option></select></label>
          <label>Severity<select class="gp-select" bind:value={draft.severity}><option value={null}>None</option>{#each SEVERITY_OPTIONS as severity}<option value={severity.value}>{severity.label}</option>{/each}</select></label>
        </div>
        <div class="pair">
          <label>Owner<input class="gp-field" value={draft.owner ?? ""} oninput={(e) => { draft.owner = e.currentTarget.value || null; }} maxlength="300" /></label>
          <label>Due<input class="gp-field" type="datetime-local" value={dueInputValue(draft.due_at)} oninput={(e) => { draft.due_at = parseDueInput(e.currentTarget.value); }} /></label>
        </div>
        <label>Labels
          <LabelInput bind:value={labelChips} disabled={enhancementBusy} />
        </label>
        <label>Home workspace<select class="gp-select" bind:value={draft.home_workspace_id}><option value={null}>None</option>{#each workspaces as group (group.id)}<option value={group.id}>{group.name}{group.archived ? " (archived)" : ""}</option>{/each}</select></label>
      {/if}
      <fieldset class="repositories"><legend>Linked repositories</legend>{#each known as repo (repo.id)}<label class="check"><input type="checkbox" checked={draft.repository_ids.includes(repo.id)} onchange={(e) => membership(repo.id, e.currentTarget.checked)} />{repo.name}</label>{/each}
      {#if addable.length}
        <small>Open in GitPulse</small>
        {#each addable as tab (tab.path)}
          <label class="check" title={tab.path}>
            <input type="checkbox" checked={false} disabled={adding} onchange={(e) => { e.currentTarget.checked = false; void addOpenPaths([tab.path]); }} />
            {tab.label}<span class="open-mark">Open</span>
          </label>
        {/each}
        {#if addable.length > 1}<button type="button" disabled={adding} onclick={() => void addOpenPaths(addable.map((tab) => tab.path))}>Add all open</button>{/if}
      {/if}
      {#each draft.repository_ids.filter((id) => !known.some((r) => r.id === id)) as missing (missing)}<small>Linked repository {missing} (load more repositories to edit)</small>{/each}</fieldset>
      <label>Primary repository<select class="gp-select" bind:value={draft.primary_repository_id} required><option value="" disabled>Select a linked repository</option>{#each draft.repository_ids as repo (repo)}<option value={repo}>{known.find((r) => r.id === repo)?.name ?? repo}</option>{/each}</select></label>
      {#if current}
        <div class="notifications-row">
          <p class="notifications-label">Notifications (profile-wide; mute this task)</p>
          <NativeNotificationSettings scope={{ kind: "global" }} taskID={current.id} />
        </div>
      {/if}
      <SettingToggle
        label="More details"
        description="Saved brief extras."
        checked={showDetails}
        onchange={(next) => { showDetails = next; }}
      />
      {#if showDetails}
        <p class="meta">Field locks live in the Manvi section above.</p>
      {/if}
      </fieldset>
    </fieldset>
    {#if error}<p role="alert" class="error">{error}</p>{/if}
    {#if shortcutBlocked}<p role="status" class="meta">{shortcutBlocked}</p>{/if}
    {#if pending}<p role="status">Save result uncertain. Retry the same write before editing further.</p>{/if}
    {#if pendingDelete}<p role="status">Delete result uncertain. Retry the same delete before editing further.</p>{/if}
    <footer>
      <button class="gp-btn-primary" disabled={saving || adding || enhancementBusy || pendingDelete !== null || !draft.repository_ids.length || (!pending && ((!draft.title.trim() && !notes.trim()) || !draft.kind.trim()))} type="submit">{saving && !pendingDelete ? "Saving…" : pending ? "Retry save" : "Save task"}</button>
      {#if current}
        <button type="button" class="gp-btn" onclick={reload} disabled={saving || enhancementBusy || pendingDelete !== null}>Reload saved</button>
        <button type="button" class="gp-btn-danger" onclick={remove} disabled={saving || adding || enhancementBusy || pending !== null}>{pendingDelete ? "Retry delete" : "Delete"}</button>
      {/if}
      {#if note}<p role="status" class="footer-note">{note}</p>{/if}
    </footer>
  </form>
  {#if current}<TaskRuns task={current} {repositories} {active} disabled={dirty || saving || pending !== null || pendingDelete !== null || enhancementBusy} />{/if}
  </div>
</aside>

<style>
  .task-editor{width:min(430px,48vw);flex-shrink:0;min-width:0;min-height:0;border-left:1px solid rgb(var(--c-border) / 0.65);overflow:hidden;padding:0;color:rgb(var(--c-text));display:flex;flex-direction:column}
  .sheet-body{flex:1;min-height:0;overflow:auto;padding:0 18px 18px}
  header,footer,.pair,.header-actions{display:flex;gap:10px;align-items:center}header{padding:16px 18px;justify-content:space-between;flex-shrink:0;z-index:1;padding-bottom:10px;background:rgb(var(--c-surface) / 0.82)}h2{font-size:16px;font-weight:650;margin:0}small,legend,.meta,.notifications-label{color:rgb(var(--c-text-muted));font-size:11px}form{font-size:12px;min-width:0}fieldset{border:0;padding:0;min-width:0}label{display:flex;flex-direction:column;gap:6px;margin-bottom:13px;flex:1}.pair{align-items:flex-start}input,textarea,select{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}textarea{resize:vertical}button:disabled{opacity:.5}footer{flex-wrap:wrap;position:sticky;bottom:0;z-index:1;padding:10px 0 0;background:rgb(var(--c-surface));border-top:1px solid rgb(var(--c-border) / 0.45)}.footer-note{margin:0;flex:1;min-width:8rem;color:rgb(var(--c-text-muted))}.check{flex-direction:row;align-items:center;margin:5px 0}.check input{width:auto}.repositories{max-height:160px;overflow:auto;margin:12px 0}.open-mark{color:rgb(var(--c-text-muted));font-size:10px;margin-left:6px}.error{color:#dc6565}p{font-size:12px;margin:10px 0}.notifications-row{margin:12px 0 16px}.kind-preset{margin-top:6px;align-self:flex-start}
</style>
