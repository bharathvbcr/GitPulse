<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { copyText } from "../desktop/clipboard";
  import TaskRuns from "./TaskRuns.svelte";
  import NativeNotificationSettings from "./NativeNotificationSettings.svelte";
  import TaskEnhancements from "./TaskEnhancements.svelte";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { explainError, getTask, getTaskBrief, newID, putTask, registerRepository, request, STATUSES, STATUS_LABELS, taskDraft, taskWrite, WorkbenchError, type EnhancementField, type Repository, type Task, type TaskDraft, type WorkspaceCard } from "../workbench/client";
  import { addableOpenTabs, openMembershipCandidates, type OpenTabRef } from "../workbench/openMembership";
  let { value, repositories, workspaces, openTabs = [], primary = "", home = null, active = true, onSaved, onClose }: {
    value: Task | null; repositories: Repository[]; workspaces: WorkspaceCard[]; openTabs?: OpenTabRef[];
    primary?: string; home?: string | null; active?: boolean; onSaved: (task: Task) => void; onClose: () => void;
  } = $props();
  // A mounted editor owns one snapshot. Background board updates never replace it.
  const initial = untrack(() => ({ value, primary, home }));
  let current = $state(initial.value);
  let draft = $state<TaskDraft>(initial.value ? taskDraft(initial.value) : {
    title: "", description: "", kind: "feature", status: "inbox", priority: 1, severity: null,
    owner: null, due_at: null, labels: [], acceptance_criteria: [], repository_ids: initial.primary ? [initial.primary] : [],
    primary_repository_id: initial.primary, home_workspace_id: initial.home, position: Date.now(), locked_fields: [],
  });
  let criteria = $state(untrack(() => draft.acceptance_criteria.join("\n")));
  let labels = $state(untrack(() => draft.labels.join(", ")));
  let error = $state("");
  let note = $state("");
  let saving = $state(false);
  let pending = $state<Record<string, unknown> | null>(null);
  let dirty = $state(false);
  let enhancementBusy = $state(false);
  let copying = $state(false);
  let extras = $state<Repository[]>([]);
  let adding = $state(false);
  let disposed = false;
  onDestroy(() => { disposed = true; });
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
  function close() { if (!enhancementBusy && ((!dirty && !pending) || window.confirm("Close this task and discard unsaved edits?"))) onClose(); }
  function membership(id: string, checked: boolean) {
    draft.repository_ids = checked ? [...new Set([...draft.repository_ids, id])] : draft.repository_ids.filter((r) => r !== id);
    if (!draft.repository_ids.includes(draft.primary_repository_id)) draft.primary_repository_id = draft.repository_ids[0] ?? "";
    dirty = true;
  }
  async function save() {
    if (saving) return;
    saving = true; error = ""; note = "";
    try {
      if (!pending) {
        draft.acceptance_criteria = criteria.split("\n").map((s) => s.trim()).filter(Boolean);
        draft.labels = labels.split(",").map((s) => s.trim()).filter(Boolean);
        pending = taskWrite(id, current?.revision ?? 0, draft);
      }
      const saved = await putTask(pending);
      current = saved; draft = taskDraft(saved); dirty = false; pending = null;
      note = "Saved"; onSaved(saved);
    } catch (cause) {
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error"].includes(cause.code)) pending = null;
    } finally { saving = false; }
  }
  async function reload() {
    if (!current || !window.confirm("Replace your unsaved edits with the latest saved revision?")) return;
    try { const latest = await getTask(id); current = latest; draft = taskDraft(latest); criteria = latest.acceptance_criteria.join("\n"); labels = latest.labels.join(", "); pending = null; dirty = false; error = ""; }
    catch (cause) { error = explainError(cause); }
  }
  async function copy() {
    if (!current || copying || saving || pending !== null || enhancementBusy) return;
    copying = true; error = ""; note = "";
    const revision = current.revision;
    try {
      const saved = await getTaskBrief(id, revision);
      if (disposed) return;
      if (current.revision !== revision) throw new Error("The saved task changed while loading its brief. Copy the latest revision again.");
      note = await copyText(saved.markdown) ? `Copied saved revision ${revision}${dirty ? "; unsaved edits are not included" : ""}` : "Clipboard unavailable";
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { copying = false; }
  }
  async function remove() {
    if (!current || !window.confirm(`Delete “${current.title}”? Its history will be preserved.`)) return;
    saving = true;
    try { await request("items.delete", { id, expected_revision: current.revision, request_id: newID() }); onSaved(current); onClose(); }
    catch (cause) { error = explainError(cause); }
    finally { saving = false; }
  }
  function applied(saved: Task) {
    current = saved; draft = taskDraft(saved); criteria = saved.acceptance_criteria.join("\n"); labels = saved.labels.join(", ");
    dirty = false; pending = null; note = `Saved revision ${saved.revision}`; onSaved(saved);
  }
</script>

<aside class="task-editor" aria-label={current ? "Task details" : "New task"}>
  <header><div><h2>{current ? "Task details" : "New task"}</h2><small>{current ? `Revision ${current.revision}` : "Link at least one repository"}</small></div><button type="button" onclick={close} disabled={enhancementBusy} aria-label="Close task details">✕</button></header>
  <form onsubmit={(e) => { e.preventDefault(); void save(); }} oninput={() => { dirty = true; }}>
    <fieldset disabled={saving || pending !== null || enhancementBusy}>
      <label>Title<input bind:value={draft.title} required maxlength="300" placeholder="Describe the result to achieve" /></label>
      <div class="pair"><label>Type<input bind:value={draft.kind} list="task-kinds" required maxlength="64" /></label><label>Status<select bind:value={draft.status}>{#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}</select></label></div>
      <datalist id="task-kinds">{#each ["issue", "bug", "feature", "improvement", "maintenance", "research", "documentation"] as kind}<option value={kind} ></option>{/each}</datalist>
      <label>Description<textarea bind:value={draft.description} rows="8" maxlength="65536" placeholder="What happens, what should happen, evidence, and constraints" ></textarea></label>
      <label>Acceptance criteria<textarea bind:value={criteria} rows="4" placeholder="One verifiable criterion per line" ></textarea></label>
      <div class="pair"><label>Priority<select bind:value={draft.priority}><option value={0}>Urgent</option><option value={1}>High</option><option value={2}>Normal</option><option value={3}>Low</option></select></label><label>Severity<select bind:value={draft.severity}><option value={null}>None</option>{#each ["low", "medium", "high", "critical"] as severity}<option value={severity}>{severity}</option>{/each}</select></label></div>
      <label>Owner<input value={draft.owner ?? ""} oninput={(e) => { draft.owner = e.currentTarget.value || null; }} maxlength="300" /></label>
      <label>Labels<input bind:value={labels} placeholder="Separate labels with commas" /></label>
      <label>Home workspace<select bind:value={draft.home_workspace_id}><option value={null}>None</option>{#each workspaces as group (group.id)}<option value={group.id}>{group.name}{group.archived ? " (archived)" : ""}</option>{/each}</select></label>
      <details>
        <summary>Details</summary>
        <fieldset class="enhancement-locks"><legend>Enhancement field locks</legend>
          <label class="check"><input type="checkbox" checked={draft.locked_fields?.includes("title") ?? false} onchange={(e) => setFieldLock("title", e.currentTarget.checked)} />Keep title during enhancements</label>
          <label class="check"><input type="checkbox" checked={draft.locked_fields?.includes("description") ?? false} onchange={(e) => setFieldLock("description", e.currentTarget.checked)} />Keep description during enhancements</label>
          <small>You can still edit these fields yourself. Save to apply the locks.</small>
        </fieldset>
        {#if current}<NativeNotificationSettings scope={{ kind: "global" }} taskID={current.id} />{/if}
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
        <label>Primary repository<select bind:value={draft.primary_repository_id} required><option value="" disabled>Select a linked repository</option>{#each draft.repository_ids as repo (repo)}<option value={repo}>{known.find((r) => r.id === repo)?.name ?? repo}</option>{/each}</select></label>
        {#if current}<button type="button" onclick={copy} disabled={copying || saving || enhancementBusy || pending !== null}>{copying ? "Loading brief…" : "Copy saved brief"}</button>{/if}
      </details>
    </fieldset>
    {#if error}<p role="alert" class="error">{error}</p>{/if}
    {#if pending}<p>The save result is uncertain. Retry the same write to reconcile it before editing further.</p>{/if}
    <footer><button class="primary" disabled={saving || adding || enhancementBusy || !draft.repository_ids.length} type="submit">{saving ? "Saving…" : pending ? "Retry save" : "Save task"}</button>{#if current}<button type="button" onclick={reload} disabled={saving || enhancementBusy}>Reload saved</button><button type="button" onclick={remove} disabled={saving || adding || enhancementBusy || pending !== null}>Delete</button>{/if}</footer>
    <p role="status">{note}</p>
  </form>
  {#if current}<TaskEnhancements task={current} disabled={dirty || saving || pending !== null} onApplied={applied} onBusy={(busy) => { enhancementBusy = busy; }} />{/if}
  {#if current}<TaskRuns task={current} {repositories} {active} disabled={dirty || saving || pending !== null || enhancementBusy} />{/if}
</aside>

<style>
  .enhancement-locks{margin:0 0 14px}details{margin:12px 0}summary{cursor:pointer;color:rgb(var(--c-text-muted));font-size:12px}
  .task-editor{width:min(430px,48vw);flex-shrink:0;border-left:1px solid rgb(var(--c-border));background:rgb(var(--c-surface));overflow:auto;padding:18px;color:rgb(var(--c-text))}
  header,footer,.pair{display:flex;gap:10px;align-items:center}header{justify-content:space-between;margin-bottom:18px}h2{font-size:16px;font-weight:650;margin:0}small,legend{color:rgb(var(--c-text-muted));font-size:11px}form{font-size:12px}fieldset{border:0;padding:0;min-width:0}label{display:flex;flex-direction:column;gap:6px;margin-bottom:13px;flex:1}.pair{align-items:flex-start}input,textarea,select{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg));color:inherit;min-width:0}textarea{resize:vertical}button{padding:6px 10px;border:1px solid rgb(var(--c-border));border-radius:7px;font-size:12px}button:hover{background:rgb(var(--c-surface-hover))}button:disabled{opacity:.5}.primary{background:rgb(var(--c-accent));color:white}footer{flex-wrap:wrap}.check{flex-direction:row;align-items:center;margin:5px 0}.check input{width:auto}.repositories{max-height:160px;overflow:auto;margin:12px 0}.open-mark{color:rgb(var(--c-text-muted));font-size:10px;margin-left:6px}.error{color:#dc6565}p{font-size:12px;margin:10px 0}
</style>
