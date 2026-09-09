<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { isTauri } from "../platform";
  import { explainError, getTask, getWorkspace, listRepositories, listTasks, listWorkspaces, putTask, registerRepository, STATUSES, STATUS_LABELS, taskDraft, taskWrite, type Page, type Repository, type Scope, type Task, type TaskCard, type TaskStatus, type Workspace, type WorkspaceCard } from "../workbench/client";
  import TaskEditor from "./TaskEditor.svelte";
  import WorkspaceEditor from "./WorkspaceEditor.svelte";
  import AutomaticEnhancements from "./AutomaticEnhancements.svelte";
  import AttentionInbox from "./AttentionInbox.svelte";

  let { repositoryPath = null, active = true }: { repositoryPath?: string | null; active?: boolean } = $props();
  let scope = $state<Scope>({ kind: "global" });
  let repositories = $state<Repository[]>([]), workspaces = $state<WorkspaceCard[]>([]);
  let repositoryCursor = $state<string | null>(null), workspaceCursor = $state<string | null>(null);
  let repositoryTotal = $state(0), workspaceTotal = $state(0);
  let columns = $state<Partial<Record<TaskStatus, Page<TaskCard>>>>({});
  let search = $state(""); let initialized = $state(false); let loading = $state(false);
  let error = $state(""); let catalogError = $state(""); let notice = $state("");
  let taskEditor = $state<{ value: Task | null } | null>(null);
  let workspaceEditor = $state<{ value: Workspace | null } | null>(null);
  let opening = $state(false); let moving = $state(false); let drag = $state<TaskCard | null>(null);
  let showInbox = $state(false);
  let revision = 0; let disposed = false; let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  const title = $derived.by(() => {
    const target = scope;
    return target.kind === "global" ? "All tasks" : target.kind === "workspace"
      ? workspaces.find((w) => w.id === target.id)?.name ?? "Workspace tasks"
      : repositories.find((r) => r.id === target.id)?.name ?? "Repository tasks";
  });
  const total = $derived(STATUSES.reduce((sum, status) => sum + (columns[status]?.total ?? 0), 0));
  const mounted = $derived(STATUSES.reduce((sum, status) => sum + (columns[status]?.shown ?? 0), 0));

  async function catalog() {
    const [repos, groups] = await Promise.all([listRepositories(), listWorkspaces()]);
    if (disposed) return;
    repositories = repos.items; repositoryTotal = repos.total; repositoryCursor = repos.next_cursor;
    workspaces = groups.items; workspaceTotal = groups.total; workspaceCursor = groups.next_cursor;
    catalogError = "";
  }
  async function loadBoard(target: Scope = scope, query: string = search) {
    const generation = ++revision; loading = true; error = ""; columns = {};
    try {
      const pages = await Promise.all(STATUSES.map(async (status) => [status, await listTasks(target, status, query)] as const));
      if (generation !== revision || disposed) return;
      columns = Object.fromEntries(pages);
    } catch (cause) { if (generation === revision && !disposed) error = explainError(cause); }
    finally { if (generation === revision && !disposed) loading = false; }
  }
  async function refresh() {
    try { await catalog(); } catch (cause) { catalogError = explainError(cause); }
    if (initialized && active && !disposed) await loadBoard();
  }
  function scheduleRefresh() {
    if (!active || disposed) return;
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => { void refresh(); }, 200);
  }
  async function initialize() {
    loading = true; catalogError = "";
    try {
      if (repositoryPath) { const repo = await registerRepository(repositoryPath); if (disposed) return; scope = { kind: "repository", id: repo.id }; }
      await catalog();
      if (!disposed) initialized = true;
    } catch (cause) { if (!disposed) { catalogError = explainError(cause); loading = false; } }
  }
  onMount(() => {
    void initialize();
    let unlisten: (() => void) | undefined;
    if (isTauri()) void listen("workbench-changed", scheduleRefresh).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch((cause) => { if (!disposed) notice = `Live updates unavailable: ${explainError(cause)}. Refresh to check for changes.`; });
    window.addEventListener("focus", scheduleRefresh);
    return () => { disposed = true; revision++; clearTimeout(refreshTimer); unlisten?.(); window.removeEventListener("focus", scheduleRefresh); };
  });
  $effect(() => {
    if (!initialized || !active) return;
    const target = scope, query = search;
    columns = {}; loading = true;
    const timer = setTimeout(() => { void loadBoard(target, query); }, 250);
    return () => { clearTimeout(timer); revision++; };
  });
  async function addRepository() {
    try { const path = await invoke<string | null>("cmd_pick_folder"); if (!path) return; const repo = await registerRepository(path); await catalog(); notice = `Added ${repo.name}`; }
    catch (cause) { catalogError = explainError(cause); }
  }
  async function moreRepositories() {
    if (!repositoryCursor) return;
    try { const next = await listRepositories(repositoryCursor); repositories = [...repositories, ...next.items.filter((r) => !repositories.some((old) => old.id === r.id))]; repositoryCursor = next.next_cursor; repositoryTotal = next.total; }
    catch (cause) { catalogError = explainError(cause); }
  }
  async function moreWorkspaces() {
    if (!workspaceCursor) return;
    try { const next = await listWorkspaces(workspaceCursor); workspaces = [...workspaces, ...next.items.filter((r) => !workspaces.some((old) => old.id === r.id))]; workspaceCursor = next.next_cursor; workspaceTotal = next.total; }
    catch (cause) { catalogError = explainError(cause); }
  }
  async function editWorkspace(id: string) {
    opening = true;
    try { const full = await getWorkspace(id); taskEditor = null; workspaceEditor = { value: full }; }
    catch (cause) { error = explainError(cause); } finally { opening = false; }
  }
  async function openTask(id: string) {
    if (taskEditor && !window.confirm("Open another task? Unsaved edits in the current editor will be discarded.")) return;
    opening = true;
    try { const full = await getTask(id); workspaceEditor = null; taskEditor = { value: full }; }
    catch (cause) { error = explainError(cause); } finally { opening = false; }
  }
  async function pageColumn(status: TaskStatus, cursor?: string) {
    const generation = revision; loading = true;
    try { const result = await listTasks(scope, status, search, cursor); if (generation === revision && !disposed) columns = { ...columns, [status]: result }; }
    catch (cause) { if (generation === revision) error = explainError(cause); }
    finally { if (generation === revision) loading = false; }
  }
  async function drop(status: TaskStatus) {
    const card = drag; drag = null;
    if (!card || card.status === status || moving) return;
    moving = true; error = "";
    try {
      const full = await getTask(card.id);
      if (full.revision !== card.revision) throw new Error("This task changed while you were moving it. Refresh and try again.");
      await putTask(taskWrite(full.id, full.revision, { ...taskDraft(full), status, position: Date.now() }));
      notice = `Moved “${full.title}” to ${STATUS_LABELS[status]}`;
      await loadBoard();
    } catch (cause) { error = explainError(cause); } finally { moving = false; }
  }
  function createTask() {
    if (taskEditor && !window.confirm("Start a new task and discard the current unsaved edits?")) return;
    workspaceEditor = null; taskEditor = { value: null };
  }
</script>

<div class="workbench" data-testid="task-board">
  {#if !repositoryPath}
    <nav class="navigator" aria-label="Task scopes">
      <div class="nav-heading">Workspaces<button type="button" title="Create workspace" onclick={() => { workspaceEditor = { value: null }; taskEditor = null; }}>+</button></div>
      <button class:selected={scope.kind === "global"} onclick={() => { scope = { kind: "global" }; }}>All tasks</button>
      {#each [...workspaces].sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.position - b.position) as group (group.id)}
        <div class="nav-row"><button class:selected={scope.kind === "workspace" && scope.id === group.id} onclick={() => { scope = { kind: "workspace", id: group.id }; }} title={group.name}>{group.icon} {group.name}{group.archived ? " · Archived" : ""}</button><button aria-label={`Edit ${group.name}`} onclick={() => editWorkspace(group.id)} disabled={opening}>⋯</button></div>
      {/each}
      {#if workspaceCursor}<button onclick={moreWorkspaces}>More workspaces ({workspaces.length}/{workspaceTotal})</button>{/if}
      <div class="nav-heading">Repositories<button type="button" onclick={addRepository} title="Add repository">+</button></div>
      {#each repositories as repo (repo.id)}<button class:selected={scope.kind === "repository" && scope.id === repo.id} onclick={() => { scope = { kind: "repository", id: repo.id }; }} title={repo.identity_key}>{repo.name}</button>{/each}
      {#if repositoryCursor}<button onclick={moreRepositories}>More repositories ({repositories.length}/{repositoryTotal})</button>{/if}
      {#if repositories.length === 0 && initialized}<p>Add repositories to start creating linked tasks.</p>{/if}
    </nav>
  {/if}
  <main class="board-main">
    <header><div><div class="eyebrow">{scope.kind === "global" ? "GLOBAL BOARD" : scope.kind === "workspace" ? "WORKSPACE BOARD" : "REPOSITORY BOARD"}</div><h1>{title}</h1><small>{loading ? "Loading…" : `${total} tasks · ${mounted} shown`}</small></div><div class="actions"><input aria-label="Search tasks" type="search" bind:value={search} placeholder="Search tasks…" maxlength="512" /><button onclick={() => initialized ? refresh() : initialize()} disabled={loading}>Refresh</button><button class="primary" onclick={createTask} disabled={!initialized || !repositories.length}>New task</button></div></header>
    {#if initialized}<AutomaticEnhancements {active} />{/if}
    {#if initialized}<div class="actions" style="padding:0 18px"><button aria-expanded={showInbox} onclick={() => { showInbox = !showInbox; }}>{showInbox ? "Hide activity inbox" : "Activity inbox"}</button></div>{/if}
    {#if showInbox}<AttentionInbox {scope} {active} onopen={openTask} />{/if}
    {#if catalogError}<div class="banner error" role="alert">{catalogError}<button onclick={() => initialized ? refresh() : initialize()}>Retry loading workspaces</button></div>{/if}
    {#if error}<div class="banner error" role="alert">{error}</div>{/if}
    {#if notice}<div class="banner" role="status">{notice}<button onclick={() => { notice = ""; }} aria-label="Dismiss notice">✕</button></div>{/if}
    <div class="columns" aria-busy={loading || moving}>
      {#each STATUSES as status}
        <section class="column" aria-label={STATUS_LABELS[status]}>
          <button class="column-title" ondragover={(e) => { if (drag) e.preventDefault(); }} ondrop={(e) => { e.preventDefault(); void drop(status); }} title="Drop a task here to change its status; use the task editor for keyboard access"><span>{STATUS_LABELS[status]}</span><span>{columns[status]?.total ?? "—"}</span></button>
          <div class="cards">
            {#each columns[status]?.items ?? [] as card (card.id)}
              <button class="card" draggable={!moving} ondragstart={(e) => { drag = card; e.dataTransfer?.setData("text/plain", card.id); }} ondragend={() => { drag = null; }} onclick={() => openTask(card.id)} disabled={opening}>
                <div class="card-meta"><span>{card.kind}</span><span>{["Urgent", "High", "Normal", "Low"][card.priority]}</span></div><h3>{card.title}</h3><div class="card-repos">{card.repository_ids.map((id) => repositories.find((r) => r.id === id)?.name ?? id).join(" · ")}</div>{#if card.labels.length}<div class="labels">{#each card.labels.slice(0, 3) as label}<span>{label}</span>{/each}{#if card.labels.length > 3}<span>+{card.labels.length - 3}</span>{/if}</div>{/if}
              </button>
            {/each}
            {#if !loading && columns[status]?.total === 0}<p class="empty">No tasks</p>{/if}
          </div>
          {#if columns[status] && (columns[status]?.total ?? 0) > 30}<div class="paging"><button onclick={() => pageColumn(status)} disabled={loading}>First page</button>{#if columns[status]?.next_cursor}<button onclick={() => pageColumn(status, columns[status]?.next_cursor ?? undefined)} disabled={loading}>Next 30 →</button>{/if}</div>{/if}
        </section>
      {/each}
    </div>
  </main>
  {#if taskEditor}{#key taskEditor}<TaskEditor {active} value={taskEditor.value} {repositories} {workspaces} primary={scope.kind === "repository" ? scope.id : repositories[0]?.id ?? ""} home={scope.kind === "workspace" ? scope.id : null} onSaved={() => { void loadBoard(); }} onClose={() => { taskEditor = null; }} />{/key}{/if}
  {#if workspaceEditor}{#key workspaceEditor}<WorkspaceEditor value={workspaceEditor.value} {repositories} onSaved={() => { scope = { kind: "global" }; void refresh(); }} onClose={() => { workspaceEditor = null; }} />{/key}{/if}
</div>

<style>
  .workbench{display:flex;flex:1;min-height:0;min-width:0;color:rgb(var(--c-text));background:rgb(var(--c-bg));overflow:hidden}.navigator{width:205px;flex-shrink:0;border-right:1px solid rgb(var(--c-border));padding:14px 10px;overflow:auto}.navigator button{display:block;width:100%;text-align:left;border:0;padding:7px 10px;border-radius:7px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px}.navigator button:hover,button:hover{background:rgb(var(--c-surface-hover))}.navigator button.selected{background:color-mix(in srgb,rgb(var(--c-accent)) 13%,transparent);color:rgb(var(--c-accent))}.nav-heading{display:flex;align-items:center;justify-content:space-between;padding:8px 10px;margin-top:8px;color:rgb(var(--c-text-muted));font-size:11px;font-weight:650}.nav-heading button,.nav-row>button:last-child{width:30px;flex-shrink:0;text-align:center}.nav-row{display:flex}.nav-row>button:first-child{min-width:0;flex:1}.navigator p{font-size:12px;padding:10px;color:rgb(var(--c-text-muted))}.board-main{flex:1;min-width:0;display:flex;flex-direction:column;overflow:hidden}header{padding:22px 24px 18px;display:flex;align-items:center;justify-content:space-between;gap:15px;flex-wrap:wrap;border-bottom:1px solid rgb(var(--c-border))}h1{font-size:23px;line-height:1.3;font-weight:650;margin:4px 0}.eyebrow{font-size:9px;font-weight:700;letter-spacing:.14em;color:rgb(var(--c-text-muted))}small{color:rgb(var(--c-text-muted));font-size:11px}.actions{display:flex;gap:8px;align-items:center;flex-wrap:wrap}button,input{font-size:12px}button{border:1px solid rgb(var(--c-border));padding:7px 11px;border-radius:7px}button:disabled{opacity:.5}.primary{background:rgb(var(--c-accent));color:white}input{background:rgb(var(--c-surface));border:1px solid rgb(var(--c-border));border-radius:7px;padding:8px 10px;color:inherit;width:180px}.columns{display:flex;gap:13px;padding:20px;overflow:auto;flex:1;min-height:0;align-items:stretch}.column{width:245px;min-width:210px;flex:1;display:flex;flex-direction:column;background:color-mix(in srgb,rgb(var(--c-surface)) 50%,transparent);border-radius:10px;border:1px solid rgb(var(--c-border));overflow:hidden}.column-title{display:flex;align-items:center;justify-content:space-between;font-weight:650;background:rgb(var(--c-surface));border:0;border-bottom:1px solid rgb(var(--c-border));border-radius:0;padding:11px}.column-title span:last-child{color:rgb(var(--c-text-muted));font-weight:400}.cards{padding:8px;overflow:auto;flex:1;min-height:120px}.card{width:100%;display:block;text-align:left;padding:11px;margin-bottom:8px;border:1px solid rgb(var(--c-border));border-radius:8px;background:rgb(var(--c-surface));box-shadow:0 1px 3px #00000008}.card:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}.card h3{font-size:12px;line-height:1.5;font-weight:550;margin:8px 0;overflow-wrap:anywhere}.card-meta{display:flex;justify-content:space-between;gap:8px;font-size:9px;color:rgb(var(--c-text-muted));text-transform:uppercase;letter-spacing:.04em}.card-repos{font-size:10px;color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.labels{display:flex;gap:4px;margin-top:8px;font-size:9px;flex-wrap:wrap}.labels span{padding:2px 5px;border-radius:4px;background:color-mix(in srgb,rgb(var(--c-accent)) 9%,transparent);color:rgb(var(--c-text-muted))}.empty{text-align:center;color:rgb(var(--c-text-muted));font-size:11px;margin:22px}.paging{display:flex;gap:5px;padding:8px}.paging button{font-size:10px;padding:4px 6px}.banner{padding:8px 20px;font-size:12px;border-bottom:1px solid rgb(var(--c-border));display:flex;align-items:center;justify-content:space-between;gap:10px}.error{color:#d15a64}
</style>
