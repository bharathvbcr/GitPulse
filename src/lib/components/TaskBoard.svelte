<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { Clipboard, Inbox, LayoutGrid, List, Plus, RefreshCw, Search, Sparkles, SquarePen, Trash2 } from "@lucide/svelte";
  import { isMacOS, isTauri } from "../platform";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { copyText } from "../desktop/clipboard";
  import { LAYERS } from "../ui/layers";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { cardFace, dragExceeded, insertIndexFromY, insertionNeighbors, insertionPosition, neighborStatus, parseColumnStatus, shouldCommitMove, visibleStatuses } from "../workbench/boardDrag";
  import {
    explainError, getTask, getTaskBrief, getWorkspace, listAttention, listRepositories, listTasks, listWorkspaces, newID, putWorkspace, registerRepository,
    STATUSES, STATUS_LABELS, taskDraft, workspaceDraft,
    type Page, type Repository, type Scope, type Task, type TaskCard, type TaskDraft, type TaskStatus, type Workspace, type WorkspaceCard,
  } from "../workbench/client";
  import { addableOpenTabs, membershipAfterAttach, openAddActionLabel, openMembershipCandidates, pickerSelectionIds } from "../workbench/openMembership";
  import { cardsById, contextMenuAnchor, duplicateTitle, flattenVisibleIds, isContextMenuKey, rangeSelect, taskMenuItems, toggleSelection, type TaskMenuItem } from "../workbench/taskMenu";
  import { cardChrome, cardMatchesFacet, collectFacetOptions, emptyFacet, facetActive, allLoadedCards, type BoardLayout, type TaskFacet } from "../workbench/taskOrganize";
  import { removeFromColumns } from "../workbench/taskDelete";
  import { joinAgentCopies, MAX_AGENT_COPY_TASKS, wrapSavedBriefForAgent } from "../workbench/taskCompose";
  import { TaskBatch, bounded, MAX_TASK_SELECTION, type TaskAction } from "../workbench/taskActions";
  import { reorderPlan } from "../workbench/taskOrganization";
  import TaskActionDialog from "./TaskActionDialog.svelte";
  import TaskEditor from "./TaskEditor.svelte";
  import WorkspaceEditor from "./WorkspaceEditor.svelte";
  import AutomaticEnhancements from "./AutomaticEnhancements.svelte";
  import AttentionInbox from "./AttentionInbox.svelte";
  import TaskContextMenu from "./TaskContextMenu.svelte";
  import QuickEnhanceSheet from "./QuickEnhanceSheet.svelte";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";

  let { repositoryPath = null, active = true }: { repositoryPath?: string | null; active?: boolean } = $props();
  let scope = $state<Scope>({ kind: "global" });
  let repositories = $state<Repository[]>([]), workspaces = $state<WorkspaceCard[]>([]);
  let repositoryCursor = $state<string | null>(null), workspaceCursor = $state<string | null>(null);
  let repositoryTotal = $state(0), workspaceTotal = $state(0);
  let columns = $state<Partial<Record<TaskStatus, Page<TaskCard>>>>({});
  let search = $state(""); let initialized = $state(false); let loading = $state(false);
  let error = $state(""); let catalogError = $state(""); let announce = $state("");
  let taskEditor = $state<{ value: Task | null; status?: TaskStatus; seed?: Partial<TaskDraft> } | null>(null);
  let workspaceEditor = $state<{ value: Workspace | null } | null>(null);
  let editorHandle = $state<{ canLeave: () => Promise<boolean> }>();
  let workspaceHandle = $state<{ canLeave: () => Promise<boolean> }>();
  let pendingUpdate = $state<TaskBatch | null>(null);
  let actionDialog = $state<{ cards: TaskCard[]; action: TaskAction } | null>(null);
  let loadedKey = $state("");
  let unreadError = $state("");
  let unreadRevision = 0, initializationRevision = 0, openingRevision = 0;
  const boardKey = $derived(JSON.stringify([scope, search]));
  const displayColumns = $derived(loadedKey === boardKey ? columns : {});
  $effect(() => { boardKey; facet; selected = new Set(); selectionAnchor = null; menu = null; });
  let opening = $state(false); let moving = $state(false); let deleting = $state(false);
  let press = $state<{ card: TaskCard; x: number; y: number } | null>(null);
  let drag = $state<{ card: TaskCard; over: TaskStatus | null; insertIndex: number; x: number; y: number } | null>(null);
  let skipClick = false;
  let showInbox = $state(false);
  let unread = $state(0);
  let addMenu = $state(false);
  let adding = $state(false);
  let addMenuEl: HTMLDivElement | undefined = $state();
  let workspaceMemberIds = $state<string[] | null>(null);
  let selected = $state<Set<string>>(new Set());
  let selectionAnchor = $state<string | null>(null);
  let menu = $state<{ cards: TaskCard[]; column: TaskStatus | null; x: number; y: number } | null>(null);
  let enhanceId = $state<string | null>(null);
  let layout = $state<BoardLayout>("board");
  let facet = $state<TaskFacet>(emptyFacet());
  let showArchived = $state(true);
  let showFilters = $state(false);
  let now = $state(Math.floor(Date.now() / 1000));
  let boardEl: HTMLDivElement | undefined = $state();
  let revision = 0; let disposed = false; let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const macos = isMacOS();
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
  const loadedCards = $derived(allLoadedCards(displayColumns));
  const facetOptions = $derived(collectFacetOptions(loadedCards));
  const filtering = $derived(facetActive(facet) || search.trim().length > 0);
  const visibleWorkspaces = $derived(showArchived ? workspaces : workspaces.filter((group) => !group.archived));
  const selectedCards = $derived(cardsById(displayColumns, selected));
  const busy = $derived(moving || opening || deleting || actionDialog !== null || pendingUpdate !== null);
  const shown = $derived.by(() => {
    const counts = Object.fromEntries(STATUSES.map((status) => [status, visibleIn(status).length])) as Partial<Record<TaskStatus, number>>;
    return visibleStatuses(counts, drag !== null);
  });
  const listCards = $derived(shown.flatMap((status) => visibleIn(status)));
  function repoName(id: string) { return repositories.find((repo) => repo.id === id)?.name; }
  function visibleIn(status: TaskStatus): TaskCard[] {
    return (displayColumns[status]?.items ?? []).filter((card) => cardMatchesFacet(card, facet, now));
  }

  async function catalog() {
    const [repos, groups] = await Promise.all([listRepositories(), listWorkspaces()]);
    if (disposed) return;
    repositories = repos.items; repositoryTotal = repos.total; repositoryCursor = repos.next_cursor;
    workspaces = groups.items; workspaceTotal = groups.total; workspaceCursor = groups.next_cursor;
    catalogError = "";
  }
  async function loadBoard(target: Scope = scope, query: string = search) {
    const generation = ++revision, key = JSON.stringify([target, query]); loading = true; error = "";
    try {
      const pages = await Promise.all(STATUSES.map(async (status) => [status, await listTasks(target, status, query)] as const));
      if (generation !== revision || disposed || key !== boardKey) return;
      columns = Object.fromEntries(pages); loadedKey = key;
      selected = new Set([...selected].filter((id) => loadedHas(id, Object.fromEntries(pages))));
    } catch (cause) { if (generation === revision && !disposed) error = explainError(cause); }
    finally { if (generation === revision && !disposed) loading = false; }
  }
  function loadedHas(id: string, pages: Partial<Record<TaskStatus, Page<TaskCard>>>): boolean {
    return Object.values(pages).some((page) => page?.items.some((item) => item.id === id));
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
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && addMenu) addMenu = false;
    };
    const clock = window.setInterval(() => { now = Math.floor(Date.now() / 1000); }, 30_000);
    window.addEventListener("focus", scheduleRefresh);
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      disposed = true; revision++; unreadRevision++; initializationRevision++; openingRevision++; pendingUpdate?.stop(); clearTimeout(refreshTimer); unlisten?.(); window.clearInterval(clock);
      window.removeEventListener("focus", scheduleRefresh);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKey);
    };
  });
  $effect(() => {
    if (!initialized || !active) return;
    const target = scope, query = search;
    loading = true;
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
  async function confirmDiscard(_message: string): Promise<boolean> {
    return (!taskEditor || await editorHandle?.canLeave() === true) && (!workspaceEditor || await workspaceHandle?.canLeave() === true);
  }
  async function newWorkspace() {
    if (busy || !await confirmDiscard("Open a new workspace?") || disposed) return;
    workspaceEditor = { value: null }; taskEditor = null; enhanceId = null;
  }
  async function editWorkspace(id: string) {
    if (busy) return;
    opening = true; const ticket = ++openingRevision;
    try {
      if (!await confirmDiscard("Open workspace settings?")) return;
      const full = await bounded(getWorkspace(id));
      if (disposed || ticket !== openingRevision) return;
      taskEditor = null; enhanceId = null; workspaceEditor = { value: full };
    } catch (cause) { if (!disposed && ticket === openingRevision) error = explainError(cause); }
    finally { if (!disposed && ticket === openingRevision) opening = false; }
  }
  async function openTask(id: string) {
    if (busy || taskEditor?.value?.id === id) return;
    opening = true; const ticket = ++openingRevision;
    try {
      if (!await confirmDiscard("Open another task?")) return;
      const full = await bounded(getTask(id));
      if (disposed || ticket !== openingRevision) return;
      workspaceEditor = null; enhanceId = null; taskEditor = { value: full };
    } catch (cause) { if (!disposed && ticket === openingRevision) error = explainError(cause); }
    finally { if (!disposed && ticket === openingRevision) opening = false; }
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
    if (e.button !== 0 || busy || menu) return;
    press = { card, x: e.clientX, y: e.clientY };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onCardPointerMove(e: PointerEvent) {
    if (!press) return;
    if (!drag) {
      if (!dragExceeded(e.clientX - press.x, e.clientY - press.y)) return;
      menu = null;
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
  function onCardClick(e: MouseEvent, card: TaskCard) {
    if (skipClick) return;
    if (e.metaKey || e.ctrlKey) {
      selected = toggleSelection(selected, card.id);
      selectionAnchor = card.id;
      return;
    }
    if (e.shiftKey) {
      const ids = flattenVisibleIds(columns, shown, (item) => cardMatchesFacet(item, facet, now));
      selected = rangeSelect(ids, selectionAnchor, card.id);
      return;
    }
    if (selected.size > 0) {
      selected = new Set();
      selectionAnchor = null;
    }
    void openTask(card.id);
  }
  function onCardPointerCancel(e: PointerEvent) {
    drag = null;
    press = null;
    releasePointer(e.currentTarget, e.pointerId);
  }
  function openCardMenu(card: TaskCard, status: TaskStatus, x: number, y: number) {
    if (!selected.has(card.id)) {
      selected = new Set([card.id]);
      selectionAnchor = card.id;
    }
    menu = { cards: cardsById(displayColumns, selected), column: status, x, y };
  }
  function onCardContextMenu(e: MouseEvent, card: TaskCard, status: TaskStatus) {
    e.preventDefault();
    e.stopPropagation();
    skipClick = true;
    requestAnimationFrame(() => { skipClick = false; });
    openCardMenu(card, status, e.clientX, e.clientY);
  }
  function onColumnContextMenu(e: MouseEvent, status: TaskStatus) {
    if (e.target instanceof Element && e.target.closest("[data-task-card]")) return;
    e.preventDefault();
    menu = { cards: [], column: status, x: e.clientX, y: e.clientY };
  }
  function onCardKeydown(e: KeyboardEvent, card: TaskCard, status: TaskStatus) {
    if (isContextMenuKey(e)) {
      e.preventDefault();
      e.stopPropagation();
      const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
      const pos = contextMenuAnchor({ clientX: 0, clientY: 0 }, rect);
      openCardMenu(card, status, pos.x, pos.y);
      return;
    }
    if (e.key === " " && (e.metaKey || e.ctrlKey || selected.size > 0)) {
      e.preventDefault();
      selected = toggleSelection(selected, card.id);
      selectionAnchor = card.id;
      return;
    }
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      e.stopPropagation();
      if (!selected.has(card.id)) selected = new Set([card.id]);
      void removeSelected();
      return;
    }
    if (e.key === "e" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      enhanceId = card.id;
      return;
    }
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const next = neighborStatus(card.status, e.key === "ArrowRight" ? 1 : -1);
    if (!next) return;
    const last = (columns[next]?.items ?? []).at(-1);
    void moveCard(card, next, insertionPosition(last?.position ?? null, null));
  }
  function onBoardKeydown(e: KeyboardEvent) {
    if (!active || !boardEl) return;
    const target = e.target;
    if (!(target instanceof Node) || !boardEl.contains(target)) return;
    if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) return;
    if (target instanceof HTMLElement && target.closest("[role=dialog], .task-editor, .workspace-editor, [data-task-menu]")) return;
    if (e.key === "Escape") {
      if (menu) { menu = null; return; }
      if (selected.size) { selected = new Set(); selectionAnchor = null; return; }
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "a") {
      e.preventDefault();
      selected = new Set(flattenVisibleIds(columns, shown, (item) => cardMatchesFacet(item, facet, now)));
      selectionAnchor = [...selected][0] ?? null;
      return;
    }
    if (e.key === "/" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      boardEl.querySelector<HTMLInputElement>("#task-search")?.focus();
      return;
    }
    if (e.key === "n" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      void createTask();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "c" && selected.size > 0) {
      e.preventDefault();
      void copyCardsForAgent(selectedCards);
      return;
    }
    if (e.key === "o" && !e.metaKey && !e.ctrlKey && !e.altKey && selected.size === 1) {
      const id = [...selected][0];
      if (id) void openTask(id);
      return;
    }
    if ((e.key === "Delete" || e.key === "Backspace") && selected.size > 0) {
      e.preventDefault();
      void removeSelected();
    }
  }
  async function applyUpdate(batch: TaskBatch) {
    if (moving || pendingUpdate && pendingUpdate !== batch) return;
    moving = true; error = ""; pendingUpdate = batch;
    try {
      await batch.run();
      if (disposed) return;
      const rows = batch.snapshot();
      if (!rows.some(row => row.state === "uncertain" || row.state === "waiting")) pendingUpdate = null;
      const done = rows.filter(row => row.state === "done").length;
      announce = `${done} of ${rows.length} tasks updated`;
      if (done) await loadBoard();
      const failure = rows.find(row => row.state === "uncertain" || row.state === "failed");
      if (failure) error = failure.error;
    } finally { moving = false; }
  }
  async function moveCard(card: TaskCard, status: TaskStatus, position: number) {
    if (busy || card.status === status && card.position === position) return;
    await applyUpdate(new TaskBatch([card], {kind:"update", changes:{status,position}}));
  }
  async function patchCards(cards: TaskCard[], patch: { status?: TaskStatus; priority?: number }) {
    if (busy || !cards.length) return;
    if (cards.length > MAX_TASK_SELECTION) { error = `Select at most ${MAX_TASK_SELECTION} loaded tasks per action.`; return; }
    await applyUpdate(new TaskBatch(cards, {kind:"update", changes:patch}));
  }
  async function createTask(status: TaskStatus = "inbox") {
    if (busy) return;
    if (!(await confirmDiscard("Start a new task and discard the current unsaved edits?"))) return;
    workspaceEditor = null; enhanceId = null; taskEditor = { value: null, status };
  }
  async function removeSelected() {
    const cards = selectedCards;
    if (busy || !cards.length || !await confirmDiscard("Delete selected tasks?")) return;
    if (cards.length > MAX_TASK_SELECTION) { error = `Select at most ${MAX_TASK_SELECTION} loaded tasks per action.`; return; }
    actionDialog = { cards: [...cards], action: {kind:"delete"} };
  }
  function tasksChanged(ids: string[]) {
    columns = removeFromColumns(columns, new Set(ids));
    selected = new Set([...selected].filter(id => !ids.includes(id)));
    if (taskEditor?.value && ids.includes(taskEditor.value.id)) taskEditor = null;
    if (enhanceId && ids.includes(enhanceId)) enhanceId = null;
    if (ids.length) { void loadBoard(); void loadUnread(); }
  }
  async function copyValues(text: string, ok: string) {
    if (await copyText(text)) { announce = ok; toastStore.success(ok); }
    else { error = "Clipboard unavailable"; toastStore.error("Clipboard unavailable"); }
  }
  async function copyCardsForAgent(cards: TaskCard[]) {
    const unique = cards.slice(0, MAX_AGENT_COPY_TASKS);
    const parts: string[] = [];
    const failed: string[] = [];
    for (const card of unique) {
      try {
        const brief = await getTaskBrief(card.id, card.revision);
        const packet = wrapSavedBriefForAgent(brief.markdown);
        if (packet) parts.push(packet);
        else failed.push(card.title);
      } catch (cause) {
        failed.push(`${card.title}: ${explainError(cause)}`);
      }
    }
    const joined = joinAgentCopies(parts);
    const extra = cards.length - unique.length;
    if (joined && await copyText(joined)) {
      announce = extra > 0
        ? `Copied ${parts.length} tasks for an agent. Held ${extra} more (cap ${MAX_AGENT_COPY_TASKS}).`
        : parts.length === 1 ? "Copied task for an agent." : `Copied ${parts.length} tasks for an agent.`;
      if (failed.length) error = failed.join(" ");
      else toastStore.success(announce);
    } else {
      error = failed[0] ?? "Clipboard unavailable";
    }
  }
  async function onMenuAction(item: TaskMenuItem) {
    const cards = menu?.cards ?? [];
    const column = menu?.column ?? null;
    menu = null;
    switch (item.action.kind) {
      case "open":
        if (cards[0]) void openTask(cards[0].id);
        break;
      case "enhance":
        if (cards[0]) {
          void confirmDiscard("Open Quick Enhance? Unsaved edits in the current editor will be discarded.").then((ok) => {
            if (!ok) return;
            taskEditor = null;
            enhanceId = cards[0].id;
          });
        }
        break;
      case "duplicate":
        if (cards[0]) {
          void confirmDiscard("Start a duplicate and discard the current unsaved edits?").then((ok) => {
            if (!ok) return;
            opening = true;
            void getTask(cards[0].id).then((full) => {
              if (disposed) return;
              workspaceEditor = null;
              enhanceId = null;
              const copy = taskDraft(full);
              taskEditor = { value: null, status: full.status, seed: { ...copy, title: duplicateTitle(full.title), position: Date.now() } };
            }).catch((cause) => { if (!disposed) error = explainError(cause); })
              .finally(() => { opening = false; });
          });
        }
        break;
      case "copyTitle":
        void copyValues(cards.map((card) => card.title).join("\n"), cards.length === 1 ? "Copied title" : `Copied ${cards.length} titles`);
        break;
      case "copyId":
        if (cards[0]) void copyValues(cards[0].id, "Copied task ID");
        break;
      case "copyBrief":
        if (cards[0]) {
          try {
            const brief = await getTaskBrief(cards[0].id, cards[0].revision);
            await copyValues(brief.markdown, `Copied saved revision ${cards[0].revision}`);
          } catch (cause) { error = explainError(cause); }
        }
        break;
      case "copyAgent":
        void copyCardsForAgent(cards);
        break;
      case "move":
        void patchCards(cards, { status: item.action.status });
        break;
      case "priority":
        void patchCards(cards, { priority: item.action.priority });
        break;
      case "selectColumn":
        if (column) {
          selected = new Set(visibleIn(column).map((card) => card.id));
          selectionAnchor = visibleIn(column)[0]?.id ?? null;
        }
        break;
      case "newInColumn":
        void createTask(item.action.status);
        break;
      case "delete":
        selected = new Set(cards.map((card) => card.id));
        void removeSelected();
        break;
    }
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
  function dueLabel(state: ReturnType<typeof cardChrome>["due"]): string | null {
    if (state === "overdue") return "Overdue";
    if (state === "soon") return "Due soon";
    return null;
  }
</script>

<svelte:window onkeydown={onBoardKeydown} />
<div bind:this={boardEl} class="workbench" class:is-dragging={drag !== null} data-testid="task-board">
  {#if !repositoryPath}
    <nav class="navigator gp-glass" aria-label="Task scopes">
      <div class="nav-heading">Workspaces<button type="button" class="icon gp-icon-btn" title="New workspace" aria-label="New workspace" onclick={newWorkspace}><Plus size={12} /></button></div>
      <button type="button" class:selected={scope.kind === "global"} onclick={() => { scope = { kind: "global" }; }}>All</button>
      {#each [...visibleWorkspaces].sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.position - b.position) as group (group.id)}
        <div class="nav-row"><button type="button" class:selected={scope.kind === "workspace" && scope.id === group.id} onclick={() => { scope = { kind: "workspace", id: group.id }; }} title={group.name}>{group.icon} {group.name}{group.archived ? " · Archived" : ""}</button><button type="button" class="icon gp-icon-btn" aria-label={`Edit ${group.name}`} onclick={() => editWorkspace(group.id)} disabled={opening}>⋯</button></div>
      {/each}
      {#if workspaceCursor}<button type="button" onclick={moreWorkspaces}>More ({workspaces.length}/{workspaceTotal})</button>{/if}
      {#if workspaces.some((group) => group.archived)}
        <label class="archive-toggle"><input type="checkbox" bind:checked={showArchived} />Show archived</label>
      {/if}
      <div class="nav-heading" data-add-repo>Repositories<button type="button" class="icon gp-icon-btn" aria-haspopup="menu" aria-expanded={addMenu} aria-controls="task-add-repo-menu" aria-busy={adding} title="Add repository" aria-label="Add repository" disabled={adding} onclick={toggleAddMenu}><Plus size={12} /></button>
        {#if addMenu}
          <div bind:this={addMenuEl} id="task-add-repo-menu" class="add-menu gp-menu" role="menu" aria-label="Add repository" tabindex="-1" style="z-index: {LAYERS.MENU}" onkeydown={onAddMenuKey}>
            {#if menuTabs.length}<div class="add-menu-label">Open</div>{/if}
            {#each menuTabs as tab (tab.path)}
              <button type="button" class="add-item gp-menu-item" role="menuitem" title={tab.path} onclick={() => void addPaths([tab.path])}>
                <span class="add-name">{tab.label}</span>
                <span class="add-path">{tab.path}</span>
              </button>
            {/each}
            {#if menuTabs.length > 1}<button type="button" class="gp-menu-item" role="menuitem" onclick={() => void addPaths(menuTabs.map((tab) => tab.path))}>Add all open</button>{/if}
            <button type="button" class="gp-menu-item" role="menuitem" onclick={() => void pickFolder()}>Choose folder…</button>
          </div>
        {/if}
      </div>
      {#each repositories as repo (repo.id)}<button type="button" class:selected={scope.kind === "repository" && scope.id === repo.id} onclick={() => { scope = { kind: "repository", id: repo.id }; }} title={repo.identity_key}>{repo.name}</button>{/each}
      {#if repositoryCursor}<button type="button" onclick={moreRepositories}>More ({repositories.length}/{repositoryTotal})</button>{/if}
    </nav>
  {/if}
  <main class="board-main">
    <header>
      <h1>{title}{#if !loading && initialized}<span>{total}</span>{/if}</h1>
      <div class="actions">
        <label class="search"><Search size={12} /><input id="task-search" class="gp-field" aria-label="Search tasks" type="search" bind:value={search} placeholder="Search tasks" maxlength="512" /></label>
        <div class="gp-segmented" class:gp-liquid-tabs={macos} role="group" aria-label="Task layout">
          <button type="button" class="gp-seg-btn" data-active={layout === "board"} aria-pressed={layout === "board"} onclick={() => { layout = "board"; }}><LayoutGrid size={12} /> Board</button>
          <button type="button" class="gp-seg-btn" data-active={layout === "list"} aria-pressed={layout === "list"} onclick={() => { layout = "list"; }}><List size={12} /> List</button>
        </div>
        {#if initialized}<AutomaticEnhancements {active} compact />{/if}
        {#if initialized}
          <button type="button" class="gp-icon-btn" aria-pressed={showInbox} aria-label="Inbox" title={unreadError ? `Notifications unavailable: ${unreadError}` : "Inbox"} onclick={() => { showInbox = !showInbox; }}>
            <Inbox size={13} />
            {#if !unreadError && unread > 0}<span class="gp-pill">{unread}</span>{/if}
          </button>
        {/if}
        <button type="button" class="gp-icon-btn" aria-label="Refresh" title="Refresh" onclick={() => initialized ? refresh() : initialize()} disabled={loading}><RefreshCw size={13} /></button>
        <button type="button" class="gp-btn" aria-pressed={showFilters || filtering} onclick={() => { showFilters = !showFilters; }}>Filters</button>
        <button type="button" class="gp-btn-primary" onclick={() => createTask()} disabled={!initialized || !repositories.length} aria-label="New task">New task</button>
        {#if initialized && !repositories.length}
          {#if emptyAddLabel}
            <button type="button" class="hint-action" onclick={() => void addPaths(menuTabs.map((tab) => tab.path))} disabled={adding}>{emptyAddLabel}</button>
          {:else}
            <span class="hint">Add a repository to create tasks</span>
          {/if}
        {/if}
      </div>
    </header>
    {#if initialized && (showFilters || filtering)}
      <div class="facets" aria-label="Organize tasks">
        <select class="gp-select" aria-label="Filter by priority" bind:value={facet.priority}>
          <option value="all">All priorities</option>
          <option value={0}>Urgent</option>
          <option value={1}>High</option>
          <option value={2}>Normal</option>
          <option value={3}>Low</option>
        </select>
        <select class="gp-select" aria-label="Filter by type" bind:value={facet.kind}>
          <option value="all">All types</option>
          {#each facetOptions.kinds as kind}<option value={kind}>{kind}</option>{/each}
        </select>
        <select class="gp-select" aria-label="Filter by owner" bind:value={facet.owner}>
          <option value="all">All owners</option>
          <option value="">Unassigned</option>
          {#each facetOptions.owners as owner}<option value={owner}>{owner}</option>{/each}
        </select>
        <select class="gp-select" aria-label="Filter by label" bind:value={facet.label}>
          <option value="all">All labels</option>
          {#each facetOptions.labels as label}<option value={label}>{label}</option>{/each}
        </select>
        <select class="gp-select" aria-label="Filter by due date" bind:value={facet.due}>
          <option value="all">Any due date</option>
          <option value="overdue">Overdue</option>
          <option value="soon">Due soon</option>
          <option value="none">No due date</option>
        </select>
        {#if filtering}<button type="button" class="gp-btn" onclick={() => { facet = emptyFacet(); search = ""; }}>Clear filters</button>{/if}
      </div>
    {/if}
    {#if selected.size > 0}
      <div class="selection gp-glass" role="status">
        <span>{selected.size} selected</span>
        {#if selected.size === 1}
          <button type="button" class="gp-btn" onclick={() => { const id = [...selected][0]; if (id) void openTask(id); }}><SquarePen size={12} /> Open</button>
          <button type="button" class="gp-btn" onclick={() => { const id = [...selected][0]; if (id) enhanceId = id; }}><Sparkles size={12} /> Quick Enhance</button>
        {/if}
        <button type="button" class="gp-btn" onclick={() => void copyCardsForAgent(selectedCards)} disabled={busy}><Clipboard size={12} /> Copy for agent</button>
        <button type="button" class="gp-btn-danger" onclick={() => void removeSelected()} disabled={busy}><Trash2 size={12} /> Delete</button>
        <button type="button" class="gp-btn" onclick={() => { selected = new Set(); selectionAnchor = null; }}>Clear</button>
      </div>
    {/if}
    {#if pendingUpdate && !moving}<div class="banner error" role="alert">Confirm the interrupted task update before making another change.<button class="gp-btn" onclick={() => { if(pendingUpdate) void applyUpdate(pendingUpdate); }}>Retry task update</button></div>{/if}
    {#if showInbox}<AttentionInbox {scope} {active} onopen={openTask} />{/if}
    {#if catalogError}<div class="banner error" role="alert">{catalogError}<button type="button" class="gp-btn" onclick={() => initialized ? refresh() : initialize()}>Retry</button></div>{/if}
    {#if error}<div class="banner error" role="alert">{error}</div>{/if}
    <div class="sr-only" role="status" aria-live="polite">{announce}</div>
    {#if !initialized && loading}
      <div class="pad"><Skeleton variant="card" count={4} /></div>
    {:else if initialized && total === 0 && !filtering}
      <EmptyState icon={Inbox} title="No tasks yet" hint="Create a task in this scope. Cards stay on this board until you delete them." action={repositories.length ? { label: "New task", onClick: () => void createTask(), variant: "primary" } : undefined} />
    {:else if initialized && listCards.length === 0 && filtering}
      <EmptyState icon={Search} title="No tasks match" hint="Clear search or filters to see the rest of this board. Server search only covers the current pages." action={{ label: "Clear filters", onClick: () => { facet = emptyFacet(); search = ""; }, variant: "secondary" }} />
    {:else if layout === "list"}
      <div class="list" data-testid="task-columns" aria-busy={loading || moving} aria-label="Task list">
        {#each listCards as card (card.id)}
          {@const face = cardFace(card, repoName)}
          {@const chrome = cardChrome(card, now)}
          <button
            type="button"
            class="row gp-card"
            class:selected={selected.has(card.id)}
            data-testid="task-card"
            data-task-card
            data-card-id={card.id}
            aria-haspopup="menu"
            aria-expanded={menu?.cards.some((item) => item.id === card.id) ?? false}
            aria-keyshortcuts="ArrowLeft ArrowRight Delete ContextMenu"
            disabled={opening}
            onclick={(e) => onCardClick(e, card)}
            oncontextmenu={(e) => onCardContextMenu(e, card, card.status)}
            onkeydown={(e) => onCardKeydown(e, card, card.status)}
          >
            <span class="status">{STATUS_LABELS[card.status]}</span>
            <span class="row-title">{face.title}</span>
            {#if face.repo}<span class="muted">{face.repo}{chrome.extraRepos ? ` +${chrome.extraRepos}` : ""}</span>{/if}
            {#if chrome.owner}<span class="muted">{chrome.owner}</span>{/if}
            {#if dueLabel(chrome.due)}<span class="due" data-due={chrome.due}>{dueLabel(chrome.due)}</span>{/if}
          </button>
        {/each}
      </div>
    {:else}
      <div class="columns" aria-busy={loading || moving} data-testid="task-columns">
        {#each shown as status (status)}
          <section
            class="column"
            class:drop-target={drag !== null && drag.over === status}
            data-task-column={status}
            data-testid="task-column"
            aria-label={STATUS_LABELS[status]}
            oncontextmenu={(e) => onColumnContextMenu(e, status)}
          >
            <div class="column-title">
              <span>{STATUS_LABELS[status]}</span>
              <span class="column-meta">
                <span>{columns[status]?.total ?? "—"}</span>
                <button type="button" class="gp-icon-btn" aria-label={`New task in ${STATUS_LABELS[status]}`} disabled={!initialized || !repositories.length} onclick={() => void createTask(status)}><Plus size={11} /></button>
              </span>
            </div>
            <div class="cards">
              {#each visibleIn(status) as card (card.id)}
                {@const face = cardFace(card, repoName)}
                {@const chrome = cardChrome(card, now)}
                {#if insertBefore(status, card.id)}<div class="insert" aria-hidden="true"></div>{/if}
                <button
                  type="button"
                  class="card gp-card"
                  class:dragging={drag?.card.id === card.id}
                  class:selected={selected.has(card.id)}
                  data-testid="task-card"
                  data-task-card
                  data-card-id={card.id}
                  draggable="false"
                  aria-grabbed={drag?.card.id === card.id}
                  aria-haspopup="menu"
                  aria-expanded={menu?.cards.some((item) => item.id === card.id) ?? false}
                  aria-keyshortcuts="ArrowLeft ArrowRight Delete ContextMenu"
                  disabled={opening}
                  onpointerdown={(e) => onCardPointerDown(e, card)}
                  onpointermove={onCardPointerMove}
                  onpointerup={onCardPointerUp}
                  onpointercancel={onCardPointerCancel}
                  onclick={(e) => onCardClick(e, card)}
                  oncontextmenu={(e) => onCardContextMenu(e, card, status)}
                  onkeydown={(e) => onCardKeydown(e, card, status)}
                >
                  <div class="card-meta">
                    {#if face.pip !== null}<span class="pip" data-priority={face.pip}></span>{/if}
                    <h3>{face.title}</h3>
                  </div>
                  {#if face.repo}<div class="card-repos">{face.repo}{chrome.extraRepos ? ` +${chrome.extraRepos}` : ""}</div>{/if}
                  <div class="card-extra">
                    {#if chrome.kind}<span class="muted">{chrome.kind}</span>{/if}
                    {#if chrome.owner}<span class="muted">{chrome.owner}</span>{/if}
                    {#if dueLabel(chrome.due)}<span class="due" data-due={chrome.due}>{dueLabel(chrome.due)}</span>{/if}
                  </div>
                  {#if face.labels.length}<div class="labels">{#each face.labels as label}<span>{label}</span>{/each}{#if chrome.extraLabels}<span>+{chrome.extraLabels}</span>{/if}</div>{/if}
                </button>
              {/each}
              {#if insertAtEnd(status)}<div class="insert" aria-hidden="true"></div>{/if}
              {#if visibleIn(status).length === 0 && drag === null}
                <p class="column-empty">Drop a card here, or add one.</p>
              {/if}
            </div>
            {#if columns[status] && (columns[status]?.total ?? 0) > 30}<div class="paging"><button type="button" class="gp-btn" onclick={() => pageColumn(status)} disabled={loading}>First</button>{#if columns[status]?.next_cursor}<button type="button" class="gp-btn" onclick={() => pageColumn(status, columns[status]?.next_cursor ?? undefined)} disabled={loading}>Next</button>{/if}</div>{/if}
          </section>
        {/each}
      </div>
    {/if}
  </main>
  {#if drag}
    <div class="ghost gp-card shadow-float" style="transform: translate({drag.x + 10}px, {drag.y + 10}px)" data-testid="task-drag-ghost">{drag.card.title}</div>
  {/if}
  {#if menu}
    <TaskContextMenu
      items={taskMenuItems({ cards: menu.cards, column: menu.column, busy })}
      x={menu.x}
      y={menu.y}
      label={menu.cards.length ? "Task actions" : "Column actions"}
      onAction={onMenuAction}
      onClose={(restore) => { menu = null; if (restore) { /* opener retains focus */ } }}
    />
  {/if}
  {#if enhanceId}
    <QuickEnhanceSheet
      taskId={enhanceId}
      {repoName}
      onClose={() => { enhanceId = null; }}
      onApplied={(saved) => {
        void loadBoard();
        if (taskEditor?.value?.id === saved.id) taskEditor = { value: saved, status: saved.status };
      }}
      onOpenEditor={(task) => { enhanceId = null; taskEditor = { value: task }; }}
    />
  {/if}
  {#if taskEditor}{#key taskEditor}<TaskEditor bind:this={editorHandle} active={active && !actionDialog} value={taskEditor.value} seed={taskEditor.seed ?? null} initialStatus={taskEditor.status ?? "inbox"} {repositories} {workspaces} openTabs={openTabRefs} primary={scope.kind === "repository" ? scope.id : repositories[0]?.id ?? ""} home={scope.kind === "workspace" ? scope.id : null} onSaved={() => { void loadBoard(); }} onClose={() => { taskEditor = null; }} />{/key}{/if}
  {#if workspaceEditor}{#key workspaceEditor}<WorkspaceEditor bind:this={workspaceHandle} value={workspaceEditor.value} {repositories} openTabs={openTabRefs} onSaved={() => { scope = { kind: "global" }; void refresh(); }} onClose={() => { workspaceEditor = null; }} />{/key}{/if}
</div>

{#if actionDialog}<TaskActionDialog tasks={actionDialog.cards} action={actionDialog.action} onChanged={tasksChanged} onClose={() => { actionDialog = null; }} />{/if}

<style>
  .workbench{position:relative;display:flex;flex:1;min-height:0;min-width:0;color:rgb(var(--c-text));background:transparent;overflow:hidden}
  .workbench.is-dragging{cursor:grabbing;user-select:none}
  .navigator{width:188px;flex-shrink:0;border-right:1px solid rgb(var(--c-border) / 0.65);padding:10px 8px;overflow:auto}
  .navigator button{display:block;width:100%;text-align:left;border:0;padding:6px 8px;border-radius:7px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px;background:transparent}
  .navigator button:hover{background:rgb(var(--c-surface-hover) / 0.7)}
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
  .archive-toggle{display:flex;align-items:center;gap:6px;padding:6px 8px;font-size:11px;color:rgb(var(--c-text-muted))}
  .board-main{flex:1;min-width:0;display:flex;flex-direction:column;overflow:hidden}
  header{padding:10px 14px;display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap;border-bottom:1px solid rgb(var(--c-border) / 0.65)}
  h1{font-size:15px;line-height:1.2;font-weight:650;margin:0;display:flex;align-items:baseline;gap:8px}
  h1 span{font-size:11px;font-weight:500;color:rgb(var(--c-text-muted))}
  .actions,.facets,.selection{display:flex;gap:6px;align-items:center;flex-wrap:wrap}
  .facets,.selection{padding:8px 14px}
  .selection{margin:8px 14px 0;padding:8px 10px;border-radius:12px}
  button,input,select{font-size:12px}
  button:disabled{opacity:.5}
  .hint{font-size:11px;color:rgb(var(--c-text-muted))}
  .search{display:flex;align-items:center;gap:6px;color:rgb(var(--c-text-muted))}
  .search input{width:160px}
  .columns{display:flex;gap:8px;padding:12px;overflow:auto;flex:1;min-height:0;align-items:stretch}
  .column{width:220px;min-width:196px;flex:1;display:flex;flex-direction:column;border-radius:12px;border:1px solid rgb(var(--c-border) / 0.65);overflow:hidden;min-height:0}
  .column.drop-target{border-color:rgb(var(--c-accent))}
  .column-title{display:flex;align-items:center;justify-content:space-between;font-weight:650;font-size:12px;border-bottom:1px solid rgb(var(--c-border) / 0.65);padding:8px 10px;background:transparent}
  .column-meta{display:flex;align-items:center;gap:4px;color:rgb(var(--c-text-muted));font-weight:400}
  .cards{padding:6px;overflow:auto;flex:1;min-height:80px}
  .card,.row{width:100%;display:block;text-align:left;padding:8px 9px;margin-bottom:6px;cursor:grab;touch-action:none;user-select:none;background:rgb(var(--c-surface) / 0.55)}
  .row{cursor:pointer;display:grid;grid-template-columns:7rem minmax(0,1fr) auto auto auto;gap:8px;align-items:center}
  .card:focus-visible,.row:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}
  .card.dragging{opacity:.35;cursor:grabbing}
  .card.selected,.row.selected{border-color:rgb(var(--c-accent));box-shadow:inset 0 0 0 1px rgb(var(--c-accent) / 0.45)}
  .card h3,.row-title{font-size:12px;line-height:1.4;font-weight:550;margin:0;overflow-wrap:anywhere;min-width:0}
  .card-meta{display:flex;align-items:flex-start;gap:6px}
  .pip{width:7px;height:7px;margin-top:4px;border-radius:99px;flex-shrink:0;background:rgb(var(--c-accent))}
  .pip[data-priority="0"]{background:#d15a64}
  .insert{height:2px;margin:2px 4px;border-radius:2px;background:rgb(var(--c-accent))}
  .card-repos,.muted,.status{font-size:10px;color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .card-repos{margin-top:4px}
  .card-extra{display:flex;gap:6px;flex-wrap:wrap;margin-top:4px}
  .due{font-size:9px;padding:1px 5px;border-radius:4px;background:rgb(245 158 11 / 0.15);color:rgb(180 83 9)}
  .due[data-due="overdue"]{background:rgb(244 63 94 / 0.15);color:#e11d48}
  .labels{display:flex;gap:4px;margin-top:6px;font-size:9px;flex-wrap:wrap}
  .labels span{padding:1px 5px;border-radius:4px;background:color-mix(in srgb,rgb(var(--c-accent)) 9%,transparent);color:rgb(var(--c-text-muted))}
  .paging{display:flex;gap:5px;padding:6px}
  .column-empty{margin:10px 8px;font-size:11px;color:rgb(var(--c-text-muted))}
  .list{padding:12px;overflow:auto;flex:1}
  .banner{padding:7px 14px;font-size:12px;border-bottom:1px solid rgb(var(--c-border) / 0.65);display:flex;align-items:center;justify-content:space-between;gap:10px}
  .error{color:#d15a64}
  .ghost{position:fixed;top:0;left:0;z-index:20;pointer-events:none;max-width:220px;padding:6px 10px;font-size:12px;font-weight:550;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .pad{padding:16px}
</style>
