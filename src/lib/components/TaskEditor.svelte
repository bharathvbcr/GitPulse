<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { Check, Copy, X, Sparkles } from "@lucide/svelte";
  import TaskActionDialog from "./TaskActionDialog.svelte";
  import { askConfirm } from "../stores/modalStore";
  import { bounded } from "../workbench/taskActions";
  import { localTaskDate } from "../workbench/taskOrganization";
  import { PRIORITY_LABELS } from "../workbench/boardDrag";
  import { copyText } from "../desktop/clipboard";
  import TaskRuns from "./TaskRuns.svelte";
  import NativeNotificationSettings from "./NativeNotificationSettings.svelte";
  import TaskEnhancements from "./TaskEnhancements.svelte";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { explainError, getTask, getTaskBrief, newID, putTask, registerRepository, STATUSES, STATUS_LABELS, taskDraft, taskWrite, WorkbenchError, type EnhancementField, type Repository, type Task, type TaskDraft, type TaskStatus, type WorkspaceCard } from "../workbench/client";
  import { addableOpenTabs, openMembershipCandidates, type OpenTabRef } from "../workbench/openMembership";
  let { value, repositories, workspaces, openTabs = [], primary = "", home = null, initialStatus = "inbox", initialMode = "details", autoEnhance = false, active = true, onSaved, onClose, onDelete }: {
    value: Task | null; repositories: Repository[]; workspaces: WorkspaceCard[]; openTabs?: OpenTabRef[];
    primary?: string; home?: string | null; initialStatus?: TaskStatus; initialMode?: "details" | "enhance"; autoEnhance?: boolean; active?: boolean; onDelete?: (task: Task) => void; onSaved: (task: Task) => void; onClose: () => void;
  } = $props();
  // A mounted editor owns one snapshot. Background board updates never replace it.
  const initial = untrack(() => ({ value, primary, home, initialStatus }));
  let current = $state(initial.value);
  let mode = $state(untrack(() => initialMode));
  let confirming = $state(false);
  let localDeletion = $state<Task | null>(null), deleted = $state(false);
  export function loadCopy(full: Task) {
    if (current || locked) return;
    draft = {...taskDraft(full),title:`Copy of ${full.title}`.slice(0,300),status:"inbox",position:Date.now()};
    criteria = full.acceptance_criteria.join("\n"); labels = full.labels.join(", "); dirty = true;
  }
  let enhancementRequest = $state(untrack(() => autoEnhance ? 1 : 0));
  export function showEnhancement() { mode = "enhance"; enhancementRequest++; }
  async function enhanceIdea() {
    if (locked && !pending || saving || !draft.description.trim() || !draft.primary_repository_id) return;
    if (!pending) draft.title = [...draft.description.trim().split("\n")[0]].slice(0,300).join("");
    await save();
    if (current && !dirty && !pending) enhancementRequest++;
  }
  async function saveAndEnhance() { await save(); if (current && !dirty && !pending) enhancementRequest++; }
  export function savedID() { return current?.id ?? null; }
  let draft = $state<TaskDraft>(initial.value ? taskDraft(initial.value) : {
    title: "", description: "", kind: "feature", status: initial.initialStatus, priority: 2, severity: null,
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
  let reloading = $state(false);
  let runsOpened = $state(false);
  let sheet: HTMLElement;
  let form: HTMLFormElement;
  const kinds = ["issue", "bug", "feature", "improvement", "maintenance", "research", "documentation"];
  const locked = $derived(saving || reloading || adding || confirming || enhancementBusy || pending !== null);
  onMount(() => { sheet.querySelector<HTMLInputElement | HTMLTextAreaElement>(mode === "enhance" && !current ? 'textarea[name="task-idea"]' : 'input[name="task-title"]')?.focus(); });
  let disposed = false;
  onDestroy(() => { disposed = true; });
  const id = initial.value?.id ?? newID();
  const formID = `task-form-${newID()}`;
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
    if (saving || reloading || adding || confirming || enhancementBusy) return false;
    if (pending) { error = "Retry the save before closing this task."; return false; }
    if (!dirty) return true;
    confirming = true;
    try { return await askConfirm({title:"Discard task edits?",message:"Your unsaved changes will be lost.",confirmLabel:"Discard edits",cancelLabel:"Keep editing"}); }
    finally { confirming = false; }
  }
  async function close() { if (await canLeave() && !disposed) onClose(); }
  function onSheetKey(event: KeyboardEvent) {
    if (!active || localDeletion || !sheet?.getClientRects().length || event.isComposing || event.defaultPrevented) return;
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); void close(); }
    if ((event.metaKey || event.ctrlKey) && event.key === "Enter") { event.preventDefault(); if (mode === "enhance" && !current) void enhanceIdea(); else form.requestSubmit(); }
  }
  function membership(id: string, checked: boolean) {
    draft.repository_ids = checked ? [...new Set([...draft.repository_ids, id])] : draft.repository_ids.filter((r) => r !== id);
    if (!draft.repository_ids.includes(draft.primary_repository_id)) draft.primary_repository_id = draft.repository_ids[0] ?? "";
    dirty = true;
  }
  async function save() {
    if (saving || reloading || adding || confirming || enhancementBusy || !draft.repository_ids.length || (!pending && (!draft.title.trim() || !draft.kind.trim()))) return;
    if (!pending && new TextEncoder().encode(draft.description).length > 65_536) { error = "Description is too long. Keep it below 64 KB."; return; }
    saving = true; error = ""; note = "";
    try {
      if (!pending) {
        draft.acceptance_criteria = criteria.split("\n").map((s) => s.trim()).filter(Boolean);
        draft.labels = labels.split(",").map((s) => s.trim()).filter(Boolean);
        pending = taskWrite(id, current?.revision ?? 0, draft);
      }
      const saved = await bounded(putTask(pending));
      if (disposed) return;
      current = saved; draft = taskDraft(saved); criteria = saved.acceptance_criteria.join("\n"); labels = saved.labels.join(", "); dirty = false; pending = null;
      note = "Saved"; onSaved(saved);
    } catch (cause) {
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) pending = null;
    } finally { saving = false; }
  }
  async function reload() {
    if (!current || locked) return;
    confirming = true;
    const approved = await askConfirm({title:"Reload saved task?",message:"Replace your edits with the latest saved task.",confirmLabel:"Reload saved task"});
    confirming = false;
    if (!approved || disposed) return;
    reloading = true;
    try { const latest = await bounded(getTask(id)); if (disposed) return; current = latest; draft = taskDraft(latest); criteria = latest.acceptance_criteria.join("\n"); labels = latest.labels.join(", "); pending = null; dirty = false; error = ""; }
    catch (cause) { error = explainError(cause); }
    finally { reloading = false; }
  }
  async function copy() {
    if (!current || copying || saving || pending !== null || enhancementBusy) return;
    copying = true; error = ""; note = "";
    const revision = current.revision;
    try {
      const saved = await bounded(getTaskBrief(id, revision));
      if (disposed) return;
      if (current.revision !== revision) throw new Error("The saved task changed while loading its brief. Copy the latest revision again.");
      note = await copyText(saved.markdown) ? `Copied saved revision ${revision}${dirty ? "; unsaved edits are not included" : ""}` : "Clipboard unavailable";
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { copying = false; }
  }
  async function remove() {
    if (!current || locked) return;
    if (onDelete) onDelete(current);
    else if (await canLeave() && !disposed) localDeletion = current;
  }
  function applied(saved: Task) {
    current = saved; draft = taskDraft(saved); criteria = saved.acceptance_criteria.join("\n"); labels = saved.labels.join(", ");
    dirty = false; pending = null; note = `Saved revision ${saved.revision}`; onSaved(saved);
  }
</script>

<svelte:window onkeydown={onSheetKey} />
<aside bind:this={sheet} class="task-editor gp-glass shadow-float" data-task-revision={current?.revision} aria-label={current ? "Task details" : "New task"}>
  <header>
    <div><h2>{mode === "enhance" ? "Quick Enhance" : current ? "Task details" : "New task"}</h2><small>{saving ? "Saving…" : dirty ? "Unsaved changes" : current ? "Saved" : "Define the outcome"}</small></div>
    <div class="header-actions">
      {#if mode === "details" || current && (dirty || pending)}<button class="save primary" form={formID} type="submit" disabled={saving || reloading || adding || confirming || enhancementBusy || !draft.repository_ids.length || !draft.title.trim() || !draft.kind.trim()} title="Save task (⌘/Ctrl Enter)"><Check size={13} />{saving ? "Saving…" : pending ? "Retry save" : "Save task"}</button>{/if}
      <button class="icon" type="button" onclick={close} disabled={saving || reloading || adding || confirming || enhancementBusy} aria-label="Close task details"><X size={16} /></button>
    </div>
  </header>
  <div class="sheet-body">
    {#if mode === "enhance" && !current}
      <form class="quick-idea" onsubmit={(event) => { event.preventDefault(); void enhanceIdea(); }}>
        <label>What needs to happen?<textarea name="task-idea" aria-label="Task idea" bind:value={draft.description} oninput={() => { dirty = true; }} placeholder="A rough idea is enough. Manvi will turn it into a clear task." rows="6" maxlength="65536" required disabled={locked}></textarea></label>
        <label>Repository<select aria-label="Quick task repository" value={draft.primary_repository_id} required disabled={locked} onchange={(event) => { membership(event.currentTarget.value,true); draft.primary_repository_id = event.currentTarget.value; }}><option value="" disabled>Choose repository</option>{#each known as repo}<option value={repo.id}>{repo.name}</option>{/each}</select></label>
        <button class="primary" type="submit" disabled={saving || (!pending && locked) || !draft.description.trim() || !draft.primary_repository_id}><Sparkles size={14} />{saving ? "Saving draft…" : pending ? "Retry save and enhance" : "Enhance with Manvi"}</button>
        <p class="hint">Saves a draft and prepares an enhancement for your review.</p>
      </form>
    {/if}
    {#if mode === "enhance" && error}<p role="alert" class="error">{error}</p>{/if}
    {#if mode === "enhance" && current}
      <details class="quick-context" aria-label="Saved task context"><summary>Task context</summary>
        <h3>{current.title}</h3><p class="context-meta">{current.kind} · {STATUS_LABELS[current.status]} · {PRIORITY_LABELS[current.priority]} priority</p>
        <p>{current.description || "No description yet. Add context or ask Manvi for a suggestion."}</p>
        <dl><div><dt>Repositories</dt><dd>{current.repository_ids.map(id => known.find(repo => repo.id === id)?.name ?? id).join(", ")}</dd></div><div><dt>Owner</dt><dd>{current.owner || "Unassigned"}</dd></div><div><dt>Labels</dt><dd>{current.labels.join(", ") || "None"}</dd></div><div><dt>Due</dt><dd>{current.due_at !== null ? new Date(current.due_at * 1000).toLocaleString() : "Not scheduled"}</dd></div></dl>
        <details open={current.acceptance_criteria.length > 0}><summary>Acceptance criteria <span>{current.acceptance_criteria.length}</span></summary>{#if current.acceptance_criteria.length}<ul>{#each current.acceptance_criteria as criterion}<li>{criterion}</li>{/each}</ul>{:else}<p>No acceptance criteria. Add verifiable outcomes in Details.</p>{/if}</details>
        {#if dirty}<p role="status">Save your edits before enhancing. Manvi uses the saved task shown here.</p>{/if}
      </details>
    {/if}
    <form class:concealed={mode === "enhance"} bind:this={form} id={formID} onsubmit={(e) => { e.preventDefault(); void save(); }} oninput={() => { dirty = true; }} onchange={() => { dirty = true; }}>
      <fieldset disabled={locked}>
        <label>Title<input name="task-title" bind:value={draft.title} required maxlength="300" placeholder="What needs to change?" /></label>
        <div class="pair"><label>Status<select bind:value={draft.status}>{#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}</select></label><label>Priority<select bind:value={draft.priority}>{#each PRIORITY_LABELS as label, priority}<option value={priority}>{label}</option>{/each}</select></label></div>
        <div class="pair"><label>Type<select bind:value={draft.kind}>{#if draft.kind && !kinds.includes(draft.kind)}<option value={draft.kind}>{draft.kind}</option>{/if}{#each kinds as kind}<option value={kind}>{kind.charAt(0).toUpperCase() + kind.slice(1)}</option>{/each}<option value="">Custom type…</option></select></label>
          <label>Primary repository<select value={draft.primary_repository_id} required onchange={(e) => { const id = e.currentTarget.value; membership(id, true); draft.primary_repository_id = id; }}><option value="" disabled>Choose repository</option>{#each known as repo (repo.id)}<option value={repo.id}>{repo.name}</option>{/each}{#each draft.repository_ids.filter((id) => !known.some((repo) => repo.id === id)) as missing (missing)}<option value={missing}>{missing}</option>{/each}</select></label>
        </div>
        {#if !kinds.includes(draft.kind)}<label>Custom type<input bind:value={draft.kind} required maxlength="64" placeholder="e.g. experiment" /></label>{/if}
        <label>Description<textarea bind:value={draft.description} rows="4" maxlength="65536" placeholder="Context, expected result, and constraints" ></textarea></label>
        <details open={draft.acceptance_criteria.length > 0}>
          <summary>Acceptance criteria{#if draft.acceptance_criteria.length}<span>{draft.acceptance_criteria.length}</span>{/if}</summary>
          <label class="detail-field"><span class="sr-only">Acceptance criteria</span><textarea aria-label="Acceptance criteria" bind:value={criteria} rows="3" placeholder="One verifiable outcome per line" ></textarea></label>
        </details>
        <details>
          <summary>Assignment & labels</summary>
          <div class="detail-field">
            <label>Owner<input value={draft.owner ?? ""} oninput={(e) => { draft.owner = e.currentTarget.value || null; }} maxlength="300" placeholder="Unassigned" /></label>
            <label>Labels<input bind:value={labels} placeholder="e.g. frontend, accessibility" /></label>
            <label>Due date<input type="datetime-local" value={localTaskDate(draft.due_at)} onchange={(event) => { const value = event.currentTarget.value; const seconds = value ? Math.floor(new Date(value).getTime()/1000) : null; if(seconds === null || Number.isFinite(seconds)) draft.due_at = seconds; }} /></label>
            <div class="pair"><label>Severity<select bind:value={draft.severity}><option value={null}>Not set</option>{#if draft.severity && !["low", "medium", "high", "critical"].includes(draft.severity)}<option value={draft.severity}>{draft.severity}</option>{/if}{#each ["low", "medium", "high", "critical"] as severity}<option value={severity}>{severity.charAt(0).toUpperCase() + severity.slice(1)}</option>{/each}</select></label>
            <label>Workspace<select bind:value={draft.home_workspace_id}><option value={null}>No workspace</option>{#if draft.home_workspace_id && !workspaces.some((group) => group.id === draft.home_workspace_id)}<option value={draft.home_workspace_id}>{draft.home_workspace_id}</option>{/if}{#each workspaces as group (group.id)}<option value={group.id}>{group.name}{group.archived ? " (archived)" : ""}</option>{/each}</select></label></div>
          </div>
        </details>
        <details>
          <summary>Linked repositories<span>{draft.repository_ids.length}</span></summary>
          <fieldset class="repositories"><legend class="sr-only">Linked repositories</legend>
            {#each known as repo (repo.id)}<label class="check"><input type="checkbox" aria-label={repo.name} checked={draft.repository_ids.includes(repo.id)} onchange={(e) => membership(repo.id, e.currentTarget.checked)} />{repo.name}{#if repo.id === draft.primary_repository_id}<small>Primary</small>{/if}</label>{/each}
            {#if addable.length}<small>Open in GitPulse</small>{#each addable as tab (tab.path)}<button class="add-open" type="button" title={tab.path} disabled={adding} onclick={() => void addOpenPaths([tab.path])}>Link {tab.label}</button>{/each}{#if addable.length > 1}<button type="button" disabled={adding} onclick={() => void addOpenPaths(addable.map((tab) => tab.path))}>Link all open repositories</button>{/if}{/if}
            {#each draft.repository_ids.filter((id) => !known.some((r) => r.id === id)) as missing (missing)}<small>Linked: {missing}</small>{/each}
          </fieldset>
        </details>
        <details>
          <summary>AI editing preferences</summary>
          <fieldset class="enhancement-locks"><legend class="sr-only">Enhancement field locks</legend>
            <label class="check"><input type="checkbox" checked={draft.locked_fields?.includes("title") ?? false} onchange={(e) => setFieldLock("title", e.currentTarget.checked)} />Keep title during enhancements</label>
            <label class="check"><input type="checkbox" checked={draft.locked_fields?.includes("description") ?? false} onchange={(e) => setFieldLock("description", e.currentTarget.checked)} />Keep description during enhancements</label>
          </fieldset>
        </details>
      </fieldset>
      {#if !draft.repository_ids.length}<p class="hint">Choose a repository to save this task.</p>{/if}
      {#if error}<p role="alert" class="error">{error}</p>{/if}
      {#if pending && !saving}<p class="hint">Save confirmation was lost. Retry save to recover the result.</p>{/if}
      {#if note}<p role="status" class="hint">{note}</p>{/if}
    </form>
    {#if current}
      {#if mode === "enhance" && dirty}<button class="primary" type="button" disabled={locked} onclick={saveAndEnhance}>Save and enhance</button>{/if}
      <TaskEnhancements task={current} {active} quick={mode === "enhance"} startRequest={enhancementRequest} disabled={dirty || saving || reloading || adding || pending !== null} onApplied={applied} onBusy={(busy) => { enhancementBusy = busy; }} />
      <details class="agent-section" class:concealed={mode === "enhance"} bind:open={runsOpened}>
        <summary>Run with a coding agent</summary>
        <TaskRuns task={current} repositories={known} active={active && runsOpened} disabled={dirty || locked} />
      </details>
      <details><summary>Notifications</summary><NativeNotificationSettings scope={{ kind: "global" }} taskID={current.id} /></details>
      <div class="task-tools">
        {#if mode === "enhance"}<button type="button" onclick={() => { mode = "details"; }}>Edit task details</button>{/if}
        <button type="button" onclick={copy} disabled={copying || locked}><Copy size={12} />{copying ? "Loading brief…" : "Copy saved brief"}</button>
        <details class="saved-actions"><summary>Saved task options</summary><div class="saved-options"><button type="button" onclick={reload} disabled={locked}>Reload saved task</button><button class="danger" type="button" onclick={remove} disabled={locked}>Delete task</button><small>Revision {current.revision}</small></div></details>
      </div>
    {/if}
  </div>
</aside>
{#if localDeletion}<TaskActionDialog tasks={[localDeletion]} action={{kind:"delete"}} onChanged={(ids) => { if(current && ids.includes(current.id)) { deleted = true; onSaved(current); } }} onClose={() => { localDeletion = null; if(deleted) onClose(); }} />{/if}

<style>
  .concealed{display:none}.quick-idea{padding-bottom:12px}.quick-idea .primary{width:100%;padding:10px}.quick-context{margin-bottom:14px}.quick-context h3{font-size:16px;line-height:1.4;margin:0 0 6px}.quick-context>p{white-space:pre-wrap;overflow-wrap:anywhere;max-height:160px;overflow:auto}.context-meta{color:rgb(var(--c-text-muted))}dl{font-size:11px;display:grid;gap:8px}dl div{display:flex;gap:10px}dt{width:78px;color:rgb(var(--c-text-muted));flex-shrink:0}dd{margin:0;overflow-wrap:anywhere}ul{padding-left:18px;font-size:12px;line-height:1.6}

  .task-editor{width:390px;max-width:100%;flex-shrink:0;display:flex;flex-direction:column;min-height:0;border-left:1px solid rgb(var(--c-border));background-color:rgb(var(--c-surface));color:rgb(var(--c-text))}
  header{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:16px 18px;border-bottom:1px solid rgb(var(--c-border));flex-shrink:0}h2{font-size:14px;font-weight:650;margin:0 0 3px}.header-actions{display:flex;gap:6px;align-items:center}.sheet-body{padding:18px;overflow:auto;min-height:0;flex:1}
  small,legend,.hint{color:rgb(var(--c-text-muted));font-size:11px}form{font-size:12px}fieldset{border:0;padding:0;min-width:0}label{display:flex;flex-direction:column;gap:6px;margin-bottom:14px;flex:1;min-width:0}.pair{display:flex;gap:12px;align-items:flex-start}input,textarea,select{width:100%;padding:8px 10px;border:1px solid rgb(var(--c-border));border-radius:7px;background:var(--mac-recess,rgb(var(--c-bg)));color:inherit;min-width:0;font-size:12px}input[name="task-title"]{font-size:15px;font-weight:550;padding:10px}textarea{resize:vertical;line-height:1.6}button{display:inline-flex;align-items:center;justify-content:center;gap:6px;padding:7px 10px;border:1px solid rgb(var(--c-border));border-radius:7px;font-size:11px}button:hover:enabled{background:rgb(var(--c-surface-hover))}button:disabled{opacity:.5}.primary{background:rgb(var(--c-accent));color:white}.primary:hover:enabled{background:color-mix(in srgb,rgb(var(--c-accent)) 85%,black)}.icon{padding:6px;border:0}
  details{border-top:1px solid rgb(var(--c-border));font-size:12px}summary{cursor:pointer;user-select:none;padding:12px 0;color:rgb(var(--c-text-muted));font-weight:550}summary span{float:right;font-weight:400}summary:hover{color:rgb(var(--c-text))}.detail-field{padding:2px 0 6px}.detail-field label:last-child{margin-bottom:6px}.check{flex-direction:row;align-items:center;margin:8px 0;gap:8px}.check input{width:auto}.check small{margin-left:auto}.repositories{max-height:200px;overflow:auto;margin:0 0 12px}.add-open{display:flex;width:100%;justify-content:flex-start;margin-top:6px}.enhancement-locks{padding-bottom:10px}.task-tools{display:flex;align-items:flex-start;gap:10px;flex-wrap:wrap;margin-top:14px}.saved-actions{border:0;flex:1}.saved-actions summary{padding:8px 0;font-size:11px}.saved-options{display:grid;gap:8px}.danger,.error{color:#dc6565}p{font-size:12px;margin:10px 0;line-height:1.5}
  :is(button,input,select,textarea,summary):focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}
  @media(max-width:1050px){.task-editor{position:absolute;right:0;top:0;bottom:0;width:min(430px,100%);z-index:6;box-shadow:-12px 0 32px #0002}}
  @media(prefers-reduced-motion:no-preference){.task-editor{animation:sheet-in 140ms ease-out}@keyframes sheet-in{from{opacity:.5;transform:translateX(12px)}to{opacity:1;transform:translateX(0)}}}
</style>
