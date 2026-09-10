<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { bounded } from "../workbench/taskActions";
  import { Clipboard } from "@lucide/svelte";
  import { copyText } from "../desktop/clipboard";
  import TaskRuns from "./TaskRuns.svelte";
  import NativeNotificationSettings from "./NativeNotificationSettings.svelte";
  import TaskEnhancements from "./TaskEnhancements.svelte";
  import TaskManviAssist from "./TaskManviAssist.svelte";
  import SettingToggle from "./SettingToggle.svelte";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { askConfirm } from "../stores/modalStore";
  import { deleteTask, explainError, getTask, getTaskBrief, newID, putTask, registerRepository, STATUSES, STATUS_LABELS, taskDraft, taskWrite, WorkbenchError, type EnhancementField, type Repository, type Task, type TaskDraft, type TaskStatus, type WorkspaceCard } from "../workbench/client";
  import { deleteAttempt, deleteConfirmCopy, isRetryableDelete } from "../workbench/taskDelete";
  import { addableOpenTabs, openMembershipCandidates, type OpenTabRef } from "../workbench/openMembership";
  import { dueInputValue, parseDueInput } from "../workbench/taskOrganize";
  import { applyNotesToDraft, canAskManvi, formatDraftAgentCopy, wrapSavedBriefForAgent } from "../workbench/taskCompose";
  let { value, seed = null, repositories, workspaces, openTabs = [], primary = "", home = null, active = true, initialStatus = "inbox", onSaved, onClose }: {
    value: Task | null; seed?: Partial<TaskDraft> | null; repositories: Repository[]; workspaces: WorkspaceCard[]; openTabs?: OpenTabRef[];
    primary?: string; home?: string | null; active?: boolean; initialStatus?: TaskStatus;
    onSaved: (task: Task) => void; onClose: () => void;
  } = $props();
  // A mounted editor owns one snapshot. Background board updates never replace it.
  const initial = untrack(() => ({ value, seed, primary, home, initialStatus }));
  let current = $state(initial.value);
  let draft = $state<TaskDraft>(initial.value ? taskDraft(initial.value) : {
    title: "", description: "", kind: "feature", status: initial.initialStatus, priority: 1, severity: null,
    owner: null, due_at: null, labels: [], acceptance_criteria: [], repository_ids: initial.primary ? [initial.primary] : [],
    primary_repository_id: initial.primary, home_workspace_id: initial.home, position: Date.now(), locked_fields: [],
    ...initial.seed,
  });
  let criteria = $state(untrack(() => draft.acceptance_criteria.join("\n")));
  let labels = $state(untrack(() => draft.labels.join(", ")));
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
  let showOrganize = $state(Boolean(initial.value));
  let notes = $state("");
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  let confirming = $state(false);
  let reloading = $state(false);
  let sheet: HTMLElement;
  let disposed = false;
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
  function setFieldLock(field: EnhancementField, checked: boolean) {
    const fields = draft.locked_fields ?? [];
    draft.locked_fields = checked ? [...new Set([...fields, field])] : fields.filter((value) => value !== field);
    dirty = true;
  }
  export async function canLeave(): Promise<boolean> {
    if (saving || adding || reloading || confirming || enhancementBusy) return false;
    if (pending || pendingDelete) { error = "Retry the pending action before closing this task."; return false; }
    if (!dirty && !notes.trim()) return true;
    confirming = true;
    try { return await askConfirm({title:"Discard task edits?",message:"Your unsaved changes will be lost.",confirmLabel:"Discard edits",cancelLabel:"Keep editing",destructive:true}); }
    finally { confirming = false; }
  }
  async function close() { if (await canLeave()) onClose(); }
  function membership(id: string, checked: boolean) {
    draft.repository_ids = checked ? [...new Set([...draft.repository_ids, id])] : draft.repository_ids.filter((r) => r !== id);
    if (!draft.repository_ids.includes(draft.primary_repository_id)) draft.primary_repository_id = draft.repository_ids[0] ?? "";
    dirty = true;
  }
  async function save(): Promise<Task | null> {
    if (saving || adding || reloading || confirming || pendingDelete) return null;
    if (!pending && new TextEncoder().encode(draft.description).length > 65_536) { error = "Description is too long. Keep it below 64 KB."; return null; }
    saving = true; error = ""; note = "";
    try {
      if (!pending) {
        const extracted = applyNotesToDraft(draft, notes);
        if (extracted.extracted) {
          if (!draft.title.trim()) draft.title = extracted.title;
          if (!draft.description.trim()) draft.description = extracted.description;
        }
        draft.acceptance_criteria = criteria.split("\n").map((s) => s.trim()).filter(Boolean);
        draft.labels = labels.split(",").map((s) => s.trim()).filter(Boolean);
        pending = taskWrite(id, current?.revision ?? 0, draft);
      }
      const saved = await bounded(putTask(pending));
      if (saved.id !== id || saved.revision !== Number(pending.expected_revision) + 1) throw new WorkbenchError("protocol_error", "Task update confirmation does not match the request.");
      if (disposed) return null;
      current = saved; draft = taskDraft(saved); dirty = false; pending = null;
      note = "Saved"; onSaved(saved);
      return saved;
    } catch (cause) {
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) pending = null;
      return null;
    } finally { saving = false; }
  }
  async function prepareForManvi(): Promise<Task | null> {
    if (saving || adding || enhancementBusy || pendingDelete !== null) return null;
    const next = applyNotesToDraft(draft, notes);
    if (next.extracted) {
      draft.title = next.title;
      draft.description = next.description;
      dirty = true;
    }
    const blocked = canAskManvi(draft, notes);
    if (blocked) { error = blocked; return null; }
    if (!current || dirty || pending) return await save();
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
        if (packet && await copyText(packet)) markCopied(`Copied saved revision ${revision} for an agent${dirty ? "; unsaved edits are not included" : ""}`);
        else error = "Clipboard unavailable";
      } else {
        const packet = formatDraftAgentCopy({
          title: draft.title,
          description: draft.description,
          kind: draft.kind,
          status: draft.status,
          priority: draft.priority,
          owner: draft.owner,
          labels: labels.split(",").map((item) => item.trim()).filter(Boolean),
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
    if (!current || !await askConfirm({
      title: "Reload saved task?",
      message: "Replace your unsaved edits with the latest saved revision?",
      confirmLabel: "Reload",
      cancelLabel: "Keep editing",
    })) return;
    try { const latest = await getTask(id); current = latest; draft = taskDraft(latest); criteria = latest.acceptance_criteria.join("\n"); labels = latest.labels.join(", "); pending = null; dirty = false; error = ""; }
    catch (cause) { error = explainError(cause); }
  }
  async function remove() {
    if (!current || saving || adding || enhancementBusy) return;
    if (!pendingDelete) {
      const copy = deleteConfirmCopy([{ id: current.id, revision: current.revision, title: current.title }]);
      if (!await askConfirm({ title: copy.title, message: copy.message, confirmLabel: copy.confirmLabel, cancelLabel: "Keep task", destructive: true })) return;
      pendingDelete = deleteAttempt({ id: current.id, revision: current.revision, title: current.title }, newID());
    }
    if (!pendingDelete) return;
    saving = true; error = "";
    try {
      await bounded(deleteTask(pendingDelete.id, pendingDelete.expected_revision, pendingDelete.request_id));
      pendingDelete = null;
      onSaved(current);
      onClose();
    } catch (cause) {
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !isRetryableDelete(cause)) pendingDelete = null;
    } finally { saving = false; }
  }
  function applied(saved: Task) {
    current = saved; draft = taskDraft(saved); criteria = saved.acceptance_criteria.join("\n"); labels = saved.labels.join(", ");
    dirty = false; pending = null; note = `Saved revision ${saved.revision}`; onSaved(saved);
  }
</script>

<svelte:window onkeydown={(e) => {
    if (!active || !(e.target instanceof Node) || !sheet?.contains(e.target)) return;
    if (e.key === "Escape") { e.preventDefault(); void close(); return; }
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void save();
      }
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "c") {
        e.preventDefault();
        void copyForAgent();
      }
    }} />

<aside bind:this={sheet} class="task-editor gp-glass" aria-label={current ? "Task details" : "New task"}>
  <header>
    <div>
      <h2>{current ? "Task details" : "New task"}</h2>
      <small>{current ? `Revision ${current.revision}` : "Describe the work, then save."}</small>
    </div>
    <div class="header-actions">
      <button type="button" class="gp-btn" onclick={() => void copyForAgent()} disabled={!copyable || copying || saving || enhancementBusy || pending !== null} aria-label="Copy task for an AI agent" title={copyable ? "Copy a packet an AI agent can paste" : "Add a title, description, or notes first"}>
        <Clipboard size={12} /> {copying ? "Copying…" : copied ? "Copied" : "Copy for agent"}
      </button>
      <button type="button" class="gp-icon-btn" onclick={close} disabled={enhancementBusy || saving || adding || reloading || confirming || pending !== null || pendingDelete !== null} aria-label="Close task details">✕</button>
    </div>
  </header>
  <form
    onsubmit={(e) => { e.preventDefault(); void save(); }}
    oninput={() => { dirty = true; }}

  >
    <fieldset disabled={saving || pending !== null || pendingDelete !== null || enhancementBusy}>
      <TaskManviAssist
        task={current}
        {notes}
        onNotes={(value) => { notes = value; dirty = true; }}
        bind:title={draft.title}
        bind:description={draft.description}
        repositoryIds={draft.repository_ids}
        lockedFields={draft.locked_fields ?? []}
        prepareTask={prepareForManvi}
        onApplied={applied}
        onBusy={(busy) => { enhancementBusy = busy; }}
        disabled={saving || pending !== null || pendingDelete !== null}
        autofocus={!current}
      />
      <div class="pair"><label>Type<input class="gp-field" bind:value={draft.kind} list="task-kinds" required maxlength="64" /></label><label>Status<select class="gp-select" bind:value={draft.status}>{#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}</select></label></div>
      <datalist id="task-kinds">{#each ["issue", "bug", "feature", "improvement", "maintenance", "research", "documentation"] as kind}<option value={kind} ></option>{/each}</datalist>
      <label>Acceptance criteria<textarea class="gp-field" bind:value={criteria} rows="4" placeholder="One verifiable criterion per line" ></textarea></label>
      <SettingToggle
        label="Schedule and labels"
        description="Priority, due date, owner, labels, and home workspace."
        checked={showOrganize}
        onchange={(next) => { showOrganize = next; }}
      />
      {#if showOrganize}
        <div class="pair"><label>Priority<select class="gp-select" bind:value={draft.priority}><option value={0}>Urgent</option><option value={1}>High</option><option value={2}>Normal</option><option value={3}>Low</option></select></label><label>Severity<select class="gp-select" bind:value={draft.severity}><option value={null}>None</option>{#each ["low", "medium", "high", "critical"] as severity}<option value={severity}>{severity}</option>{/each}</select></label></div>
        <div class="pair">
          <label>Owner<input class="gp-field" value={draft.owner ?? ""} oninput={(e) => { draft.owner = e.currentTarget.value || null; }} maxlength="300" /></label>
          <label>Due<input class="gp-field" type="datetime-local" value={dueInputValue(draft.due_at)} oninput={(e) => { draft.due_at = parseDueInput(e.currentTarget.value); }} /></label>
        </div>
        <label>Labels<input class="gp-field" bind:value={labels} placeholder="Separate labels with commas" /></label>
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
      <SettingToggle
        label="More details"
        description="Manvi field locks, notifications, and the saved brief."
        checked={showDetails}
        onchange={(next) => { showDetails = next; }}
      />
      {#if showDetails}
        <fieldset class="enhancement-locks"><legend>Enhancement field locks</legend>
          <SettingToggle label="Keep title during enhancements" checked={draft.locked_fields?.includes("title") ?? false} onchange={(next) => setFieldLock("title", next)} />
          <SettingToggle label="Keep description during enhancements" checked={draft.locked_fields?.includes("description") ?? false} onchange={(next) => setFieldLock("description", next)} />
          <small>You can still edit these fields yourself. Save to apply the locks.</small>
        </fieldset>
        {#if current}<NativeNotificationSettings scope={{ kind: "global" }} taskID={current.id} />{/if}
      {/if}
    </fieldset>
    {#if error}<p role="alert" class="error">{error}</p>{/if}
    {#if pending}<p>The save result is uncertain. Retry the same write to reconcile it before editing further.</p>{/if}
    {#if pendingDelete}<p>The delete result is uncertain. Retry the same delete to reconcile it before editing further.</p>{/if}
    <footer>
      <button class="gp-btn-primary" disabled={saving || adding || enhancementBusy || pendingDelete !== null || !draft.repository_ids.length} type="submit">{saving && !pendingDelete ? "Saving…" : pending ? "Retry save" : "Save task"}</button>
      {#if current}
        <button type="button" class="gp-btn" onclick={reload} disabled={saving || enhancementBusy || pendingDelete !== null}>Reload saved</button>
        <button type="button" class="gp-btn-danger" onclick={remove} disabled={saving || adding || enhancementBusy || pending !== null}>{pendingDelete ? "Retry delete" : "Delete"}</button>
      {/if}
      {#if note}<p role="status" class="footer-note">{note}</p>{/if}
    </footer>
  </form>
  {#if current}<TaskEnhancements task={current} disabled={dirty || saving || pending !== null || pendingDelete !== null} onApplied={applied} onBusy={(busy) => { enhancementBusy = busy; }} />{/if}
  {#if current}<TaskRuns task={current} {repositories} {active} disabled={dirty || saving || pending !== null || pendingDelete !== null || enhancementBusy} />{/if}
</aside>

<style>
  .enhancement-locks{margin:0 0 14px}
  .task-editor{width:min(430px,48vw);flex-shrink:0;border-left:1px solid rgb(var(--c-border) / 0.65);background:transparent;overflow:auto;padding:18px;color:rgb(var(--c-text));display:flex;flex-direction:column}
  header,footer,.pair,.header-actions{display:flex;gap:10px;align-items:center}header{justify-content:space-between;margin-bottom:18px;position:sticky;top:0;z-index:1;padding-bottom:10px;background:rgb(var(--c-surface) / 0.82)}h2{font-size:16px;font-weight:650;margin:0}small,legend{color:rgb(var(--c-text-muted));font-size:11px}form{font-size:12px;flex:1;min-height:0}fieldset{border:0;padding:0;min-width:0}label{display:flex;flex-direction:column;gap:6px;margin-bottom:13px;flex:1}.pair{align-items:flex-start}input,textarea,select{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}textarea{resize:vertical}button:disabled{opacity:.5}footer{flex-wrap:wrap;position:sticky;bottom:0;padding:10px 0 0;background:rgb(var(--c-surface) / 0.82);border-top:1px solid rgb(var(--c-border) / 0.45)}.footer-note{margin:0;flex:1;min-width:8rem;color:rgb(var(--c-text-muted))}.check{flex-direction:row;align-items:center;margin:5px 0}.check input{width:auto}.repositories{max-height:160px;overflow:auto;margin:12px 0}.open-mark{color:rgb(var(--c-text-muted));font-size:10px;margin-left:6px}.error{color:#dc6565}p{font-size:12px;margin:10px 0}
</style>
