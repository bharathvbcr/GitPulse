<script lang="ts">
  import { untrack } from "svelte";
  import { explainError, newID, putWorkspace, request, workspaceDraft, WorkbenchError, type Repository, type Workspace, type WorkspaceDraft } from "../workbench/client";
  let { value, repositories, onSaved, onClose }: { value: Workspace | null; repositories: Repository[]; onSaved: () => void; onClose: () => void } = $props();
  const initial = untrack(() => value);
  let draft = $state<WorkspaceDraft>(initial ? workspaceDraft(initial) : { name: "", description: "", icon: "", color: "", position: Date.now(), pinned: false, archived: false, repository_ids: [] });
  let error = $state(""); let saving = $state(false); let pending = $state<Record<string, unknown> | null>(null);
  const id = initial?.id ?? newID();
  async function save() {
    saving = true; error = "";
    try { pending ??= { ...draft, id, expected_revision: value?.revision ?? 0, request_id: newID() }; await putWorkspace(pending); onSaved(); onClose(); }
    catch (cause) { error = explainError(cause); if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error"].includes(cause.code)) pending = null; }
    finally { saving = false; }
  }
  async function remove() {
    if (!value || !window.confirm(`Delete workspace “${value.name}”? Repositories, tasks and history will be kept.`)) return;
    saving = true;
    try { await request("workspaces.delete", { id, expected_revision: value.revision, request_id: newID() }); onSaved(); onClose(); }
    catch (cause) { error = explainError(cause); } finally { saving = false; }
  }
</script>
<aside class="workspace-editor" aria-label="Workspace settings">
  <header><h2>{value ? "Workspace settings" : "New workspace"}</h2><button onclick={onClose} type="button" aria-label="Close workspace settings">✕</button></header>
  <form onsubmit={(e) => { e.preventDefault(); void save(); }}>
    <fieldset disabled={saving || pending !== null}>
      <label>Name<input bind:value={draft.name} required maxlength="300" /></label>
      <label>Description<textarea bind:value={draft.description} rows="3" maxlength="16384" ></textarea></label>
      <label>Icon<input bind:value={draft.icon} maxlength="64" placeholder="Optional emoji" /></label>
      <label>Color<input bind:value={draft.color} maxlength="64" placeholder="Optional color name" /></label>
      <label class="check"><input type="checkbox" bind:checked={draft.pinned} />Pinned</label><label class="check"><input type="checkbox" bind:checked={draft.archived} />Archived</label>
      <fieldset><legend>Repositories</legend><p>A repository can belong to several workspaces.</p>{#each repositories as repo (repo.id)}<label class="check"><input type="checkbox" checked={draft.repository_ids.includes(repo.id)} onchange={(e) => { draft.repository_ids = e.currentTarget.checked ? [...new Set([...draft.repository_ids, repo.id])] : draft.repository_ids.filter((id) => id !== repo.id); }} />{repo.name}</label>{/each}
      {#each draft.repository_ids.filter((id) => !repositories.some((r) => r.id === id)) as missing (missing)}<p>Linked repository {missing} (not on this page)</p>{/each}</fieldset>
    </fieldset>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if pending}<p>Retry this save to reconcile the uncertain result.</p>{/if}
    <footer><button disabled={saving} class="primary" type="submit">{saving ? "Saving…" : pending ? "Retry save" : "Save workspace"}</button>{#if value}<button type="button" onclick={remove} disabled={saving || pending !== null}>Delete workspace</button>{/if}</footer>
  </form>
</aside>
<style>
  .workspace-editor{width:min(380px,45vw);flex-shrink:0;border-left:1px solid rgb(var(--c-border));padding:18px;overflow:auto;background:rgb(var(--c-surface));font-size:12px}header{display:flex;justify-content:space-between;align-items:center;margin-bottom:16px}h2{font-size:16px;font-weight:650}fieldset{border:0;padding:0;margin:12px 0}label{display:flex;flex-direction:column;gap:6px;margin:12px 0}input,textarea{padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg));color:inherit}.check{flex-direction:row;align-items:center;gap:8px}button{padding:7px 10px;border:1px solid rgb(var(--c-border));border-radius:7px}.primary{background:rgb(var(--c-accent));color:white}footer{display:flex;gap:8px;flex-wrap:wrap}p{color:rgb(var(--c-text-muted));margin:10px 0}.error{color:#dc6565}
</style>
