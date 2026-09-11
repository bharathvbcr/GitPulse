<script lang="ts">
  import { askConfirm } from "../stores/modalStore";
  import { untrack } from "svelte";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import SettingToggle from "./SettingToggle.svelte";
  import { deleteWorkspace, explainError, newID, putWorkspace, registerRepository, workspaceDraft, WorkbenchError, type Repository, type Workspace, type WorkspaceDraft } from "../workbench/client";
  import { addableOpenTabs, openMembershipCandidates, withRepositoryId, type OpenTabRef } from "../workbench/openMembership";
  let { value, repositories, openTabs = [], onSaved, onClose }: {
    value: Workspace | null; repositories: Repository[]; openTabs?: OpenTabRef[]; onSaved: () => void; onClose: () => void;
  } = $props();
  const initial = untrack(() => value);
  let draft = $state<WorkspaceDraft>(initial ? workspaceDraft(initial) : { name: "", description: "", icon: "", color: "", position: Date.now(), pinned: false, archived: false, repository_ids: [] });
  const originalDraft = JSON.stringify(untrack(() => draft));
  let confirming = $state(false);
  export async function canLeave(): Promise<boolean> {
    if (saving || adding || confirming) return false;
    if (pending) { error = "Retry the save before closing this workspace."; return false; }
    if (JSON.stringify(draft) === originalDraft) return true;
    confirming = true;
    try { return await askConfirm({title:"Discard workspace edits?",message:"Your unsaved changes will be lost.",confirmLabel:"Discard edits",cancelLabel:"Keep editing"}); }
    finally { confirming = false; }
  }
  async function close() { if (await canLeave()) onClose(); }
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
    if (adding || saving || confirming) return;
    saving = true; error = "";
    try { pending ??= { ...draft, id, expected_revision: value?.revision ?? 0, request_id: newID() }; await putWorkspace(pending); onSaved(); onClose(); }
    catch (cause) { error = explainError(cause); if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) pending = null; }
    finally { saving = false; }
  }
  async function remove() {
    if (!value) return;
    if (!await askConfirm({
      title: `Delete workspace “${value.name}”?`,
      message: "Repositories, tasks and history will be kept. This workspace ID cannot be reused.",
      confirmLabel: "Delete workspace",
      cancelLabel: "Keep workspace",
      destructive: true,
    })) return;
    saving = true;
    try { await deleteWorkspace(id, value.revision, newID()); onSaved(); onClose(); }
    catch (cause) { error = explainError(cause); } finally { saving = false; }
  }
</script>
<aside class="workspace-editor gp-glass bg-surface" aria-label="Workspace settings">
  <header class="gp-glass shadow-float"><h2>{value ? "Workspace settings" : "New workspace"}</h2><button onclick={close} disabled={saving || adding || confirming || pending !== null} type="button" aria-label="Close workspace settings">✕</button></header>
  <form onsubmit={(e) => { e.preventDefault(); void save(); }}>
    <div class="sheet-body">
    <fieldset disabled={saving || adding || confirming || pending !== null}>
      <label>Name<input class="gp-field" bind:value={draft.name} required maxlength="300" /></label>
      <label>Description<textarea class="gp-field" bind:value={draft.description} rows="3" maxlength="16384" ></textarea></label>
      <label>Icon<input class="gp-field" bind:value={draft.icon} maxlength="64" placeholder="Optional emoji" /></label>
      <label>Color<input class="gp-field" bind:value={draft.color} maxlength="64" placeholder="Optional color name" /></label>
      <SettingToggle label="Pinned" description="Keep this workspace at the top of the navigator." checked={draft.pinned} onchange={(next) => { draft.pinned = next; }} />
      <SettingToggle label="Archived" description="Hide this workspace unless Show archived is on." checked={draft.archived} onchange={(next) => { draft.archived = next; }} />
      <fieldset><legend>Repositories</legend><p>A repository can belong to several workspaces.</p>{#each known as repo (repo.id)}<label class="check"><input class="gp-field" type="checkbox" checked={draft.repository_ids.includes(repo.id)} onchange={(e) => { draft.repository_ids = e.currentTarget.checked ? [...new Set([...draft.repository_ids, repo.id])] : draft.repository_ids.filter((id) => id !== repo.id); }} />{repo.name}</label>{/each}
      {#if addable.length}
        <p>Open in GitPulse</p>
        {#each addable as tab (tab.path)}
          <label class="check" title={tab.path}>
            <input class="gp-field" type="checkbox" checked={false} disabled={adding} onchange={(e) => { e.currentTarget.checked = false; void addOpenPaths([tab.path]); }} />
            {tab.label}<span class="open-mark">Open</span>
          </label>
        {/each}
        {#if addable.length > 1}<button type="button" class="open-add gp-btn" disabled={adding} onclick={() => void addOpenPaths(addable.map((tab) => tab.path))}>Add all open</button>{/if}
      {/if}
      {#each draft.repository_ids.filter((id) => !known.some((r) => r.id === id)) as missing (missing)}<p>Linked repository {missing} (not on this page)</p>{/each}</fieldset>
    </fieldset>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if pending}<p>Retry this save to reconcile the uncertain result.</p>{/if}
    </div>
    <footer><button disabled={saving || adding} class="gp-btn-primary" type="submit">{saving ? "Saving…" : pending ? "Retry save" : "Save workspace"}</button>{#if value}<button type="button" class="gp-btn-danger" onclick={remove} disabled={saving || adding || pending !== null}>Delete workspace</button>{/if}</footer>
  </form>
</aside>
<style>
  .workspace-editor{width:min(380px,45vw);flex-shrink:0;min-height:0;border-left:1px solid rgb(var(--c-border) / 0.65);padding:0;overflow:hidden;font-size:12px;display:flex;flex-direction:column}form{flex:1;min-height:0;display:flex;flex-direction:column;overflow:hidden}.sheet-body{flex:1;min-height:0;overflow:auto;padding:0 18px 18px}header{padding:16px 18px;display:flex;justify-content:space-between;align-items:center;flex-shrink:0}h2{font-size:16px;font-weight:650}fieldset{border:0;padding:0;margin:12px 0}label{display:flex;flex-direction:column;gap:6px;margin:12px 0}input,textarea{padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit}.check{flex-direction:row;align-items:center;gap:8px}footer{display:flex;gap:8px;flex-wrap:wrap;flex-shrink:0;padding:10px 18px 16px;border-top:1px solid rgb(var(--c-border) / 0.45)}p{color:rgb(var(--c-text-muted));margin:10px 0}.error{color:#dc6565}.open-add{display:block;width:100%;text-align:left;margin:4px 0}.open-mark{color:rgb(var(--c-text-muted));font-size:10px;margin-left:6px}
</style>
