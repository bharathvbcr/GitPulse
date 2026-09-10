<script lang="ts">
  import { onMount } from "svelte";
  import { crossfade } from "svelte/transition";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { Inbox, Plus, RefreshCw, Search } from "@lucide/svelte";
  import { isMacOS, isTauri } from "../platform";
  import { liquidSelection } from "../ui/transitions";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { repoStore } from "../stores/repoStore";
  import { LAYERS } from "../ui/layers";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { cardFace, dragExceeded, insertIndexFromY, insertionNeighbors, insertionPosition, neighborStatus, parseColumnStatus, shouldCommitMove, visibleStatuses } from "../workbench/boardDrag";
  import { explainError, getTask, getWorkspace, listAttention, listRepositories, listTasks, listWorkspaces, newID, putTask, putWorkspace, registerRepository, STATUSES, STATUS_LABELS, taskDraft, taskWrite, workspaceDraft, type Page, type Repository, type Scope, type Task, type TaskCard, type TaskStatus, type Workspace, type WorkspaceCard } from "../workbench/client";
  import { addableOpenTabs, membershipAfterAttach, openAddActionLabel, openMembershipCandidates, pickerSelectionIds } from "../workbench/openMembership";
  import TaskEditor from "./TaskEditor.svelte";
  import WorkspaceEditor from "./WorkspaceEditor.svelte";
  import AutomaticEnhancements from "./AutomaticEnhancements.svelte";
  import AttentionInbox from "./AttentionInbox.svelte";

  let { repositoryPath = null, active = true }: { repositoryPath?: string | null; active?: boolean } = $props();
  const macos = isMacOS();
  const [sendScope, receiveScope] = crossfade(liquidSelection());
  let scope = $state<Scope>({ kind: "global" });
  let repositories = $state<Repository[]>([]), workspaces = $state<WorkspaceCard[]>([]);
  let repositoryCursor = $state<string | null>(null), workspaceCursor = $state<string | null>(null);
  let repositoryTotal = $state(0), workspaceTotal = $state(0);
  let columns = $state<Partial<Record<TaskStatus, Page<TaskCard>>>>({});
  let search = $state(""); let initialized = $state(false); let loading = $state(false);
  let error = $state(""); let catalogError = $state(""); let announce = $state("");
  let taskEditor = $state<{ value: Task | null } | null>(null);
  let workspaceEditor = $state<{ value: Workspace | null } | null>(null);
  let opening = $state(false); let moving = $state(false);
  let press = $state<{ card: TaskCard; x: number; y: number } | null>(null);
  let drag = $state<{ card: TaskCard; over: TaskStatus | null; insertIndex: number; x: number; y: number } | null>(null);
  let skipClick = false;
  let showInbox = $state(false);
  let unread = $state(0);
  let addMenu = $state(false);
  let adding = $state(false);
  let addMenuEl: HTMLDivElement | undefined = $state();
  let workspaceMemberIds = $state<string[] | null>(null);
  let revision = 0; let disposed = false; let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const openTabRefs = $derived($repoStore.openTabs.map((tab) => ({ path: tab.path, label: tab.label })));
  const catalogIds = $derived(repositories.map((repo) => repo.id));
  const selectedForMenu = $derived(pickerSelectionIds(scope.kind, workspaceMemberIds, catalogIds));
  const menuTabs = $derived(addableOpenTabs(openMembershipCandidates(openTabRefs, repositories, selectedForMenu, pathOpts)));
  const emptyAddLabel = $derived(openAddActionLabel(menuTabs));
  const title = $derived.by(() => {
    const target = scope;
    return target.kind === "global" ? "Tasks" : target.kind === "workspace"
      ? workspaces.find((w) => w.id === target.id)?.name ?? "Workspace"
      : repositories.find((r) => r.id === target.id)?.name ?? "Repository";
  });
  const total = $derived(STATUSES.reduce((sum, status) => sum + (columns[status]?.total ?? 0), 0));
  const counts = $derived(Object.fromEntries(STATUSES.map((status) => [status, columns[status]?.total ?? 0])) as Partial<Record<TaskStatus, number>>);
  const shown = $derived(visibleStatuses(counts, drag !== null));
  function repoName(id: string) { return repositories.find((repo) => repo.id === id)?.name; }

  async function catalog() {
    const [repos, groups] = await Promise.all([listRepositories(), listWorkspaces()]);
    if (disposed) return;
    repositories = repos.items; repositoryTotal = repos.total; repositoryCursor = repos.next_cursor;
    workspaces = groups.items; workspaceTotal = groups.total; workspaceCursor = groups.next_cursor;
    catalogError = "";
  }
  async function loadBoard(target: Scope = scope, query: string = search) {
    const generation = ++revision; loading = true; error = "";
    try {
      const pages = await Promise.all(STATUSES.map(async (status) => [status, await listTasks(target, status, query)] as const));
      if (generation !== revision || disposed) return;
      columns = Object.fromEntries(pages);
    } catch (cause) { if (generation === revision && !disposed) error = explainError(cause); }
    finally { if (generation === revision && !disposed) loading = false; }
  }
  async function loadUnread() {
    try {
      const page = await listAttention(scope, "unread");
      if (!disposed) unread = page.total;
    } catch { /* keep the last badge rather than invent a zero */ }
  }
  async function refresh() {
    try { await catalog(); } catch (cause) { catalogError = explainError(cause); }
    if (initialized && active && !disposed) { await loadBoard(); await loadUnread(); }
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
    if (isTauri()) void listen("workbench-changed", scheduleRefresh).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch((cause) => { if (!disposed) error = `Live updates unavailable: ${explainError(cause)}`; });
    const onPointerDown = (event: PointerEvent) => { if (addMenu && shouldDismissOverlay(event.target, "[data-add-repo]")) addMenu = false; };
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape" && addMenu) addMenu = false; };
    window.addEventListener("focus", scheduleRefresh);
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      disposed = true; revision++; clearTimeout(refreshTimer); unlisten?.();
      window.removeEventListener("focus", scheduleRefresh);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKey);
    };
  });
  $effect(() => {
    if (!initialized || !active) return;
    const target = scope, query = search;
    loading = true;
    const timer = setTimeout(() => { void loadBoard(target, query); void loadUnread(); }, 250);
    return () => { clearTimeout(timer); revision++; };
  });
  $effect(() => {
    if (scope.kind !== "workspace") {
      workspaceMemberIds = null;
      return;
    }
    const id = scope.id;
    void getWorkspace(id).then((full) => {
      if (disposed || scope.kind !== "workspace" || scope.id !== id) return;
      workspaceMemberIds = full.repository_ids;
    }).catch((cause) => { if (!disposed) catalogError = explainError(cause); });
  });
  $effect(() => {
    if (addMenu && addMenuEl) addMenuEl.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
  });
  async function attachRegistered(repos: Repository[]) {
    if (scope.kind !== "workspace" || repos.length === 0) return;
    const full = await getWorkspace(scope.id);
    if (disposed) return;
    const next = membershipAfterAttach(full.repository_ids, repos.map((repo) => repo.id));
    if (next.length === full.repository_ids.length) {
      workspaceMemberIds = full.repository_ids;
      return;
    }
    const saved = await putWorkspace({ ...workspaceDraft(full), id: full.id, expected_revision: full.revision, request_id: newID(), repository_ids: next });
    if (scope.kind === "workspace" && scope.id === saved.id) workspaceMemberIds = saved.repository_ids;
  }
  async function addPaths(paths: string[]) {
    addMenu = false;
    if (paths.length === 0 || adding) return;
    adding = true;
    try {
      const added: Repository[] = [];
      for (const path of paths) {
        added.push(await registerRepository(path));
        if (disposed) return;
      }
      await attachRegistered(added);
      if (disposed) return;
      await catalog();
      announce = added.length === 1 ? `Added ${added[0].name}` : `Added ${added.length} repositories`;
    } catch (cause) { catalogError = explainError(cause); }
    finally { adding = false; }
  }
  async function pickFolder() {
    addMenu = false;
    try {
      const path = await invoke<string | null>("cmd_pick_folder");
      if (!path) return;
      await addPaths([path]);
    } catch (cause) { catalogError = explainError(cause); }
  }
  function toggleAddMenu() {
    if (adding) return;
    if (menuTabs.length === 0) { void pickFolder(); return; }
    addMenu = !addMenu;
  }
  function onAddMenuKey(event: KeyboardEvent) {
    const items = [...(event.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('[role="menuitem"]')];
    if (items.length === 0) return;
    const index = Math.max(0, items.indexOf(event.target as HTMLElement));
    if (event.key === "ArrowDown") { event.preventDefault(); items[(index + 1) % items.length]?.focus(); }
    else if (event.key === "ArrowUp") { event.preventDefault(); items[(index - 1 + items.length) % items.length]?.focus(); }
    else if (event.key === "Home") { event.preventDefault(); items[0]?.focus(); }
    else if (event.key === "End") { event.preventDefault(); items[items.length - 1]?.focus(); }
    else if (event.key === "Escape") { event.preventDefault(); addMenu = false; }
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
  function statusAtPoint(x: number, y: number): TaskStatus | null {
    const node = document.elementFromPoint(x, y);
    if (!(node instanceof Element)) return null;
    return parseColumnStatus(node.closest("[data-task-column]")?.getAttribute("data-task-column"));
  }
  function slotAtPoint(y: number, status: TaskStatus | null, draggedId: string): number {
    if (!status) return 0;
    const col = document.querySelector(`[data-task-column="${status}"]`);
    if (!col) return 0;
    const mids: number[] = [];
    for (const el of col.querySelectorAll("[data-task-card]")) {
      if (!(el instanceof HTMLElement) || el.dataset.cardId === draggedId) continue;
      const rect = el.getBoundingClientRect();
      mids.push(rect.top + rect.height / 2);
    }
    return insertIndexFromY(mids, y);
  }
  function releasePointer(target: EventTarget | null, pointerId: number) {
    if (target instanceof HTMLElement && target.hasPointerCapture(pointerId)) target.releasePointerCapture(pointerId);
  }
  function onCardPointerDown(e: PointerEvent, card: TaskCard) {
    if (e.button !== 0 || moving || opening) return;
    press = { card, x: e.clientX, y: e.clientY };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onCardPointerMove(e: PointerEvent) {
    if (!press) return;
    if (!drag) {
      if (!dragExceeded(e.clientX - press.x, e.clientY - press.y)) return;
      drag = { card: press.card, over: press.card.status, insertIndex: 0, x: e.clientX, y: e.clientY };
    }
    const over = statusAtPoint(e.clientX, e.clientY);
    drag = { card: drag.card, over, insertIndex: slotAtPoint(e.clientY, over, drag.card.id), x: e.clientX, y: e.clientY };
  }
  function onCardPointerUp(e: PointerEvent) {
    const current = drag;
    drag = null;
    press = null;
    releasePointer(e.currentTarget, e.pointerId);
    if (!current) return;
    skipClick = true;
    requestAnimationFrame(() => { skipClick = false; });
    const over = current.over;
    const fromIndex = (columns[current.card.status]?.items ?? []).findIndex((item) => item.id === current.card.id);
    if (!shouldCommitMove(current.card.status, over, { moving, fromIndex, insertIndex: current.insertIndex })) return;
    const items = columns[over]?.items ?? [];
    const { before, after } = insertionNeighbors(items, current.card.id, current.insertIndex);
    void moveCard(current.card, over, insertionPosition(before, after));
  }
  function onCardClick(card: TaskCard) {
    if (skipClick) return;
    void openTask(card.id);
  }
  function onCardPointerCancel(e: PointerEvent) {
    drag = null;
    press = null;
    releasePointer(e.currentTarget, e.pointerId);
  }
  function onCardKeydown(e: KeyboardEvent, card: TaskCard) {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const next = neighborStatus(card.status, e.key === "ArrowRight" ? 1 : -1);
    if (!next) return;
    const last = (columns[next]?.items ?? []).at(-1);
    void moveCard(card, next, insertionPosition(last?.position ?? null, null));
  }
  async function moveCard(card: TaskCard, status: TaskStatus, position: number) {
    if (moving) return;
    if (card.status === status && card.position === position) return;
    moving = true; error = "";
    try {
      const full = await getTask(card.id);
      if (full.revision !== card.revision) throw new Error("This task changed while you were moving it. Refresh and try again.");
      await putTask(taskWrite(full.id, full.revision, { ...taskDraft(full), status, position }));
      announce = `Moved to ${STATUS_LABELS[status]}`;
      await loadBoard();
    } catch (cause) { error = explainError(cause); } finally { moving = false; }
  }
  function createTask() {
    if (taskEditor && !window.confirm("Start a new task and discard the current unsaved edits?")) return;
    workspaceEditor = null; taskEditor = { value: null };
  }
  function insertBefore(status: TaskStatus, cardId: string): boolean {
    if (!drag || drag.over !== status || drag.card.id === cardId) return false;
    const rest = (columns[status]?.items ?? []).filter((item) => item.id !== drag?.card.id);
    return rest.findIndex((item) => item.id === cardId) === drag.insertIndex;
  }
  function insertAtEnd(status: TaskStatus): boolean {
    if (!drag || drag.over !== status) return false;
    const rest = (columns[status]?.items ?? []).filter((item) => item.id !== drag?.card.id);
    return drag.insertIndex >= rest.length;
  }
</script>

{#snippet scopeSelection(selected: boolean)}
  {#if macos && selected}
    <span class="gp-liquid-selection gp-gpu" aria-hidden="true"
      in:receiveScope={{ key: "task-scope" }} out:sendScope={{ key: "task-scope" }}></span>
  {/if}
{/snippet}

<div class="workbench bg-background" class:is-dragging={drag !== null} data-testid="task-board">
  {#if !repositoryPath}
    <nav class="navigator gp-glass" class:gp-liquid-tabs={macos} aria-label="Task scopes">
      <div class="nav-heading">Workspaces<button type="button" class="icon" title="New workspace" aria-label="New workspace" onclick={() => { workspaceEditor = { value: null }; taskEditor = null; }}><Plus size={12} /></button></div>
      <button type="button" class="gp-seg-btn" class:selected={scope.kind === "global"} aria-pressed={scope.kind === "global"} data-active={scope.kind === "global"} onclick={() => { scope = { kind: "global" }; }}>
        {@render scopeSelection(scope.kind === "global")}<span>All</span>
      </button>
      {#each [...workspaces].sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.position - b.position) as group (group.id)}
        <div class="nav-row"><button type="button" class="gp-seg-btn" class:selected={scope.kind === "workspace" && scope.id === group.id} aria-pressed={scope.kind === "workspace" && scope.id === group.id} data-active={scope.kind === "workspace" && scope.id === group.id} onclick={() => { scope = { kind: "workspace", id: group.id }; }} title={group.name}>
          {@render scopeSelection(scope.kind === "workspace" && scope.id === group.id)}<span>{group.icon} {group.name}{group.archived ? " · Archived" : ""}</span>
        </button><button type="button" class="icon" aria-label={`Edit ${group.name}`} onclick={() => editWorkspace(group.id)} disabled={opening}>⋯</button></div>
      {/each}
      {#if workspaceCursor}<button type="button" onclick={moreWorkspaces}>More ({workspaces.length}/{workspaceTotal})</button>{/if}
      <div class="nav-heading" data-add-repo>Repositories<button type="button" class="icon" aria-haspopup="menu" aria-expanded={addMenu} aria-controls="task-add-repo-menu" aria-busy={adding} title="Add repository" aria-label="Add repository" disabled={adding} onclick={toggleAddMenu}><Plus size={12} /></button>
        {#if addMenu}
          <div bind:this={addMenuEl} id="task-add-repo-menu" class="add-menu gp-menu" role="menu" aria-label="Add repository" tabindex="-1" style="z-index: {LAYERS.MENU}" onkeydown={onAddMenuKey}>
            {#if menuTabs.length}<div class="add-menu-label">Open</div>{/if}
            {#each menuTabs as tab (tab.path)}
              <button type="button" class="add-item" role="menuitem" title={tab.path} onclick={() => void addPaths([tab.path])}>
                <span class="add-name">{tab.label}</span>
                <span class="add-path">{tab.path}</span>
              </button>
            {/each}
            {#if menuTabs.length > 1}<button type="button" role="menuitem" onclick={() => void addPaths(menuTabs.map((tab) => tab.path))}>Add all open</button>{/if}
            <button type="button" role="menuitem" onclick={() => void pickFolder()}>Choose folder…</button>
          </div>
        {/if}
      </div>
      {#each repositories as repo (repo.id)}<button type="button" class="gp-seg-btn" class:selected={scope.kind === "repository" && scope.id === repo.id} aria-pressed={scope.kind === "repository" && scope.id === repo.id} data-active={scope.kind === "repository" && scope.id === repo.id} onclick={() => { scope = { kind: "repository", id: repo.id }; }} title={repo.identity_key}>
        {@render scopeSelection(scope.kind === "repository" && scope.id === repo.id)}<span>{repo.name}</span>
      </button>{/each}
      {#if repositoryCursor}<button type="button" onclick={moreRepositories}>More ({repositories.length}/{repositoryTotal})</button>{/if}
    </nav>
  {/if}
  <main class="board-main">
    <header class="gp-glass">
      <h1>{title}{#if !loading && initialized}<span>{total}</span>{/if}</h1>
      <div class="actions">
        <label class="search bg-surface"><Search size={12} /><input aria-label="Search tasks" type="search" bind:value={search} placeholder="Search" maxlength="512" /></label>
        {#if initialized}<AutomaticEnhancements {active} compact />{/if}
        {#if initialized}
          <button type="button" class="gp-icon-btn" aria-pressed={showInbox} aria-label="Inbox" title="Inbox" onclick={() => { showInbox = !showInbox; }}>
            <Inbox size={13} />
            {#if unread > 0}<span class="gp-pill">{unread}</span>{/if}
          </button>
        {/if}
        <button type="button" class="gp-icon-btn" aria-label="Refresh" title="Refresh" onclick={() => initialized ? refresh() : initialize()} disabled={loading}><RefreshCw size={13} /></button>
        <button type="button" class="gp-btn-primary" onclick={createTask} disabled={!initialized || !repositories.length}>New</button>
        {#if initialized && !repositories.length}
          {#if emptyAddLabel}
            <button type="button" class="hint-action" onclick={() => void addPaths(menuTabs.map((tab) => tab.path))} disabled={adding}>{emptyAddLabel}</button>
          {:else}
            <span class="hint">Add a repository to create tasks</span>
          {/if}
        {/if}
      </div>
    </header>
    {#if showInbox}<AttentionInbox {scope} {active} onopen={openTask} />{/if}
    {#if catalogError}<div class="banner error" role="alert">{catalogError}<button type="button" onclick={() => initialized ? refresh() : initialize()}>Retry</button></div>{/if}
    {#if error}<div class="banner error" role="alert">{error}</div>{/if}
    <div class="sr-only" role="status" aria-live="polite">{announce}</div>
    <div class="columns" aria-busy={loading || moving} data-testid="task-columns">
      {#each shown as status (status)}
        <section
          class="column gp-glass bg-surface/45"
          class:drop-target={drag !== null && drag.over === status}
          data-task-column={status}
          data-testid="task-column"
          aria-label={STATUS_LABELS[status]}
        >
          <div class="column-title bg-surface"><span>{STATUS_LABELS[status]}</span><span>{columns[status]?.total ?? "—"}</span></div>
          <div class="cards">
            {#each columns[status]?.items ?? [] as card (card.id)}
              {@const face = cardFace(card, repoName)}
              {#if insertBefore(status, card.id)}<div class="insert" aria-hidden="true"></div>{/if}
              <button
                type="button"
                class="card bg-surface"
                class:dragging={drag?.card.id === card.id}
                data-testid="task-card"
                data-task-card
                data-card-id={card.id}
                draggable="false"
                aria-grabbed={drag?.card.id === card.id}
                aria-keyshortcuts="ArrowLeft ArrowRight"
                disabled={opening}
                onpointerdown={(e) => onCardPointerDown(e, card)}
                onpointermove={onCardPointerMove}
                onpointerup={onCardPointerUp}
                onpointercancel={onCardPointerCancel}
                onclick={() => onCardClick(card)}
                onkeydown={(e) => onCardKeydown(e, card)}
              >
                <div class="card-meta">
                  {#if face.pip !== null}<span class="pip" data-priority={face.pip}></span>{/if}
                  <h3>{face.title}</h3>
                </div>
                {#if face.repo}<div class="card-repos">{face.repo}</div>{/if}
                {#if face.labels.length}<div class="labels">{#each face.labels as label}<span>{label}</span>{/each}</div>{/if}
              </button>
            {/each}
            {#if insertAtEnd(status)}<div class="insert" aria-hidden="true"></div>{/if}
          </div>
          {#if columns[status] && (columns[status]?.total ?? 0) > 30}<div class="paging"><button type="button" class="gp-btn" onclick={() => pageColumn(status)} disabled={loading}>First</button>{#if columns[status]?.next_cursor}<button type="button" class="gp-btn" onclick={() => pageColumn(status, columns[status]?.next_cursor ?? undefined)} disabled={loading}>Next</button>{/if}</div>{/if}
        </section>
      {/each}
    </div>
  </main>
  {#if drag}
    <div class="ghost gp-glass bg-surface shadow-float" style="transform: translate({drag.x + 10}px, {drag.y + 10}px)" data-testid="task-drag-ghost">{drag.card.title}</div>
  {/if}
  {#if taskEditor}{#key taskEditor}<TaskEditor {active} value={taskEditor.value} {repositories} {workspaces} openTabs={openTabRefs} primary={scope.kind === "repository" ? scope.id : repositories[0]?.id ?? ""} home={scope.kind === "workspace" ? scope.id : null} onSaved={() => { void loadBoard(); }} onClose={() => { taskEditor = null; }} />{/key}{/if}
  {#if workspaceEditor}{#key workspaceEditor}<WorkspaceEditor value={workspaceEditor.value} {repositories} openTabs={openTabRefs} onSaved={() => { scope = { kind: "global" }; void refresh(); }} onClose={() => { workspaceEditor = null; }} />{/key}{/if}
</div>

<style>
  .workbench{position:relative;display:flex;flex:1;min-height:0;min-width:0;color:rgb(var(--c-text));overflow:hidden}
  .workbench.is-dragging{cursor:grabbing;user-select:none}
  .navigator{width:188px;flex-shrink:0;border-right:1px solid rgb(var(--c-border));padding:10px 8px;overflow:auto}
  .navigator button{display:block;width:100%;text-align:left;border:0;padding:6px 8px;border-radius:7px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px}
  .navigator button:hover,button:hover{background:var(--mac-fill-surface-hover,rgb(var(--c-surface-hover)))}
  .navigator button.selected{background:color-mix(in srgb,rgb(var(--c-accent)) 13%,transparent);color:rgb(var(--c-accent))}
  .navigator.gp-liquid-tabs{padding:10px 8px}
  .navigator.gp-liquid-tabs button.selected{background:transparent}
  .navigator .gp-seg-btn > span:not(:global(.gp-liquid-selection)){display:block;overflow:hidden;text-overflow:ellipsis}
  .nav-heading{position:relative;display:flex;align-items:center;justify-content:space-between;padding:10px 8px 4px;color:rgb(var(--c-text-muted));font-size:10px;font-weight:650;letter-spacing:.04em;text-transform:uppercase}
  .nav-heading > button,.icon,.nav-row>button:last-child{width:26px;height:26px;padding:0;flex-shrink:0;display:inline-flex;align-items:center;justify-content:center}
  .add-menu{position:absolute;right:0;top:calc(100% + 4px);width:min(260px,70vw);max-height:min(16rem,50vh);overflow:auto}
  .add-menu-label{padding:4px 8px;font-size:10px;color:rgb(var(--c-text-muted))}
  .navigator .add-menu button{width:100%;height:auto;padding:6px 8px;white-space:normal;overflow:visible}
  .add-item{display:flex;flex-direction:column;align-items:stretch;gap:1px}
  .add-name,.add-path{display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .add-path{font-size:10px;color:rgb(var(--c-text-muted))}
  .hint-action{border:0;background:transparent;padding:0;color:rgb(var(--c-accent));font-size:11px}
  .nav-row{display:flex}
  .nav-row>button:first-child{min-width:0;flex:1}
  .board-main{flex:1;min-width:0;display:flex;flex-direction:column;overflow:hidden}
  header{padding:10px 14px;display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap;border-bottom:1px solid rgb(var(--c-border))}
  h1{font-size:15px;line-height:1.2;font-weight:650;margin:0;display:flex;align-items:baseline;gap:8px}
  h1 span{font-size:11px;font-weight:500;color:rgb(var(--c-text-muted))}
  .actions{display:flex;gap:6px;align-items:center;flex-wrap:wrap}
  button,input{font-size:12px}
  button:disabled{opacity:.5}
  .hint{font-size:11px;color:rgb(var(--c-text-muted))}
  .search{display:flex;align-items:center;gap:6px;border:1px solid rgb(var(--c-border));border-radius:7px;padding:0 8px;color:rgb(var(--c-text-muted))}
  .search input{border:0;background:transparent;padding:6px 0;width:140px;color:inherit}
  .columns{display:flex;gap:8px;padding:12px;overflow:auto;flex:1;min-height:0;align-items:stretch}
  .column{width:220px;min-width:196px;flex:1;display:flex;flex-direction:column;border-radius:10px;border:1px solid rgb(var(--c-border));overflow:hidden;min-height:0}
  .column.drop-target{border-color:rgb(var(--c-accent));background:color-mix(in srgb,rgb(var(--c-accent)) 10%,transparent)}
  .column-title{display:flex;align-items:center;justify-content:space-between;font-weight:650;font-size:12px;border-bottom:1px solid rgb(var(--c-border));padding:8px 10px}
  .column-title span:last-child{color:rgb(var(--c-text-muted));font-weight:400}
  .cards{padding:6px;overflow:auto;flex:1;min-height:80px}
  .card{width:100%;display:block;text-align:left;padding:8px 9px;margin-bottom:6px;border:1px solid rgb(var(--c-border));border-radius:8px;cursor:grab;touch-action:none;user-select:none}
  .card:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}
  .card.dragging{opacity:.35;cursor:grabbing}
  .card h3{font-size:12px;line-height:1.4;font-weight:550;margin:0;overflow-wrap:anywhere;min-width:0}
  .card-meta{display:flex;align-items:flex-start;gap:6px}
  .pip{width:7px;height:7px;margin-top:4px;border-radius:99px;flex-shrink:0;background:rgb(var(--c-accent))}
  .pip[data-priority="0"]{background:#d15a64}
  .insert{height:2px;margin:2px 4px;border-radius:2px;background:rgb(var(--c-accent))}
  .card-repos{font-size:10px;color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap;margin-top:4px}
  .labels{display:flex;gap:4px;margin-top:6px;font-size:9px;flex-wrap:wrap}
  .labels span{padding:1px 5px;border-radius:4px;background:color-mix(in srgb,rgb(var(--c-accent)) 9%,transparent);color:rgb(var(--c-text-muted))}
  .paging{display:flex;gap:5px;padding:6px}
  .paging button{font-size:10px;padding:3px 6px}
  .banner{padding:7px 14px;font-size:12px;border-bottom:1px solid rgb(var(--c-border));display:flex;align-items:center;justify-content:space-between;gap:10px}
  .error{color:#d15a64}
  .ghost{position:fixed;top:0;left:0;z-index:20;pointer-events:none;max-width:220px;padding:6px 10px;border-radius:8px;border:1px solid rgb(var(--c-accent));font-size:12px;font-weight:550;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
</style>
