<script lang="ts">
  import { crossfade } from "svelte/transition";
  import { liquidSelection } from "../ui/transitions";
  import { onMount, untrack } from "svelte";
  import { invoke } from "../ipc/invoke";
  import { listen } from "@tauri-apps/api/event";
  import { Archive, Bot, ChevronDown, ChevronUp, Clipboard, EyeOff, FolderSync, Import, Inbox, LayoutGrid, List, Plus, RefreshCw, Search, Sparkles, SquarePen, Trash2, Undo2, X } from "@lucide/svelte";
  import { isMacOS, isTauri } from "../platform";
  import { createListenerTracker } from "../dom/listenerTracker";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { copyText } from "../desktop/clipboard";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { popover, restoreFocusTo } from "../ui/popover";
  import { cardFace, dragExceeded, insertIndexFromY, insertionNeighbors, insertionPosition, neighborStatus, parseColumnStatus, shouldCommitMove } from "../workbench/boardDrag";
  import {
    explainError, getTask, getTaskBrief, getWorkspace, listAttention, listRepositories, listTasks, listWorkspaces, newID, putTask, registerRepository,
    STATUSES, STATUS_LABELS, taskDraft, taskWrite,
    type Page, type Repository, type Scope, type Task, type TaskCard, type TaskDraft, type TaskStatus, type Workspace, type WorkspaceCard,
  } from "../workbench/client";
  import { addableOpenTabs, attachRepositories, openAddActionLabel, openMembershipCandidates, pickerSelectionIds } from "../workbench/openMembership";
  import { quickAddRefusal, taskCreation } from "../workbench/taskCreation";
  import { defaultRelinkIO, relinkCheckout, type RelinkOutcome } from "../workbench/repositoryRelink";
  import { applyMove, moveWrites, navigatorOrder } from "../workbench/workspaceOrder";
  import { importSummary, importTabGroups, tabGroups } from "../workbench/workspaceImport";
  import { workspaceMembershipLabel } from "../workbench/taskRepositories";
  import { cardsById, contextMenuAnchor, duplicateTitle, flattenVisibleIds, isContextMenuKey, rangeSelect, taskMenuItems, toggleSelection, type TaskMenuItem } from "../workbench/taskMenu";
  import { checkoutCandidates, handoffFromTarget, type HandoffSettings } from "../workbench/taskHandoff";
  import { linkTaskToIssue, MAX_TASK_ISSUE_BATCH, prepareTaskIssues, runTaskIssues, summarizeTaskIssues, taskIssuesConfirmation, type TaskIssueResult, type TaskIssueTarget } from "../workbench/taskIssue";
  import { askConfirm } from "../stores/modalStore";
  import { openExternal } from "../desktop/openExternal";
  import { cardChrome, cardMatchesFacet, collectFacetOptions, emptyFacet, facetActive, allLoadedCards, reorderPlan, type TaskFacet } from "../workbench/taskOrganize";
  import { plural } from "../format";
  import { ARCHIVE_STATUS, archiveAction, archivable, archiveState, offersArchive } from "../workbench/taskArchive";
  import { interfaceStore } from "../stores/interfaceStore";
  import { hiddenColumnReport, visibleBoardStatuses } from "../ui/taskView";
  import { consumeTaskOpen, taskOpenRequest } from "../workbench/taskOpen";
  import { parseQuickAddDue, quickAddDraft, type QuickAddMode, type QuickAddResult } from "../workbench/taskQuickAdd";
  import { removeFromColumns } from "../workbench/taskDelete";
  import { joinAgentCopies, MAX_AGENT_COPY_TASKS, wrapSavedBriefForAgent } from "../workbench/taskCompose";
  import { createBoardAgents } from "../workbench/boardAgents";
  import { taskAgentSummaries, type TaskAgentSummary } from "../workbench/taskSessions";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { taskTerminalRequests } from "../terminal/taskLaunches";
  import { terminalSessionLimit } from "../terminal/sessionLimit";
  import { sessionActivity } from "../terminal/sessionActivity";
  import { DEFERRED_DELETE_MS, TaskBatch, UNDO_OFFER_MS, bounded, defer, describeTaskAction, MAX_TASK_SELECTION, type TaskAction, type TaskChanges, type UndoPlan } from "../workbench/taskActions";
  import {
    MAX_TASK_TABS,
    TASK_EDITOR_PANE_ID,
    activateTaskTab,
    canOpenTaskTab,
    closeTaskTab,
    emptyTaskTabs,
    openSavedTaskIds,
    openTaskTab,
    publishTaskChrome,
    retargetTaskTab,
    updateTaskTab,
    taskTabLabel,
    type TaskTabState,
  } from "../workbench/taskTabs";
  import { focusTabAt, handleTablistKeydown } from "../dom/tablist";
  import ScrollCue from "./ScrollCue.svelte";
  import TaskActionDialog from "./TaskActionDialog.svelte";
  import TaskEditor from "./TaskEditor.svelte";
  import WorkspaceEditor from "./WorkspaceEditor.svelte";
  import AutomaticEnhancements from "./AutomaticEnhancements.svelte";
  import AttentionInbox from "./AttentionInbox.svelte";
  import TaskArchive from "./TaskArchive.svelte";
  import TaskContextMenu from "./TaskContextMenu.svelte";
  import QuickEnhanceSheet from "./QuickEnhanceSheet.svelte";
  import EmptyState from "./EmptyState.svelte";
  import TaskViewMenu from "./TaskViewMenu.svelte";
  import TaskQuickAdd from "./TaskQuickAdd.svelte";
  import TaskHandoffSheet from "./TaskHandoffSheet.svelte";
  import Skeleton from "./Skeleton.svelte";

  let { repositoryPath = null, active = true, acceptsOpenRequests = false }: {
    repositoryPath?: string | null;
    active?: boolean;
    /** Only the global board opens a task asked for from elsewhere (`taskOpen.ts`). */
    acceptsOpenRequests?: boolean;
  } = $props();
  const macos = isMacOS();
  const [sendScope, receiveScope] = crossfade(liquidSelection());
  let scope = $state<Scope>({ kind: "global" });
  let repositories = $state<Repository[]>([]), workspaces = $state<WorkspaceCard[]>([]);
  let repositoryCursor = $state<string | null>(null), workspaceCursor = $state<string | null>(null);
  let repositoryTotal = $state(0), workspaceTotal = $state(0);
  let columns = $state<Partial<Record<TaskStatus, Page<TaskCard>>>>({});
  let search = $state(""); let initialized = $state(false); let loading = $state(false);
  let error = $state(""); let catalogError = $state(""); let announce = $state("");
  let taskTabs = $state<TaskTabState>(emptyTaskTabs());
  let session = $state<{ tabId: string; pane: string; value: Task | null; status?: TaskStatus; seed?: Partial<TaskDraft> } | null>(null);
  let workspaceEditor = $state<{ value: Workspace | null } | null>(null);
  let editorHandle = $state<{ canLeave: () => Promise<boolean> }>();
  let tabStrip: HTMLDivElement | undefined = $state();
  let workspaceHandle = $state<{ canLeave: () => Promise<boolean> }>();
  let pendingUpdate = $state<TaskBatch | null>(null);
  let actionDialog = $state<{ cards: TaskCard[]; action: TaskAction; batch?: TaskBatch } | null>(null);
  /** The latest write the board can still take back (`offerUndo`). */
  let undoOffer = $state<UndoPlan | null>(null);
  let undoTimer: ReturnType<typeof setTimeout> | undefined;
  type PendingDeletion = { batch: TaskBatch; cards: TaskCard[]; ids: ReadonlySet<string>; running: boolean; window: ReturnType<typeof defer> };
  /**
   * A confirmed deletion that has not been written yet (`deferDeletion`).
   * Raw, not proxied: the timer and the strip must act on the same entry,
   * and `running` is read by both.
   */
  let pendingDelete = $state.raw<PendingDeletion | null>(null);
  /** Deferred deletions being written now; a board writing one is busy. */
  let deletesRunning = $state(0);
  let loadedKey = $state("");
  let unreadError = $state("");
  let unreadRevision = 0, initializationRevision = 0, openingRevision = 0;
  const boardKey = $derived(JSON.stringify([scope, search]));
  // A deletion waiting out its undo window is already gone from the board:
  // its cards, its counts and anything a selection could act on.
  const displayColumns = $derived(loadedKey === boardKey ? (pendingDelete ? removeFromColumns(columns, pendingDelete.ids) : columns) : {});
  $effect(() => { boardKey; facet; selected = new Set(); selectionAnchor = null; menu = null; });
  // An undo offer belongs to the board it was made on. A pending deletion is
  // not withdrawn by a scope change: it is still the reader's to undo.
  $effect(() => { JSON.stringify(scope); untrack(() => offerUndo(null)); });
  let opening = $state(false); let moving = $state(false); let deleting = $state(false);
  let press = $state<{ card: TaskCard; x: number; y: number } | null>(null);
  let drag = $state<{ card: TaskCard; over: TaskStatus | null; insertIndex: number; x: number; y: number } | null>(null);
  let skipClick = false;
  let showInbox = $state(false);
  let showArchive = $state(false);
  let archiveToken = $state(0);
  let unread = $state(0);
  let addMenu = $state(false);
  let addRepoTriggerEl: HTMLButtonElement | undefined = $state();
  let adding = $state(false);
  let relinking = $state(false);
  let relinkPending = $state<Extract<RelinkOutcome, { kind: "uncertain" }> | null>(null);
  let addMenuEl: HTMLDivElement | undefined = $state();
  let workspaceMemberIds = $state<string[] | null>(null);
  let membershipToken = $state(0);
  let selected = $state<Set<string>>(new Set());
  let selectionAnchor = $state<string | null>(null);
  let menu = $state<{ cards: TaskCard[]; column: TaskStatus | null; x: number; y: number } | null>(null);
  let enhanceId = $state<string | null>(null);
  /**
   * Bumped to ask a freshly opened Quick Enhance to start generating at once.
   *
   * A counter rather than a boolean: quick-adding two tasks in a row must
   * start two drafts, and a flag that is already true the second time would
   * start none.
   */
  let enhanceStart = $state(0);
  let facet = $state<TaskFacet>(emptyFacet());
  let showFilters = $state(false);
  let quickAdding = $state(false);
  let quickAddEl = $state<{ focus: () => void }>();
  let handoff = $state<{ card: TaskCard; settings: HandoffSettings } | null>(null);
  let now = $state(Math.floor(Date.now() / 1000));
  let boardEl: HTMLDivElement | undefined = $state();
  let revision = 0; let disposed = false; let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const openTabRefs = $derived($repoStore.openTabs.map((tab) => ({ path: tab.path, label: tab.label })));
  const catalogIds = $derived(repositories.map((repo) => repo.id));
  const selectedForMenu = $derived(pickerSelectionIds(scope.kind, workspaceMemberIds, catalogIds));
  const menuTabs = $derived(addableOpenTabs(openMembershipCandidates(openTabRefs, repositories, selectedForMenu, pathOpts)));
  const emptyAddLabel = $derived(openAddActionLabel(menuTabs));
  /**
   * Registered repositories a workspace scope could still take in.
   *
   * Empty outside a workspace: in the global and repository scopes the catalog
   * is the membership, so there is nothing to join.
   */
  const attachable = $derived.by(() => {
    const members = workspaceMemberIds;
    if (scope.kind !== "workspace" || !members) return [];
    return repositories.filter((repo) => !members.includes(repo.id));
  });
  const title = $derived.by(() => {
    const target = scope;
    return target.kind === "global" ? "Tasks" : target.kind === "workspace"
      ? workspaces.find((w) => w.id === target.id)?.name ?? "Workspace"
      : repositories.find((r) => r.id === target.id)?.name ?? "Repository";
  });
  const addRepoLabel = $derived(scope.kind === "workspace" ? `Add repository to ${title}` : "Add repository");
  /**
   * Portaled and clamped, from the shared popover owner.
   *
   * A CSS `absolute` panel under this heading used to live inside the 188px
   * `.navigator` scroller. `right: 0` plus a 260px width grew the sidebar
   * sideways and clipped the rows — the menu was a header dropdown, not a
   * left-rail control. Portal + an element anchor is how every other overlay
   * on this page escapes a clipping ancestor.
   *
   * `onAddMenuKey` takes Escape first whenever the menu has focus; bubble
   * Escape here is the fallback for focus that has left it.
   */
  function closeAddMenu(options?: { restoreFocus?: boolean }) {
    const opener = addRepoTriggerEl;
    addMenu = false;
    if (options?.restoreFocus) restoreFocusTo(opener);
  }
  const addMenuDismissal = $derived({
    anchor: { kind: "element" as const, element: addRepoTriggerEl, gap: 6 },
    estimate: { width: 280, height: 240 },
    revision: menuTabs.length + attachable.length,
    inset: 8,
    dismiss: {
      inside: "[data-add-repo], [data-add-repo-popup]",
      scroll: true,
      resize: true,
      escape: "bubble" as const,
    },
    onDismiss: (reason: string) => closeAddMenu({ restoreFocus: reason === "escape" }),
  });
  const total = $derived(STATUSES.reduce((sum, status) => sum + (displayColumns[status]?.total ?? 0), 0));
  const loadedCards = $derived(allLoadedCards(displayColumns));
  const facetOptions = $derived(collectFacetOptions(loadedCards));
  const filtering = $derived(facetActive(facet) || search.trim().length > 0);
  // Layout, density, hidden columns and card chips are reader preferences the
  // profile remembers; the header and the View menu write them back.
  const layout = $derived($interfaceStore.taskLayout);
  const compact = $derived($interfaceStore.taskDensity === "compact");
  const cardFields = $derived(new Set($interfaceStore.taskCardFields));
  const hiddenColumns = $derived($interfaceStore.taskHiddenColumns);
  const showArchived = $derived($interfaceStore.taskShowArchivedWorkspaces);
  const visibleWorkspaces = $derived(showArchived ? workspaces : workspaces.filter((group) => !group.archived));
  /** The navigator's order; moves are computed against this same list. */
  const orderedWorkspaces = $derived(navigatorOrder(visibleWorkspaces));
  /** Named groups on the repository tab strip that could become workspaces. */
  const importableGroups = $derived(tabGroups($repoStore.openTabs, $repoStore.groupColors));
  let movingGroup = $state(false);
  let importingGroups = $state(false);
  const columnTotals = $derived(Object.fromEntries(STATUSES.map((status) => [status, displayColumns[status]?.total ?? 0])) as Partial<Record<TaskStatus, number>>);
  /**
   * Work a hidden column is keeping off screen.
   *
   * Hiding a column is a layout choice; hiding the *tasks* in it is not one
   * the board gets to make silently. The banner names the columns and the
   * count, and offers the one action that undoes it.
   */
  const hiddenWork = $derived(initialized && !loading ? hiddenColumnReport(hiddenColumns, columnTotals) : null);
  const quickAddRepositories = $derived(repositories.map((repo) => ({ id: repo.id, name: repo.name })));
  /**
   * Whether this scope can hold a new task, why not, and what one starts as.
   *
   * One answer, read by the header button, the empty state, quick add and the
   * seed handed to the sheet. They used to decide separately and disagree: the
   * empty state offered New task where the header refused it, and an empty
   * workspace refused it everywhere with nothing on screen saying why.
   */
  const creation = $derived.by(() => {
    const target = scope;
    return taskCreation(target, {
      initialized,
      repositories,
      workspaceMembers: workspaceMemberIds,
      workspaceName: target.kind === "workspace" ? workspaces.find((group) => group.id === target.id)?.name ?? "" : "",
    });
  });
  const selectedCards = $derived(cardsById(displayColumns, selected));
  /** Whether the selection bar's Archive would change anything. */
  const selectionArchived = $derived(archiveState(selectedCards));
  /** Tasks are being filed as GitHub issues; one run at a time, since each issue is published. */
  let filingIssue = $state(false);
  /** Which issue of the run is being created, for the progress line. */
  let issueProgress = $state<{ index: number; total: number; title: string } | null>(null);
  /** Set by Stop; the run checks it before each creation, never mid-`gh`. */
  let issueStop = $state(false);
  /**
   * Issues runs created but could not link, each offered as a Link button.
   * Kept until linked, across runs and reloads: such a task has no link
   * label, so this is also what stops a re-run filing it twice.
   */
  let relinks = $state<{ taskId: string; number: number; title: string }[]>([]);
  const busy = $derived(moving || opening || deleting || deletesRunning > 0 || filingIssue || actionDialog !== null || pendingUpdate !== null);
  const openCardIds = $derived(openSavedTaskIds(taskTabs));
  const inProgressCount = $derived(displayColumns.in_progress?.total ?? 0);
  /** The server's count for this scope, so the badge is not a page size. */
  const completedCount = $derived(displayColumns[ARCHIVE_STATUS]?.total ?? 0);
  $effect(() => {
    if (repositoryPath) return;
    publishTaskChrome({ openTabs: taskTabs.tabs.length, inProgress: inProgressCount });
  });
  const shown = $derived.by(() => {
    const counts = Object.fromEntries(STATUSES.map((status) => [status, visibleIn(status).length])) as Partial<Record<TaskStatus, number>>;
    return visibleBoardStatuses(hiddenColumns, counts, drag !== null);
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
  /**
   * A task write finished; tell the archive dock to reload.
   *
   * The dock listens for `workbench-changed` itself, but that event is a
   * delivery hint the host is allowed to lose — and the tasks fixture never
   * emits it at all. A task that reached Done and did not appear in the
   * archive, or a restored one that stayed in it, would read as a write that
   * did not happen, so the board tells the dock directly at each of the three
   * points where it knows a write committed. Every write goes through one of
   * them: the batch dialog, an inline move, or the editor's save.
   *
   * It is deliberately not called from `loadBoard`, which also runs on every
   * debounced keystroke and would reset the dock's paging and selection under
   * a reader who is only typing.
   */
  function taskWritten() { archiveToken++; }
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
    catalogError = "";
    // A refresh re-reads the workspace's membership too. Without this the
    // Retry beside a failed membership read reloaded everything except the
    // thing that failed.
    membershipToken++;
    try { await catalog(); } catch (cause) { catalogError = explainError(cause); }
    if (initialized && active && !disposed) { await loadBoard(); await loadUnread(); }
  }
  function scheduleRefresh() {
    if (!active || disposed) return;
    clearTimeout(refreshTimer);
    refreshTimer = undefined;
    if (readBackgroundDocument()) return;
    refreshTimer = setTimeout(() => {
      refreshTimer = undefined;
      if (readBackgroundDocument() || !active || disposed) return;
      void refresh();
    }, 200);
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
  // The back link from a terminal session (see `taskOpen.ts`). Waits for the
  // board to finish starting — its restore of the last open tab would
  // otherwise replace the task asked for — and for any edit in progress,
  // then takes the request so it is opened exactly once.
  $effect(() => {
    const id = $taskOpenRequest;
    if (!id || !acceptsOpenRequests || !active || !initialized || busy) return;
    untrack(() => {
      if (!consumeTaskOpen(id)) return;
      // Said where it can be seen: the ceiling's own message is for a screen
      // reader, and this request came from another surface.
      if (refuseAtCeiling(id)) { error = announce; return; }
      void openTask(id);
    });
  });
  // ---- Agents working on the cards ----------------------------------------
  // The same reads and judgement as each task's Agents pane
  // (`boardAgents.ts`), so a card marked "needs you" opens on a pane that
  // says so too. Read only while the board is shown.
  const boardAgents = createBoardAgents();
  $effect(() => {
    if (!active) { boardAgents.stop(); return; }
    untrack(() => boardAgents.start());
    return () => boardAgents.stop();
  });
  const agentsByTask = $derived.by((): ReadonlyMap<string, TaskAgentSummary> => {
    const read = $boardAgents;
    if (read.readAt === null) return new Map();
    return taskAgentSummaries(read.runs, {
      records: $terminalSessions,
      requests: $taskTerminalRequests,
      capacityFull: $terminalSessions.length >= $terminalSessionLimit,
      activity: (sessionId) => $sessionActivity.get(sessionId),
      pending: (runId) => read.pending.get(runId),
      now: read.readAt,
      clock: read.readAt,
    });
  });
  /** "2 agents working · 1 needs you", or "at least …" when the read was a floor. */
  function agentsTitle(summary: TaskAgentSummary): string {
    const floor = $boardAgents.complete ? "" : "at least ";
    const working = `${floor}${summary.working} ${summary.working === 1 ? "agent" : "agents"} working`;
    return summary.asking ? `${working} · ${summary.asking} ${summary.asking === 1 ? "needs" : "need"} you` : working;
  }
  onMount(() => {
    const listeners = createListenerTracker();
    const changed = () => { scheduleRefresh(); void boardAgents.refresh(); };
    if (isTauri()) void listen("workbench-changed", changed).then((stop) => listeners.track(stop)).catch((cause) => { if (!disposed) error = `Live updates unavailable: ${explainError(cause)}`; });
    const stopClock = createAdaptiveTimer(() => { now = Math.floor(Date.now() / 1000); }, 30_000);
    const onForeground = () => {
      boardAgents.wake();
      if (readBackgroundDocument()) {
        clearTimeout(refreshTimer);
        refreshTimer = undefined;
        return;
      }
      scheduleRefresh();
    };
    listeners.track(stopClock);
    listeners.track(bindForegroundChanges(document, typeof window === "undefined" ? null : window, onForeground));
    return () => {
      disposed = true; revision++; unreadRevision++; initializationRevision++; openingRevision++; pendingUpdate?.stop(); clearTimeout(refreshTimer); clearTimeout(undoTimer); listeners.dispose();
      // Leaving the board ends the undo window rather than dropping it: the
      // reader saw these tasks go, so they go.
      if (pendingDelete && !pendingDelete.running) void commitPendingDelete();
    };
  });
  $effect(() => {
    if (!initialized || !active) return;
    const target = scope, query = search;
    loading = true;
    const timer = setTimeout(() => { void loadBoard(target, query); void loadUnread(target); }, 250);
    return () => { clearTimeout(timer); revision++; };
  });
  // `membershipToken` is what makes this read retryable. Keyed on `scope`
  // alone, a failed membership read could never be repeated: the banner's
  // Retry calls refresh(), which does not change the scope, so the effect
  // never re-ran and the board stayed on a membership it had not read.
  $effect(() => {
    membershipToken;
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
    if (!addMenu || !addMenuEl) return;
    // Deferred so the portal has moved the node and the popover has measured
    // it before we steal focus. A sync focus on a node still in the heading
    // was fine; a sync focus on a node mid-reparent is not.
    const id = window.setTimeout(() => {
      addMenuEl?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    }, 0);
    return () => clearTimeout(id);
  });
  /** Join these repositories to the workspace scope, if that is where we are. */
  async function attachToScope(ids: string[]) {
    if (scope.kind !== "workspace" || ids.length === 0) return;
    const saved = await attachRepositories(scope.id, ids);
    if (disposed) return;
    if (scope.kind === "workspace" && scope.id === saved.id) workspaceMemberIds = saved.repository_ids;
  }
  /** What just happened, said in full: adding here also joins a workspace. */
  function addedAnnouncement(names: string[]): string {
    const what = names.length === 1 ? names[0] : `${names.length} repositories`;
    return scope.kind === "workspace" ? `Added ${what} to ${title}` : `Added ${what}`;
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
      await attachToScope(added.map((repo) => repo.id));
      if (disposed) return;
      await catalog();
      announce = addedAnnouncement(added.map((repo) => repo.name));
      toastStore.success(announce);
    } catch (cause) { catalogError = explainError(cause); }
    finally { adding = false; }
  }
  /**
   * Join repositories the catalog already holds to this workspace.
   *
   * The add menu used to reach only open tabs and the folder picker, so a
   * registered repository that happened to be closed could not be added to a
   * workspace from this board at all — the workspace editor was the only way.
   */
  async function addRegistered(ids: string[]) {
    addMenu = false;
    if (ids.length === 0 || adding || scope.kind !== "workspace") return;
    adding = true;
    try {
      await attachToScope(ids);
      if (disposed) return;
      await catalog();
      announce = addedAnnouncement(ids.map((id) => repositories.find((repo) => repo.id === id)?.name ?? id));
      toastStore.success(announce);
    } catch (cause) { catalogError = explainError(cause); }
    finally { adding = false; }
  }
  /**
   * Point a repository at the checkout it moved to (`repositoryRelink.ts`).
   *
   * An uncertain reply keeps a Retry that resends the same request id, so a
   * lost answer can be confirmed without relinking twice.
   */
  async function relinkRepo(repo: Repository) {
    if (relinking || busy) return;
    relinking = true;
    try {
      const io = defaultRelinkIO(() => invoke<string | null>("cmd_pick_folder"), (prompt) => askConfirm(prompt));
      await settleRelink(await relinkCheckout(repo, io));
    } finally { if (!disposed) relinking = false; }
  }
  async function retryRelink() {
    const pending = relinkPending;
    if (!pending || relinking) return;
    relinking = true;
    try { await settleRelink(await pending.retry()); }
    finally { if (!disposed) relinking = false; }
  }
  async function settleRelink(outcome: RelinkOutcome) {
    if (disposed || outcome.kind === "cancelled") return;
    if (outcome.kind === "uncertain") { relinkPending = outcome; return; }
    relinkPending = null;
    if (outcome.kind === "failed") { catalogError = outcome.message; return; }
    announce = outcome.message;
    toastStore.success(outcome.message);
    // A repository write, not a task write: the tasks are untouched, so the
    // archive dock has nothing to reload. The board re-reads the catalog.
    await refresh();
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
    // With nothing to list, the menu would be one "Choose folder…" row; go
    // straight there. A workspace with registered repositories to join has
    // rows even when no tab is open, so it must not take that shortcut.
    if (menuTabs.length === 0 && attachable.length === 0) { void pickFolder(); return; }
    addMenu = !addMenu;
  }
  function onAddMenuKey(event: KeyboardEvent) {
    event.stopPropagation();
    if (event.key === "Tab" || event.key === "Escape") {
      event.preventDefault();
      closeAddMenu({ restoreFocus: true });
      return;
    }
    const items = [...(event.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('[role="menuitem"]')];
    if (items.length === 0) return;
    const index = Math.max(0, items.indexOf(event.target as HTMLElement));
    if (event.key === "ArrowDown") { event.preventDefault(); items[(index + 1) % items.length]?.focus(); }
    else if (event.key === "ArrowUp") { event.preventDefault(); items[(index - 1 + items.length) % items.length]?.focus(); }
    else if (event.key === "Home") { event.preventDefault(); items[0]?.focus(); }
    else if (event.key === "End") { event.preventDefault(); items[items.length - 1]?.focus(); }
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
  async function canLeaveSession(): Promise<boolean> {
    if (!session) return true;
    return (await editorHandle?.canLeave()) !== false;
  }
  function dropDraftSessionTab() {
    if (session?.value === null) taskTabs = closeTaskTab(taskTabs, session.tabId);
  }
  async function confirmDiscard(_message: string): Promise<boolean> {
    if (!(await canLeaveSession())) return false;
    dropDraftSessionTab();
    return !workspaceEditor || await workspaceHandle?.canLeave() === true;
  }
  async function openQuickEnhance(id: string) {
    if (!(await confirmDiscard("Open Quick Enhance? Unsaved edits in the current editor will be discarded."))) return;
    session = null;
    enhanceId = id;
    // Opening the sheet to look at a task must never spend a model request.
    // The counter is compared against a fresh instance's zero, so leaving a
    // drafting run's value standing would auto-start on whatever is opened
    // next. Asking is the only thing that raises it.
    enhanceStart = 0;
  }
  function refuseAtCeiling(id: string): boolean {
    if (canOpenTaskTab(taskTabs, id)) return false;
    announce = `At most ${MAX_TASK_TABS} tasks can be open. Close one before opening another.`;
    return true;
  }
  async function restoreActiveTab() {
    const id = taskTabs.activeId;
    if (!id || session?.tabId === id) return;
    const tab = taskTabs.tabs.find((item) => item.id === id);
    if (!tab || tab.draft) {
      if (tab?.draft) taskTabs = closeTaskTab(taskTabs, id);
      return;
    }
    opening = true; const ticket = ++openingRevision;
    try {
      const full = await bounded(getTask(id));
      if (disposed || ticket !== openingRevision) return;
      session = { tabId: full.id, pane: full.id, value: full };
      taskTabs = updateTaskTab(taskTabs, id, { title: full.title, status: full.status, draft: false });
    } catch (cause) {
      if (!disposed && ticket === openingRevision) {
        error = explainError(cause);
        taskTabs = closeTaskTab(taskTabs, id);
      }
    } finally { if (!disposed && ticket === openingRevision) opening = false; }
  }
  async function closeOpenTab(id: string) {
    if (session?.tabId === id) {
      if (!(await canLeaveSession())) return;
      session = null;
    }
    taskTabs = closeTaskTab(taskTabs, id);
    if (!session && taskTabs.activeId) void restoreActiveTab();
  }
  async function focusOpenTab(id: string) {
    if (session?.tabId === id || busy) return;
    if (!(await canLeaveSession())) return;
    dropDraftSessionTab();
    taskTabs = activateTaskTab(taskTabs, id);
    session = null;
    void restoreActiveTab();
  }
  function onTaskTabStripKeydown(event: KeyboardEvent) {
    const current = taskTabs.tabs.findIndex((tab) => tab.id === taskTabs.activeId);
    const move = handleTablistKeydown(event.key, current, taskTabs.tabs.length);
    if (!move) return;
    event.preventDefault();
    const target = taskTabs.tabs[move.index];
    if (!target) return;
    void focusOpenTab(target.id);
    focusTabAt(tabStrip, move.index);
  }
  /**
   * Move a workspace one place up or down the navigator (`workspaceOrder.ts`).
   * Positions are stored, so the order survives a reload; a move the store
   * only partly took is reported with both numbers, never as done.
   */
  async function moveGroup(id: string, delta: -1 | 1) {
    if (movingGroup || busy) return;
    const writes = moveWrites(orderedWorkspaces, id, delta);
    if (!writes) return;
    const name = workspaces.find((group) => group.id === id)?.name ?? "Workspace";
    movingGroup = true;
    try {
      const result = await applyMove(writes);
      if (disposed) return;
      try { await catalog(); } catch (cause) { catalogError = explainError(cause); }
      if (result.failed.length) {
        catalogError = `${name} did not finish moving: ${result.failed.length} of ${writes.length} ${writes.length === 1 ? "workspace" : "workspaces"} could not be updated. ${result.failed[0]?.error ?? ""}`;
      } else {
        announce = `${name} moved ${delta < 0 ? "up" : "down"}`;
      }
      boardEl?.querySelector<HTMLElement>(`[data-workspace-row="${CSS.escape(id)}"] > button`)?.focus();
    } finally { if (!disposed) movingGroup = false; }
  }
  /**
   * Make a workspace of every named tab group that has none yet
   * (`workspaceImport.ts`). The name check needs every workspace, so a
   * catalog with more pages is read to the end first rather than trusted.
   */
  async function importGroups() {
    if (importingGroups || busy || !importableGroups.length) return;
    importingGroups = true;
    try {
      for (let page = 0; workspaceCursor && page < 50; page++) await moreWorkspaces();
      if (workspaceCursor) { catalogError = "Not every workspace could be read, so the import could not tell which tab groups already have one."; return; }
      const report = await importTabGroups(importableGroups, workspaces);
      if (disposed) return;
      try { await catalog(); } catch (cause) { catalogError = explainError(cause); }
      announce = importSummary(report);
      if (report.created.length) toastStore.success(announce);
      if (report.failed.length) catalogError = `${announce} ${report.failed.map((entry) => `${entry.name}: ${entry.error}`).join(" ")}`;
      else if (!report.created.length) toastStore.info(announce);
    } catch (cause) { if (!disposed) catalogError = explainError(cause); }
    finally { if (!disposed) importingGroups = false; }
  }
  async function newWorkspace() {
    if (busy || !await confirmDiscard("Open a new workspace?") || disposed) return;
    session = null; taskTabs = emptyTaskTabs(); enhanceId = null; workspaceEditor = { value: null };
  }
  async function editWorkspace(id: string) {
    if (busy) return;
    opening = true; const ticket = ++openingRevision;
    try {
      if (!await confirmDiscard("Open workspace settings?")) return;
      const full = await bounded(getWorkspace(id));
      if (disposed || ticket !== openingRevision) return;
      session = null; taskTabs = emptyTaskTabs(); enhanceId = null; workspaceEditor = { value: full };
    } catch (cause) { if (!disposed && ticket === openingRevision) error = explainError(cause); }
    finally { if (!disposed && ticket === openingRevision) opening = false; }
  }
  async function openTask(id: string) {
    if (session?.tabId === id && session.value) return;
    if (busy) return;
    if (refuseAtCeiling(id)) return;
    opening = true; const ticket = ++openingRevision;
    try {
      if (!(await canLeaveSession())) return;
      dropDraftSessionTab();
      if (disposed || ticket !== openingRevision) return;
      const full = await bounded(getTask(id));
      if (disposed || ticket !== openingRevision) return;
      workspaceEditor = null; enhanceId = null;
      taskTabs = openTaskTab(taskTabs, { id: full.id, title: full.title, status: full.status, draft: false });
      session = { tabId: full.id, pane: full.id, value: full };
    } catch (cause) { if (!disposed && ticket === openingRevision) error = explainError(cause); }
    finally { if (!disposed && ticket === openingRevision) opening = false; }
  }
  function openLoaded(task: Task) {
    workspaceEditor = null; enhanceId = null;
    taskTabs = openTaskTab(taskTabs, { id: task.id, title: task.title, status: task.status, draft: false });
    session = { tabId: task.id, pane: task.id, value: task };
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
    if (opening) return;
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
    void dropCard(current.card, current.over, current.insertIndex);
  }
  /**
   * Put a card at `insertIndex` of a column (counted without the card itself).
   * The one place a drop becomes a write, for the pointer and the keyboard.
   */
  async function dropCard(card: TaskCard, over: TaskStatus | null, insertIndex: number) {
    const fromIndex = (displayColumns[card.status]?.items ?? []).findIndex((item) => item.id === card.id);
    if (!shouldCommitMove(card.status, over, { moving, fromIndex, insertIndex })) return;
    const items = displayColumns[over]?.items ?? [];
    const { before, after } = insertionNeighbors(items, card.id, insertIndex);
    const position = insertionPosition(before,after);
    if (after !== null && (position >= after || before !== null && position <= before)) {
      const plan = reorderPlan(items,card,insertIndex);
      if (columns[over]?.next_cursor || plan.cards.length > MAX_TASK_SELECTION) { error = `This column needs re-spacing. Load its remaining tasks first; up to ${MAX_TASK_SELECTION} tasks can be reordered together. Priority and title sorting remain available.`; return; }
      await applyUpdate(new TaskBatch(plan.cards,{kind:"reorder",status:over,positions:plan.positions}));
    } else await moveCard(card, over, position);
  }
  function onCardClick(e: MouseEvent, card: TaskCard) {
    // A card is not `disabled` while a task opens: a disabled button loses
    // focus, so the editor recorded <body> as its opener and returned focus
    // there on close. The same refusal is made here instead.
    if (opening) return;
    if (skipClick) return;
    if (e.metaKey || e.ctrlKey) {
      selected = toggleSelection(selected, card.id);
      selectionAnchor = card.id;
      return;
    }
    if (e.shiftKey) {
      const ids = flattenVisibleIds(displayColumns, shown, (item) => cardMatchesFacet(item, facet, now));
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
    if (opening) return;
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
    // A card is not `disabled` while a task opens: a disabled button loses
    // focus, so the editor recorded <body> as its opener and returned focus
    // there on close. The same refusal is made here instead.
    if (opening) return;
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
      void openQuickEnhance(card.id);
      return;
    }
    // `x` selects without a modifier. Space only toggles once something is
    // selected, and Command- or Control-Space belong to the system on macOS,
    // so without this a keyboard could not start a selection at all.
    if (e.key.toLowerCase() === "x" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      selected = toggleSelection(selected, card.id);
      selectionAnchor = card.id;
      return;
    }
    if ((e.key === "ArrowUp" || e.key === "ArrowDown") && e.altKey && !e.metaKey && !e.ctrlKey) {
      e.preventDefault();
      void nudgeCard(card, e.key === "ArrowUp" ? -1 : 1);
      return;
    }
    if (e.key === "ArrowUp" || e.key === "ArrowDown" || e.key === "Home" || e.key === "End") {
      const next = cardNeighbor(e.currentTarget as HTMLElement, e.key);
      if (!next) return;
      e.preventDefault();
      next.focus();
      const id = next.dataset.cardId;
      if (id && e.shiftKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
        const ids = flattenVisibleIds(displayColumns, shown, (item) => cardMatchesFacet(item, facet, now));
        if (!selectionAnchor) selectionAnchor = card.id;
        selected = rangeSelect(ids, selectionAnchor, id);
      }
      return;
    }
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const next = neighborStatus(card.status, e.key === "ArrowRight" ? 1 : -1);
    if (!next) return;
    const last = (columns[next]?.items ?? []).at(-1);
    void moveCard(card, next, insertionPosition(last?.position ?? null, null));
  }
  /**
   * The card a vertical key lands on: the next or previous card in the same
   * column (or in the list), or its first or last. Read from the DOM because
   * that is the order the reader sees, filters and hidden columns applied.
   */
  function cardNeighbor(from: HTMLElement, key: string): HTMLElement | null {
    const group = from.closest("[data-task-column], [data-task-list]");
    if (!group) return null;
    const cards = [...group.querySelectorAll<HTMLElement>("[data-task-card]")];
    const index = cards.indexOf(from);
    if (index === -1) return null;
    const target = key === "Home" ? 0 : key === "End" ? cards.length - 1 : index + (key === "ArrowUp" ? -1 : 1);
    return target === index ? null : cards[target] ?? null;
  }
  /**
   * Move a card one place up or down its column from the keyboard — the drag
   * a pointer would do, through the same position rules and the same write.
   */
  async function nudgeCard(card: TaskCard, delta: -1 | 1) {
    if (busy) return;
    const items = displayColumns[card.status]?.items ?? [];
    const from = items.findIndex((item) => item.id === card.id);
    const to = from + delta;
    if (from === -1 || to < 0 || to >= items.length) return;
    await dropCard(card, card.status, to);
    if (disposed) return;
    announce = `${card.title} moved ${delta < 0 ? "up" : "down"} in ${STATUS_LABELS[card.status]}`;
    // The board reloaded; put the focus back on the card it was on.
    boardEl?.querySelector<HTMLElement>(`[data-task-card][data-card-id="${CSS.escape(card.id)}"]`)?.focus();
  }
  function onBoardKeydown(e: KeyboardEvent) {
    if (!active || !boardEl) return;
    const target = e.target;
    if (!(target instanceof Node) || !boardEl.contains(target)) return;
    if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) return;
    if (target instanceof HTMLElement && target.closest("[role=dialog], .task-editor, .workspace-editor, [data-task-menu], .editor-dock")) return;
    if (e.key === "Escape") {
      if (menu) { menu = null; return; }
      if (selected.size) { selected = new Set(); selectionAnchor = null; return; }
    }
    if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "z" && (pendingDelete || undoOffer)) {
      e.preventDefault();
      void undoLast();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "a") {
      e.preventDefault();
      selected = new Set(flattenVisibleIds(displayColumns, shown, (item) => cardMatchesFacet(item, facet, now)));
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
    // Only claim the key when there is somewhere to put the cursor. A profile
    // with no repositories draws no quick-add field, and swallowing `a` there
    // would make the board eat a keystroke and do nothing with it.
    if (e.key === "a" && !e.metaKey && !e.ctrlKey && !e.altKey && quickAddEl) {
      e.preventDefault();
      quickAddEl.focus();
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
      // Every write this board makes through here can be taken back, except
      // a restore: its undo would be a redo nobody asked for.
      const plan = batch.undo();
      offerUndo(plan);
      announce = plan ? `${plan.label}${done < rows.length ? ` (${done} of ${rows.length})` : ""}. Undo is available.` : `${done} of ${rows.length} tasks updated`;
      if (done) { taskWritten(); await loadBoard(); }
      const failure = rows.find(row => row.state === "uncertain" || row.state === "failed");
      if (failure) error = failure.error;
    } finally { moving = false; }
  }
  /**
   * Offer to undo a batch that wrote something.
   *
   * One offer at a time: the latest write replaces it, and a scope change or
   * a minute's wait withdraws it. The undo itself is revision-checked
   * (`TaskBatch.undo`), so an old offer can only ever be refused, never
   * revert a task someone edited since.
   */
  function offerUndo(plan: UndoPlan | null) {
    clearTimeout(undoTimer);
    undoOffer = plan;
    if (plan) undoTimer = setTimeout(() => { if (undoOffer === plan) undoOffer = null; }, UNDO_OFFER_MS);
  }
  async function undoLast() {
    if (pendingDelete && !pendingDelete.running) { cancelPendingDelete(); return; }
    const plan = undoOffer;
    if (!plan || busy) return;
    offerUndo(null);
    await applyUpdate(new TaskBatch(plan.cards, plan.action));
    if (!disposed && !error) announce = `Undone: ${plan.label}`;
  }
  /**
   * Hide a confirmed deletion and wait before writing it.
   *
   * The store never reuses a deleted id, so a deletion that was sent cannot be
   * taken back. This is the only undo it can have: the cards leave the board
   * at once, and nothing is written until the window closes or the reader
   * says Delete now. Undo in that window writes nothing at all.
   */
  function deferDeletion(batch: TaskBatch, cards: TaskCard[]) {
    actionDialog = null;
    const previous = pendingDelete;
    if (previous && !previous.running) void commitPendingDelete(previous);
    offerUndo(null);
    const ids = new Set(cards.map((card) => card.id));
    selected = new Set([...selected].filter((id) => !ids.has(id)));
    const entry: PendingDeletion = { batch, cards, ids, running: false, window: defer(DEFERRED_DELETE_MS, () => { void commitPendingDelete(entry); }) };
    pendingDelete = entry;
    announce = `${describeTaskAction({kind:"delete"}, cards.length)}. Undo is available for ${DEFERRED_DELETE_MS / 1000} seconds.`;
  }
  function deferDialogDeletion(batch: TaskBatch) {
    const cards = actionDialog?.cards;
    if (cards) deferDeletion(batch, cards);
  }
  function cancelPendingDelete() {
    const entry = pendingDelete;
    if (!entry || entry.running || !entry.window.cancel()) return;
    pendingDelete = null;
    announce = `Kept ${plural(entry.cards.length, "task")}. Nothing was deleted.`;
  }
  /**
   * Write a deferred deletion now. Rows that fail or need confirmation open
   * the action dialog on the same batch, so its receipts and its exact retry
   * are the ones every other task action uses.
   */
  async function commitPendingDelete(entry: PendingDeletion | null = pendingDelete) {
    if (!entry || entry.running) return;
    entry.window.cancel();
    entry.running = true;
    deletesRunning++;
    // Keep them off the board while they are written, even once a newer
    // deletion has taken the pending slot; a failure reloads them.
    columns = removeFromColumns(columns, entry.ids);
    await entry.batch.run();
    const rows = entry.batch.snapshot();
    const done = rows.filter((row) => row.state === "done").map((row) => row.id);
    if (disposed) {
      // The board went away mid-window; the deletion still went out, so say
      // what did not, where it can still be read.
      const missed = rows.length - done.length;
      if (missed) toastStore.error(`${plural(missed, "task")} could not be deleted. Open Tasks to retry.`);
      return;
    }
    deletesRunning--;
    if (pendingDelete === entry) pendingDelete = null;
    tasksChanged(done);
    if (done.length === rows.length) { announce = describeTaskAction({kind:"delete"}, done.length); return; }
    void loadBoard();
    actionDialog = { cards: entry.cards, action: {kind:"delete"}, batch: entry.batch };
  }
  async function moveCard(card: TaskCard, status: TaskStatus, position: number) {
    if (busy || card.status === status && card.position === position) return;
    await applyUpdate(new TaskBatch([card], {kind:"update", changes:{status,position}}));
  }
  /**
   * Run one already-built action over a selection.
   *
   * The single inline write path. `patchCards` builds the action from a field
   * patch; Archive hands in `archiveAction()` instead, so what archiving *is*
   * stays in `taskArchive.ts` and the board only decides when to run it.
   */
  async function applyTaskAction(cards: TaskCard[], action: TaskAction) {
    if (busy || !cards.length) return;
    if (cards.length > MAX_TASK_SELECTION) { error = `Select at most ${MAX_TASK_SELECTION} loaded tasks per action.`; return; }
    await applyUpdate(new TaskBatch(cards, action));
  }
  async function patchCards(cards: TaskCard[], patch: TaskChanges) {
    await applyTaskAction(cards, {kind:"update", changes:patch});
  }
  /**
   * Archive a selection from the card menu or the selection bar.
   *
   * Writes only the tasks archiving would change. A mixed selection is a
   * normal thing to have — Command-click four cards, one of them already
   * Done — and re-writing that one would spend a revision to store the value
   * it already has and give the batch one more write to fail on.
   *
   * Silently doing nothing for an entirely archived selection would be the
   * board answering a click with nothing, so that no-op is refused out loud.
   * Both surfaces already disable the control; this is the guard for the one
   * that is wrong, not a second copy of the rule.
   */
  async function archiveCards(cards: TaskCard[]) {
    if (busy || !cards.length) return;
    const wanted = archivable(cards);
    if (!wanted.length) {
      error = cards.length === 1 ? "That task is already archived." : "Those tasks are already archived.";
      return;
    }
    const already = cards.length - wanted.length;
    await applyTaskAction(wanted, archiveAction());
    // `applyUpdate` announces what it wrote. The skipped tasks are not a
    // failure and not a write, so they are said separately rather than
    // folded into a count that would then not match the receipt.
    if (already > 0 && !error) announce = `${announce} ${plural(already, "task")} already archived.`;
  }
  /** Defaults a quick-added task inherits from wherever it was typed. */
  function quickAddDefaults(status: TaskStatus) {
    return { status, kind: "feature", ...creation.seed, position: Date.now() };
  }

  /**
   * Save a parsed quick-add line straight to the store.
   *
   * Returns whether the field may clear, so a refused or failed write leaves
   * the typed line exactly where the reader can fix it. Nothing is invented:
   * `quickAddDraft` refuses when no repository is linked, and the refusal names
   * the two ways out — the `^` marker, or the editor.
   */
  async function createFromQuickAdd(parsed: QuickAddResult, status: TaskStatus = "inbox", mode: QuickAddMode = "manual"): Promise<boolean> {
    if (busy || quickAdding) return false;
    const draft = quickAddDraft(parsed, quickAddDefaults(status));
    if (!draft) {
      // One refusal for both modes. Drafting cannot open a picker either, so
      // inventing a second message here would give the reader two different
      // answers to the same question.
      error = quickAddRefusal(creation);
      return false;
    }
    // Asked *before* the write, not after. Drafting ends by opening a sheet
    // over the editor, and reversing these leaves a reader who answers "Keep
    // editing" with a task already on the board and no way to review it.
    if (mode === "assist" && !(await confirmDiscard("Draft this task? Unsaved edits in the current editor will be discarded."))) return false;
    quickAdding = true; error = "";
    try {
      const saved = await bounded(putTask(taskWrite(newID(), 0, draft)));
      if (disposed) return true;
      announce = `Added ${saved.title}`;
      toastStore.success(`Added ${saved.title}`);
      await loadBoard();
      if (disposed) return true;
      // The card is already on the board carrying the reader's own words, so a
      // model that never answers costs them nothing. Opening the sheet is what
      // makes the request reviewable — and refusable, with a reason.
      if (mode === "assist") {
        session = null;
        workspaceEditor = null;
        enhanceId = saved.id;
        enhanceStart += 1;
      }
      return true;
    } catch (cause) {
      if (!disposed) error = explainError(cause);
      return false;
    } finally { if (!disposed) quickAdding = false; }
  }

  /** Hand a typed quick-add line to the full editor rather than saving it. */
  async function expandQuickAdd(parsed: QuickAddResult, status: TaskStatus = "inbox") {
    if (busy) return;
    const draft = quickAddDraft(parsed, quickAddDefaults(status));
    if (!draft) { await createTask(status); return; }
    const draftId = `draft-${newID()}`;
    if (refuseAtCeiling(draftId)) return;
    if (!(await confirmDiscard("Start a new task and discard the current unsaved edits?"))) return;
    workspaceEditor = null; enhanceId = null;
    taskTabs = openTaskTab(taskTabs, { id: draftId, title: draft.title || "New task", status, draft: true });
    session = { tabId: draftId, pane: draftId, value: null, status, seed: draft };
  }

  /** Relative due targets the context menu offers, resolved against now. */
  function dueFromChoice(choice: "today" | "tomorrow" | "next_week" | "clear"): number | null {
    if (choice === "clear") return null;
    const word = choice === "next_week" ? "next week" : choice;
    return parseQuickAddDue(word);
  }

  async function createTask(status: TaskStatus = "inbox") {
    if (busy) return;
    // Every caller reads `creation` before offering this, so a refusal here is
    // a keyboard shortcut or a stale click — it still has to say why rather
    // than open a sheet that can never be saved.
    if (!creation.allowed) { error = creation.blocked ?? "This scope cannot hold a new task."; return; }
    const draftId = `draft-${newID()}`;
    if (refuseAtCeiling(draftId)) return;
    if (!(await confirmDiscard("Start a new task and discard the current unsaved edits?"))) return;
    workspaceEditor = null; enhanceId = null;
    taskTabs = openTaskTab(taskTabs, { id: draftId, title: "New task", status, draft: true });
    session = { tabId: draftId, pane: draftId, value: null, status };
  }
  function onEditorSaved(saved: Task) {
    taskWritten();
    void loadBoard();
    if (!session) return;
    taskTabs = retargetTaskTab(taskTabs, session.tabId, { id: saved.id, title: saved.title, status: saved.status, draft: false });
    session = { ...session, tabId: saved.id, value: saved, status: saved.status };
  }
  function onEditorClose() {
    const id = session?.tabId;
    session = null;
    if (id) taskTabs = closeTaskTab(taskTabs, id);
    if (taskTabs.activeId) void restoreActiveTab();
  }
  async function removeSelected() {
    const cards = selectedCards;
    if (busy || !cards.length || !await confirmDiscard("Delete selected tasks?")) return;
    if (cards.length > MAX_TASK_SELECTION) { error = `Select at most ${MAX_TASK_SELECTION} loaded tasks per action.`; return; }
    actionDialog = { cards: [...cards], action: {kind:"delete"} };
  }
  /**
   * Restore and delete from the archive dock.
   *
   * Routed into the board's own `actionDialog` rather than run by the dock:
   * one confirm step, one batch, one interrupted-write recovery path for
   * every task mutation this board performs, wherever it was started.
   */
  async function archiveDockAction(cards: TaskCard[], action: TaskAction) {
    if (busy || !cards.length) return;
    if (cards.length > MAX_TASK_SELECTION) { error = `Select at most ${MAX_TASK_SELECTION} loaded tasks per action.`; return; }
    if (!await confirmDiscard("Change archived tasks?")) return;
    actionDialog = { cards: [...cards], action };
  }
  function tasksChanged(ids: string[]) {
    columns = removeFromColumns(columns, new Set(ids));
    if (ids.length) taskWritten();
    selected = new Set([...selected].filter(id => !ids.includes(id)));
    const dropped = Boolean(session?.value && ids.includes(session.value.id));
    if (dropped) session = null;
    for (const id of ids) taskTabs = closeTaskTab(taskTabs, id);
    if (enhanceId && ids.includes(enhanceId)) enhanceId = null;
    if (dropped && taskTabs.activeId) void restoreActiveTab();
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
  /** Where a task's issue would be filed, or why it cannot be. */
  function issueTarget(card: TaskCard): TaskIssueTarget | string {
    const repositoryId = card.primary_repository_id || card.repository_ids[0] || "";
    const repository = repositories.find((repo) => repo.id === repositoryId);
    if (!repository) return "its repository is not loaded, so there is no remote to file on";
    const checkout = checkoutCandidates(repository.id, repositories, openTabRefs, pathOpts)[0];
    if (!checkout) return `no working checkout of ${repository.name} is known; open it in GitPulse first`;
    return { repositoryName: repository.name, checkout: checkout.path, derived: checkout.source === "derived" };
  }
  /** Carry a freshly linked task into the editor tab that has it open. */
  function adoptLinked(task: Task) {
    if (session?.value?.id !== task.id) return;
    session = { ...session, value: task, status: task.status };
    taskTabs = updateTaskTab(taskTabs, task.id, { title: task.title, status: task.status });
  }
  /**
   * File saved tasks as issues on their repositories' GitHub remotes, linking
   * each task to its issue.
   *
   * Each remote is read from a working checkout of the task's own repository,
   * which need not be the repository open in the window — resolved the way a
   * handoff resolves it, and named in the one confirmation the run asks for.
   * `taskIssue.ts` owns the run itself: creations go through the guarded
   * issue owner one at a time, and the run stops at the first task that does
   * not end linked, so a refusal, an unknown outcome or a lost link is
   * reported against that task instead of repeated down the selection.
   */
  async function fileTaskIssues(cards: TaskCard[]) {
    if (busy || !cards.length) return;
    if (cards.length > MAX_TASK_ISSUE_BATCH) { error = `File at most ${MAX_TASK_ISSUE_BATCH} tasks as issues at a time.`; return; }
    filingIssue = true;
    issueStop = false;
    error = "";
    try {
      const { ready, skipped } = await prepareTaskIssues(cards, {
        resolve: issueTarget,
        read: getTask,
        unlinked: (id) => relinks.find((entry) => entry.taskId === id)?.number ?? null,
      });
      if (disposed) return;
      if (!ready.length) { error = summarizeTaskIssues([], skipped, false).message; return; }
      const one = ready.length === 1;
      const confirmed = await askConfirm({
        title: one ? "Create GitHub issue" : "Create GitHub issues",
        message: taskIssuesConfirmation(ready, skipped),
        confirmLabel: one ? "Create issue" : `Create ${ready.length} issues`,
      });
      if (!confirmed || disposed) return;
      const results = await runTaskIssues(ready, {
        create: (entry) => repoStore.reportIssue(entry.draft.title, entry.draft.body, [], { repoPath: entry.target.checkout }),
        stopped: () => issueStop || disposed,
        progress: (index, total, title) => { issueProgress = { index, total, title }; announce = `Filing issue ${index} of ${total}: ${title}`; },
      });
      const summary = summarizeTaskIssues(results, skipped, issueStop);
      const linked = results.filter((r): r is Extract<TaskIssueResult, { state: "linked" }> => r.state === "linked");
      const urls = results.flatMap((r) => (r.state === "linked" || r.state === "unlinked") && r.url ? [r.url] : []);
      const open = urls.length === 1 ? { label: "Open", onClick: () => openExternal(urls[0]) } : undefined;
      const lost = results.find((r): r is Extract<TaskIssueResult, { state: "unlinked" }> => r.state === "unlinked" && r.number !== null);
      if (lost && lost.number !== null) relinks = [...relinks.filter((entry) => entry.taskId !== lost.taskId), { taskId: lost.taskId, number: lost.number, title: lost.title }];
      // Reload before reporting: a reload clears `error` as it starts, and
      // would otherwise erase the account of what this run did not file.
      if (linked.length) {
        taskWritten();
        for (const result of linked) adoptLinked(result.task);
        await loadBoard();
        if (disposed) return;
      }
      if (summary.ok) { announce = summary.message; toastStore.success(summary.message, open); }
      else { error = summary.message; announce = summary.message; if (linked.length || urls.length) toastStore.warning("Some issues were filed; see the board for what was not.", open); }
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      filingIssue = false;
      issueProgress = null;
      issueStop = false;
    }
  }
  /**
   * Link a task to the issue a run created but could not link — usually
   * because the task was edited while the run was going. Reads the task again
   * and links at its current revision, so nothing is filed twice.
   */
  async function retryIssueLink(pending: { taskId: string; number: number; title: string }) {
    if (busy) return;
    filingIssue = true;
    try {
      const fresh = await bounded(getTask(pending.taskId));
      const linked = await linkTaskToIssue(fresh, pending.number);
      if (disposed) return;
      if (!linked.ok) { error = `Issue #${pending.number} still is not linked to “${pending.title}”: ${linked.reason}.`; return; }
      relinks = relinks.filter((entry) => entry.taskId !== pending.taskId);
      error = "";
      announce = `Linked “${pending.title}” to issue #${pending.number}.`;
      toastStore.success(announce);
      taskWritten();
      adoptLinked(linked.task);
      void loadBoard();
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      filingIssue = false;
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
        if (cards[0]) void openQuickEnhance(cards[0].id);
        break;
      case "duplicate":
        if (cards[0]) {
          const draftId = `draft-${newID()}`;
          if (refuseAtCeiling(draftId)) return;
          void confirmDiscard("Start a duplicate and discard the current unsaved edits?").then((ok) => {
            if (!ok) return;
            opening = true;
            void getTask(cards[0].id).then((full) => {
              if (disposed) return;
              workspaceEditor = null;
              enhanceId = null;
              const copy = taskDraft(full);
              const title = duplicateTitle(full.title);
              taskTabs = openTaskTab(taskTabs, { id: draftId, title, status: full.status, draft: true });
              session = { tabId: draftId, pane: draftId, value: null, status: full.status, seed: { ...copy, title, position: Date.now() } };
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
      case "due": {
        const due = dueFromChoice(item.action.choice);
        if (item.action.choice !== "clear" && due === null) { error = "Could not resolve that due date."; break; }
        void patchCards(cards, { due_at: due });
        break;
      }
      case "owner":
        void patchCards(cards, { owner: item.action.owner });
        break;
      case "label": {
        const { label, add } = item.action;
        if (busy || !cards.length) break;
        if (cards.length > MAX_TASK_SELECTION) { error = `Select at most ${MAX_TASK_SELECTION} loaded tasks per action.`; break; }
        void applyUpdate(new TaskBatch(cards, { kind: "label", label, add }));
        break;
      }
      case "githubIssue":
        void fileTaskIssues(cards);
        break;
      case "agent":
        if (cards[0]) handoff = { card: cards[0], settings: handoffFromTarget(item.action.target, $interfaceStore.taskHandoff) };
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
      case "archive":
        void archiveCards(cards);
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

{#snippet scopeSelection(selected: boolean)}
  {#if macos && selected}<span class="gp-liquid-selection gp-gpu" aria-hidden="true" in:receiveScope={{ key: "task-scope" }} out:sendScope={{ key: "task-scope" }}></span>{/if}
{/snippet}
<svelte:window onkeydown={onBoardKeydown} />
<div bind:this={boardEl} class="workbench bg-background" class:is-dragging={drag !== null} class:is-compact={compact} data-testid="task-board">
  {#if !repositoryPath}
    <nav class="navigator gp-glass" class:gp-liquid-tabs={macos} aria-label="Task scopes">
      <div class="nav-heading">Workspaces<span class="heading-actions">{#if importableGroups.length}<button type="button" class="icon gp-icon-btn" data-testid="import-tab-groups" title={`Import ${plural(importableGroups.length, "tab group")} as workspaces`} aria-label={`Import ${plural(importableGroups.length, "tab group")} as workspaces`} disabled={importingGroups || busy} onclick={() => void importGroups()}><Import size={12} /></button>{/if}<button type="button" class="icon gp-icon-btn" title="New workspace" aria-label="New workspace" onclick={newWorkspace}><Plus size={12} /></button></span></div>
      <button type="button" class="gp-seg-btn" aria-pressed={scope.kind === "global"} data-active={scope.kind === "global"} class:selected={scope.kind === "global"} onclick={() => { scope = { kind: "global" }; }}>{@render scopeSelection(scope.kind === "global")}<span>All</span></button>
      {#each orderedWorkspaces as group, index (group.id)}
        {@const siblings = orderedWorkspaces.filter((other) => other.pinned === group.pinned)}
        <div class="nav-row" data-workspace-row={group.id}><button type="button" class="gp-seg-btn" aria-pressed={scope.kind === "workspace" && scope.id === group.id} data-active={scope.kind === "workspace" && scope.id === group.id} class:selected={scope.kind === "workspace" && scope.id === group.id} aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown" onkeydown={(e) => { if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) { e.preventDefault(); void moveGroup(group.id, e.key === "ArrowUp" ? -1 : 1); } }} onclick={() => { scope = { kind: "workspace", id: group.id }; }} title="{group.name} — {workspaceMembershipLabel(group.repository_count)}">{@render scopeSelection(scope.kind === "workspace" && scope.id === group.id)}<span>{group.icon} {group.name}{group.repository_count === 0 ? " · Empty" : ""}{group.archived ? " · Archived" : ""}</span></button><span class="reorder"><button type="button" class="gp-icon-btn" aria-label={`Move ${group.name} up`} disabled={movingGroup || busy || siblings[0]?.id === group.id} onclick={() => void moveGroup(group.id, -1)}><ChevronUp size={11} /></button><button type="button" class="gp-icon-btn" aria-label={`Move ${group.name} down`} disabled={movingGroup || busy || siblings.at(-1)?.id === group.id || (index === orderedWorkspaces.length - 1)} onclick={() => void moveGroup(group.id, 1)}><ChevronDown size={11} /></button></span><button type="button" class="icon gp-icon-btn" aria-label={`Edit ${group.name}`} onclick={() => editWorkspace(group.id)} disabled={opening}>⋯</button></div>
      {/each}
      {#if workspaceCursor}<button type="button" onclick={moreWorkspaces}>More ({workspaces.length}/{workspaceTotal})</button>{/if}
      {#if workspaces.some((group) => group.archived)}
        <!-- "workspaces", not just "archived": this page also has an Archive
             that holds completed tasks, and two unqualified uses of the word
             on one screen read as one feature with a broken control. -->
        <label class="archive-toggle"><input type="checkbox" checked={showArchived} onchange={(e) => interfaceStore.setTaskShowArchivedWorkspaces(e.currentTarget.checked)} />Show archived workspaces</label>
      {/if}
      <!-- In a workspace scope this control has a second effect: whatever it
           adds also joins the workspace. It says so rather than leaving the
           membership write to be discovered. -->
      <div class="nav-heading" data-add-repo>Repositories<button bind:this={addRepoTriggerEl} type="button" class="icon gp-icon-btn" aria-haspopup="menu" aria-expanded={addMenu} aria-controls="task-add-repo-menu" aria-busy={adding} title={addRepoLabel} aria-label={addRepoLabel} disabled={adding} onclick={toggleAddMenu}><Plus size={12} /></button></div>
      {#if addMenu}
        <div
          bind:this={addMenuEl}
          use:portal={"body"}
          use:popover={addMenuDismissal}
          id="task-add-repo-menu"
          data-add-repo-popup
          data-testid="task-add-repo-menu"
          class="add-menu gp-menu gp-pop fixed"
          role="menu"
          aria-label={addRepoLabel}
          tabindex="-1"
          style="z-index: {LAYERS.MENU}"
          onkeydown={onAddMenuKey}
        >
          {#if menuTabs.length}<div class="add-menu-label">Open</div>{/if}
          {#each menuTabs as tab (tab.path)}
            <button type="button" class="add-item gp-menu-item" role="menuitem" title={tab.path} onclick={() => void addPaths([tab.path])}>
              <span class="add-name">{tab.label}</span>
              <span class="add-path">{tab.path}</span>
            </button>
          {/each}
          {#if menuTabs.length > 1}<button type="button" class="gp-menu-item" role="menuitem" onclick={() => void addPaths(menuTabs.map((tab) => tab.path))}>Add all open</button>{/if}
          {#if attachable.length}
            <div class="add-menu-label">Registered</div>
            {#each attachable as repo (repo.id)}
              <button type="button" class="add-item gp-menu-item" role="menuitem" title={repo.identity_key} onclick={() => void addRegistered([repo.id])}>
                <span class="add-name">{repo.name}</span>
              </button>
            {/each}
            {#if attachable.length > 1}<button type="button" class="gp-menu-item" role="menuitem" onclick={() => void addRegistered(attachable.map((repo) => repo.id))}>Add all registered</button>{/if}
          {/if}
          <button type="button" class="gp-menu-item" role="menuitem" onclick={() => void pickFolder()}>Choose folder…</button>
        </div>
      {/if}
      {#each repositories as repo (repo.id)}<div class="nav-row" data-repository-row={repo.id}><button type="button" class="gp-seg-btn" class:selected={scope.kind === "repository" && scope.id === repo.id} aria-pressed={scope.kind === "repository" && scope.id === repo.id} data-active={scope.kind === "repository" && scope.id === repo.id} onclick={() => { scope = { kind: "repository", id: repo.id }; }} title={repo.identity_key}>
        {@render scopeSelection(scope.kind === "repository" && scope.id === repo.id)}<span>{repo.name}</span>
      </button><button type="button" class="icon gp-icon-btn" aria-label={`Relink ${repo.name} to a moved checkout`} title={`Relink ${repo.name} — its checkout moved or was cloned again`} disabled={relinking || busy} onclick={() => void relinkRepo(repo)}><FolderSync size={12} /></button></div>{/each}
      {#if repositoryCursor}<button type="button" onclick={moreRepositories}>More ({repositories.length}/{repositoryTotal})</button>{/if}
    </nav>
  {/if}
  <main class="board-main">
    <header class="gp-glass">
      <div class="heading">
        <h1>{title}{#if !loading && initialized}<span>{total}</span>{/if}</h1>
        {#if inProgressCount > 0}<span class="gp-pill">{inProgressCount} in progress</span>{/if}
      </div>
      <div class="actions">
        <label class="search"><Search size={12} /><input id="task-search" class="gp-field" aria-label="Search tasks" type="search" bind:value={search} placeholder="Search tasks" maxlength="512" /></label>
        <div class="gp-segmented" class:gp-liquid-tabs={macos} role="group" aria-label="Task layout">
          <button type="button" class="gp-seg-btn" data-active={layout === "board"} aria-pressed={layout === "board"} onclick={() => interfaceStore.setTaskLayout("board")}><LayoutGrid size={12} /> Board</button>
          <button type="button" class="gp-seg-btn" data-active={layout === "list"} aria-pressed={layout === "list"} onclick={() => interfaceStore.setTaskLayout("list")}><List size={12} /> List</button>
        </div>
        {#if initialized}<AutomaticEnhancements {active} compact />{/if}
        {#if initialized}
          <button type="button" class="gp-icon-btn" aria-pressed={showInbox} aria-label="Inbox" title={unreadError ? `Notifications unavailable: ${unreadError}` : "Inbox"} onclick={() => { showInbox = !showInbox; }}>
            <Inbox size={13} />
            {#if !unreadError && unread > 0}<span class="gp-pill">{unread}</span>{/if}
          </button>
          <button type="button" class="gp-icon-btn" aria-pressed={showArchive} aria-controls="task-archive-dock" aria-label="Archive" title={`Archive — tasks in this scope that reached ${STATUS_LABELS[ARCHIVE_STATUS]}`} onclick={() => { showArchive = !showArchive; }}>
            <Archive size={13} />
            {#if completedCount > 0}<span class="gp-pill">{completedCount}</span>{/if}
          </button>
        {/if}
        <button type="button" class="gp-icon-btn" aria-label="Refresh" title="Refresh" onclick={() => { void boardAgents.refresh(); if (initialized) void refresh(); else void initialize(); }} disabled={loading}><RefreshCw size={13} /></button>
        <button type="button" class="gp-btn" aria-pressed={showFilters || filtering} onclick={() => { showFilters = !showFilters; }}>Filters</button>
        <TaskViewMenu disabled={!initialized} />
        <button type="button" class="gp-btn-primary" onclick={() => void createTask()} disabled={!creation.allowed} title={creation.blocked ?? creation.caveat ?? "New task"} aria-label="New task">New task</button>
        <!-- A disabled New task always says why, and a caveat says what the
             sheet will ask for. This used to render only for a profile with no
             repositories at all, so an empty workspace left the button dead
             and silent. -->
        {#if creation.blocked ?? creation.caveat}
          {#if creation.blocked && emptyAddLabel}
            <button type="button" class="hint-action" onclick={() => void addPaths(menuTabs.map((tab) => tab.path))} disabled={adding}>{emptyAddLabel}</button>
          {:else}
            <span class="hint" data-testid="task-create-hint">{creation.blocked ?? creation.caveat}</span>
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
    {#if creation.allowed}
      <div
        class="quick-add-row"
        class:dimmed={taskTabs.tabs.length > 0}
        data-testid="task-quick-add-row"
      >
        <TaskQuickAdd
          bind:this={quickAddEl}
          repositories={quickAddRepositories}
          busy={quickAdding}
          disabled={busy || taskTabs.tabs.length > 0}
          compact={compact}
          placeholder="Add a task — try: Fix retry loop !1 #ci @ada due:friday"
          mode={$interfaceStore.taskQuickAddAssist ? "assist" : "manual"}
          onMode={(next) => interfaceStore.setTaskQuickAddAssist(next === "assist")}
          onSubmit={(parsed, mode) => createFromQuickAdd(parsed, "inbox", mode)}
          onExpand={(parsed) => void expandQuickAdd(parsed)}
        />
      </div>
    {/if}
    {#if hiddenWork}
      <!-- Hiding a column is a layout choice. Hiding the work in it is not one
           this board makes on the reader's behalf, so the cost is named. -->
      <p class="hidden-note" role="status" data-testid="task-hidden-columns">
        <EyeOff size={11} />
        {hiddenWork.summary} hidden from this board.
        {#if offersArchive(hiddenWork.statuses)}
          <!-- Completed work is the one hidden column with somewhere else to
               be read, so it is the one that earns a second door here. -->
          <button type="button" class="link" onclick={() => { showArchive = true; }}>Open archive</button>
        {/if}
        <button type="button" class="link" onclick={() => interfaceStore.showAllTaskColumns()}>Show all columns</button>
      </p>
    {/if}
    {#if pendingDelete || undoOffer}
      <!-- The one place a board write can be taken back. A deletion waiting
           out its window has written nothing yet; anything else here was
           written and is undone by a revision-checked restore. -->
      <div class="undo-strip gp-glass" role="status" aria-label="Undo" data-testid="task-undo">
        {#if pendingDelete}
          <span>{deletesRunning > 0 && pendingDelete.running ? `Deleting ${plural(pendingDelete.cards.length, "task")}…` : `${describeTaskAction({kind:"delete"}, pendingDelete.cards.length)}. Nothing is written until ${DEFERRED_DELETE_MS / 1000} seconds pass.`}</span>
          <button type="button" class="gp-btn" aria-keyshortcuts="Meta+Z Control+Z" disabled={deletesRunning > 0 && pendingDelete.running} onclick={() => cancelPendingDelete()}><Undo2 size={12} /> Undo</button>
          <button type="button" class="gp-btn-danger" disabled={deletesRunning > 0 && pendingDelete.running} onclick={() => void commitPendingDelete()}><Trash2 size={12} /> Delete now</button>
        {:else if undoOffer}
          <span>{undoOffer.label}</span>
          <button type="button" class="gp-btn" aria-keyshortcuts="Meta+Z Control+Z" disabled={busy} onclick={() => void undoLast()}><Undo2 size={12} /> Undo</button>
          <button type="button" class="gp-icon-btn" aria-label="Dismiss undo" onclick={() => offerUndo(null)}><X size={12} /></button>
        {/if}
      </div>
    {/if}
    {#if selected.size > 0}
      <div class="selection gp-glass" role="status" aria-label="Selected task actions">
        <span>{selected.size} selected</span>
        {#if selected.size === 1}
          <button type="button" class="gp-btn" onclick={() => { const id = [...selected][0]; if (id) void openTask(id); }}><SquarePen size={12} /> Open</button>
          <button type="button" class="gp-btn" onclick={() => { const id = [...selected][0]; if (id) void openQuickEnhance(id); }}><Sparkles size={12} /> Quick Enhance</button>
          {#if repositories.length}
            <button type="button" class="gp-btn" disabled={busy} onclick={() => { const card = selectedCards[0]; if (card) handoff = { card, settings: $interfaceStore.taskHandoff }; }}><Bot size={12} /> Send to agent</button>
          {/if}
        {/if}
        <button type="button" class="gp-btn" onclick={() => void copyCardsForAgent(selectedCards)} disabled={busy}><Clipboard size={12} /> Copy for agent</button>
        <!-- Status, priority, labels, owner and due date for the whole
             selection: the card menu's own rows, opened from here so the
             bulk edits are on screen and not only behind a right click. -->
        <button
          type="button"
          class="gp-btn"
          data-testid="task-selection-change"
          aria-haspopup="menu"
          aria-expanded={menu !== null && menu.column === null && menu.cards.length > 0}
          disabled={busy}
          onclick={(e) => { const rect = e.currentTarget.getBoundingClientRect(); menu = { cards: selectedCards, column: null, x: rect.left, y: rect.bottom + 4 }; }}
        >Change…</button>
        <!-- The bulk half of the card menu's Archive row, disabled for the
             same reason and titled with where the work goes. Without it the
             only bulk end-of-life action on this bar was Delete. -->
        <button
          type="button"
          class="gp-btn"
          data-testid="task-archive-selected"
          onclick={() => void archiveCards(selectedCards)}
          disabled={busy || selectionArchived === "all"}
          title={selectionArchived === "all"
            ? `Already in ${STATUS_LABELS[ARCHIVE_STATUS]}`
            : `Archive — moves to ${STATUS_LABELS[ARCHIVE_STATUS]}`}
        ><Archive size={12} /> Archive</button>
        <button type="button" class="gp-btn-danger" onclick={() => void removeSelected()} disabled={busy}><Trash2 size={12} /> Delete</button>
        <button type="button" class="gp-btn" onclick={() => { selected = new Set(); selectionAnchor = null; }}>Clear</button>
      </div>
    {/if}
    {#if pendingUpdate && !moving}<div class="banner error" role="alert">Confirm the interrupted task update before making another change.<button class="gp-btn" onclick={() => { if(pendingUpdate) void applyUpdate(pendingUpdate); }}>Retry task update</button></div>{/if}
    {#if showInbox}<AttentionInbox {scope} {active} onopen={openTask} />{/if}
    {#if showArchive}
      <TaskArchive
        {scope}
        {active}
        {busy}
        {hiddenColumns}
        refreshToken={archiveToken}
        hiddenIds={pendingDelete?.ids}
        onopen={openTask}
        onaction={(cards, action) => void archiveDockAction(cards, action)}
        ontogglecolumn={() => interfaceStore.toggleTaskColumn(ARCHIVE_STATUS)}
      />
    {/if}
    {#if relinkPending}<div class="banner error" role="alert" data-testid="repository-relink-uncertain"><span>{relinkPending.message}</span><button type="button" class="gp-btn" disabled={relinking} onclick={() => void retryRelink()}>Retry relink</button></div>{/if}
    {#if catalogError}<div class="banner error" role="alert">{catalogError}<button type="button" class="gp-btn" onclick={() => initialized ? refresh() : initialize()}>Retry</button></div>{/if}
    {#if issueProgress}<div class="banner" data-testid="task-issue-progress"><span>Filing issue {issueProgress.index} of {issueProgress.total}: {issueProgress.title}</span>{#if issueProgress.total > 1}<button type="button" class="gp-btn" disabled={issueStop} onclick={() => { issueStop = true; }}>{issueStop ? "Stopping…" : "Stop"}</button>{/if}</div>{/if}
    {#if error}<div class="banner error" role="alert">{error}</div>{/if}
    <!-- Cards carry no agent marks while this stands: an old reading would
         claim agents nobody checked. -->
    {#if $boardAgents.error}<p class="agents-unread" role="status" data-testid="board-agents-error">Agents working on these tasks could not be read: {$boardAgents.error}</p>{/if}
    <!-- Its own banner, not part of `error`: a board reload clears `error`,
         and the issue would still exist with nothing on screen to link it. -->
    {#each relinks as entry (entry.taskId)}<div class="banner error" role="alert" data-testid="task-issue-relink"><span>Issue #{entry.number} exists but is not linked to “{entry.title}”.</span><button type="button" class="gp-btn" disabled={busy} onclick={() => void retryIssueLink(entry)}>Link to #{entry.number}</button></div>{/each}
    <div class="sr-only" role="status" aria-live="polite">{announce}</div>
    {#if !initialized && loading}
      <div class="pad"><Skeleton variant="card" count={4} /></div>
    {:else if initialized && error && loadedKey !== boardKey}
      <EmptyState icon={Inbox} title="Tasks unavailable" hint="Retry to load this scope." action={{label:"Retry loading tasks",onClick:()=>void loadBoard(),variant:"secondary"}} />
    {:else if initialized && !loading && total === 0 && !filtering}
      <!-- Gated on the same decision as the header button. Offering New task
           here where the header refuses it opened a sheet that could never be
           saved, because nothing had linked a repository. -->
      <EmptyState icon={Inbox} title="No tasks yet" hint={creation.blocked ?? creation.caveat ?? "Create a task in this scope. Cards stay on this board until you delete them."} action={creation.allowed ? { label: "New task", onClick: () => void createTask(), variant: "primary" } : undefined} />
    {:else if initialized && !loading && listCards.length === 0 && filtering}
      <EmptyState icon={Search} title="No tasks match" hint="Clear search or filters to see the rest of this board. Server search only covers the current pages." action={{ label: "Clear filters", onClick: () => { facet = emptyFacet(); search = ""; }, variant: "secondary" }} />
    {:else if layout === "list"}
      <div class="list" role="group" data-testid="task-columns" data-task-list aria-busy={loading || moving} aria-label="Task list">
        {#each listCards as card (card.id)}
          {@const face = cardFace(card, repoName)}
          {@const chrome = cardChrome(card, now)}
          {@const agents = agentsByTask.get(card.id)}
          <button
            type="button"
            class="row gp-card"
            class:selected={selected.has(card.id)}
            class:open={openCardIds.has(card.id)}
            data-testid="task-card"
            data-task-card
            data-card-id={card.id}
            data-open-task={openCardIds.has(card.id) || undefined}
            aria-haspopup="menu"
            aria-expanded={menu?.cards.some((item) => item.id === card.id) ?? false}
            aria-keyshortcuts="ArrowUp ArrowDown Home End Shift+ArrowUp Shift+ArrowDown Alt+ArrowUp Alt+ArrowDown ArrowLeft ArrowRight X Delete ContextMenu"
            aria-disabled={opening || undefined}
            onclick={(e) => onCardClick(e, card)}
            oncontextmenu={(e) => onCardContextMenu(e, card, card.status)}
            onkeydown={(e) => onCardKeydown(e, card, card.status)}
          >
            <span class="status">{STATUS_LABELS[card.status]}</span>
            <span class="row-title">{face.title}</span>{#if selected.has(card.id)}<span class="sr-only">, selected</span>{/if}
            {#if agents}<span class="agents-chip" data-testid="card-agents" data-tone={agents.tone ?? undefined} data-asking={agents.asking || undefined} title={agentsTitle(agents)}><Bot size={11} aria-hidden="true" />{agents.working}{$boardAgents.complete ? "" : "+"}<span class="sr-only"> {agents.working === 1 ? "agent" : "agents"} working</span>{#if agents.asking}<span class="asking"> · {agents.asking} {agents.asking === 1 ? "needs" : "need"} you</span>{/if}</span>{/if}
            {#if openCardIds.has(card.id)}<span class="open-mark">Open</span>{/if}
            {#if face.repo && cardFields.has("repo")}<span class="muted">{face.repo}{chrome.extraRepos ? ` +${chrome.extraRepos}` : ""}</span>{/if}
            {#if chrome.owner && cardFields.has("owner")}<span class="muted">{chrome.owner}</span>{/if}
            {#if dueLabel(chrome.due) && cardFields.has("due")}<span class="due" data-due={chrome.due}>{dueLabel(chrome.due)}</span>{/if}
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
                <button type="button" class="gp-icon-btn" aria-label={`New task in ${STATUS_LABELS[status]}`} title={creation.blocked ?? creation.caveat ?? `New task in ${STATUS_LABELS[status]}`} disabled={!creation.allowed} onclick={() => void createTask(status)}><Plus size={11} /></button>
              </span>
            </div>
            <div class="cards">
              {#each visibleIn(status) as card (card.id)}
                {@const face = cardFace(card, repoName)}
                {@const chrome = cardChrome(card, now)}
                {@const agents = agentsByTask.get(card.id)}
                {#if insertBefore(status, card.id)}<div class="insert" aria-hidden="true"></div>{/if}
                <button
                  type="button"
                  class="card bg-surface"
                  class:dragging={drag?.card.id === card.id}
                  class:selected={selected.has(card.id)}
                  class:open={openCardIds.has(card.id)}
                  data-testid="task-card"
                  data-task-card
                  data-card-id={card.id}
                  data-open-task={openCardIds.has(card.id) || undefined}
                  draggable="false"
                  aria-haspopup="menu"
                  aria-expanded={menu?.cards.some((item) => item.id === card.id) ?? false}
                  aria-keyshortcuts="ArrowUp ArrowDown Home End Shift+ArrowUp Shift+ArrowDown Alt+ArrowUp Alt+ArrowDown ArrowLeft ArrowRight X Delete ContextMenu"
                  aria-disabled={opening || undefined}
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
                    <h3>{face.title}{#if selected.has(card.id)}<span class="sr-only">, selected</span>{/if}</h3>
                    {#if openCardIds.has(card.id)}<span class="open-mark">Open</span>{/if}
                  </div>
                  {#if agents}<span class="agents-chip" data-testid="card-agents" data-tone={agents.tone ?? undefined} data-asking={agents.asking || undefined} title={agentsTitle(agents)}><Bot size={11} aria-hidden="true" />{agents.working}{$boardAgents.complete ? "" : "+"}<span class="sr-only"> {agents.working === 1 ? "agent" : "agents"} working</span>{#if agents.asking}<span class="asking"> · {agents.asking} {agents.asking === 1 ? "needs" : "need"} you</span>{/if}</span>{/if}
                  {#if face.repo && cardFields.has("repo")}<div class="card-repos">{face.repo}{chrome.extraRepos ? ` +${chrome.extraRepos}` : ""}</div>{/if}
                  {#if (chrome.kind && cardFields.has("type")) || (chrome.owner && cardFields.has("owner")) || (dueLabel(chrome.due) && cardFields.has("due"))}
                    <div class="card-extra">
                      {#if chrome.kind && cardFields.has("type")}<span class="muted">{chrome.kind}</span>{/if}
                      {#if chrome.owner && cardFields.has("owner")}<span class="muted">{chrome.owner}</span>{/if}
                      {#if dueLabel(chrome.due) && cardFields.has("due")}<span class="due" data-due={chrome.due}>{dueLabel(chrome.due)}</span>{/if}
                    </div>
                  {/if}
                  {#if face.labels.length && cardFields.has("labels")}<div class="labels">{#each face.labels as label}<span>{label}</span>{/each}{#if chrome.extraLabels}<span>+{chrome.extraLabels}</span>{/if}</div>{/if}
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
    <div class="ghost gp-glass bg-surface shadow-float" style="transform: translate({drag.x + 10}px, {drag.y + 10}px)" data-testid="task-drag-ghost">{drag.card.title}</div>
  {/if}
  {#if menu}
    <TaskContextMenu
      items={taskMenuItems({
        cards: menu.cards,
        column: menu.column,
        busy,
        vocabulary: { owners: facetOptions.owners, labels: facetOptions.labels },
        canHandoff: repositories.length > 0,
        canFileIssue: repositories.length > 0,
      })}
      x={menu.x}
      y={menu.y}
      label={menu.cards.length ? "Task actions" : "Column actions"}
      onAction={onMenuAction}
      onClose={(restore) => { menu = null; if (restore) { /* opener retains focus */ } }}
    />
  {/if}
  {#if enhanceId}
    <!-- Keyed on the task: the sheet loads once, on mount, so swapping the id
         under a live instance would leave it showing the previous task — and
         now also aim a drafting request at it. A different task is a different
         sheet. -->
    {#key enhanceId}
    <QuickEnhanceSheet
      taskId={enhanceId}
      startRequest={enhanceStart}
      {repoName}
      onClose={() => { enhanceId = null; if (!session && taskTabs.activeId) void restoreActiveTab(); else quickAddEl?.focus(); }}
      onApplied={(saved) => {
        void loadBoard();
        if (session?.value?.id === saved.id) {
          session = { ...session, value: saved, status: saved.status };
          taskTabs = updateTaskTab(taskTabs, saved.id, { title: saved.title, status: saved.status });
        }
      }}
      onOpenEditor={(task) => {
        enhanceId = null;
        if (refuseAtCeiling(task.id)) return;
        void canLeaveSession().then((ok) => {
          if (!ok) return;
          dropDraftSessionTab();
          openLoaded(task);
        });
      }}
    />
    {/key}
  {/if}
  {#if taskTabs.tabs.length}
    <div class="editor-dock">
      <div class="task-tabs">
        <div
          bind:this={tabStrip}
          class="task-tab-strip gp-header-scroll"
          role="tablist"
          aria-label="Open tasks"
          tabindex="-1"
          onkeydown={onTaskTabStripKeydown}
        >
          {#each taskTabs.tabs as tab (tab.id)}
            {@const isActive = tab.id === taskTabs.activeId}
            <div class="task-tab-shell" class:is-active={isActive} class:is-draft={tab.draft}>
              <div
                role="tab"
                data-task-tab={tab.id}
                tabindex={isActive ? 0 : -1}
                aria-selected={isActive}
                aria-controls={TASK_EDITOR_PANE_ID}
                title={tab.draft ? `${taskTabLabel(tab)} (draft)` : tab.title}
                onclick={() => void focusOpenTab(tab.id)}
                onkeydown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    void focusOpenTab(tab.id);
                  }
                  if (e.key === "Delete" || e.key === "Backspace") {
                    e.preventDefault();
                    void closeOpenTab(tab.id);
                  }
                }}
              >
                {#if !tab.draft}<span class="tab-pip" data-status={tab.status ?? "inbox"}></span>{/if}
                <span class="tab-title">{taskTabLabel(tab)}</span>
              </div>
              <button
                type="button"
                class="tab-close"
                tabindex="-1"
                data-testid="task-tab-close"
                aria-label={`Close ${taskTabLabel(tab)}`}
                title="Close tab"
                onclick={(e) => { e.stopPropagation(); void closeOpenTab(tab.id); }}
              ><X size={11} /></button>
            </div>
          {/each}
        </div>
        <ScrollCue target={tabStrip} axis="x" />
      </div>
      {#if session}
        {#key session.pane}
          <div id={TASK_EDITOR_PANE_ID} role="tabpanel" class="task-panel">
            <TaskEditor bind:this={editorHandle} active={active && !actionDialog} value={session.value} seed={session.seed ?? null} initialStatus={session.status ?? "inbox"} {repositories} {workspaces} openTabs={openTabRefs} primary={creation.seed.primaryRepositoryId} home={creation.seed.homeWorkspaceId} onSaved={onEditorSaved} onClose={onEditorClose} />
          </div>
        {/key}
      {/if}
    </div>
  {/if}
  {#if workspaceEditor}{#key workspaceEditor}<WorkspaceEditor bind:this={workspaceHandle} value={workspaceEditor.value} {repositories} openTabs={openTabRefs} onSaved={() => { scope = { kind: "global" }; void refresh(); }} onClose={() => { workspaceEditor = null; }} />{/key}{/if}
</div>

{#if actionDialog}<TaskActionDialog tasks={actionDialog.cards} action={actionDialog.action} batch={actionDialog.batch} onDefer={actionDialog.action.kind === "delete" && !actionDialog.batch ? deferDialogDeletion : undefined} onChanged={tasksChanged} onClose={() => { actionDialog = null; }} />{/if}
{#if handoff}
  <TaskHandoffSheet
    card={handoff.card}
    settings={handoff.settings}
    {repositories}
    openTabs={openTabRefs}
    onClose={() => { handoff = null; }}
    onLaunched={() => { handoff = null; void loadBoard(); }}
  />
{/if}

<style>
  .workbench{position:relative;display:flex;flex:1;min-height:0;min-width:0;color:rgb(var(--c-text));overflow:hidden}
  .workbench.is-dragging{cursor:grabbing;user-select:none}
  .navigator{width:188px;flex-shrink:0;border-right:1px solid rgb(var(--c-border) / 0.65);padding:10px 8px;overflow:auto}
  .navigator button{display:block;width:100%;text-align:left;border:0;padding:6px 8px;border-radius:7px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px;background:transparent}
  .navigator button:hover{background:rgb(var(--c-surface-hover) / 0.7)}
  .navigator button.selected{background:color-mix(in srgb,rgb(var(--c-accent)) 13%,transparent);color:rgb(var(--c-accent))}
  .navigator.gp-liquid-tabs{padding:10px 8px}
  .navigator.gp-liquid-tabs button.selected{background:transparent}
  .navigator .gp-seg-btn > span:not(:global(.gp-liquid-selection)){display:block;overflow:hidden;text-overflow:ellipsis}
  .nav-heading{position:relative;display:flex;align-items:center;justify-content:space-between;padding:10px 8px 4px;color:rgb(var(--c-text-muted));font-size:10px;font-weight:650;letter-spacing:.04em;text-transform:uppercase}
  .nav-heading > button,.icon,.nav-row>button:last-child{width:26px;height:26px;padding:0;flex-shrink:0;display:inline-flex;align-items:center;justify-content:center}
  /* Portaled: `fixed` + the popover owner set left/top. Width and height are
     viewport-capped so a long path or a long registered list cannot grow the
     panel off the window the way the old 260px-in-188px absolute menu did.
     `gp-menu`'s overflow:hidden is overridden so the list can scroll. */
  .add-menu{width:min(20rem,calc(100vw - 16px));max-width:calc(100vw - 16px);max-height:min(20rem,calc(100vh - 16px));overflow:hidden auto;box-sizing:border-box}
  .add-menu-label{padding:4px 8px;font-size:10px;color:rgb(var(--c-text-muted))}
  .add-menu button{width:100%;height:auto;min-width:0;padding:6px 8px;white-space:normal;overflow:visible;text-align:left}
  .add-item{display:flex;flex-direction:column;align-items:stretch;gap:1px;min-width:0}
  .add-name,.add-path{display:block;min-width:0;overflow-wrap:anywhere;word-break:break-word;white-space:normal}
  .add-path{font-size:10px;color:rgb(var(--c-text-muted))}
  .hint-action{border:0;background:transparent;padding:0;color:rgb(var(--c-accent));font-size:11px}
  .nav-row{display:flex;align-items:center}
  .nav-row .reorder{display:none;flex-shrink:0}
  .nav-row:hover .reorder,.nav-row:focus-within .reorder{display:inline-flex}
  .reorder>button{width:18px;height:26px;padding:0;display:inline-flex;align-items:center;justify-content:center}
  .heading-actions{display:inline-flex;gap:2px}
  .heading-actions>button{width:26px;height:26px;padding:0;display:inline-flex;align-items:center;justify-content:center}
  .nav-row>button:first-child{min-width:0;flex:1}
  .archive-toggle{display:flex;align-items:center;gap:6px;padding:6px 8px;font-size:11px;color:rgb(var(--c-text-muted))}
  .board-main{flex:1;min-width:0;display:flex;flex-direction:column;overflow:hidden}
  header{padding:10px 14px;display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap;border-bottom:1px solid rgb(var(--c-border) / 0.65)}
  h1{font-size:15px;line-height:1.2;font-weight:650;margin:0;display:flex;align-items:baseline;gap:8px}
  h1 span{font-size:11px;font-weight:500;color:rgb(var(--c-text-muted))}
  .actions,.facets,.selection,.undo-strip{display:flex;gap:6px;align-items:center;flex-wrap:wrap}
  .facets,.selection{padding:8px 14px}
  .selection,.undo-strip{margin:8px 14px 0;padding:8px 10px;border-radius:12px}
  .undo-strip>span{flex:1;min-width:0;font-size:12px}
  button,input,select{font-size:12px}
  button:disabled{opacity:.5}
  .hint{font-size:11px;color:rgb(var(--c-text-muted))}
  .search{display:flex;align-items:center;gap:6px;color:rgb(var(--c-text-muted))}
  .search input{width:160px}
  .columns{display:flex;gap:8px;padding:12px;overflow:auto;flex:1;min-height:0;align-items:stretch}
  .column{width:220px;min-width:196px;flex:1;display:flex;flex-direction:column;border-radius:12px;border:1px solid rgb(var(--c-border) / 0.65);overflow:hidden;min-height:0}
  .column.drop-target{border-color:rgb(var(--c-accent))}
  .column-title{display:flex;align-items:center;justify-content:space-between;font-weight:650;font-size:12px;border-bottom:1px solid rgb(var(--c-border) / 0.65);padding:8px 10px;}
  .column-meta{display:flex;align-items:center;gap:4px;color:rgb(var(--c-text-muted));font-weight:400}
  .cards{padding:6px;overflow:auto;flex:1;min-height:80px}
  .card{border:1px solid rgb(var(--c-border) / 0.65);border-radius:10px}
  .card,.row{width:100%;display:block;text-align:left;padding:8px 9px;margin-bottom:6px;cursor:grab;touch-action:none;user-select:none}
  .row{cursor:pointer;display:grid;grid-template-columns:7rem minmax(0,1fr) auto auto auto;gap:8px;align-items:center}
  .card:focus-visible,.row:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}
  .card.dragging{opacity:.35;cursor:grabbing}
  .card.selected,.row.selected,.card.open,.row.open{border-color:rgb(var(--c-accent));box-shadow:inset 0 0 0 1px rgb(var(--c-accent) / 0.45)}
  .heading{display:flex;align-items:baseline;gap:8px;min-width:0;flex-wrap:wrap}
  .open-mark{font-size:9px;font-weight:650;letter-spacing:.04em;text-transform:uppercase;color:rgb(var(--c-accent));flex-shrink:0;margin-top:2px}
  /* Agents working on the card's task; amber when one needs the reader, as in the task's Agents pane. */
  .agents-chip{display:inline-flex;align-items:center;gap:3px;align-self:flex-start;flex-shrink:0;font-size:10px;font-weight:600;line-height:1;padding:2px 5px;border-radius:999px;color:rgb(var(--c-text-muted));background:rgb(var(--c-text-muted) / .12);font-variant-numeric:tabular-nums;white-space:nowrap}
  .agents-chip[data-asking]{color:#d29922;background:rgb(210 153 34 / .16)}
  .agents-chip[data-tone="error"]{color:#dc6565;background:rgb(220 101 101 / .16)}
  .card .agents-chip{margin-top:4px}
  .editor-dock{display:flex;flex-direction:column;flex-shrink:0;min-width:0;min-height:0;align-self:stretch}
  /* Same token the sheet below uses (app.css): the strip and the sheet are
     one column and must not be able to disagree about its width. */
  .task-tabs{position:relative;width:var(--gp-task-sheet-w);flex-shrink:0;border-left:1px solid rgb(var(--c-border) / 0.65);border-bottom:1px solid rgb(var(--c-border) / 0.65)}
  .task-tab-strip{display:flex;align-items:stretch;gap:4px;min-width:0;overflow-x:auto;padding:6px 8px}
  .task-tab-shell{display:flex;align-items:center;gap:2px;flex-shrink:0;max-width:190px;border-radius:8px;border:1px solid transparent;padding:0 2px 0 8px}
  .task-tab-shell.is-active{border-color:rgb(var(--c-accent) / 0.45);background:color-mix(in srgb,rgb(var(--c-accent)) 12%,transparent)}
  .task-tab-shell.is-draft [role="tab"]{font-style:italic}
  .task-tab-shell [role="tab"]{display:flex;align-items:center;gap:6px;min-width:0;flex:1;padding:5px 0;cursor:pointer;font-size:11px}
  .tab-title{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;min-width:0}
  .tab-pip{width:6px;height:6px;border-radius:99px;flex-shrink:0;background:rgb(var(--c-accent))}
  .tab-close{width:18px;height:18px;padding:0;border:0;background:transparent;color:rgb(var(--c-text-muted));display:inline-flex;align-items:center;justify-content:center;border-radius:5px;flex-shrink:0}
  .tab-close:hover{color:#e11d48;background:var(--mac-fill-surface-hover, rgb(var(--c-surface-hover)))}
  .task-panel{flex:1;min-height:0;display:flex}
  .task-panel :global(.task-editor){flex:1}
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
  .quick-add-row{padding:8px 14px 0}
  .quick-add-row.dimmed{opacity:.45;pointer-events:none;transition:opacity .15s ease}
  .hidden-note{display:flex;align-items:center;gap:6px;flex-wrap:wrap;margin:0;padding:6px 14px;font-size:11px;color:rgb(var(--c-text-muted))}
  .link{border:0;background:transparent;padding:0;font-size:11px;color:rgb(var(--c-accent))}
  .link:hover{text-decoration:underline}
  /* Compact trades the card's breathing room for roughly a third more cards
     on screen. Only padding and gaps change: nothing is dropped, so the same
     card reads the same way at either density. */
  .is-compact .card,.is-compact .row{padding:5px 7px;margin-bottom:4px}
  .is-compact .card-repos,.is-compact .card-extra{margin-top:2px}
  .is-compact .labels{margin-top:3px}
  .is-compact .columns{gap:6px;padding:8px}
  .is-compact .cards{padding:4px}
</style>
