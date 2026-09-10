<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { Inbox, Plus, RefreshCw, Search, X, FolderGit2, Layers, CheckCheck, Ellipsis, List, Columns3, Trash2, Sparkles } from "@lucide/svelte";
  import { isTauri } from "../platform";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { repoStore } from "../stores/repoStore";
  import { LAYERS } from "../ui/layers";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { PRIORITY_LABELS, cardFace, dragExceeded, insertIndexFromY, insertionNeighbors, insertionPosition, neighborStatus, parseColumnStatus, shouldCommitMove, visibleStatuses } from "../workbench/boardDrag";
  import { explainError, getTask, getWorkspace, listAttention, listRepositories, listTasks, listWorkspaces, newID, putWorkspace, registerRepository, STATUSES, STATUS_LABELS, workspaceDraft, type Page, type Repository, type Scope, type Task, type TaskCard, type TaskStatus, type Workspace, type WorkspaceCard } from "../workbench/client";
  import { addableOpenTabs, membershipAfterAttach, openAddActionLabel, openMembershipCandidates, pickerSelectionIds } from "../workbench/openMembership";
  import TaskContextMenu from "./TaskContextMenu.svelte";
  import TaskActionDialog from "./TaskActionDialog.svelte";
  import { copyText } from "../desktop/clipboard";
  import { getTaskBrief } from "../workbench/client";
  import { MAX_TASK_SELECTION, TaskBatch, bounded, type TaskAction } from "../workbench/taskActions";
  import { organizeTasks, selectedRange, reorderPlan, type TaskFilter, type TaskSort } from "../workbench/taskOrganization";
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
  let error = $state(""); let catalogError = $state(""); let announce = $state("");
  let taskEditor = $state<{ value: Task | null; status?: TaskStatus; mode?: "details" | "enhance"; autoEnhance?: boolean; primary?: string; home?: string | null } | null>(null);
  let editorHandle = $state<{ canLeave: () => Promise<boolean>; showEnhancement: () => void; savedID: () => string | null; loadCopy: (task: Task) => void }>();
  let workspaceHandle = $state<{ canLeave: () => Promise<boolean> }>();
  let returnFocus: HTMLElement | null = null;
  let board: HTMLDivElement;
  let taskSearch: HTMLInputElement;
  let layout = $state("board"), sort = $state<TaskSort>("manual"), filter = $state<TaskFilter>("all");
  let labelFilter = $state(""), kindFilter = $state("");
  let selection = $state<string[]>([]), selectionAnchor: string | null = null;
  let contextMenu = $state<{x:number; y:number; cards:TaskCard[]; focus:HTMLElement|null} | null>(null);
  let actionDialog = $state<{cards:TaskCard[]; action:TaskAction} | null>(null);
  let pendingUpdate = $state<TaskBatch | null>(null);
  let actionNote = $state("");
  const loadedCards = $derived(STATUSES.flatMap(status => displayColumns[status]?.items ?? []));
  const organized = $derived(Object.fromEntries(STATUSES.map(status => [status, organizeTasks(displayColumns[status]?.items ?? [],sort,filter,labelFilter,kindFilter)])));
  const visibleCards = $derived(STATUSES.flatMap(status => organized[status] ?? []));
  const selectedCards = $derived(visibleCards.filter(card => selection.includes(card.id)));
  const filtered = $derived(filter !== "all" || Boolean(labelFilter.trim()) || Boolean(kindFilter));
  const canReorder = $derived(sort === "manual" && !filtered && layout === "board");
  const kinds = $derived([...new Set(loadedCards.map(card => card.kind))].sort());
  $effect(() => { boardKey; repositoryPath; filter; sort; labelFilter; kindFilter; selection = []; selectionAnchor = null; contextMenu = null; });
  onMount(() => { try { const saved = localStorage.getItem("gitpulse.tasks.layout"); if (saved === "list" || saved === "board") layout = saved; } catch { /* Layout preference is optional. */ } });
  $effect(() => { try { localStorage.setItem("gitpulse.tasks.layout",layout); } catch { /* Storage restrictions do not block task work. */ } });
  let loadedKey = $state("");
  let unreadError = $state("");
  let unreadRevision = 0, initializationRevision = 0, openingRevision = 0;
  const boardKey = $derived(JSON.stringify([scope, search]));
  const displayColumns = $derived(loadedKey === boardKey ? columns : {});
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
  const primaryForNew = $derived(scope.kind === "repository" ? scope.id : scope.kind === "workspace" ? workspaceMemberIds?.[0] ?? "" : repositories[0]?.id ?? "");

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
  const total = $derived(STATUSES.reduce((sum, status) => sum + (displayColumns[status]?.total ?? 0), 0));
  const counts = $derived(Object.fromEntries(STATUSES.map((status) => [status, displayColumns[status]?.total ?? 0])) as Partial<Record<TaskStatus, number>>);
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
    const generation = ++revision; const key = JSON.stringify([target, query]); loading = true; error = "";
    try {
      const pages = await Promise.all(STATUSES.map(async (status) => [status, await listTasks(target, status, query)] as const));
      if (generation !== revision || disposed || key !== boardKey) return;
      columns = Object.fromEntries(pages); loadedKey = key;
    } catch (cause) { if (generation === revision && !disposed) error = explainError(cause); }
    finally { if (generation === revision && !disposed) loading = false; }
  }
  async function loadUnread(target: Scope = scope) {
    const ticket = ++unreadRevision, key = JSON.stringify(target);
    try {
      const page = await listAttention(target, "unread");
      if (!disposed && ticket === unreadRevision && key === JSON.stringify(scope)) { unread = page.total; unreadError = ""; }
    } catch (cause) {
      if (!disposed && ticket === unreadRevision && key === JSON.stringify(scope)) unreadError = explainError(cause);
    }
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
  async function initialize(path: string | null = repositoryPath) {
    const ticket = ++initializationRevision;
    revision++; unreadRevision++; initialized = false; loading = true; catalogError = "";
    try {
      const repo = path ? await registerRepository(path) : null;
      if (disposed || ticket !== initializationRevision) return;
      scope = repo ? { kind: "repository", id: repo.id } : { kind: "global" };
      await catalog();
      if (!disposed && ticket === initializationRevision) initialized = true;
    } catch (cause) { if (!disposed && ticket === initializationRevision) { catalogError = explainError(cause); loading = false; } }
  }
  $effect(() => { const path = repositoryPath; untrack(() => { void initialize(path); }); });
  onMount(() => {
    let unlisten: (() => void) | undefined;
    if (isTauri()) void listen("workbench-changed", scheduleRefresh).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch((cause) => { if (!disposed) error = `Live updates unavailable: ${explainError(cause)}`; });
    const onPointerDown = (event: PointerEvent) => { if (addMenu && shouldDismissOverlay(event.target, "[data-add-repo]")) addMenu = false; };
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape" && addMenu) addMenu = false; };
    window.addEventListener("focus", scheduleRefresh);
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      disposed = true; revision++; unreadRevision++; initializationRevision++; openingRevision++; clearTimeout(refreshTimer); unlisten?.();
      window.removeEventListener("focus", scheduleRefresh);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKey);
    };
  });
  $effect(() => {
    if (!initialized || !active) return;
    const target = scope, query = search;
    loading = true; press = null; drag = null;
    const timer = setTimeout(() => { void loadBoard(target, query); void loadUnread(target); }, 250);
    return () => { clearTimeout(timer); revision++; };
  });
  $effect(() => {
    if (scope.kind !== "workspace") {
      workspaceMemberIds = null;
      return;
    }
    const id = scope.id;
    workspaceMemberIds = null;
    void getWorkspace(id).then((full) => {
      if (disposed || scope.kind !== "workspace" || scope.id !== id) return;
      workspaceMemberIds = full.repository_ids;
    }).catch((cause) => { if (!disposed && scope.kind === "workspace" && scope.id === id) catalogError = explainError(cause); });
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
  async function leaveSheet(): Promise<boolean> { return (!taskEditor || await editorHandle?.canLeave() === true) && (!workspaceEditor || await workspaceHandle?.canLeave() === true); }
  async function newWorkspace() {
    if (opening || !await leaveSheet() || disposed) return;
    workspaceEditor = { value: null }; taskEditor = null;
  }
  async function editWorkspace(id: string) {
    if (opening) return;
    opening = true;
    const ticket = ++openingRevision;
    try { const full = await getWorkspace(id); if (disposed || ticket !== openingRevision || !await leaveSheet() || disposed || ticket !== openingRevision) return; taskEditor = null; workspaceEditor = { value: full }; }
    catch (cause) { if (!disposed && ticket === openingRevision) error = explainError(cause); } finally { if (!disposed && ticket === openingRevision) opening = false; }
  }
  async function openTask(id: string, mode: "details" | "enhance" = "details") {
    if (opening || actionDialog) return;
    if (editorHandle?.savedID() === id) { if (mode === "enhance") editorHandle.showEnhancement(); return; }
    returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    opening = true;
    const ticket = ++openingRevision;
    try { const full = await bounded(getTask(id)); if (disposed || ticket !== openingRevision || !await leaveSheet() || disposed || ticket !== openingRevision) return; workspaceEditor = null; taskEditor = { value: full, mode, autoEnhance:mode === "enhance" }; }
    catch (cause) { if (!disposed && ticket === openingRevision) error = explainError(cause); } finally { if (!disposed && ticket === openingRevision) opening = false; }
  }
  function closeTask() {
    taskEditor = null; openingRevision++; opening = false;
    void tick().then(() => { if (!disposed && returnFocus?.isConnected) returnFocus.focus(); });
  }
  async function pageColumn(status: TaskStatus, cursor?: string) {
    if (loading || moving || loadedKey !== boardKey) return;
    const generation = revision, key = boardKey;
    loading = true; error = "";
    try {
      const result = await listTasks(scope, status, search, cursor);
      if (generation !== revision || disposed || key !== boardKey) return;
      const items = cursor ? [...new Map([...(columns[status]?.items ?? []), ...result.items].map((item) => [item.id, item])).values()] : result.items;
      columns = { ...columns, [status]: { ...result, items, shown: items.length } };
    } catch (cause) { if (generation === revision && !disposed) error = explainError(cause); }
    finally { if (generation === revision && !disposed) loading = false; }
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
    if (e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || !canReorder || pendingUpdate || moving || opening || loading) return;
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
    const position = insertionPosition(before,after);
    if (after !== null && (position >= after || before !== null && position <= before)) {
      const plan = reorderPlan(items,current.card,current.insertIndex);
      if (columns[over]?.next_cursor || plan.cards.length > MAX_TASK_SELECTION) { error = `This column needs re-spacing. Load its remaining tasks first; up to ${MAX_TASK_SELECTION} tasks can be reordered together. Priority and title sorting remain available.`; return; }
      void applyUpdate(new TaskBatch(plan.cards,{kind:"reorder",status:over,positions:plan.positions}));
    } else void moveCard(current.card, over, position);
  }
  function onCardClick(card: TaskCard, event: MouseEvent) {
    if (skipClick) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey) { toggleSelection(card, event.shiftKey); return; }
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
  async function applyUpdate(batch: TaskBatch) {
    if (moving || (pendingUpdate && pendingUpdate !== batch)) return;
    pendingUpdate = batch; moving = true; error = "";
    await batch.run();
    if (disposed) return;
    const rows = batch.snapshot();
    moving = false;
    if (!rows.some(row => row.state === "uncertain")) pendingUpdate = null;
    const failure = rows.find(row => row.state !== "done");
    const done = rows.filter(row => row.state === "done").length;
    if (done) { actionNote = `${done} of ${rows.length} tasks updated`; await loadBoard(); }
    if (failure) error = failure.error;
  }
  async function moveCard(card: TaskCard, status: TaskStatus, position: number) {
    if (moving || loading || pendingUpdate || actionDialog) return;
    if (card.status === status && card.position === position) return;
    await applyUpdate(new TaskBatch([card],{kind:"update",changes:{status,position}}));
  }
  async function createTask(status: TaskStatus = "inbox", mode: "details" | "enhance" = "details") {
    const primary = primaryForNew, home = scope.kind === "workspace" ? scope.id : null;
    if (opening || !initialized || !primary || !await leaveSheet() || disposed) return;
    returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    workspaceEditor = null; taskEditor = { value: null, status, primary, home, mode };
  }
  function changeStatus(card: TaskCard, event: Event) {
    const select = event.currentTarget;
    if (!(select instanceof HTMLSelectElement)) return;
    const status = parseColumnStatus(select.value);
    select.value = card.status;
    if (status && status !== card.status) {
      const last = columns[status]?.items.at(-1);
      void moveCard(card, status, insertionPosition(last?.position ?? null, null));
    }
  }
  function toggleSelection(card: TaskCard, extend = false) {
    selection = selectedRange(visibleCards.map(card => card.id),selection,card.id,selectionAnchor,extend,MAX_TASK_SELECTION);
    selectionAnchor = card.id;
    if (selection.length === MAX_TASK_SELECTION) actionNote = `Selection is limited to ${MAX_TASK_SELECTION} loaded tasks per action.`;
  }
  function selectLoaded() {
    selection = visibleCards.slice(0,MAX_TASK_SELECTION).map(card => card.id);
    actionNote = `${selection.length} of ${visibleCards.length} visible loaded tasks selected. ${total - loadedCards.length} tasks remain unloaded.`;
  }
  function showTaskMenu(event: MouseEvent | KeyboardEvent, card: TaskCard, anchor?: HTMLElement | null) {
    event.preventDefault(); event.stopPropagation(); press = null; drag = null;
    if (opening || loading || moving || pendingUpdate || actionDialog) return;
    const target = anchor ?? (event.currentTarget instanceof HTMLElement ? event.currentTarget : null);
    const rect = target?.getBoundingClientRect();
    contextMenu = {cards:selection.includes(card.id) ? selectedCards : [card], x:event instanceof MouseEvent && event.clientX ? event.clientX : rect?.left ?? 8, y:event instanceof MouseEvent && event.clientY ? event.clientY : rect?.bottom ?? 8, focus:target};
  }
  function closeTaskMenu(restore = true) { const focus = contextMenu?.focus; contextMenu = null; if (restore && focus?.isConnected) focus.focus(); }
  async function confirmAction(cards: TaskCard[], action: TaskAction) {
    if (!cards.length || moving || pendingUpdate || actionDialog || !await leaveSheet() || disposed) return;
    actionDialog = {cards:[...cards],action};
  }
  function tasksChanged(ids: string[]) {
    if (ids.includes(editorHandle?.savedID() ?? "")) closeTask();
    selection = selection.filter(id => !ids.includes(id));
    if (ids.length) void loadBoard();
  }
  async function menuAction(action: "open" | "enhance" | "duplicate" | "copy-title" | "copy-brief" | "select" | "delete" | {status:TaskStatus} | {priority:number}) {
    const cards = contextMenu?.cards;
    closeTaskMenu();
    if (!cards?.length) return;
    const card = cards[0];
    if (action === "open" || action === "enhance") { await openTask(card.id,action === "enhance" ? "enhance" : "details"); return; }
    if (action === "select") { if (selection.includes(card.id)) selection = []; else toggleSelection(card); return; }
    if (action === "delete") { await confirmAction(cards,{kind:"delete"}); return; }
    if (typeof action === "object") {
      if (cards.length > 1) { await confirmAction(cards,{kind:"update",changes:action}); return; }
      await applyUpdate(new TaskBatch(cards,{kind:"update",changes:action})); return;
    }
    let duplicateTicket: number | null = null;
    try {
      if (action === "copy-title") { actionNote = await copyText(cards.map(card => card.title).join("\n")) ? "Task title copied" : "Clipboard unavailable"; return; }
      if (action === "copy-brief") { const brief = await getTaskBrief(card.id,card.revision); actionNote = await copyText(brief.markdown) ? "Saved task brief copied" : "Clipboard unavailable"; return; }
      if (opening) return;
      opening = true;
      const ticket = ++openingRevision; duplicateTicket = ticket;
      const full = await bounded(getTask(card.id));
      if (disposed || ticket !== openingRevision || !await leaveSheet()) return;
      workspaceEditor = null;
      taskEditor = {value:null,primary:full.primary_repository_id,home:full.home_workspace_id};
      await tick();
      // The existing editor owns validation and saving of the unsaved copy.
      editorHandle?.loadCopy(full);
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (duplicateTicket !== null && duplicateTicket === openingRevision) opening = false; }
  }
  function boardKeydown(event: KeyboardEvent) {
    if (!active || event.defaultPrevented || event.isComposing || actionDialog || !(event.target instanceof Node) || !board?.contains(event.target) || event.target instanceof HTMLElement && event.target.closest('input,textarea,select,[contenteditable="true"],.task-editor,.workspace-editor')) return;
    const focused = event.target instanceof HTMLElement ? event.target.closest<HTMLElement>('[data-card-id]') : null;
    const card = visibleCards.find(card => card.id === focused?.dataset.cardId);
    if ((event.key === "ContextMenu" || event.shiftKey && event.key === "F10") && card) { showTaskMenu(event,card,focused); return; }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "a") { event.preventDefault(); selectLoaded(); }
    else if (event.key === "Escape") { selection = []; closeTaskMenu(); }
    else if ((event.key === "Delete" || event.key === "Backspace") && (selectedCards.length || card)) { event.preventDefault(); void confirmAction(selectedCards.length ? selectedCards : card ? [card] : [],{kind:"delete"}); }
    else if (event.key === "/") { event.preventDefault(); taskSearch.focus(); }
    else if (event.key.toLowerCase() === "n" && !event.metaKey && !event.ctrlKey && !event.altKey) { event.preventDefault(); void createTask(); }
    else if (event.key.toLowerCase() === "e" && card && !event.metaKey && !event.ctrlKey && !event.altKey) { event.preventDefault(); void openTask(card.id,"enhance"); }
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

<svelte:window onkeydown={boardKeydown} />

<div bind:this={board} class="workbench gp-glass" class:list-view={layout === "list"} class:is-dragging={drag !== null} data-testid="task-board">
  {#if !repositoryPath}
    <nav class="navigator" aria-label="Task scopes">
      <div class="nav-heading">Workspaces<button type="button" class="icon" title="New workspace" aria-label="New workspace" onclick={newWorkspace}><Plus size={12} /></button></div>
      <button type="button" class:selected={scope.kind === "global"} onclick={() => { scope = { kind: "global" }; }}><Layers size={13} />All tasks</button>
      {#each [...workspaces].sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.position - b.position) as group (group.id)}
        <div class="nav-row"><button type="button" class:selected={scope.kind === "workspace" && scope.id === group.id} onclick={() => { scope = { kind: "workspace", id: group.id }; }} title={group.name}>{group.icon} {group.name}{group.archived ? " · Archived" : ""}</button><button type="button" class="icon" aria-label={`Edit ${group.name}`} onclick={() => editWorkspace(group.id)} disabled={opening}>⋯</button></div>
      {/each}
      {#if workspaceCursor}<button type="button" onclick={moreWorkspaces}>Load more workspaces ({workspaces.length}/{workspaceTotal})</button>{/if}
      <div class="nav-heading" data-add-repo>Repositories<button type="button" class="icon" aria-haspopup="menu" aria-expanded={addMenu} aria-controls="task-add-repo-menu" aria-busy={adding} title="Add repository" aria-label="Add repository" disabled={adding} onclick={toggleAddMenu}><Plus size={12} /></button>
        {#if addMenu}
          <div bind:this={addMenuEl} id="task-add-repo-menu" class="add-menu gp-menu" role="menu" aria-label="Add repository" tabindex="-1" style="z-index: {LAYERS.MENU}" onkeydown={onAddMenuKey}>
            {#if menuTabs.length}<div class="add-menu-label">Open repositories</div>{/if}
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
      {#each repositories as repo (repo.id)}<button type="button" class:selected={scope.kind === "repository" && scope.id === repo.id} onclick={() => { scope = { kind: "repository", id: repo.id }; }} title={repo.identity_key}>{repo.name}</button>{/each}
      {#if repositoryCursor}<button type="button" onclick={moreRepositories}>Load more repositories ({repositories.length}/{repositoryTotal})</button>{/if}
    </nav>
  {/if}
  <main class="board-main">
    <header>
      <h1>{title}{#if !loading && initialized}<span>{total}</span>{/if}</h1>
      <div class="actions">
        <label class="search"><Search size={14} /><input bind:this={taskSearch} aria-label="Search tasks" type="search" bind:value={search} placeholder="Search tasks…" maxlength="512" />{#if search}<button type="button" class="clear-search" aria-label="Clear task search" onclick={() => { search = ""; }}><X size={12} /></button>{/if}</label>
        {#if initialized}<AutomaticEnhancements {active} compact />{/if}
        {#if initialized}
          <button type="button" class="gp-icon-btn" aria-pressed={showInbox} aria-label="Task notifications" title={unreadError ? `Notifications unavailable: ${unreadError}` : "Task notifications"} onclick={() => { showInbox = !showInbox; }}>
            <Inbox size={13} />
            {#if !unreadError && unread > 0}<span class="gp-pill">{unread}</span>{/if}
          </button>
        {/if}
        <button type="button" class="gp-icon-btn" aria-label="Refresh tasks" title="Refresh tasks" onclick={() => initialized ? refresh() : initialize()} disabled={loading}><RefreshCw size={13} /></button>
        <button type="button" class="gp-btn-primary" onclick={() => createTask("inbox","enhance")} disabled={!initialized || !primaryForNew}><Sparkles size={13} />Quick Enhance</button>
        <button type="button" class="gp-btn" onclick={() => createTask()} disabled={!initialized || !primaryForNew} title={scope.kind === "workspace" && !primaryForNew ? "Link a repository to this workspace first" : "Create a task"}><Plus size={13} />New task</button>
        {#if initialized && !repositories.length}
          {#if emptyAddLabel}
            <button type="button" class="hint-action" onclick={() => void addPaths(menuTabs.map((tab) => tab.path))} disabled={adding}>{emptyAddLabel}</button>
          {:else}
            <span class="hint">Add a repository to create tasks</span>
          {/if}
        {/if}
      </div>
    </header>
    <div class="organization" aria-label="Task organization">
      <div class="view-picker"><button class="gp-icon-btn" aria-label="Board view" aria-pressed={layout === "board"} onclick={() => { layout = "board"; }}><Columns3 size={14} /></button><button class="gp-icon-btn" aria-label="List view" aria-pressed={layout === "list"} onclick={() => { layout = "list"; }}><List size={14} /></button></div>
      <label><span class="sr-only">Sort loaded tasks</span><select aria-label="Sort loaded tasks" bind:value={sort}><option value="manual">Manual order</option><option value="priority">Priority</option><option value="recent">Recently updated</option><option value="title">Title</option></select></label>
      <label><span class="sr-only">Filter loaded tasks</span><select aria-label="Filter loaded tasks" bind:value={filter}><option value="all">All tasks</option><option value="unassigned">Unassigned</option><option value="high">High priority</option><option value="overdue">Overdue</option></select></label>
      <select aria-label="Filter by task type" bind:value={kindFilter}><option value="">All types</option>{#each kinds as kind}<option value={kind}>{kind}</option>{/each}</select>
      <input aria-label="Filter by task label" placeholder="Label…" bind:value={labelFilter} maxlength="128" />
      {#if filtered}<button class="gp-btn" onclick={() => { filter = "all"; labelFilter = ""; kindFilter = ""; }}>Clear filters</button>{/if}
      <span class="loaded-count">{visibleCards.length} shown · {loadedCards.length} loaded / {total} total</span>
      <details class="shortcut-help"><summary>Shortcuts</summary><p>N · New task<br />/ · Search<br />⌘/Ctrl click · Select<br />Shift click · Select range<br />⌘/Ctrl A · Select loaded<br />E · Quick Enhance<br />Shift F10 · Task actions<br />Delete · Review deletion<br />← / → · Move task<br />Esc · Clear selection</p></details>
    </div>
    {#if selectedCards.length}<div class="selection-bar" role="region" aria-label="Selected task actions"><strong>{selectedCards.length} selected</strong><button class="gp-btn" onclick={selectLoaded}>Select loaded tasks</button><select aria-label="Move selected tasks" value="" onchange={(e) => { const status = parseColumnStatus(e.currentTarget.value); e.currentTarget.value = ""; if(status) void confirmAction(selectedCards,{kind:"update",changes:{status}}); }}><option value="" disabled>Move to…</option>{#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}</select><select aria-label="Set selected tasks priority" value="" onchange={(e) => { const priority = Number(e.currentTarget.value); e.currentTarget.value = ""; if(Number.isInteger(priority) && priority >= 0 && priority <= 3) void confirmAction(selectedCards,{kind:"update",changes:{priority}}); }}><option value="" disabled>Set priority…</option>{#each PRIORITY_LABELS as label, priority}<option value={priority}>{label}</option>{/each}</select><button class="gp-btn danger" onclick={() => confirmAction(selectedCards,{kind:"delete"})}><Trash2 size={13} />Delete selected tasks</button><button class="gp-icon-btn" aria-label="Clear task selection" onclick={() => { selection = []; }}><X size={14} /></button></div>{/if}
    {#if actionNote}<div class="action-note" role="status">{actionNote}<button class="gp-icon-btn" aria-label="Dismiss task message" onclick={() => { actionNote = ""; }}><X size={12} /></button></div>{/if}
    {#if pendingUpdate && !moving}<div class="banner error" role="alert">Confirm the interrupted task update before making another change.<button class="gp-btn" onclick={() => { if(pendingUpdate) void applyUpdate(pendingUpdate); }}>Retry task update</button></div>{/if}
    {#if filtered && !visibleCards.length && loadedCards.length}<div class="filter-empty" role="status">No loaded tasks match these filters. Load more below or clear the filters.</div>{/if}
    {#if showInbox}<AttentionInbox {scope} {active} onopen={openTask} />{/if}
    {#if catalogError}<div class="banner error" role="alert">{catalogError}<button type="button" onclick={() => initialized ? refresh() : initialize()}>Retry</button></div>{/if}
    {#if error}<div class="banner error" role="alert">{error}<button type="button" class="gp-btn" onclick={() => loadBoard()} disabled={loading}>Retry loading tasks</button></div>{/if}
    <div class="sr-only" role="status" aria-live="polite">{announce}</div>
    {#if !initialized || loadedKey !== boardKey || (!loading && total === 0)}
      <div class="empty-state" role="status">
        {#if loading}<RefreshCw size={22} /><h2>Loading tasks…</h2>
        {:else if error || catalogError}<h2>Tasks unavailable</h2><p>Retry loading to see your tasks.</p>
        {:else if search}<Search size={24} /><h2>No matching tasks</h2><p>Try a different search.</p><button class="gp-btn" onclick={() => { search = ""; }}>Clear task search</button>
        {:else if !repositories.length}<FolderGit2 size={24} /><h2>Add a repository to get started</h2><button class="gp-btn" onclick={() => pickFolder()}>Choose repository folder…</button>
        {:else}<CheckCheck size={24} /><h2>No tasks yet</h2>{#if primaryForNew}<button class="gp-btn-primary" onclick={() => createTask()}>Create first task</button>{:else}<p>Link a repository to this workspace to create tasks.</p>{/if}{/if}
      </div>
    {/if}
    <div class="columns" class:concealed={!initialized || loadedKey !== boardKey || (!loading && total === 0)} aria-busy={loading || moving} data-testid="task-columns">
      {#each shown as status (status)}
        <section
          class="column"
          class:drop-target={drag !== null && drag.over === status}
          data-task-column={status}
          data-testid="task-column"
          aria-label={STATUS_LABELS[status]}
        >
          <div class="column-title"><span class="status-dot" data-status={status}></span><h2>{STATUS_LABELS[status]}</h2><span>{displayColumns[status]?.total ?? "—"}</span><button class="column-add" type="button" aria-label={`Add task to ${STATUS_LABELS[status]}`} title={`Add task to ${STATUS_LABELS[status]}`} disabled={!primaryForNew || loading} onclick={() => createTask(status)}><Plus size={13} /></button></div>
          <div class="cards">
            {#each organized[status] ?? [] as card (card.id)}
              {@const face = cardFace(card, repoName)}
              {#if insertBefore(status, card.id)}<div class="insert" aria-hidden="true"></div>{/if}
              <div class="task-card-wrap" class:checked={selection.includes(card.id)} class:selected={taskEditor?.value?.id === card.id}>
              <div class="card-tools"><input type="checkbox" aria-label={`Select ${card.title}`} checked={selection.includes(card.id)} onclick={(event) => { event.preventDefault(); toggleSelection(card,event.shiftKey); }} /><button class="gp-icon-btn" aria-label={`Task actions for ${card.title}`} aria-haspopup="menu" onclick={(event) => showTaskMenu(event,card)}><Ellipsis size={15} /></button></div>
              <button
                type="button"
                class="card"
                class:dragging={drag?.card.id === card.id}
                data-testid="task-card"
                data-task-card
                data-card-id={card.id}
                draggable="false"
                aria-grabbed={drag?.card.id === card.id}
                aria-label={`${card.title}, ${STATUS_LABELS[card.status]}, ${PRIORITY_LABELS[card.priority]} priority`}
                aria-keyshortcuts="ArrowLeft ArrowRight"
                disabled={opening || moving || loading || pendingUpdate !== null}
                onpointerdown={(e) => onCardPointerDown(e, card)}
                onpointermove={onCardPointerMove}
                onpointerup={onCardPointerUp}
                onpointercancel={onCardPointerCancel}
                onclick={(event) => onCardClick(card,event)}
                oncontextmenu={(event) => showTaskMenu(event,card)}
                onkeydown={(e) => onCardKeydown(e, card)}
              >
                <div class="card-eyebrow"><span>{card.kind}</span>{#if face.pip !== null}<span class="priority-label" data-priority={face.pip}>{PRIORITY_LABELS[card.priority]}</span>{/if}</div>
                <div class="card-meta">
                  {#if face.pip !== null}<span class="pip" data-priority={face.pip}></span>{/if}
                  <h3>{face.title}</h3>
                </div>
                {#if face.repo}<div class="card-repos" title={card.repository_ids.map((id) => repoName(id) ?? id).join(", ")}>{repoName(card.primary_repository_id) ?? face.repo}{#if card.repository_ids.length > 1}<span>+{card.repository_ids.length - 1}</span>{/if}</div>{/if}
                {#if face.labels.length}<div class="labels">{#each face.labels as label}<span title={label}>{label}</span>{/each}{#if card.labels.length > face.labels.length}<span title={card.labels.slice(face.labels.length).join(", ")}>+{card.labels.length - face.labels.length}</span>{/if}</div>{/if}
              </button>
              <div class="card-controls"><button class="quick-enhance-card gp-icon-btn" aria-label={`Quick Enhance ${card.title}`} title="Quick Enhance with Manvi" disabled={opening || moving || pendingUpdate !== null} onclick={() => openTask(card.id,"enhance")}><Sparkles size={13} /></button><span class="owner" title={card.owner ?? "Unassigned"}>{card.owner ?? "Unassigned"}</span><select aria-label={`Change status of ${card.title}`} value={card.status} disabled={opening || loading || moving || pendingUpdate !== null} onchange={(event) => changeStatus(card, event)}>{#each STATUSES as choice}<option value={choice}>{STATUS_LABELS[choice]}</option>{/each}</select></div>
              </div>
            {/each}
            {#if insertAtEnd(status)}<div class="insert" aria-hidden="true"></div>{/if}
          </div>
          {#if displayColumns[status]?.next_cursor}<div class="paging"><span>{displayColumns[status]?.items.length} of {displayColumns[status]?.total}</span><button type="button" class="gp-btn" aria-label={`Load more ${STATUS_LABELS[status]} tasks`} onclick={() => pageColumn(status, displayColumns[status]?.next_cursor ?? undefined)} disabled={loading || moving}>Load more</button></div>{/if}
        </section>
      {/each}
    </div>
  </main>
  {#if drag}
    <div class="ghost" style="transform: translate({drag.x + 10}px, {drag.y + 10}px)" data-testid="task-drag-ghost">{drag.card.title}</div>
  {/if}
  {#if taskEditor}{#key taskEditor}<TaskEditor bind:this={editorHandle} active={active && !actionDialog} value={taskEditor.value} {repositories} {workspaces} openTabs={openTabRefs} primary={taskEditor.primary} home={taskEditor.home} initialStatus={taskEditor.status} initialMode={taskEditor.mode} autoEnhance={taskEditor.autoEnhance} onDelete={(task) => confirmAction([task],{kind:"delete"})} onSaved={() => { void loadBoard(); }} onClose={closeTask} />{/key}{/if}
  {#if workspaceEditor}{#key workspaceEditor}<WorkspaceEditor bind:this={workspaceHandle} value={workspaceEditor.value} {repositories} openTabs={openTabRefs} onSaved={() => { scope = { kind: "global" }; void refresh(); }} onClose={() => { workspaceEditor = null; }} />{/key}{/if}
</div>
{#if contextMenu}<TaskContextMenu x={contextMenu.x} y={contextMenu.y} count={contextMenu.cards.length} selected={selection.includes(contextMenu.cards[0]?.id)} onAction={menuAction} onClose={closeTaskMenu} />{/if}
{#if actionDialog}<TaskActionDialog tasks={actionDialog.cards} action={actionDialog.action} onChanged={tasksChanged} onClose={() => { actionDialog = null; }} />{/if}

<style>
  .organization,.selection-bar{padding:9px 14px;display:flex;align-items:center;gap:8px;flex-wrap:wrap;border-bottom:1px solid rgb(var(--c-border)/.4);font-size:11px}.organization select,.organization input,.selection-bar select{font-size:11px;max-width:150px;border:1px solid rgb(var(--c-border)/.5);border-radius:7px;padding:5px 7px;background:var(--mac-recess,rgb(var(--c-surface)));color:inherit}.organization input{width:85px}.view-picker{display:flex;gap:2px}.view-picker [aria-pressed="true"]{background:rgb(var(--c-accent)/.15);color:rgb(var(--c-accent))}.loaded-count{color:rgb(var(--c-text-muted));margin-left:auto}.selection-bar{background:rgb(var(--c-accent)/.08)}.danger{color:#dc6565}.action-note{font-size:11px;padding:7px 14px;display:flex;align-items:center;justify-content:space-between;gap:12px;color:rgb(var(--c-text-muted))}.shortcut-help{position:relative;color:rgb(var(--c-text-muted))}.shortcut-help summary{cursor:pointer}.shortcut-help p{position:absolute;right:0;top:20px;width:210px;padding:12px;border:1px solid rgb(var(--c-border));border-radius:9px;background:rgb(var(--c-surface));z-index:7;line-height:1.8}.filter-empty{padding:12px 20px;font-size:12px;color:rgb(var(--c-text-muted))}.card-tools{display:flex;justify-content:space-between;align-items:center;position:absolute;inset:7px 8px auto;pointer-events:none}.card-tools input,.card-tools button{pointer-events:auto}.task-card-wrap{position:relative}.card-eyebrow{padding:0 22px}.list-view .card-eyebrow{padding:0}.list-view .card-tools{position:static}.card-tools input{accent-color:rgb(var(--c-accent))}.task-card-wrap.checked{outline:2px solid rgb(var(--c-accent));outline-offset:-1px}.list-view .columns{flex-direction:column;align-items:stretch}.list-view .column{width:100%;max-width:none;min-height:fit-content;flex:none}.list-view .cards{display:flex;flex-direction:column;overflow:visible}.list-view .task-card-wrap{margin:0;display:grid;grid-template-columns:62px minmax(0,1fr) 150px;align-items:center}.list-view .card{min-height:0;padding:10px 12px}.list-view .card-tools{padding:8px}.list-view .card-controls{border:0;flex-wrap:wrap;gap:6px}.list-view .column-title{padding:10px 14px}

  .workbench{position:relative;display:flex;flex:1;min-height:0;min-width:0;color:rgb(var(--c-text));background-color:rgb(var(--c-bg));overflow:hidden}
  .workbench.is-dragging{cursor:grabbing;user-select:none}
  .navigator{width:188px;flex-shrink:0;border-right:1px solid rgb(var(--c-border));padding:10px 8px;overflow:auto}
  .navigator button{display:block;width:100%;text-align:left;border:0;padding:6px 8px;border-radius:7px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px}
  .navigator button:hover,button:hover{background:rgb(var(--c-surface-hover))}
  .navigator button.selected{background:color-mix(in srgb,rgb(var(--c-accent)) 13%,transparent);color:rgb(var(--c-accent))}
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
  .search{display:flex;align-items:center;gap:6px;background:var(--mac-fill-surface,rgb(var(--c-surface)));border:1px solid rgb(var(--c-border));border-radius:7px;padding:0 8px;color:rgb(var(--c-text-muted))}
  .search input{border:0;background:transparent;padding:6px 0;width:140px;color:inherit}
  .columns{display:flex;gap:14px;padding:20px;overflow:auto;flex:1;min-height:0;align-items:stretch}
  .column{width:256px;min-width:220px;max-width:340px;flex:1;display:flex;flex-direction:column;background:color-mix(in srgb,rgb(var(--c-surface)) 45%,transparent);border-radius:10px;border:1px solid rgb(var(--c-border));overflow:hidden;min-height:0}
  .column.drop-target{border-color:rgb(var(--c-accent));background:color-mix(in srgb,rgb(var(--c-accent)) 10%,transparent)}
  .column-title{display:flex;align-items:center;justify-content:space-between;font-weight:650;font-size:12px;background:var(--mac-fill-surface,rgb(var(--c-surface)));border-bottom:1px solid rgb(var(--c-border));padding:8px 10px}
  .column-title span:last-child{color:rgb(var(--c-text-muted));font-weight:400}
  .cards{padding:8px;overflow:auto;flex:1;min-height:80px}
  .card{width:100%;display:block;text-align:left;padding:12px 12px 8px;margin:0;border:0;border-radius:9px;background:transparent;cursor:grab;touch-action:none;user-select:none}
  .card:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}
  .card.dragging{opacity:.35;cursor:grabbing}
  .card h3{font-size:13px;line-height:1.4;font-weight:550;margin:0;overflow-wrap:anywhere;min-width:0}
  .card-meta{display:flex;align-items:flex-start;gap:6px}
  .pip{width:7px;height:7px;margin-top:4px;border-radius:99px;flex-shrink:0;background:rgb(var(--c-accent))}
  .pip[data-priority="0"]{background:#d15a64}
  .insert{height:2px;margin:2px 4px;border-radius:2px;background:rgb(var(--c-accent))}
  .card-repos{font-size:10px;color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap;margin-top:4px}
  .labels{display:flex;gap:4px;margin-top:6px;font-size:9px;flex-wrap:wrap}
  .labels span{max-width:110px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;padding:2px 6px;border-radius:4px;background:color-mix(in srgb,rgb(var(--c-accent)) 9%,transparent);color:rgb(var(--c-text-muted))}
  .paging{display:flex;gap:5px;padding:6px}
  .paging button{font-size:10px;padding:3px 6px}
  .banner{padding:7px 14px;font-size:12px;border-bottom:1px solid rgb(var(--c-border));display:flex;align-items:center;justify-content:space-between;gap:10px}
  .error{color:#d15a64}
  .ghost{position:fixed;top:0;left:0;z-index:20;pointer-events:none;max-width:220px;padding:6px 10px;border-radius:8px;background:var(--mac-fill-surface,rgb(var(--c-surface)));border:1px solid rgb(var(--c-accent));font-size:12px;font-weight:550;box-shadow:0 8px 24px #00000022;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .concealed{display:none}.empty-state{flex:1;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:12px;color:rgb(var(--c-text-muted));padding:32px;text-align:center}.empty-state h2{font-size:16px;font-weight:600;color:rgb(var(--c-text));margin:0}.empty-state p{font-size:12px;margin:0}.task-card-wrap{border:1px solid rgb(var(--c-border));border-radius:10px;margin-bottom:8px;background:var(--mac-fill-surface,rgb(var(--c-surface)));overflow:hidden}.task-card-wrap:hover{border-color:color-mix(in srgb,rgb(var(--c-accent)) 45%,rgb(var(--c-border)))}.task-card-wrap.selected{border-color:rgb(var(--c-accent));box-shadow:0 0 0 1px rgb(var(--c-accent)/.2)}.card-eyebrow{display:flex;align-items:center;justify-content:space-between;gap:6px;margin-bottom:7px;text-transform:capitalize;font-size:10px;color:rgb(var(--c-text-muted))}.priority-label{font-size:9px}.priority-label[data-priority="0"]{color:#dc6565}.card-controls{display:flex;align-items:center;justify-content:space-between;gap:8px;padding:4px 10px 10px;font-size:10px}.card-controls select{min-width:0;max-width:100px;border:1px solid transparent;border-radius:5px;background:rgb(var(--c-bg));padding:3px 5px;color:rgb(var(--c-text-muted));font-size:10px}.card-controls select:hover{border-color:rgb(var(--c-border));color:rgb(var(--c-text))}.owner{color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.card-repos span{margin-left:6px}.column-title h2{font-size:12px;font-weight:600;margin:0;flex:1}.column-add{display:flex;align-items:center;justify-content:center;padding:3px;border:0;border-radius:4px;color:rgb(var(--c-text-muted))}.column-add:hover{background:rgb(var(--c-surface-hover))}.column-title{gap:7px}.status-dot{width:6px;height:6px;border-radius:50%;background:rgb(var(--c-text-muted));flex-shrink:0}.status-dot[data-status="in_progress"]{background:rgb(var(--c-accent))}.status-dot[data-status="review"]{background:#c79853}.status-dot[data-status="done"]{background:#58a786}.paging{justify-content:space-between;align-items:center;color:rgb(var(--c-text-muted));font-size:10px}.clear-search{display:flex;align-items:center;justify-content:center;border:0;padding:3px}.navigator>button{display:flex;align-items:center;gap:7px}.navigator{width:176px}.search input{width:clamp(90px,13vw,200px)}:is(button,select,input):focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}
  @media(max-width:720px){.navigator{width:138px}.columns{padding:12px}.actions{gap:5px}}
</style>
