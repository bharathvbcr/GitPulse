<script lang="ts">
  import { untrack } from "svelte";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { explainError, newID, putWorkspace, registerRepository, request, workspaceDraft, WorkbenchError, type Repository, type Workspace, type WorkspaceDraft } from "../workbench/client";
  import { addableOpenTabs, openMembershipCandidates, withRepositoryId, type OpenTabRef } from "../workbench/openMembership";
  let { value, repositories, openTabs = [], onSaved, onClose }: {
    value: Workspace | null; repositories: Repository[]; openTabs?: OpenTabRef[]; onSaved: () => void; onClose: () => void;
  } = $props();
  const initial = untrack(() => value);
  let draft = $state<WorkspaceDraft>(initial ? workspaceDraft(initial) : { name: "", description: "", icon: "", color: "", position: Date.now(), pinned: false, archived: false, repository_ids: [] });
  let extras = $state<Repository[]>([]);
  let error = $state(""); let saving = $state(false); let adding = $state(false); let pending = $state<Record<string, unknown> | null>(null);
  const id = initial?.id ?? newID();
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
        if (!extras.some((item) => item.id === repo.id)) extras = [...extras, repo];
        draft.repository_ids = withRepositoryId(draft.repository_ids, repo.id);
      }
    } catch (cause) { error = explainError(cause); }
    finally { adding = false; }
  }
  async function save() {
    if (adding) return;
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
<aside class="workspace-editor gp-glass bg-surface" aria-label="Workspace settings">
  <header class="gp-glass shadow-float"><h2>{value ? "Workspace settings" : "New workspace"}</h2><button class="gp-btn" onclick={onClose} type="button" aria-label="Close workspace settings">✕</button></header>
  <form onsubmit={(e) => { e.preventDefault(); void save(); }}>
    <fieldset disabled={saving || pending !== null}>
      <label>Name<input class="gp-field" bind:value={draft.name} required maxlength="300" /></label>
      <label>Description<textarea class="gp-field gp-field-multi" bind:value={draft.description} rows="3" maxlength="16384" ></textarea></label>
      <label>Icon<input class="gp-field" bind:value={draft.icon} maxlength="64" placeholder="Optional emoji" /></label>
      <label>Color<input class="gp-field" bind:value={draft.color} maxlength="64" placeholder="Optional color name" /></label>
      <label class="check"><input type="checkbox" bind:checked={draft.pinned} />Pinned</label><label class="check"><input type="checkbox" bind:checked={draft.archived} />Archived</label>
      <fieldset><legend>Repositories</legend><p>A repository can belong to several workspaces.</p>{#each known as repo (repo.id)}<label class="check"><input type="checkbox" checked={draft.repository_ids.includes(repo.id)} onchange={(e) => { draft.repository_ids = e.currentTarget.checked ? [...new Set([...draft.repository_ids, repo.id])] : draft.repository_ids.filter((id) => id !== repo.id); }} />{repo.name}</label>{/each}
      {#if addable.length}
        <p>Open in GitPulse</p>
        {#each addable as tab (tab.path)}
          <label class="check" title={tab.path}>
            <input type="checkbox" checked={false} disabled={adding} onchange={(e) => { e.currentTarget.checked = false; void addOpenPaths([tab.path]); }} />
            {tab.label}<span class="open-mark">Open</span>
          </label>
        {/each}
        {#if addable.length > 1}<button type="button" class="open-add gp-btn" disabled={adding} onclick={() => void addOpenPaths(addable.map((tab) => tab.path))}>Add all open</button>{/if}
      {/if}
      {#each draft.repository_ids.filter((id) => !known.some((r) => r.id === id)) as missing (missing)}<p>Linked repository {missing} (not on this page)</p>{/each}</fieldset>
    </fieldset>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if pending}<p>Retry this save to reconcile the uncertain result.</p>{/if}
    <footer><button disabled={saving || adding} class="gp-btn-primary" type="submit">{saving ? "Saving…" : pending ? "Retry save" : "Save workspace"}</button>{#if value}<button class="gp-btn" type="button" onclick={remove} disabled={saving || adding || pending !== null}>Delete workspace</button>{/if}</footer>
  </form>
</aside>
<style>
  .workspace-editor{width:min(380px,45vw);flex-shrink:0;border-left:1px solid rgb(var(--c-border));padding:0;overflow:auto;font-size:12px}header{position:sticky;top:0;z-index:1;display:flex;justify-content:space-between;align-items:center;padding:16px 18px;border-bottom:1px solid rgb(var(--c-border))}form{padding:0 18px 18px}h2{font-size:16px;font-weight:650}fieldset{border:0;padding:0;margin:12px 0}label{display:flex;flex-direction:column;gap:6px;margin:12px 0}input,textarea{padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:var(--mac-fill-bg,rgb(var(--c-bg)));color:inherit}.check{flex-direction:row;align-items:center;gap:8px}footer{display:flex;gap:8px;flex-wrap:wrap}p{color:rgb(var(--c-text-muted));margin:10px 0}.error{color:#dc6565}.open-add{display:block;width:100%;text-align:left;margin:4px 0}.open-mark{color:rgb(var(--c-text-muted));font-size:10px;margin-left:6px}
</style>
