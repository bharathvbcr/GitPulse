<script lang="ts">
  import { createFrameScheduler } from "../motion/frameScheduler";
  import { crossfade, fly } from "svelte/transition";
  import { backdropFade, backdropFadeOut, liquidSelection } from "../ui/transitions";
  import { createResizeObservation } from "../dom/observeResize";
  import { onMount, tick, untrack } from "svelte";
  import { invoke } from "../ipc/invoke";
  import { AlertTriangle, Archive, Bot, CircleAlert, ChevronDown, ChevronRight, ChevronUp, Clipboard, CornerDownRight, Ellipsis, Eye, EyeOff, FolderSync, Import, Inbox, LayoutGrid, List, Merge, PanelLeft, Plus, RefreshCw, Search, Sparkles, SquarePen, Trash2, Undo2, X } from "@lucide/svelte";
  import { isMacOS } from "../platform";
  import { createListenerTracker } from "../dom/listenerTracker";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { copyText } from "../desktop/clipboard";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { popover, restoreFocusTo, popoverExit } from "../ui/popover";
  import { MAX_COLUMN_PAGES, readOn, readToEnd, reloadPaged, reloadSize, samePage, sameRecords } from "../workbench/boardPaging";
  import { createLiveLoader } from "../workbench/liveLoader";
  import { cardFace, dragExceeded, grabPoint, type GrabPoint, insertIndexFromY, insertionNeighbors, insertionPosition, neighborStatus, parseColumnStatus, shouldCommitMove } from "../workbench/boardDrag";
  import {
    explainError, getTask, getTaskBrief, getWorkspace, listAttention, listRepositories, listTasks, listWorkspaces, newID, organizeTasks, putTask, registerRepository, restoreDeletedTask, suggestTasks, WorkbenchError,
    NAVIGATOR_PAGE_SIZE, STATUSES, subscribeWorkbenchChanges, STATUS_LABELS, TASK_PAGE_SIZE, taskDraft, taskWrite,
    type Page, type Repository, type Scope, type Task, type TaskCard, type TaskDraft, type TaskListFilter, type TaskOrganization, type TaskStatus, type TaskSuggestions, type Workspace, type WorkspaceCard,
  } from "../workbench/client";
  import { addableOpenTabs, attachRepositories, openAddActionLabel, openMembershipCandidates, pickerSelectionIds } from "../workbench/openMembership";
  import { quickAddRefusal, taskCreation } from "../workbench/taskCreation";
  import { checkoutFlag, defaultRelinkIO, readCheckouts, relinkCheckout, type CheckoutHealth, type RelinkOutcome } from "../workbench/repositoryRelink";
  import { resolveGitRoot } from "../desktop/nativeShell";
  import { applyMove, moveWrites, navigatorOrder } from "../workbench/workspaceOrder";
  import { importSummary, importTabGroups, tabGroups } from "../workbench/workspaceImport";
  import { workspaceMembershipLabel } from "../workbench/taskRepositories";
  import { hiddenByRepository, hiddenRepositoryNote, hiddenRepositoryPhrase, hidesRepositories } from "../workbench/taskHiddenRepositories";
  import { cardsById, contextMenuAnchor, duplicateTitle, flattenVisibleIds, isContextMenuKey, rangeSelect, taskMenuItems, toggleSelection, type TaskMenuItem } from "../workbench/taskMenu";
  import { checkoutCandidates, handoffFromTarget, type HandoffSettings } from "../workbench/taskHandoff";
  import { linkTaskToIssue, MAX_TASK_ISSUE_BATCH, prepareTaskIssues, runTaskIssues, summarizeTaskIssues, taskIssuesConfirmation, type TaskIssueResult, type TaskIssueTarget } from "../workbench/taskIssue";
  import { askConfirm } from "../stores/modalStore";
  import { openExternal } from "../desktop/openExternal";
  import { cardMatchesQuery, dueRangeFilter, parseTaskQuery, queryChips, queryControlValue, queryHas, removeQueryTerm, repositoryMatches, setQueryControl, toggleQueryTerm, QUERY_HELP, type QueryContext, type SuggestionSources } from "../workbench/taskQuery";
  import { describeMatch, searchCards, type RankContext, type RankedCard, type SearchOrder } from "../workbench/taskRank";
  import { CANDIDATE_PAGE, MAX_CANDIDATES, bucketByStatus, canLoadMore, candidatesComplete, describeSearch, nextCandidates, planSearch, readAllCandidates, reloadCandidates, type CandidateRead, type CandidateSet, type SearchPlan } from "../workbench/taskSearchPlan";
  import TaskSearchField from "./TaskSearchField.svelte";
  import { applyOrganizedPriority, cardChrome, cardMatchesFacet, collectFacetOptions, emptyFacet, facetActive, allLoadedCards, priorityLabel, priorityOrderNote, reorderPlan, partitionLanes, listRows, sortByPriority, type TaskFacet, type TaskLane } from "../workbench/taskOrganize";
  import { plural } from "../format";
  import { archiveAction, archivable, archiveControl, archiveRefusal, archiveSkipped, archiveState } from "../workbench/taskArchive";
  import { interfaceStore } from "../stores/interfaceStore";
  import { hiddenColumnReport, visibleBoardStatuses } from "../ui/taskView";
  import { boardKey as viewBoardKey, boardPrefs, effectiveSwimlane, wipState, type SavedTaskView, type TaskViewSnapshot } from "../ui/taskBoardViews";
  import TaskSavedViews from "./TaskSavedViews.svelte";
  import TaskSuggestPanel from "./TaskSuggestPanel.svelte";
  import TaskOrganizePanel from "./TaskOrganizePanel.svelte";
  import { createSuggestWatch, suggestSignature } from "../workbench/suggestFreshness";
  import WavePanel from "./WavePanel.svelte";
  import { consumeTaskOpen, taskOpenRequest } from "../workbench/taskOpen";
  import { parseQuickAddDue, quickAddDraft, type QuickAddMode, type QuickAddResult } from "../workbench/taskQuickAdd";
  import { removeFromColumns, restoreSummary, restoreTasks } from "../workbench/taskDelete";
  import { joinAgentCopies, MAX_AGENT_COPY_TASKS, wrapSavedBriefForAgent } from "../workbench/taskCompose";
  import { createBoardAgents } from "../workbench/boardAgents";
  import { taskAgentSummaries, type TaskAgentSummary } from "../workbench/taskSessions";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { attemptNotices, taskTerminalRequests } from "../terminal/taskLaunches";
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
  import TaskMergeDialog from "./TaskMergeDialog.svelte";
  import CardVerdict from "./CardVerdict.svelte";
  import { getBoardVerdicts, sameVerdicts, type CardVerdict as Verdict } from "../workbench/verification";
  import { childProgressLabel, getBoardLineage, sameLineage, withLineage, type CardLineage } from "../workbench/lineage";
  import { mergeEligibility } from "../workbench/taskMerge";
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
  import Checkbox from "./Checkbox.svelte";
  import { describeFailure, unknownState } from "../ui/copy/describeFailure";

  let { repositoryPath = null, active = true, acceptsOpenRequests = false }: {
    repositoryPath?: string | null;
    active?: boolean;
    /** Only the global board opens a task asked for from elsewhere (`taskOpen.ts`). */
    acceptsOpenRequests?: boolean;
  } = $props();
  const macos = isMacOS();
  /** "Couldn't <verb> <object>. <Reason>." for an error line; the reason is the store's own wording. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, explainError(cause));
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }
  const [sendScope, receiveScope] = crossfade(liquidSelection());
  let scope = $state<Scope>({ kind: "global" });
  let repositories = $state<Repository[]>([]), workspaces = $state<WorkspaceCard[]>([]);
  let repositoryCursor = $state<string | null>(null), workspaceCursor = $state<string | null>(null);
  let repositoryTotal = $state(0), workspaceTotal = $state(0);
  let columns = $state<Partial<Record<TaskStatus, Page<TaskCard>>>>({});
  /** Verification verdicts for the cards loaded, read once per board load. */
  let verdicts = $state<ReadonlyMap<string, Verdict>>(new Map());
  let verdictError = $state("");
  /** Each loaded card's parent and subtask progress, read with the verdicts. A card absent here is unknown, not childless. */
  let lineage = $state<ReadonlyMap<string, CardLineage>>(new Map());
  let lineageError = $state("");
  let search = $state(""); let initialized = $state(false); let loading = $state(false);
  let error = $state(""); let catalogError = $state(""); let liveNotice = $state(""); let announce = $state("");
  let taskTabs = $state<TaskTabState>(emptyTaskTabs());
  let session = $state<{ tabId: string; pane: string; value: Task | null; status?: TaskStatus; seed?: Partial<TaskDraft> } | null>(null);
  let workspaceEditor = $state<{ value: Workspace | null } | null>(null);
  let editorHandle = $state<{ canLeave: () => Promise<boolean> }>();
  let enhanceHandle = $state<{ canLeave: () => Promise<boolean> }>();
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
  /** The scope the loaded columns were read for, whatever the search was. */
  let loadedScope = $state("");
  let unreadError = $state("");
  let unreadRevision = 0, initializationRevision = 0, openingRevision = 0, catalogRevision = 0;
  // ---- Search (`taskQuery.ts`, `taskRank.ts`, `taskSearchPlan.ts`) --------
  /**
   * The search box read once, by the parser the archive and the navigator
   * share: filters, words for ranking, and problems to report. Filters never
   * reach the store, so typing `is:bug` does not search for "is" and "bug".
   */
  const parsedQuery = $derived(parseTaskQuery(search));
  /**
   * The Filters bar's status select and due-date range read and write the search
   * (`status:review`, `due:FROM..TO`), so a saved view keeps them with it. null:
   * the search holds more than a control can show, and it keeps the say.
   */
  const statusControl = $derived(queryControlValue(parsedQuery, "status"));
  const rangeControl = $derived(queryControlValue(parsedQuery, "due-range"));
  const rangeEnds = $derived<[string, string]>(rangeControl ? (rangeControl.split("..") as [string, string]) : ["", ""]);
  const setDueRange = (from: string, to: string) => { search = setQueryControl(search, "due-range", dueRangeFilter(from, to)); };
  /** The Filters bar's choices: applied with the search's filters by `cardVisible`, kept by saved views. */
  let facet = $state<TaskFacet>(emptyFacet());
  // ---- Hidden repositories (`taskHiddenRepositories.ts`) ------------------
  // A reader preference: the global and workspace boards leave out the tasks
  // whose primary repository is hidden, and say how many they left out.
  const hiddenRepositories = $derived($interfaceStore.taskHiddenRepositories);
  const hiddenRepositorySet = $derived(new Set(hiddenRepositories));
  const hidingRepositories = $derived(hidesRepositories(scope, hiddenRepositories));
  /** Whether the navigator's Hidden group is open. */
  let showHiddenRepositories = $state(false);
  /** Hidden ids the loaded catalog has no row for: a later page, or a repository no longer registered. */
  const hiddenUnloaded = $derived(hiddenRepositories.filter((id) => !repositoryById.has(id)).length);
  /**
   * A search or a Filters choice switches the board from per-column pages to
   * one candidate set for the scope, ranked and filtered here. The fetch key
   * changes only with what the store is asked, so typing re-ranks and
   * re-filters what is loaded without a request. Hiding a repository does the
   * same: a column page would count tasks the board cannot show.
   */
  const plan = $derived(planSearch(scope, parsedQuery, facet, repositories, repositoryCursor === null, hidingRepositories));
  /** A repository the reader narrowed the board to; its tasks show even while it is hidden. */
  const narrowedTo = $derived(plan.scope.kind === "repository" ? plan.scope.id : facet.repository !== "all" ? facet.repository : null);
  function hiddenHere(card: TaskCard): boolean {
    return hidingRepositories && hiddenByRepository(card, hiddenRepositorySet, narrowedTo);
  }
  const searching = $derived(plan.mode === "search");
  let sort = $state<SearchOrder>("relevance");
  /** How a column is drawn when no search ranks it: the most urgent first (the default), or the board's own order. A reader preference; positions are never written. */
  const boardSort = $derived($interfaceStore.taskOrder);
  /** Manvi's ranking of what to start next (`work.tasks.suggest`), asked for on demand. */
  let showSuggest = $state(false);
  let suggestion = $state<TaskSuggestions | null>(null);
  let suggestLoading = $state(false);
  let suggestError = $state("");
  let suggestPriority = $state<number | "all">("all");
  let suggestTicket = 0;
  /** Manvi's reviewable priority proposal (`work.tasks.organize`). Asked for on demand; nothing is written until a row is accepted. */
  let showOrganize = $state(false);
  let organization = $state<TaskOrganization | null>(null);
  let organizeLoading = $state(false);
  let organizeApplying = $state(false);
  let organizeError = $state("");
  let organizeOutcomes = $state<Record<string, string>>({});
  let organizeTicket = 0;
  let organizeEpoch = $state(0);
  /** The search's candidates; null while the board browses columns. */
  let candidates = $state.raw<CandidateSet | null>(null);
  /** What a Load all is walking — the search's candidates or one column — and the flag Stop sets for it to check between pages. */
  let loadingAll = $state<"search" | TaskStatus | null>(null); let stopLoadingAll = false;
  const boardKey = $derived(JSON.stringify([scope, plan.key]));
  /**
   * A new search on the same scope while its candidates load, or a cleared
   * one while its columns load: the cards already loaded are real tasks of
   * this scope, so the board filters and ranks them at once instead of going
   * blank. Another scope's cards are never shown, and nothing pages, counts
   * or reports until the load lands.
   */
  const provisional = $derived(loadedKey !== "" && loadedKey !== boardKey && loadedScope === JSON.stringify(scope) && (searching || candidates !== null));
  // A deletion waiting out its undo window is already gone from the board:
  // its cards, its counts and anything a selection could act on.
  const displayColumns = $derived(loadedKey === boardKey || provisional ? (pendingDelete ? removeFromColumns(columns, pendingDelete.ids) : columns) : {});
  // `JSON.stringify(facet)`, not `facet`: the Filters selects write a field in
  // place, which reading the object alone does not track, so a filter change
  // used to leave a selection it had hidden — for a bulk Delete to act on.
  $effect(() => { boardKey; JSON.stringify(facet); search; selected = new Set(); selectionAnchor = null; menu = null; });
  // An undo offer belongs to the board it was made on. A pending deletion is
  // not withdrawn by a scope change: it is still the reader's to undo.
  $effect(() => { JSON.stringify(scope); untrack(() => offerUndo(null)); });
  // A suggestion ranks one board's tasks: another scope drops it, and an open panel asks again.
  $effect(() => { JSON.stringify(scope); untrack(() => { suggestWatch.reset(); suggestTicket++; suggestion = null; suggestError = ""; suggestLoading = false; if (showSuggest) void askSuggest(); }); });
  // A task whose priority or status changed, or that came or went, is ranked
  // from its old fields until Manvi is asked again: one debounced re-ask per
  // burst of changes while the panel is open. A closed panel asks on reopening.
  const suggestWatch = createSuggestWatch(() => { if (showSuggest && !disposed) void askSuggest(); }, 400);
  const suggestSig = $derived(suggestSignature(columns));
  $effect(() => { const key = loadedKey === boardKey ? boardKey : null, signature = suggestSig; untrack(() => suggestWatch.observe(key, signature)); });
  let opening = $state(false); let moving = $state(false); let deleting = $state(false);
  /**
   * A held card, before and during its drag. Owned by one pointer: every
   * later event for the press is matched on `pointerId`, so a second finger
   * or pen can neither move nor end it. `target` holds the capture.
   */
  let press = $state<{ card: TaskCard; pointerId: number; x: number; y: number; grab: GrabPoint; target: HTMLElement } | null>(null);
  let drag = $state<{ card: TaskCard; over: TaskStatus | null; insertIndex: number; lane: string; local: number; x: number; y: number } | null>(null);
  /**
   * Set while a drop or a context-menu press is still being dispatched, so
   * the click that same gesture produces does not also open the card. The
   * browser dispatches that click in the same task as the release; a timer
   * clears the guard in the next one. Not the next animation frame: a hidden
   * window runs no frames, so the guard stayed up and every card click after
   * a drop was swallowed until the window showed again.
   */
  let skipClick = false;
  let skipClickTimer: ReturnType<typeof setTimeout> | undefined;
  function swallowGestureClick() {
    skipClick = true;
    clearTimeout(skipClickTimer);
    skipClickTimer = setTimeout(() => { skipClick = false; }, 0);
  }
  let showInbox = $state(false);
  let showArchive = $state(false);
  let archiveToken = $state(0);
  /** What every board column reads: live tasks that are not archived. */
  const BOARD: TaskListFilter = { archived: false };
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
  let showFilters = $state(false);
  let showSearchHelp = $state(false);
  /** One-click filters; each is a search filter, so a saved view keeps it and a chip removes it. */
  const QUICK_FILTERS = [
    { label: "Bugs", filter: "is:bug" },
    { label: "Features", filter: "is:feature" },
    { label: "Urgent", filter: "priority:urgent" },
    { label: "High", filter: "priority:high" },
    { label: "Overdue", filter: "due:overdue" },
    { label: "Unassigned", filter: "owner:none" },
    { label: "Open", filter: "status:open" },
  ] as const;
  let quickAdding = $state(false);
  let quickAddEl = $state<{ focus: () => void }>();
  let handoff = $state<{ card: TaskCard; settings: HandoffSettings } | null>(null);
  let now = $state(Math.floor(Date.now() / 1000));
  let boardEl: HTMLDivElement | undefined = $state();
  let revision = 0; let disposed = false;
  /** Bumped by each read the reader asks for, so a background read begun before it is not applied over it. */
  let interactions = 0;
  /** The banner's error came from a background read, so the next one that succeeds may clear it. */
  let refreshError = false;
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
  /**
   * The scope navigator is a rail beside the board only while the board is wide
   * enough for it AND the reader has not collapsed it. The rail is 188px and a
   * column is at least 196px, so below this width the board kept a column and a
   * half behind a horizontal scroller, with the third column cut off at the
   * window's edge. Collapsed or narrow, the same navigator opens from the
   * toolbar as an overlay over the board instead.
   */
  const RAIL_MIN_BOARD_PX = 860;
  /** 0 until the board has been measured; a hidden pane measures 0 and keeps the last real width. */
  let boardWidth = $state(0);
  const railNarrow = $derived(boardWidth > 0 && boardWidth < RAIL_MIN_BOARD_PX);
  const railPinnedAway = $derived($interfaceStore.taskNavigatorCollapsed);
  const railDocked = $derived(!railNarrow && !railPinnedAway);
  let railOpen = $state(false);
  /** Where the overlay starts: under the toolbar, measured when it opens. */
  let railTop = $state(0);
  let railToggleEl: HTMLButtonElement | undefined = $state();
  let railEl: HTMLElement | undefined = $state();
  $effect(() => {
    const el = boardEl;
    if (!el) return;
    const read = () => { if (el.clientWidth > 0) boardWidth = el.clientWidth; };
    read();
    const observation = createResizeObservation(read);
    observation.observe(el);
    return () => observation.disconnect();
  });
  $effect(() => { if (railDocked) railOpen = false; });
  function closeRail(options?: { restoreFocus?: boolean }) {
    if (!railOpen) return;
    railOpen = false;
    if (options?.restoreFocus) restoreFocusTo(railToggleEl);
  }
  function toggleRail() {
    if (railDocked) { interfaceStore.setTaskNavigatorCollapsed(true); return; }
    if (railOpen) { closeRail({ restoreFocus: true }); return; }
    // Below the toolbar, so the toggle that opened it stays in reach to close it.
    const header = boardEl?.querySelector("header");
    railTop = header && boardEl ? Math.max(0, Math.round(header.getBoundingClientRect().bottom - boardEl.getBoundingClientRect().top)) : 0;
    railOpen = true;
    void tick().then(() => (railEl?.querySelector<HTMLElement>('.nav-scope[aria-pressed="true"]') ?? railEl?.querySelector<HTMLElement>(".nav-scope"))?.focus());
  }
  /** Back beside the board, for a reader who collapsed the rail on a wide window. */
  function dockRail() {
    interfaceStore.setTaskNavigatorCollapsed(false);
    railOpen = false;
    restoreFocusTo(railToggleEl);
  }
  /** Picking a scope from the overlay is the end of that visit: the board is what they came for. */
  function chooseScope(next: Scope) {
    scope = next;
    closeRail({ restoreFocus: true });
  }
  const railDismissal = $derived({
    dismiss: {
      inside: "[data-task-rail], [data-nav-toggle], [data-add-repo-popup]",
      pointer: "pointerdown" as const,
      escape: "bubble" as const,
    },
    onDismiss: (reason: string) => {
      // Escape belongs to the add-repository menu while it is open; it closes
      // itself, and the rail stays for the next one.
      if (reason === "escape" && addMenu) return;
      closeRail({ restoreFocus: reason === "escape" });
    },
  });
  /**
   * Up and Down move between scope rows, Home and End jump to the ends. Focus
   * only: Enter or Space picks, as on any list of buttons. Alt+Up and Alt+Down
   * are the workspace reorder (handled on the row, which has already prevented
   * default by the time this runs) and are never claimed here.
   */
  function onRailKeydown(event: KeyboardEvent) {
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const keys = ["ArrowDown", "ArrowUp", "Home", "End"];
    if (!keys.includes(event.key) || !railEl) return;
    const target = event.target;
    if (!(target instanceof HTMLElement)) return;
    const rows = [...railEl.querySelectorAll<HTMLElement>(".nav-scope")];
    if (rows.length === 0) return;
    const fromSearch = target.matches('[data-testid="task-nav-search"]');
    const index = rows.indexOf(target);
    if (index < 0 && !(fromSearch && event.key === "ArrowDown")) return;
    event.preventDefault();
    const next = event.key === "Home" ? 0
      : event.key === "End" ? rows.length - 1
      : event.key === "ArrowDown" ? Math.min(rows.length - 1, index + 1)
      : Math.max(0, index - 1);
    rows[next]?.focus();
  }
  const loadedCards = $derived(allLoadedCards(displayColumns));
  /** Titles of the cards loaded, so a subtask can name its parent when the parent is on the board. */
  const loadedTitles = $derived(new Map(loadedCards.map((card) => [card.id, card.title])));
  const facetOptions = $derived(collectFacetOptions(loadedCards));
  /**
   * The repository filter's choices, by name; an id the catalog has not
   * loaded shows as itself. The chosen one stays listed even when no loaded
   * card links it, so the select never shows a blank for an active filter.
   */
  const facetRepositories = $derived([...new Set([...facetOptions.repositories, ...(facet.repository === "all" ? [] : [facet.repository])])]
    .map((id) => ({ id, name: repositoryById.get(id)?.name ?? id }))
    .sort((a, b) => a.name.localeCompare(b.name)));
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
  // ---- Navigator search -------------------------------------------------
  // Narrows the workspace and repository rows by name (and a repository's
  // identity, its path or remote). The selected scope stays listed, so the
  // reader never loses sight of which board is open. Only loaded rows are
  // searched; while more pages exist the navigator says so beside "More".
  let navFilter = $state("");
  const navNeedle = $derived(navFilter.trim().toLowerCase());
  const navFiltering = $derived(navNeedle.length > 0);
  function navShowsWorkspace(group: WorkspaceCard): boolean {
    return !navFiltering || (scope.kind === "workspace" && scope.id === group.id) || group.name.toLowerCase().includes(navNeedle);
  }
  function navShowsRepository(repo: Repository): boolean {
    return !navFiltering || (scope.kind === "repository" && scope.id === repo.id) || repositoryMatches(repo, navNeedle);
  }
  const navWorkspaceMatches = $derived(orderedWorkspaces.filter(navShowsWorkspace).length);
  const navRepositoryMatches = $derived(repositories.filter(navShowsRepository).length);
  /** Named groups on the repository tab strip that could become workspaces. */
  const importableGroups = $derived(tabGroups($repoStore.openTabs, $repoStore.groupColors));
  let movingGroup = $state(false);
  let importingGroups = $state(false);
  const columnTotals = $derived(Object.fromEntries(STATUSES.map((status) => [status, columnCount(status)])) as Partial<Record<TaskStatus, number>>);
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
  /** Whether the selection can be merged, and in which repository. */
  const selectionMerge = $derived(mergeEligibility(selectedCards));
  /** The merge dialog's cards and repository while it is open. */
  let mergeDialog = $state<{ cards: TaskCard[]; repositoryId: string } | null>(null);
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
  const busy = $derived(moving || opening || deleting || deletesRunning > 0 || filingIssue || actionDialog !== null || mergeDialog !== null || pendingUpdate !== null);
  /** The selection bar's Archive button: disabled with its reason when nothing would change. */
  const archiveBar = $derived(archiveControl(selectionArchived, busy));
  const openCardIds = $derived(openSavedTaskIds(taskTabs));
  /**
   * The scope's in-progress total. A search's candidates only know it when
   * they are every task of the board's own scope; otherwise the last
   * browsing total stands rather than a count of whatever loaded.
   */
  let inProgressCount = $state(0);
  /** The server's count of archived tasks in this scope, so the badge is not a page size. */
  let archivedCount = $state(0);
  $effect(() => {
    if (repositoryPath) return;
    publishTaskChrome({ openTabs: taskTabs.tabs.length, inProgress: inProgressCount });
  });
  const shown = $derived.by(() => {
    const counts = Object.fromEntries(STATUSES.map((status) => [status, visibleIn(status).length])) as Partial<Record<TaskStatus, number>>;
    return visibleBoardStatuses(hiddenColumns, counts, drag !== null);
  });
  /** The list's rows: a relevance ranking as ranked, otherwise across every status in the reader's order. */
  const listCards = $derived(listRows(shown.map((status) => visibleIn(status)), listOrder()));
  /** What is on screen per column, in the order it is drawn, for range and select-all. */
  const drawnColumns = $derived(Object.fromEntries(STATUSES.map((status) => [status, { items: visibleIn(status) }])) as Partial<Record<TaskStatus, { items: TaskCard[] }>>);
  // ---- Saved views, lanes and limits, per board (`taskBoardViews.ts`) -----
  const viewBoard = $derived(viewBoardKey(scope));
  const wipLimits = $derived(boardPrefs($interfaceStore.taskBoards, viewBoard).wip);
  const swimlane = $derived(effectiveSwimlane(layout, $interfaceStore.taskSwimlane));
  /** Lanes over the cards on screen, or null when the board draws none. */
  const laneList = $derived(swimlane === "none" ? null : partitionLanes(listCards, swimlane, (status) => STATUS_LABELS[status], STATUSES));
  /** One grid for the head row and every lane, so a column lines up down the board. */
  const laneGrid = $derived(`grid-template-columns: repeat(${shown.length}, minmax(196px, 1fr))`);
  const viewSnapshot = $derived<TaskViewSnapshot>({
    layout,
    density: $interfaceStore.taskDensity,
    swimlane: $interfaceStore.taskSwimlane,
    order: boardSort,
    hiddenColumns: [...hiddenColumns],
    cardFields: [...$interfaceStore.taskCardFields],
    facet: { ...facet },
    search,
  });
  /** Against the store's total for the column, never its loaded page; a search's counts are matches, not the column's load. */
  function wipOf(status: TaskStatus) {
    return loadedKey === boardKey && !searching ? wipState(displayColumns[status]?.total, wipLimits[status]) : null;
  }
  function laneAria(group: TaskLane): string {
    const by = swimlane === "label" ? "first label" : swimlane;
    return `${group.label} — ${plural(group.cards.length, "task")}, grouped by ${by}`;
  }
  /** Put a saved view back: the layout preferences, then this board's filters. */
  function applyView(view: SavedTaskView) {
    interfaceStore.applyTaskView(view);
    facet = { ...view.facet };
    search = view.search;
    if (facetActive(view.facet)) showFilters = true;
    announce = `Showing the saved view ${view.name}`;
  }
  function repoName(id: string) { return repositories.find((repo) => repo.id === id)?.name; }
  const repositoryById = $derived(new Map(repositories.map((repo) => [repo.id, repo])));
  /** Names of hidden repositories the catalog has loaded, in the stored order. */
  const hiddenRepositoryLabels = $derived(hiddenRepositories.flatMap((id) => {
    const name = repositoryById.get(id)?.name?.trim();
    return name ? [name] : [];
  }));
  /** Collapsed heading. Names the repositories; a count only when none of them are loaded. */
  const hiddenGroupLabel = $derived(hiddenRepositoryLabels.length > 0
    ? `Hidden · ${hiddenRepositoryPhrase(hiddenRepositoryLabels, hiddenRepositories.length)}`
    : `Hidden (${hiddenRepositories.length})`);
  const queryContext = $derived<QueryContext>({ repository: (id) => repositoryById.get(id), nowSec: now });
  /** The Filters bar and the search box's filters: what the words are ranked over. */
  function filtersHold(card: TaskCard): boolean {
    return !hiddenHere(card) && cardMatchesFacet(card, facet, now) && cardMatchesQuery(card, parsedQuery, queryContext);
  }
  /** Relevance reorders only a search with words; filters alone keep board order. */
  const order = $derived<SearchOrder>(searching && parsedQuery.words ? sort : "board");
  /**
   * Cards keep their place under Relevance only as a reading order: a drop
   * or a keyboard nudge there would write a position relative to neighbours
   * that are not the column's.
   */
  const reorderLocked = $derived((searching && order === "relevance") || boardSort === "priority");
  // Description hits belong to the word they were read for: never credited
  // to a new word while its own read is in flight.
  const rankContext = $derived.by<RankContext>(() => {
    const hits = loadedKey === boardKey ? candidates?.textHits : undefined, word = plan.word;
    return { repoName, storeMatched: (id, typed) => word !== null && hits !== undefined && hits.has(id) && typed.startsWith(word) };
  });
  /** The words ranked over the loaded cards the filters pass; null without words. */
  const ranking = $derived(searching && parsedQuery.words && (loadedKey === boardKey || provisional)
    ? searchCards(loadedCards.filter(filtersHold), parsedQuery.words, rankContext, { order })
    : null);
  /** The ranked cards by id, and per column in the order the ranking gives. */
  const ranked = $derived.by(() => {
    const byId = new Map<string, RankedCard>();
    const byStatus: Partial<Record<TaskStatus, TaskCard[]>> = {};
    for (const hit of ranking?.cards ?? []) {
      byId.set(hit.card.id, hit);
      (byStatus[hit.card.status] ??= []).push(hit.card);
    }
    return { byId, byStatus };
  });
  /** The one test for "on screen": the Filters bar, the search box's filters, and its words. */
  function cardVisible(card: TaskCard): boolean {
    return filtersHold(card) && (ranking === null || ranked.byId.has(card.id));
  }
  /** Cards on screen across every column, hidden ones included; the filter bar's count. */
  const visibleCount = $derived(loadedCards.filter(cardVisible).length);
  function visibleIn(status: TaskStatus): TaskCard[] {
    if (ranking && order === "relevance") return ranked.byStatus[status] ?? [];
    const items = (displayColumns[status]?.items ?? []).filter(cardVisible);
    return boardSort === "priority" ? sortByPriority(items) : items;
  }
  /** The list keeps a relevance ranking as ranked; otherwise it follows the reader's order across every status. */
  function listOrder(): "priority" | "board" {
    return ranking && order === "relevance" ? "board" : boardSort;
  }
  /** A column's count: the store's total while browsing, the matches while searching. */
  function columnCount(status: TaskStatus): number {
    return searching ? visibleIn(status).length : displayColumns[status]?.total ?? 0;
  }
  const total = $derived(searching ? visibleCount : STATUSES.reduce((sum, status) => sum + (displayColumns[status]?.total ?? 0), 0));
  /** The coverage line: what a search read, matched and ranked, and whether it is partial. */
  const searchReport = $derived.by(() => {
    if (!searching) return null;
    if (provisional) return "Searching every task on this board…";
    if (!candidates || loadedKey !== boardKey) return null;
    return describeSearch({ matched: visibleCount, set: candidates, word: plan.word, relaxed: ranking?.relaxed ?? false, ranked: order === "relevance" });
  });
  const moreCandidates = $derived(searching && candidates !== null && loadedKey === boardKey && canLoadMore(candidates));
  /** What hidden repositories keep off this board, counted over the loaded tasks; null when nothing is hidden here. */
  const hiddenRepositoryReport = $derived.by(() => {
    if (!hidingRepositories || !initialized || !candidates || loadedKey !== boardKey) return null;
    const tasks = loadedCards.filter(hiddenHere).length;
    return { tasks, line: hiddenRepositoryNote({ tasks, repositories: hiddenRepositories.length, complete: candidatesComplete(candidates) }, hiddenRepositoryLabels) };
  });
  /** Completions for the search box: the loaded values, and every registered repository. */
  const suggestionSources = $derived.by<SuggestionSources>(() => ({
    kind: facetOptions.kinds,
    owner: facetOptions.owners,
    label: facetOptions.labels,
    severity: [...new Set(loadedCards.map((card) => card.severity?.trim() ?? "").filter(Boolean))].sort((a, b) => a.localeCompare(b)),
    repo: scope.kind === "repository" ? [] : repositories.map((repo) => repo.name),
  }));

  /**
   * The navigator's lists, re-read as far as they were paged in. Only the
   * newest read lands: two overlapping ones (a write's refresh and a
   * workspace move's) would otherwise leave whichever finished last, which
   * may be the older answer.
   */
  async function catalog() {
    const ticket = ++catalogRevision;
    const [repos, groups] = await Promise.all([
      reloadPaged((cursor) => listRepositories(cursor), repositories.length, NAVIGATOR_PAGE_SIZE),
      reloadPaged((cursor) => listWorkspaces(cursor), workspaces.length, NAVIGATOR_PAGE_SIZE),
    ]);
    if (disposed || ticket !== catalogRevision) return;
    // An unchanged list keeps its identity, so the navigator and everything
    // derived from it (the search plan, completions) are not recomputed.
    if (!sameRecords(repositories, repos.items)) repositories = repos.items;
    repositoryTotal = repos.total; repositoryCursor = repos.next_cursor;
    if (!sameRecords(workspaces, groups.items)) workspaces = groups.items;
    workspaceTotal = groups.total; workspaceCursor = groups.next_cursor;
    catalogError = "";
  }
  /** Each column as read, keeping the column already shown wherever the read found nothing new. */
  function keepUnchanged(next: Partial<Record<TaskStatus, Page<TaskCard>>>): Partial<Record<TaskStatus, Page<TaskCard>>> {
    const kept = Object.fromEntries(Object.entries(next).map(([status, page]) => {
      const shown = columns[status as TaskStatus];
      return [status, samePage(shown, page) ? shown : page];
    })) as Partial<Record<TaskStatus, Page<TaskCard>>>;
    const keys = Object.keys(kept);
    const same = keys.length === Object.keys(columns).length && keys.every((status) => kept[status as TaskStatus] === columns[status as TaskStatus]);
    return same ? columns : kept;
  }
  /**
   * Read the board for `target`. Returns whether the answer was applied.
   *
   * A `quiet` read is the board keeping itself current after a change made
   * elsewhere: it shows no loading state (which blanked the count and
   * disabled every button once a second while the store was busy), leaves an
   * error the reader caused alone, and is dropped if the reader started
   * anything meanwhile — a "Load more" or a move would otherwise be
   * overwritten by an answer read before it.
   */
  async function loadBoard(target: Scope = scope, current: SearchPlan = plan, quiet = false): Promise<boolean> {
    // Before the revision moves: a quiet read that bumped it under a running
    // "Load more" discarded that load's pages and left the board loading.
    if (quiet && (loading || moving)) return false;
    const generation = ++revision, key = JSON.stringify([target, current.key]), touched = interactions;
    if (!quiet) { loading = true; error = ""; refreshError = false; }
    const stale = () => generation !== revision || disposed || key !== boardKey || (quiet && (loading || moving || touched !== interactions));
    try {
      // Columns hold what is on the board: an archived task leaves them
      // whatever its status. The badge is the archive's own total for the
      // scope — the board's search does not narrow it; the dock has its own.
      const archived = listTasks(target, null, "", undefined, 1, { archived: true });
      if (current.mode === "search") {
        // One status-less candidate set, filtered and ranked here, and one
        // full-text read for the word descriptions may hold (`taskSearchPlan.ts`),
        // each re-read as far as the reader had loaded it.
        const kept = candidates !== null && loadedKey === key ? candidates : null;
        const [set, archive] = await Promise.all([
          reloadCandidates((query, cursor) => listTasks(current.scope, null, query, cursor, CANDIDATE_PAGE, BOARD), current.word, kept),
          archived,
        ]);
        if (stale()) return false;
        const pages = bucketByStatus(set.cards);
        candidates = set; columns = keepUnchanged(pages); loadedKey = key; loadedScope = JSON.stringify(target); archivedCount = archive.total;
        // A hidden repository's tasks are not this board's work in progress.
        if (candidatesComplete(set) && JSON.stringify(current.scope) === JSON.stringify(target)) inProgressCount = (pages.in_progress?.items ?? []).filter((card) => !hiddenHere(card)).length;
        keepSelected(pages);
        settled(quiet);
        return true;
      }
      // Columns are re-read as far as they were paged in (`boardPaging.ts`),
      // so a refresh after any write keeps "Load more" where the reader left
      // it. Another scope's columns, or a search's candidates bucketed into
      // columns, say nothing about this scope's pages.
      const kept = candidates === null && loadedScope === JSON.stringify(target);
      const [pages, archive] = await Promise.all([
        Promise.all(STATUSES.map(async (status) => {
          // A column read past 600 cards (Load all) is read back at the store's page size, not cut to 600.
          const loaded = kept ? columns[status]?.items.length ?? 0 : 0, size = reloadSize(loaded, TASK_PAGE_SIZE, CANDIDATE_PAGE);
          return [status, await reloadPaged((cursor) => listTasks(target, status, "", cursor, size, BOARD), loaded, size)] as const;
        })),
        archived,
      ]);
      if (stale()) return false;
      const read = Object.fromEntries(pages);
      columns = keepUnchanged(read); candidates = null; loadedKey = key; loadedScope = JSON.stringify(target); archivedCount = archive.total;
      inProgressCount = columns.in_progress?.total ?? 0;
      keepSelected(read);
      settled(quiet);
      void loadCardReads(pages.flatMap(([, page]) => page.items.map((card) => card.id)), generation);
      return true;
    } catch (cause) {
      // A background read that failed says so, and the next one that
      // succeeds clears what it said; neither touches the reader's own error.
      if (generation === revision && !disposed && !(quiet && error && !refreshError)) { error = failure("load", "tasks", cause); refreshError = quiet; }
      return false;
    }
    finally { if (!quiet && generation === revision && !disposed) loading = false; }
  }
  /** A read landed: a failure only an earlier background read reported is over. */
  function settled(quiet: boolean) { if (quiet && refreshError) { error = ""; refreshError = false; } }
  /** Drops from the selection what the read no longer holds; an unchanged selection keeps its identity. */
  function keepSelected(pages: Partial<Record<TaskStatus, Page<TaskCard>>>) {
    const kept = [...selected].filter((id) => loadedHas(id, pages));
    if (kept.length !== selected.size) selected = new Set(kept);
  }
  /**
   * One batched read of the verdicts for every card just loaded. A failure is
   * shown, never drawn as "no verdicts": a card that was verified and failed
   * must not look like one nobody checked.
   */
  async function loadVerdicts(ids: string[], generation: number) {
    try {
      const read = await getBoardVerdicts(ids);
      if (generation === revision && !disposed) { if (!sameVerdicts(verdicts, read)) verdicts = read; verdictError = ""; }
    } catch (cause) {
      if (generation === revision && !disposed) { verdicts = new Map(); verdictError = explainError(cause); }
    }
  }
  /**
   * One batched read of each loaded card's parent and subtask progress. A
   * failure is shown, never drawn as "no subtasks": a parent with open
   * subtasks must not look like a task with none.
   */
  async function loadLineage(ids: string[], generation: number) {
    try {
      const read = await getBoardLineage(ids);
      if (generation === revision && !disposed) { if (!sameLineage(lineage, read)) lineage = read; lineageError = ""; }
    } catch (cause) {
      if (generation === revision && !disposed) { lineage = new Map(); lineageError = explainError(cause); }
    }
  }
  /** Everything a card shows that the card list does not carry: its verdict and its place in a task tree. */
  function loadCardReads(ids: string[], generation: number) {
    void loadVerdicts(ids, generation);
    void loadLineage(ids, generation);
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
    try { await catalog(); } catch (cause) { catalogError = failure("refresh", "repositories and workspaces", cause); }
    if (initialized && active && !disposed) { await loadBoard(); await loadUnread(); }
  }
  /**
   * Keeping the board current after a change made elsewhere — an agent over
   * MCP, a background sweep, another window — or the window coming back.
   * Through the same loader as the archive dock and the inbox: one read at a
   * time, at most one every `LIVE_MIN_GAP_MS`, and quiet (see `loadBoard`).
   * A read that had to give way to the reader is asked again afterwards.
   */
  const background = createLiveLoader({
    canRun: () => initialized && active && !disposed && !readBackgroundDocument(),
    read: async () => {
      // The agents on the cards follow the same spaced, foreground-only
      // cadence; on every change they cost a read of runs and one per live run.
      void boardAgents.refresh();
      if (loading || moving) { background.schedule(); return; }
      membershipToken++;
      try { await catalog(); } catch (cause) { if (!disposed) catalogError = failure("refresh", "repositories and workspaces", cause); }
      if (!(await loadBoard(scope, plan, true)) && !disposed) background.schedule();
      await loadUnread();
    },
    onError: (cause) => { if (!disposed) liveNotice = `${failure("keep", "live updates running", cause)} Use Refresh.`; },
  });
  function scheduleRefresh() { background.schedule(); }
  async function initialize(path: string | null = repositoryPath) {
    const ticket = ++initializationRevision;
    revision++; unreadRevision++; initialized = false; loading = true; catalogError = "";
    try {
      const repo = path ? await registerRepository(path) : null;
      if (disposed || ticket !== initializationRevision) return;
      scope = repo ? { kind: "repository", id: repo.id } : { kind: "global" };
      await catalog();
      if (!disposed && ticket === initializationRevision) initialized = true;
    } catch (cause) { if (!disposed && ticket === initializationRevision) { catalogError = failure("load", "tasks", cause); loading = false; } }
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
    // `asksForReader` (reached through `taskAgentSummaries`) reads a module flag
    // Svelte cannot track. Reading the bell setting first is what recomputes the
    // chips when it flips; the store pushes the flag in before it notifies, so
    // the recount sees the new answer. The pane's `askingCount` does the same.
    void $interfaceStore.bellAsksForReader;
    if (read.readAt === null) return new Map();
    return taskAgentSummaries(read.runs, {
      records: $terminalSessions,
      requests: $taskTerminalRequests,
      capacityFull: $terminalSessions.length >= $terminalSessionLimit,
      activity: (sessionId) => $sessionActivity.get(sessionId),
      pending: (runId) => read.pending.get(runId),
      notices: (runId) => $attemptNotices.get(runId),
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
    const changed = () => { scheduleRefresh(); };
    listeners.track(subscribeWorkbenchChanges({ changed, unavailable: (message) => { if (!disposed) liveNotice = `Couldn't watch for live updates. ${message} Use Refresh.`; } }));
    const stopClock = createAdaptiveTimer(() => { now = Math.floor(Date.now() / 1000); }, 30_000);
    const onForeground = () => {
      boardAgents.wake();
      // A checkout is moved in a file manager, not here; coming back to the
      // window is when that can have happened.
      if (!readBackgroundDocument()) checkoutToken++;
      if (readBackgroundDocument()) {
        background.reset();
        return;
      }
      scheduleRefresh();
    };
    listeners.track(stopClock);
    listeners.track(bindForegroundChanges(document, typeof window === "undefined" ? null : window, onForeground));
    return () => {
      disposed = true; suggestWatch.reset(); revision++; unreadRevision++; initializationRevision++; openingRevision++; pendingUpdate?.stop(); background.dispose(); clearTimeout(undoTimer); clearTimeout(skipClickTimer); dropFrame.cancel(); listeners.dispose();
      // Leaving the board ends the undo window rather than dropping it: the
      // reader saw these tasks go, so they go.
      if (pendingDelete && !pendingDelete.running) void commitPendingDelete();
    };
  });
  $effect(() => {
    if (!initialized || !active) return;
    // Keyed on what the store is asked, not the text: a keystroke that only
    // changes the ranking or a filter re-reads nothing.
    boardKey;
    const target = untrack(() => scope), current = untrack(() => plan);
    loading = true;
    const timer = setTimeout(() => { void loadBoard(target, current); void loadUnread(target); }, 250);
    return () => { clearTimeout(timer); revision++; };
  });
  // ---- Where each repository's checkout is -------------------------------
  // Read before anyone acts, so a moved or deleted checkout is marked on its
  // row and on its cards instead of being discovered by a launch that fails.
  // Re-read when the catalog changes, on Refresh and when the window comes
  // back to the front — not on every store change, which says nothing about
  // the filesystem. Each check is a filesystem walk on the host, no git.
  let checkouts = $state.raw<ReadonlyMap<string, CheckoutHealth>>(new Map());
  let checkoutToken = $state(0);
  let checkoutRevision = 0;
  const checkoutKey = $derived(repositories.map((repo) => `${repo.id}|${repo.identity_key}`).join("\n"));
  const uncheckedCheckouts = $derived([...checkouts.values()].filter((health) => health.state === "unknown"));
  /** The failure a red notice above the read notes already states. */
  const statedFailure = $derived(catalogError || error);
  /** A read note's cause, or a pointer to the notice above when it says the same thing. */
  function noteCause(reason: string): string {
    return reason === statedFailure ? "The same failure as above." : reason;
  }
  /** A read-note line: "Couldn't read <object>. <What the board does instead>. <Reason>." */
  function readNote(object: string, reason: string, consequence: string): string {
    const copy = describeFailure({ verb: "read", object }, noteCause(reason));
    return [copy.headline, consequence, copy.detail].filter(Boolean).join(" ");
  }
  const hasReadNotes = $derived(Boolean(verdictError || lineageError || $boardAgents.error));
  $effect(() => {
    checkoutToken;
    void checkoutKey;
    if (!initialized || !active) return;
    const list = untrack(() => repositories.slice());
    const ticket = ++checkoutRevision;
    void readCheckouts(list, resolveGitRoot, pathOpts).then((read) => {
      if (!disposed && ticket === checkoutRevision) checkouts = read;
    });
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
    }).catch((cause) => { if (!disposed) catalogError = failure("read", "this workspace's repositories", cause); });
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
    } catch (cause) { catalogError = failure("add", "repositories", cause); }
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
    } catch (cause) { catalogError = failure("add", "repositories", cause); }
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
    } catch (cause) { catalogError = failure("add", "the folder", cause); }
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
    catch (cause) { catalogError = failure("load", "more repositories", cause); }
  }
  async function moreWorkspaces() {
    if (!workspaceCursor) return;
    try { const next = await listWorkspaces(workspaceCursor); workspaces = [...workspaces, ...next.items.filter((r) => !workspaces.some((old) => old.id === r.id))]; workspaceCursor = next.next_cursor; workspaceTotal = next.total; }
    catch (cause) { catalogError = failure("load", "more workspaces", cause); }
  }
  /**
   * Whether the open task, and the Quick Enhance sheet if one is up, may be
   * replaced. Every route that swaps either comes through here, so unsaved
   * edits to a task or to a suggestion are asked about by the same door.
   */
  async function canLeaveSession(): Promise<boolean> {
    if (enhanceId && enhanceHandle && (await enhanceHandle.canLeave()) === false) return false;
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
        error = failure("open", "the task", cause);
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
    // A move swaps with the neighbour in the full order, which a filtered
    // navigator may be hiding; the row would not visibly move.
    if (movingGroup || busy || navFiltering) return;
    const writes = moveWrites(orderedWorkspaces, id, delta);
    if (!writes) return;
    const name = workspaces.find((group) => group.id === id)?.name ?? "Workspace";
    movingGroup = true;
    try {
      const result = await applyMove(writes);
      if (disposed) return;
      try { await catalog(); } catch (cause) { catalogError = failure("refresh", "repositories and workspaces", cause); }
      if (result.failed.length) {
        catalogError = `${name} did not finish moving: ${result.failed.length} of ${writes.length} ${writes.length === 1 ? "workspace" : "workspaces"} couldn't be updated. ${result.failed[0]?.error ?? ""}`;
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
      if (workspaceCursor) { catalogError = "Not every workspace could be read, so the import couldn't tell which tab groups already have one."; return; }
      const report = await importTabGroups(importableGroups, workspaces);
      if (disposed) return;
      try { await catalog(); } catch (cause) { catalogError = failure("refresh", "repositories and workspaces", cause); }
      announce = importSummary(report);
      if (report.created.length) toastStore.success(announce);
      if (report.failed.length) catalogError = `${announce} ${report.failed.map((entry) => `${entry.name}: ${entry.error}`).join(" ")}`;
      else if (!report.created.length) toastStore.info(announce);
    } catch (cause) { if (!disposed) catalogError = failure("import", "tab groups", cause); }
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
    } catch (cause) { if (!disposed && ticket === openingRevision) error = failure("open", "workspace settings", cause); }
    finally { if (!disposed && ticket === openingRevision) opening = false; }
  }
  /** Ask Manvi which open tasks to start next. The newest ask wins; a stale answer never replaces it. */
  async function askSuggest() {
    const ticket = ++suggestTicket;
    suggestLoading = true; suggestError = "";
    try {
      const answer = await suggestTasks({
        limit: 5,
        ...(suggestPriority === "all" ? {} : { max_priority: suggestPriority }),
        ...(scope.kind === "repository" ? { repository_id: scope.id } : {}),
      });
      if (disposed || ticket !== suggestTicket) return;
      suggestion = answer;
    } catch (cause) {
      if (disposed || ticket !== suggestTicket) return;
      suggestion = null; suggestError = failure("ask", "Manvi for suggestions", cause);
    } finally { if (!disposed && ticket === suggestTicket) suggestLoading = false; }
  }
  function toggleSuggest() {
    showSuggest = !showSuggest;
    // Always ask on opening: a kept answer ranks tasks as they were when it was asked.
    if (showSuggest && !suggestLoading) void askSuggest();
  }
  /** Ask Manvi for a priority proposal. The newest ask wins. Accepting a row is separate and does not replace the proposal. */
  async function askOrganize() {
    const ticket = ++organizeTicket;
    organizeLoading = true; organizeError = "";
    try {
      const answer = await organizeTasks(scope.kind === "repository" ? { repository_id: scope.id } : {});
      if (disposed || ticket !== organizeTicket) return;
      organization = answer;
      organizeOutcomes = {};
      organizeEpoch++;
    } catch (cause) {
      if (disposed || ticket !== organizeTicket) return;
      organization = null; organizeError = failure("ask", "Manvi to organize tasks", cause);
    } finally { if (!disposed && ticket === organizeTicket) organizeLoading = false; }
  }
  function toggleOrganize() {
    showOrganize = !showOrganize;
    if (showOrganize && !organizeLoading) void askOrganize();
  }
  function organizeRowRefusal(error: unknown): boolean {
    return error instanceof WorkbenchError && (error.code === "revision_conflict" || error.code === "field_locked" || error.code === "not_found");
  }
  /**
   * Write the accepted rows. Each one is read again first. A task edited since
   * the proposal is shown and skipped; the fresh revision is not reused. A
   * refusal on one row does not stop the others. An unexpected store error stops
   * the rest and says how many were already written.
   */
  async function acceptOrganized(ids: string[]) {
    const proposal = organization;
    if (!proposal || organizeApplying || ids.length === 0) return;
    organizeApplying = true; organizeError = "";
    const outcomes = { ...organizeOutcomes };
    let applied = 0;
    const refusalText = (reason: "different_task" | "edited" | "priority_moved") =>
      reason === "edited" ? "This task was edited since the proposal. It was not changed."
      : reason === "priority_moved" ? "Its priority changed since the proposal. It was not changed."
      : "The task that was read is not the one in the proposal. It was not changed.";
    try {
      for (const id of ids) {
        const change = proposal.changes.find((row) => row.id === id);
        if (!change || outcomes[id] !== undefined) continue;
        let current: Task;
        try {
          current = await bounded(getTask(id));
        } catch (cause) {
          outcomes[id] = explainError(cause);
          if (!organizeRowRefusal(cause)) {
            organizeError = `Stopped after ${applied} accepted. ${explainError(cause)}`;
            break;
          }
          continue;
        }
        if (disposed) return;
        try {
          const decision = await applyOrganizedPriority(change, current, (priority) =>
            bounded(putTask(taskWrite(id, change.revision, { ...taskDraft(current), priority }))));
          if (decision.action === "apply") {
            outcomes[id] = `Accepted: ${priorityLabel(change.from)} → ${priorityLabel(change.to)}.`;
            applied++;
          } else if (decision.action === "unchanged") outcomes[id] = "Already at that priority.";
          else outcomes[id] = refusalText(decision.reason);
        } catch (cause) {
          outcomes[id] = explainError(cause);
          if (!organizeRowRefusal(cause)) {
            organizeError = `Stopped after ${applied} accepted. ${explainError(cause)}`;
            break;
          }
        }
      }
    } finally {
      organizeOutcomes = outcomes;
      organizeApplying = false;
      if (applied > 0 && !disposed) {
        try { await refresh(); } catch (cause) { if (!organizeError) organizeError = failure("refresh", "the board", cause); }
      }
    }
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
    } catch (cause) { if (!disposed && ticket === openingRevision) error = failure("open", "the task", cause); }
    finally { if (!disposed && ticket === openingRevision) opening = false; }
  }
  function openLoaded(task: Task) {
    workspaceEditor = null; enhanceId = null;
    taskTabs = openTaskTab(taskTabs, { id: task.id, title: task.title, status: task.status, draft: false });
    session = { tabId: task.id, pane: task.id, value: task };
  }
  /**
   * The next page of a search's candidates (and of its description hits), or
   * with `all` every page after it up to `MAX_CANDIDATES`, drawn as each one
   * lands. Stop, a new search or a new scope ends an `all` walk between pages.
   */
  async function loadMoreCandidates(all = false) {
    const set = candidates, current = plan;
    if (!set || loading || moving || loadedKey !== boardKey || !canLoadMore(set)) return;
    const generation = revision, key = boardKey;
    const stale = () => generation !== revision || disposed || key !== boardKey;
    interactions++; loading = true; loadingAll = all ? "search" : null; stopLoadingAll = false; error = ""; refreshError = false;
    const read: CandidateRead = (query, cursor) => listTasks(current.scope, null, query, cursor, CANDIDATE_PAGE, BOARD);
    const show = (next: CandidateSet) => {
      if (stale()) return false;
      candidates = next; columns = bucketByStatus(next.cards);
      announce = `Searched ${next.broad.loaded} of ${next.broad.total} tasks.`;
      return !stopLoadingAll;
    };
    try {
      if (all) await readAllCandidates(set, current.word, read, show);
      else show(await nextCandidates(set, current.word, read));
    } catch (cause) { if (generation === revision && !disposed) error = failure("load", "more tasks", cause); }
    finally { loadingAll = null; if (generation === revision && !disposed) loading = false; }
  }
  /** Whether a column has cards beyond the loaded ones, so the end of what is loaded is not its end. */
  function tailUnknown(status: TaskStatus): boolean {
    return Boolean(columns[status]?.next_cursor) || (searching && candidates !== null && !candidatesComplete(candidates));
  }
  /**
   * Read a column to its true end, so a card sent to the bottom is positioned
   * after the last card and not after page one. False, with the reason on the
   * banner, when the end could not be reached: the move is then refused
   * rather than written where it would disappear on the next read.
   */
  async function loadColumnRest(status: TaskStatus): Promise<boolean> {
    if (!tailUnknown(status)) return true;
    const refusal = `${STATUS_LABELS[status]} has more tasks than are loaded, so the card was not moved to its end. ${searching ? "Load more tasks, or clear the search," : "Try again,"} then move it again.`;
    const page = columns[status];
    if (searching || !page || loadedKey !== boardKey) { error = refusal; return false; }
    const generation = revision, key = boardKey;
    interactions++; loading = true; error = ""; refreshError = false;
    try {
      const all = await readToEnd((cursor) => listTasks(scope, status, "", cursor, TASK_PAGE_SIZE, BOARD), page);
      if (generation !== revision || disposed || key !== boardKey) return false;
      if (!all) { error = refusal; return false; }
      columns = { ...columns, [status]: all };
      return true;
    } catch (cause) { if (generation === revision && !disposed) error = failure("load", "the column", cause); return false; }
    finally { if (generation === revision && !disposed) loading = false; }
  }
  async function pageColumn(status: TaskStatus, cursor?: string) {
    if (searching || loading || moving || loadedKey !== boardKey) return;
    const generation = revision, key = boardKey;
    interactions++; loading = true; error = ""; refreshError = false;
    try {
      const result = await listTasks(scope, status, "", cursor, TASK_PAGE_SIZE, BOARD);
      if (generation !== revision || disposed || key !== boardKey) return;
      const items = cursor ? [...new Map([...(columns[status]?.items ?? []), ...result.items].map((item) => [item.id, item])).values()] : result.items;
      columns = { ...columns, [status]: { ...result, items, shown: items.length } };
      loadCardReads(Object.values(columns).flatMap((page) => page?.items.map((card) => card.id) ?? []), generation);
    } catch (cause) { if (generation === revision && !disposed) error = failure("load", "more tasks", cause); }
    finally { if (generation === revision && !disposed) loading = false; }
  }
  /** Cards loaded across every column: what Load all budgets against `MAX_CANDIDATES`, since each one is drawn. */
  const columnCards = $derived(Object.values(columns).reduce((sum, page) => sum + (page?.items.length ?? 0), 0));
  /**
   * A column read to its end, `CANDIDATE_PAGE` cards a request, drawn as each
   * page lands. It stops once the board holds `MAX_CANDIDATES` cards, on Stop,
   * or when the scope or a search takes the board; what it read stays, with
   * Load more from where it stopped.
   */
  async function loadColumnAll(status: TaskStatus) {
    const page = columns[status];
    if (searching || loading || moving || loadedKey !== boardKey || !page?.next_cursor) return;
    const generation = revision, key = boardKey, target = scope;
    const stale = () => generation !== revision || disposed || key !== boardKey;
    const budget = MAX_CANDIDATES - (columnCards - page.items.length);
    if (budget <= page.items.length) return;
    interactions++; loading = true; loadingAll = status; stopLoadingAll = false; error = ""; refreshError = false;
    try {
      await readOn((cursor) => listTasks(target, status, "", cursor, CANDIDATE_PAGE, BOARD), page, MAX_COLUMN_PAGES, {
        maxRows: budget,
        onPage: (next) => {
          if (stale()) return false;
          columns = { ...columns, [status]: next };
          return !stopLoadingAll;
        },
      });
      if (!stale()) loadCardReads(Object.values(columns).flatMap((one) => one?.items.map((card) => card.id) ?? []), generation);
    } catch (cause) { if (generation === revision && !disposed) error = failure("load", "all tasks", cause); }
    finally { loadingAll = null; if (generation === revision && !disposed) loading = false; }
  }
  /**
   * Where a drag at this point would drop.
   *
   * The cell under the pointer is the one found, not the first element for
   * the status: with lanes, a status has one cell per lane, and measuring the
   * first one put a card dropped in the third lane at a slot computed from
   * the first. `local` is the slot among the cards drawn in that cell (what
   * the insertion line shows); `index` is the same slot in the whole loaded
   * column, which is what the write needs.
   */
  function dropAtPoint(x: number, y: number, draggedId: string): { over: TaskStatus | null; lane: string; local: number; index: number } {
    const node = document.elementFromPoint(x, y);
    const cell = node instanceof Element ? node.closest("[data-task-column]") : null;
    const over = parseColumnStatus(cell?.getAttribute("data-task-column"));
    const lane = cell?.closest<HTMLElement>("[data-task-lane]")?.dataset.taskLane ?? "";
    if (!cell || !over) return { over, lane, local: 0, index: 0 };
    const drawn = [...cell.querySelectorAll<HTMLElement>("[data-task-card]")].filter((el) => el.dataset.cardId !== draggedId);
    const local = insertIndexFromY(drawn.map((el) => { const rect = el.getBoundingClientRect(); return rect.top + rect.height / 2; }), y);
    const rest = (displayColumns[over]?.items ?? []).filter((item) => item.id !== draggedId);
    const at = (id: string | undefined) => rest.findIndex((item) => item.id === id);
    const before = drawn[local], last = drawn.at(-1);
    const index = before ? at(before.dataset.cardId) : last ? at(last.dataset.cardId) + 1 : rest.length;
    return { over, lane, local, index: index < 0 ? rest.length : index };
  }
  function releasePointer(target: EventTarget | null, pointerId: number) {
    if (target instanceof HTMLElement && target.hasPointerCapture(pointerId)) target.releasePointerCapture(pointerId);
  }
  /**
   * The press and drag end on the window, not the card: a card a refresh or
   * another writer removes mid-drag takes its capture with it, and the
   * release then lands on whatever is under the pointer instead.
   */
  function ownsPointer(e: PointerEvent): boolean {
    return press !== null && e.pointerId === press.pointerId;
  }
  /** Ends a press or drag without dropping it. */
  function cancelDrag() {
    const held = press;
    dropFrame.cancel();
    dropMeasure = null;
    drag = null;
    press = null;
    if (held) releasePointer(held.target, held.pointerId);
  }
  // A board hidden mid-drag (a surface switch from the keyboard) would leave
  // the preview on <body>, over whatever is shown instead.
  $effect(() => { if (!active) untrack(cancelDrag); });
  function onCardPointerDown(e: PointerEvent, card: TaskCard) {
    if (opening) return;
    if (e.button !== 0 || busy || menu || press || reorderLocked) return;
    const target = e.currentTarget as HTMLElement;
    press = { card, pointerId: e.pointerId, x: e.clientX, y: e.clientY, grab: grabPoint({ x: e.clientX, y: e.clientY }, target.getBoundingClientRect()), target };
    // Capture keeps the moves coming once the pointer leaves the window. A
    // pointer the browser has already stopped tracking cannot be captured;
    // the window listeners still see that press through to its end.
    try { target.setPointerCapture(e.pointerId); } catch { /* uncaptured press, ended by the window listeners */ }
  }
  // The preview follows every move; the drop target (an elementFromPoint plus
  // a rect read per card) is measured once per frame, from the latest point.
  const dropFrame = createFrameScheduler();
  function onCardPointerMove(e: PointerEvent) {
    if (!press || !ownsPointer(e)) return;
    const first = !drag;
    if (!drag) {
      if (!dragExceeded(e.clientX - press.x, e.clientY - press.y)) return;
      menu = null;
      drag = { card: press.card, over: press.card.status, insertIndex: 0, lane: "", local: 0, x: e.clientX, y: e.clientY };
    }
    drag = { ...drag, x: e.clientX, y: e.clientY };
    dropMeasure = { x: e.clientX, y: e.clientY, id: drag.card.id };
    // The move that starts the drag is measured at once, against the layout the
    // press was made on; every later one waits for its frame.
    if (first) measureDrop(); else dropFrame.schedule(measureDrop);
  }
  let dropMeasure: { x: number; y: number; id: string } | null = null;
  /** Measure the drop target for the newest pointer position not yet measured. */
  function measureDrop() {
    const at = dropMeasure;
    dropMeasure = null;
    if (!at || !drag || drag.card.id !== at.id) return;
    const target = dropAtPoint(at.x, at.y, at.id);
    drag = { card: drag.card, over: target.over, insertIndex: target.index, lane: target.lane, local: target.local, x: at.x, y: at.y };
  }
  function onCardPointerUp(e: PointerEvent) {
    if (!ownsPointer(e)) return;
    // A frame may not have run since the last move; the drop is where the
    // preview last pointed, so measure that move now, against the same layout.
    if (dropFrame.isScheduled()) measureDrop();
    const current = drag;
    cancelDrag();
    if (!current) return;
    swallowGestureClick();
    void dropCard(current.card, current.over, current.insertIndex);
  }
  /**
   * Put a card at `insertIndex` of a column (counted without the card itself).
   * The one place a drop becomes a write, for the pointer and the keyboard.
   */
  async function dropCard(card: TaskCard, over: TaskStatus | null, insertIndex: number) {
    const fromIndex = (displayColumns[card.status]?.items ?? []).findIndex((item) => item.id === card.id);
    if (!shouldCommitMove(card.status, over, { moving, fromIndex, insertIndex })) return;
    let items = displayColumns[over]?.items ?? [];
    let { before, after } = insertionNeighbors(items, card.id, insertIndex);
    let position = insertionPosition(before, after, undefined, tailUnknown(over));
    if (position === null) {
      // The slot is the bottom of a column with more behind it: read the rest
      // first, then the bottom is the real one.
      if (!(await loadColumnRest(over))) return;
      items = displayColumns[over]?.items ?? [];
      ({ before, after } = insertionNeighbors(items, card.id, items.length));
      position = insertionPosition(before, after);
    }
    if (after !== null && (position >= after || before !== null && position <= before)) {
      const plan = reorderPlan(items,card,insertIndex);
      if (columns[over]?.next_cursor || (candidates && !candidatesComplete(candidates)) || plan.cards.length > MAX_TASK_SELECTION) { error = `This column needs re-spacing. Load its remaining tasks first; up to ${MAX_TASK_SELECTION} tasks can be reordered together. Priority and title sorting remain available.`; return; }
      await applyUpdate(new TaskBatch(plan.cards,{kind:"reorder",status:over,positions:plan.positions}));
    } else await moveCard(card, over, position);
  }
  /** The keyboard's move to the neighbouring column: to its bottom, which is only known once the column is read to its end. */
  async function moveToEnd(card: TaskCard, status: TaskStatus) {
    if (moving || !(await loadColumnRest(status))) return;
    const last = (columns[status]?.items ?? []).at(-1);
    await moveCardAcross(card, status, insertionPosition(last?.position ?? null, null));
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
      const ids = flattenVisibleIds(drawnColumns, shown, cardVisible);
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
    if (ownsPointer(e)) cancelDrag();
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
    swallowGestureClick();
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
        const ids = flattenVisibleIds(drawnColumns, shown, cardVisible);
        if (!selectionAnchor) selectionAnchor = card.id;
        selected = rangeSelect(ids, selectionAnchor, id);
      }
      return;
    }
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const next = neighborStatus(card.status, e.key === "ArrowRight" ? 1 : -1);
    if (!next) return;
    void moveToEnd(card, next);
  }
  /**
   * The keyboard move to a neighbouring column. The write reloads the board
   * and the pressed card is replaced, which drops focus to <body>; focus goes
   * back to it and the new column is named, as Alt+Arrow does. Announced only
   * once the card is drawn in that column, so a refused write says nothing.
   */
  async function moveCardAcross(card: TaskCard, status: TaskStatus, position: number) {
    await moveCard(card, status, position);
    if (disposed) return;
    const moved = boardEl?.querySelector<HTMLElement>(`[data-task-column="${status}"] [data-task-card][data-card-id="${CSS.escape(card.id)}"]`);
    if (!moved) return;
    announce = `${card.title} moved to ${STATUS_LABELS[status]}`;
    moved.focus();
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
    if (reorderLocked) { announce = boardSort === "priority" ? "Cards are ordered by priority. Switch to Board order in the View menu to move a card within its column." : "Matches are ordered by relevance. Switch to Board order to move a card within its column."; return; }
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
    // Before the focus checks: Safari does not focus a pressed button, so
    // during a drag the focus can be anywhere, or nowhere.
    if (press && e.key === "Escape") {
      e.preventDefault();
      cancelDrag();
      return;
    }
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
      selected = new Set(flattenVisibleIds(drawnColumns, shown, cardVisible));
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
    interactions++; moving = true; error = ""; refreshError = false; pendingUpdate = batch;
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
      if (missed) toastStore.error(`Couldn't delete ${plural(missed, "task")}. Open Tasks to try again.`);
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
      error = archiveRefusal(cards.length);
      return;
    }
    const already = cards.length - wanted.length;
    await applyTaskAction(wanted, archiveAction());
    // `applyUpdate` announces what it wrote. The skipped tasks are not a
    // failure and not a write, so they are said separately rather than
    // folded into a count that would then not match the receipt.
    const skipped = archiveSkipped(already);
    if (skipped && !error) announce = `${announce} ${skipped}`;
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
      if (!disposed) error = failure("create", "the task", cause);
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
  /**
   * Bring deleted tasks back from the archive dock's Deleted view.
   *
   * The bounded pass in `taskDelete.ts`, the same one a delete runs, so the
   * cap, the timeout and the single same-request retry are one policy. Each
   * card's revision is the one its deletion produced, which is what the store
   * checks a restore against.
   */
  async function restoreDeleted(cards: TaskCard[]): Promise<string> {
    if (busy || !cards.length) return "";
    const result = await restoreTasks(cards, async (attempt) => { await restoreDeletedTask(attempt.id, attempt.expected_revision, attempt.request_id); }, { newID });
    if (result.deleted.length) { taskWritten(); void loadBoard(); }
    const line = restoreSummary(result);
    announce = line;
    return line;
  }
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
      if (!disposed) error = failure("file", "issues", cause);
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
      if (!disposed) error = failure("link", "the issue", cause);
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
            }).catch((cause) => { if (!disposed) error = failure("duplicate", "the task", cause); })
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
          } catch (cause) { error = failure("copy", "the brief", cause); }
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
        if (item.action.choice !== "clear" && due === null) { error = "Couldn't resolve that due date."; break; }
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
  /** Whether the insertion line goes before `cardId` in the cell drawing `drawn`. */
  function insertBefore(status: TaskStatus, lane: string, drawn: readonly TaskCard[], cardId: string): boolean {
    if (!drag || drag.over !== status || drag.lane !== lane || drag.card.id === cardId) return false;
    const draggedId = drag.card.id;
    return drawn.filter((item) => item.id !== draggedId).findIndex((item) => item.id === cardId) === drag.local;
  }
  function insertAtEnd(status: TaskStatus, lane: string, drawn: readonly TaskCard[]): boolean {
    if (!drag || drag.over !== status || drag.lane !== lane) return false;
    const draggedId = drag.card.id;
    return drag.local >= drawn.filter((item) => item.id !== draggedId).length;
  }
  function dueLabel(state: ReturnType<typeof cardChrome>["due"]): string | null {
    if (state === "overdue") return "Overdue";
    if (state === "soon") return "Due soon";
    return null;
  }
</script>

{#snippet columnTitle(status: TaskStatus)}
  {@const limit = wipLimits[status]}
  {@const wip = wipOf(status)}
  <span>{STATUS_LABELS[status]}</span>
  <span class="column-meta">
    <!-- The store's count for the column, not its loaded page: a column
         showing thirty of forty is over a limit of thirty-five. -->
    <span data-testid="task-column-count" title={searching ? `${columnCount(status)} matching in ${STATUS_LABELS[status]}` : limit ? `${columns[status]?.total ?? 0} of a work-in-progress limit of ${limit}` : undefined}>{columns[status] && !(provisional && !searching) ? columnCount(status) : "—"}{#if limit && !searching}<span class="limit">/{limit}</span>{/if}</span>
    {#if wip === "over"}<span class="gp-chip over-badge" data-kind="label" data-tone="danger" data-testid="task-column-over">Over limit<span class="sr-only">: {STATUS_LABELS[status]} holds {columns[status]?.total ?? 0} tasks against a limit of {limit}</span></span>{/if}
    <button type="button" class="gp-icon-btn" aria-label={`New task in ${STATUS_LABELS[status]}`} title={creation.blocked ?? creation.caveat ?? `New task in ${STATUS_LABELS[status]}`} disabled={!creation.allowed} onclick={() => void createTask(status)}><Plus size={11} /></button>
  </span>
{/snippet}
{#snippet boardCard(card: TaskCard, status: TaskStatus)}
  {@const face = cardFace(card, repoName)}
  {@const chrome = cardChrome(card, now)}
  {@const agents = agentsByTask.get(card.id)}
  {@const flag = checkoutFlag(checkouts.get(card.primary_repository_id))}
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
    onclick={(e) => onCardClick(e, card)}
    oncontextmenu={(e) => onCardContextMenu(e, card, status)}
    onkeydown={(e) => onCardKeydown(e, card, status)}
  >
    <div class="card-meta">
      {#if face.pip !== null}<span class="pip" data-priority={face.pip} role="img" aria-label={`${priorityLabel(face.pip)} priority`} title={`${priorityLabel(face.pip)} priority`}></span>{/if}
      <h3>{face.title}{#if selected.has(card.id)}<span class="sr-only">, selected</span>{/if}</h3>
      {#if openCardIds.has(card.id)}<span class="gp-chip open-mark" data-kind="label" data-tone="accent">Open</span>{/if}
    </div>
    {#if agents}<span class="agents-host" data-testid="card-agents" data-tone={agents.tone ?? undefined} data-asking={agents.asking || undefined}><span class="gp-chip agents-mark" data-kind="label" data-tone={agents.asking ? "warn" : agents.tone === "error" ? "danger" : "neutral"} title={agentsTitle(agents)}><Bot size={11} aria-hidden="true" />{agents.working}{$boardAgents.complete ? "" : "+"}<span class="sr-only"> {agents.working === 1 ? "agent" : "agents"} working</span>{#if agents.asking}<span class="asking"> · {agents.asking} {agents.asking === 1 ? "needs" : "need"} you</span>{/if}</span></span>{/if}
    {#if flag}<span class="gp-chip checkout-note" data-kind="label" data-tone="warn" data-testid="card-checkout" title={flag.detail}><AlertTriangle size={10} aria-hidden="true" /> {flag.label}</span>{/if}
    {#if verdicts.get(card.id)}<CardVerdict verdict={verdicts.get(card.id)!} />{/if}
    {@render lineageMarks(card)}
    {#if face.repo && cardFields.has("repo")}<div class="card-repos">{face.repo}{chrome.extraRepos ? ` +${chrome.extraRepos}` : ""}</div>{/if}
    {#if (chrome.kind && cardFields.has("type")) || (chrome.owner && cardFields.has("owner")) || (dueLabel(chrome.due) && cardFields.has("due"))}
      <div class="card-extra">
        {#if chrome.kind && cardFields.has("type")}<span class="muted">{chrome.kind}</span>{/if}
        {#if chrome.owner && cardFields.has("owner")}<span class="muted">{chrome.owner}</span>{/if}
        {#if dueLabel(chrome.due) && cardFields.has("due")}<span class="gp-chip due" data-kind="label" data-tone={chrome.due === "overdue" ? "danger" : "warn"} data-due={chrome.due}>{dueLabel(chrome.due)}</span>{/if}
      </div>
    {/if}
    {#if face.labels.length && cardFields.has("labels")}<div class="labels">{#each face.labels as label}<span class="gp-chip" data-kind="label" data-tone="neutral">{label}</span>{/each}{#if chrome.extraLabels}<span class="gp-chip" data-kind="label" data-tone="neutral">+{chrome.extraLabels}</span>{/if}</div>{/if}
    {@render matchHint(card)}
  </button>
{/snippet}
{#snippet listRow(card: TaskCard)}
  {@const face = cardFace(card, repoName)}
  {@const chrome = cardChrome(card, now)}
  {@const agents = agentsByTask.get(card.id)}
  {@const flag = checkoutFlag(checkouts.get(card.primary_repository_id))}
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
    {#if agents}<span class="agents-host" data-testid="card-agents" data-tone={agents.tone ?? undefined} data-asking={agents.asking || undefined}><span class="gp-chip agents-mark" data-kind="label" data-tone={agents.asking ? "warn" : agents.tone === "error" ? "danger" : "neutral"} title={agentsTitle(agents)}><Bot size={11} aria-hidden="true" />{agents.working}{$boardAgents.complete ? "" : "+"}<span class="sr-only"> {agents.working === 1 ? "agent" : "agents"} working</span>{#if agents.asking}<span class="asking"> · {agents.asking} {agents.asking === 1 ? "needs" : "need"} you</span>{/if}</span></span>{/if}
    {#if flag}<span class="gp-chip checkout-note" data-kind="label" data-tone="warn" data-testid="card-checkout" title={flag.detail}>{flag.label}</span>{/if}
    {#if verdicts.get(card.id)}<CardVerdict verdict={verdicts.get(card.id)!} />{/if}
    {@render lineageMarks(card)}
    {#if openCardIds.has(card.id)}<span class="gp-chip open-mark" data-kind="label" data-tone="accent">Open</span>{/if}
    {#if face.repo && cardFields.has("repo")}<span class="muted">{face.repo}{chrome.extraRepos ? ` +${chrome.extraRepos}` : ""}</span>{/if}
    {#if chrome.owner && cardFields.has("owner")}<span class="muted">{chrome.owner}</span>{/if}
    {#if dueLabel(chrome.due) && cardFields.has("due")}<span class="gp-chip due" data-kind="label" data-tone={chrome.due === "overdue" ? "danger" : "warn"} data-due={chrome.due}>{dueLabel(chrome.due)}</span>{/if}
    {@render matchHint(card)}
  </button>
{/snippet}
{#snippet lineageMarks(card: TaskCard)}
  <!-- Where the card sits in a task tree. A card the lineage read did not name
       shows neither mark: unknown is not "no subtasks". -->
  {@const known = withLineage(card, lineage)}
  {@const progress = childProgressLabel(known.children)}
  {@const parentTitle = known.parent_item_id ? loadedTitles.get(known.parent_item_id) : undefined}
  {#if known.parent_item_id}<span class="card-lineage" data-testid="card-parent" data-parent-id={known.parent_item_id} title={parentTitle ? `Subtask of ${parentTitle}` : "Subtask of a task that is not loaded on this board"}><CornerDownRight size={10} aria-hidden="true" /> Subtask of {parentTitle ?? "another task"}</span>{/if}
  {#if progress && known.children}<span class="card-lineage" data-testid="card-subtasks" data-done={known.children.done} data-total={known.children.total}>{progress}</span>{/if}
{/snippet}
{#snippet matchHint(card: TaskCard)}
  <!-- Why a search matched a card it would not obviously match: a related
       word, a close spelling, or a description only the store can read. -->
  {@const hit = ranked.byId.get(card.id)}
  {@const why = hit ? describeMatch(hit) : ""}
  {#if why}<span class="match-hint" data-testid="task-match-hint">Matched {why}</span>{/if}
{/snippet}
{#snippet columnPaging(status: TaskStatus, named = false)}
  <!-- One control for both layouts. The list used to have none, so a task
       past a column's first page could not be reached from it at all, and the
       board's said "Next" for what appends rather than turns a page. -->
  {@const pageOf = columns[status]}
  {#if pageOf && (pageOf.next_cursor || pageOf.items.length > TASK_PAGE_SIZE)}
    <div class="paging" data-task-paging={status}>
      <span class="paging-count">{named ? `${STATUS_LABELS[status]}: ` : ""}{pageOf.items.length} of {pageOf.total}</span>
      {#if boardSort === "priority" && !searching && pageOf.next_cursor}
        {@const note = priorityOrderNote(pageOf.items.length, pageOf.total)}
        {#if note}<p class="paging-note" data-testid="task-priority-partial">{note} Load all orders the rest.</p>{/if}
      {/if}
      {#if pageOf.next_cursor}<button type="button" class="gp-btn" aria-label={`Load more ${STATUS_LABELS[status]} tasks`} onclick={() => pageColumn(status, pageOf.next_cursor ?? undefined)} disabled={loading}>Load more</button>{/if}
      {#if pageOf.next_cursor}
        {#if loadingAll === status}<button type="button" class="gp-btn" aria-label={`Stop loading ${STATUS_LABELS[status]} tasks`} onclick={() => { stopLoadingAll = true; }}>Stop</button>
        {:else}<button type="button" class="gp-btn" aria-label={`Load all ${STATUS_LABELS[status]} tasks`} title={columnCards >= MAX_CANDIDATES ? `The board already holds ${MAX_CANDIDATES.toLocaleString("en-US")} cards; show only the first page of another column, or search, to reach the rest` : `Read the rest of ${STATUS_LABELS[status]}, ${CANDIDATE_PAGE} tasks at a time`} onclick={() => void loadColumnAll(status)} disabled={loading || columnCards >= MAX_CANDIDATES}>Load all</button>{/if}
      {/if}
      {#if pageOf.items.length > TASK_PAGE_SIZE}<button type="button" class="gp-btn" aria-label={`Show only the first ${STATUS_LABELS[status]} page`} onclick={() => pageColumn(status)} disabled={loading}>First page</button>{/if}
    </div>
  {/if}
{/snippet}
{#snippet repositoryRow(repo: Repository, hidden: boolean)}
  {@const health = checkouts.get(repo.id)}
  {@const selected = scope.kind === "repository" && scope.id === repo.id}
  <div class="nav-row" data-repository-row={repo.id} data-hidden={hidden || undefined} data-checkout={health?.state ?? "unread"}>
    <button type="button" class="gp-row nav-scope" class:selected aria-pressed={selected} data-active={selected} onclick={() => chooseScope({ kind: "repository", id: repo.id })} title={health && health.state !== "available" ? health.detail : repo.identity_key}>
      {@render scopeSelection(selected)}<span class="nav-name">{repo.name}</span>{#if health?.state === "missing"}<span class="nav-meta checkout-mark" data-testid="repository-checkout-missing"><AlertTriangle size={10} aria-hidden="true" /> Missing</span>{:else if health?.state === "remote"}<span class="nav-meta" data-testid="repository-remote-only">Remote only</span>{:else if health?.state === "unknown"}<span class="nav-meta" data-testid="repository-checkout-unknown">Not checked</span>{/if}
    </button>
    <span class="nav-trail">
      {#if hidden}
        <button type="button" class="trail-item gp-icon-btn" data-size="sm" data-persist data-testid="repository-show" aria-label={`Show ${repo.name}'s tasks on the board`} title={`Show ${repo.name}'s tasks on the All and workspace boards again`} onclick={() => interfaceStore.toggleTaskRepositoryHidden(repo.id)}><Eye size={12} /></button>
      {:else}
        <button type="button" class="trail-item gp-icon-btn" data-size="sm" data-testid="repository-hide" aria-label={`Hide ${repo.name}'s tasks from the board`} title={`Hide ${repo.name}'s tasks from the All and workspace boards — its own board still shows them`} onclick={() => interfaceStore.toggleTaskRepositoryHidden(repo.id)}><EyeOff size={12} /></button>
      {/if}
      {#if health?.state === "remote"}
        <button type="button" class="trail-item gp-icon-btn" data-size="sm" data-persist aria-label={`Link ${repo.name} to a local checkout`} title={`Link ${repo.name} to a checkout on this machine — its tasks, workspaces and history keep their place`} disabled={relinking || busy} onclick={() => void relinkRepo(repo)}><FolderSync size={12} /></button>
      {:else}
        <button type="button" class="trail-item gp-icon-btn" data-size="sm" class:urgent={health?.state === "missing"} data-persist={health?.state === "missing" || undefined} aria-label={`Relink ${repo.name} to a moved checkout`} title={health?.state === "missing" ? health.detail : `Relink ${repo.name} — its checkout moved or was cloned again`} disabled={relinking || busy} onclick={() => void relinkRepo(repo)}><FolderSync size={12} /></button>
      {/if}
    </span>
  </div>
{/snippet}
{#snippet scopeSelection(selected: boolean)}
  {#if macos && selected}<span class="gp-liquid-selection gp-gpu" aria-hidden="true" in:receiveScope={{ key: "task-scope" }} out:sendScope={{ key: "task-scope" }}></span>{/if}
{/snippet}
{#snippet scopeRail(overlay: boolean)}
  <!-- Justified: the keydown only moves focus among the rail's own buttons (the roving-list pattern); the nav itself is not a control. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <nav bind:this={railEl} id="task-scope-rail" class="navigator" class:is-overlay={overlay} class:gp-glass={overlay} class:shadow-float={overlay} class:nav-liquid={macos} data-task-rail aria-label="Task scopes" onkeydown={onRailKeydown}>
    {#if overlay && !railNarrow}
      <div class="nav-heading gp-label nav-dock">Task scopes<button type="button" class="gp-icon-btn" data-size="sm" aria-label="Dock the task scopes beside the board" title="Dock the task scopes beside the board" onclick={dockRail}><PanelLeft size={12} /></button></div>
    {/if}
    <label class="nav-search"><Search size={11} aria-hidden="true" /><input class="gp-field" type="search" data-testid="task-nav-search" aria-label="Find a workspace or repository" placeholder="Find" maxlength="200" bind:value={navFilter} onkeydown={(e) => { if (e.key === "Escape" && navFilter) { e.preventDefault(); e.stopPropagation(); navFilter = ""; } }} /></label>
    <div class="nav-heading gp-label">Workspaces<span class="heading-actions">{#if importableGroups.length}<button type="button" class="gp-icon-btn" data-size="sm" data-testid="import-tab-groups" title={`Import ${plural(importableGroups.length, "tab group")} as workspaces`} aria-label={`Import ${plural(importableGroups.length, "tab group")} as workspaces`} disabled={importingGroups || busy} onclick={() => void importGroups()}><Import size={12} /></button>{/if}<button type="button" class="gp-icon-btn" data-size="sm" title="New workspace" aria-label="New workspace" onclick={newWorkspace}><Plus size={12} /></button></span></div>
    <div class="nav-row"><button type="button" class="gp-row nav-scope" aria-pressed={scope.kind === "global"} data-active={scope.kind === "global"} class:selected={scope.kind === "global"} onclick={() => chooseScope({ kind: "global" })}>{@render scopeSelection(scope.kind === "global")}<span class="nav-name">All</span></button></div>
    {#each orderedWorkspaces as group, index (group.id)}
      {@const siblings = orderedWorkspaces.filter((other) => other.pinned === group.pinned)}
      {#if navShowsWorkspace(group)}
        {@const selected = scope.kind === "workspace" && scope.id === group.id}
        {@const meta = [group.repository_count === 0 ? "Empty" : "", group.archived ? "Archived" : ""].filter(Boolean).join(" · ")}
        <div class="nav-row" data-workspace-row={group.id}>
          <button type="button" class="gp-row nav-scope" aria-pressed={selected} data-active={selected} class:selected aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown" onkeydown={(e) => { if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) { e.preventDefault(); void moveGroup(group.id, e.key === "ArrowUp" ? -1 : 1); } }} onclick={() => chooseScope({ kind: "workspace", id: group.id })} title="{group.name} — {workspaceMembershipLabel(group.repository_count)}">{@render scopeSelection(selected)}<span class="nav-name">{group.icon} {group.name}</span>{#if meta}<span class="nav-meta">{meta}</span>{/if}</button>
          <span class="nav-trail">
            <button type="button" class="trail-item gp-icon-btn" data-size="sm" aria-label={`Move ${group.name} up`} disabled={movingGroup || busy || navFiltering || siblings[0]?.id === group.id} onclick={() => void moveGroup(group.id, -1)}><ChevronUp size={12} /></button>
            <button type="button" class="trail-item gp-icon-btn" data-size="sm" aria-label={`Move ${group.name} down`} disabled={movingGroup || busy || navFiltering || siblings.at(-1)?.id === group.id || (index === orderedWorkspaces.length - 1)} onclick={() => void moveGroup(group.id, 1)}><ChevronDown size={12} /></button>
            <button type="button" class="trail-item gp-icon-btn" data-size="sm" aria-label={`Edit ${group.name}`} title={`Edit ${group.name}`} onclick={() => editWorkspace(group.id)} disabled={opening}><Ellipsis size={12} /></button>
          </span>
        </div>
      {/if}
    {/each}
    {#if navFiltering && navWorkspaceMatches === 0}<p class="nav-empty" data-testid="task-nav-no-workspaces">No workspace matches “{navFilter.trim()}”{workspaceCursor ? " in the loaded rows" : ""}.</p>{/if}
    {#if workspaceCursor}<button type="button" class="gp-btn nav-more" data-variant="ghost" data-size="sm" onclick={moreWorkspaces}>More ({workspaces.length}/{workspaceTotal}){navFiltering ? " — search covers loaded rows" : ""}</button>{/if}
    {#if workspaces.some((group) => group.archived)}
      <!-- "workspaces", not just "archived": this page also has an Archive
           that holds completed tasks, and two unqualified uses of the word
           on one screen read as one feature with a broken control. -->
      <div class="archive-pick"><Checkbox label="Show archived workspaces" checked={showArchived} onchange={(next) => interfaceStore.setTaskShowArchivedWorkspaces(next)} /></div>
    {/if}
    <!-- In a workspace scope this control has a second effect: whatever it
         adds also joins the workspace. It says so rather than leaving the
         membership write to be discovered. -->
    <div class="nav-heading gp-label" data-add-repo>Repositories<button bind:this={addRepoTriggerEl} type="button" class="gp-icon-btn" data-size="sm" aria-haspopup="menu" aria-expanded={addMenu} aria-controls="task-add-repo-menu" aria-busy={adding} title={addRepoLabel} aria-label={addRepoLabel} disabled={adding} onclick={toggleAddMenu}><Plus size={12} /></button></div>
    {#if addMenu}
      <div
        bind:this={addMenuEl}
        use:portal={"body"}
        use:popover={addMenuDismissal} out:popoverExit
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
    {#each repositories as repo (repo.id)}
      {#if !hiddenRepositorySet.has(repo.id) && navShowsRepository(repo)}{@render repositoryRow(repo, false)}{/if}
    {/each}
    <!-- Hidden repositories keep their rows: their own boards still open
         from here, and Show is one click away. Collapsed unless the reader
         opened it, is on one of their boards, or is searching the navigator. -->
    {#if hiddenRepositories.length}
      {@const expanded = showHiddenRepositories || navFiltering || (scope.kind === "repository" && hiddenRepositorySet.has(scope.id))}
      <div class="nav-heading gp-label" data-testid="task-hidden-repositories">
        <button type="button" class="gp-btn nav-disclosure" data-variant="ghost" data-size="inline" data-testid="task-hidden-toggle" aria-expanded={expanded} aria-controls="task-hidden-repository-rows" onclick={() => { showHiddenRepositories = !expanded; }}>{#if expanded}<ChevronDown size={11} />{:else}<ChevronRight size={11} />{/if}<span class="hidden-label">{hiddenGroupLabel}</span></button>
        <button type="button" class="gp-icon-btn" data-size="sm" title="Show every hidden repository's tasks on the board" aria-label="Show every hidden repository" onclick={() => interfaceStore.showAllTaskRepositories()}><Eye size={12} /></button>
      </div>
      {#if expanded}
        <div id="task-hidden-repository-rows">
          {#each repositories as repo (repo.id)}
            {#if hiddenRepositorySet.has(repo.id) && navShowsRepository(repo)}{@render repositoryRow(repo, true)}{/if}
          {/each}
          {#if hiddenUnloaded > 0}<p class="nav-empty">{plural(hiddenUnloaded, "hidden repository is", "hidden repositories are")} not among the loaded rows.</p>{/if}
        </div>
      {/if}
    {/if}
    {#if navFiltering && navRepositoryMatches === 0}<p class="nav-empty" data-testid="task-nav-no-repositories">No repository matches “{navFilter.trim()}”{repositoryCursor ? " in the loaded rows" : ""}.</p>{/if}
    {#if repositoryCursor}<button type="button" class="gp-btn nav-more" data-variant="ghost" data-size="sm" onclick={moreRepositories}>More ({repositories.length}/{repositoryTotal}){navFiltering ? " — search covers loaded rows" : ""}</button>{/if}
  </nav>
{/snippet}
<svelte:window onkeydown={onBoardKeydown} onpointermove={onCardPointerMove} onpointerup={onCardPointerUp} onpointercancel={onCardPointerCancel} onlostpointercapture={onCardPointerCancel} onblur={cancelDrag} />
<div bind:this={boardEl} class="workbench bg-background" class:is-dragging={drag !== null} class:is-compact={compact} data-testid="task-board">
  {#if !repositoryPath}
    {#if railDocked}
      {@render scopeRail(false)}
    {:else if railOpen}
      <div class="rail-overlay" style="z-index: {LAYERS.SCROLL_CUE}; top: {railTop}px" use:popover={railDismissal} in:fly={{ x: -16, opacity: 0, ...backdropFade() }} out:fly={{ x: -16, opacity: 0, ...backdropFadeOut() }}>
        {@render scopeRail(true)}
      </div>
    {/if}
  {/if}
  <main class="board-main">
    <header class="gp-glass gp-surface-header">
      <div class="heading">
        {#if !repositoryPath}
          <button bind:this={railToggleEl} type="button" class="gp-icon-btn" data-nav-toggle data-testid="task-nav-toggle" aria-expanded={railDocked || railOpen} aria-controls="task-scope-rail" aria-label={railDocked ? "Hide task scopes" : "Show task scopes"} title={railDocked ? "Hide task scopes" : "Show task scopes"} onclick={toggleRail}><PanelLeft size={13} /></button>
        {/if}
        <h1 class="gp-surface-title">{title}{#if !loading && initialized}<span>{total}</span>{/if}</h1>
        {#if inProgressCount > 0}<span class="gp-chip" data-kind="label" data-tone="accent">{inProgressCount} in progress</span>{/if}
        <!-- A repository's board has no navigator, so this is its one way to
             the boards for every task and for its workspaces. -->
        {#if repositoryPath}<button type="button" class="gp-btn" data-variant="ghost" data-size="sm" data-testid="task-board-all" title="Open the Tasks board for every repository and workspace" onclick={() => interfaceStore.setGlobalSurface("tasks")}>All tasks</button>{/if}
      </div>
      <div class="actions">
        <div class="search"><TaskSearchField id="task-search" bind:value={search} sources={suggestionSources} busy={loading && searching} describedby="task-search-hint" title="Words are ranked over every task on this board — titles, labels, type, owner and repository, with word forms, related words and close spellings — and the store searches descriptions. is:bug, priority:high, repository:name, owner:, label:, status:, due:, severity: filter. OR between words, quotes for a phrase, commas for any of several values, a leading - excludes." /></div>
        <span id="task-search-hint" class="sr-only">Words are ranked over every task on this board, including related words and close spellings. Filters such as is:bug, priority:high or repository:name narrow it; a comma list is any of its values; OR between words is either; quote a phrase; a leading minus excludes.</span>
        <button type="button" class="gp-icon-btn" aria-pressed={showSearchHelp} aria-controls="task-search-help" aria-label="Search syntax" title="Search syntax" onclick={() => { showSearchHelp = !showSearchHelp; }}>?</button>
        <div class="gp-segmented" class:gp-liquid-tabs={macos} role="group" aria-label="Task layout">
          <!-- inline-flex: an icon is a block-level svg, so without it the
               label wrapped under the icon and doubled the header's height. -->
          <button type="button" class="gp-seg-btn inline-flex items-center gap-1.5" data-active={layout === "board"} aria-pressed={layout === "board"} onclick={() => interfaceStore.setTaskLayout("board")}><LayoutGrid size={12} /> Board</button>
          <button type="button" class="gp-seg-btn inline-flex items-center gap-1.5" data-active={layout === "list"} aria-pressed={layout === "list"} onclick={() => interfaceStore.setTaskLayout("list")}><List size={12} /> List</button>
        </div>
        {#if initialized}<AutomaticEnhancements {active} compact />{/if}
        {#if initialized}
          <button type="button" class="gp-icon-btn" aria-pressed={showInbox} aria-label="Inbox" title={unreadError ? `Notifications unavailable: ${unreadError}` : "Inbox"} onclick={() => { showInbox = !showInbox; }}>
            <Inbox size={13} />
            {#if !unreadError && unread > 0}<span class="gp-chip" data-kind="count" data-tone="accent">{unread}</span>{/if}
          </button>
          <button type="button" class="gp-icon-btn" aria-pressed={showArchive} aria-controls="task-archive-dock" aria-label="Archive" title="Archive — tasks filed away from this board, and deleted tasks to restore" onclick={() => { showArchive = !showArchive; }}>
            <Archive size={13} />
            {#if archivedCount > 0}<span class="gp-chip" data-kind="count" data-tone="neutral">{archivedCount}</span>{/if}
          </button>
        {/if}
        <button type="button" class="gp-icon-btn" aria-label="Refresh" title="Refresh" onclick={() => { void boardAgents.refresh(); checkoutToken++; if (initialized) void refresh(); else void initialize(); }} disabled={loading}><RefreshCw size={13} /></button>
        <button type="button" class="gp-btn" aria-pressed={showFilters || filtering} onclick={() => { showFilters = !showFilters; }}>Filters</button>
        <button type="button" class="gp-btn" data-testid="task-suggest-toggle" aria-pressed={showSuggest} aria-controls="task-suggest" disabled={!initialized} title="Ask Manvi which tasks to start next, ranked by priority, severity, due date and blockers" onclick={toggleSuggest}>Suggest next</button>
        <button type="button" class="gp-btn" data-testid="task-organize-toggle" aria-pressed={showOrganize} aria-controls="task-organize" disabled={!initialized} title="Ask Manvi for a reviewable priority proposal. Nothing changes until you accept a row." onclick={toggleOrganize}>Organize</button>
        <TaskSavedViews disabled={!initialized} board={viewBoard} boardName={title} current={viewSnapshot} onApply={applyView} />
        <TaskViewMenu disabled={!initialized} board={viewBoard} boardName={title} />
        <button type="button" class="gp-btn" data-variant="primary" onclick={() => void createTask()} disabled={!creation.allowed} title={creation.blocked ?? creation.caveat ?? "New task"} aria-label="New task">New task</button>
        <!-- A disabled New task always says why, and a caveat says what the
             sheet will ask for. This used to render only for a profile with no
             repositories at all, so an empty workspace left the button dead
             and silent. -->
        {#if creation.blocked ?? creation.caveat}
          {#if creation.blocked && emptyAddLabel}
            <button type="button" class="gp-btn" data-variant="ghost" data-size="sm" onclick={() => void addPaths(menuTabs.map((tab) => tab.path))} disabled={adding}>{emptyAddLabel}</button>
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
        <select class="gp-select" aria-label="Filter by severity" bind:value={facet.severity}>
          <option value="all">Any severity</option>
          <option value="">No severity</option>
          {#each facetOptions.severities as severity}<option value={severity}>{severity}</option>{/each}
        </select>
        <select class="gp-select" aria-label="Filter by due date" bind:value={facet.due}>
          <option value="all">Any due date</option>
          <option value="overdue">Overdue</option>
          <option value="soon">Due soon</option>
          <option value="none">No due date</option>
        </select>
        <select class="gp-select" aria-label="Filter by status" data-testid="task-filter-status" value={statusControl ?? "several"} disabled={statusControl === null}
          title={statusControl === null ? "The search holds several statuses or an exclusion; change them there." : undefined}
          onchange={(event) => { search = setQueryControl(search, "status", event.currentTarget.value ? `status:${event.currentTarget.value}` : ""); }}>
          <option value="">Any status</option>
          <option value="open">Not done</option>
          {#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}
          {#if statusControl === null}<option value="several" disabled>Several, set in the search</option>{/if}
        </select>
        <label class="range" title={rangeControl === null ? "The search holds several date ranges; change them there." : "Tasks due on or between these days"}>Due
          <input class="gp-field" type="date" aria-label="Due on or after" data-testid="task-filter-due-from" value={rangeEnds[0]} max={rangeEnds[1] || undefined} disabled={rangeControl === null}
            onchange={(event) => setDueRange(event.currentTarget.value, rangeEnds[1])} />
          to
          <input class="gp-field" type="date" aria-label="Due on or before" data-testid="task-filter-due-to" value={rangeEnds[1]} min={rangeEnds[0] || undefined} disabled={rangeControl === null}
            onchange={(event) => setDueRange(rangeEnds[0], event.currentTarget.value)} />
        </label>
        <!-- A repository's own board already shows only its tasks, so the
             select is offered there only while a filter carried over from
             another board is still on — never as an invisible constraint. -->
        {#if (scope.kind !== "repository" && facetRepositories.length) || facet.repository !== "all"}
          <select class="gp-select" aria-label="Filter by repository" data-testid="task-filter-repository" bind:value={facet.repository}>
            <option value="all">All repositories</option>
            {#each facetRepositories as repo (repo.id)}<option value={repo.id}>{repo.name}</option>{/each}
          </select>
        {/if}
        {#if filtering}<button type="button" class="gp-btn" onclick={() => { facet = emptyFacet(); search = ""; }}>Clear filters</button>{/if}
      </div>
    {/if}
    {#if initialized && showSuggest}
      <TaskSuggestPanel
        result={suggestion} loading={suggestLoading} error={suggestError} priority={suggestPriority}
        onPriority={(value) => { suggestPriority = value; void askSuggest(); }}
        onOpen={(id) => void openTask(id)} onRefresh={() => void askSuggest()} onClose={() => { showSuggest = false; }}
      />
      <WavePanel repositoryId={scope.kind === "repository" ? scope.id : null} maxPriority={suggestPriority} />
    {/if}
    {#if initialized && showOrganize}
      {#key organizeEpoch}
        <TaskOrganizePanel
          result={organization} loading={organizeLoading} applying={organizeApplying} error={organizeError} outcomes={organizeOutcomes}
          onOpen={(id) => void openTask(id)} onAccept={(id) => void acceptOrganized([id])} onAcceptAll={(ids) => void acceptOrganized(ids)}
          onRefresh={() => void askOrganize()} onClose={() => { showOrganize = false; }}
        />
      {/key}
    {/if}
    {#if initialized && (showFilters || filtering || showSearchHelp)}
      <div class="query-bar" data-testid="task-query-bar">
        <div class="quick-filters" role="group" aria-label="Quick filters">
          {#each QUICK_FILTERS as quick (quick.filter)}
            {@const on = queryHas(parsedQuery, quick.filter)}
            <button type="button" class="gp-btn quick" data-size="sm" aria-pressed={on} data-active={on} title={on ? `Remove ${quick.filter}` : `Add ${quick.filter} to the search`} onclick={() => { search = toggleQueryTerm(search, quick.filter); }}>{quick.label}</button>
          {/each}
        </div>
        {#if parsedQuery.terms.length}
          <ul class="query-chips" aria-label="Search filters">
            {#each queryChips(parsedQuery) as term (`${term.start}:${term.raw}`)}
              <li class="gp-chip query-term" data-kind="removable" class:negated={term.negated}><span>{term.raw}</span><button type="button" class="gp-icon-btn" data-size="sm" aria-label={`Remove filter ${term.raw}`} onclick={() => { search = removeQueryTerm(search, term); }}><X size={10} /></button></li>
            {/each}
          </ul>
        {/if}
        {#if searchReport}
          <!-- What the search read, matched and ranked. A partial answer says
               it is partial and offers the next page; it never reads as
               everything that matches. -->
          <span class="query-coverage" role="status" data-testid="task-query-coverage">{searchReport}</span>
        {/if}
        {#if moreCandidates}
          <button type="button" class="gp-btn quick" data-size="sm" disabled={loading} onclick={() => void loadMoreCandidates()}>Load more tasks</button>
          {#if loadingAll === "search"}
            <button type="button" class="gp-btn quick" data-size="sm" onclick={() => { stopLoadingAll = true; }}>Stop loading</button>
          {:else}
            <button type="button" class="gp-btn quick" data-size="sm" disabled={loading} title={`Read every remaining page, ${CANDIDATE_PAGE} tasks at a time, up to ${MAX_CANDIDATES.toLocaleString("en-US")}`} onclick={() => void loadMoreCandidates(true)}>Load all tasks</button>
          {/if}
        {/if}
        {#if searching && parsedQuery.words}
          <div class="gp-segmented" role="group" aria-label="Order matches">
            <button type="button" class="gp-seg-btn" data-active={sort === "relevance"} aria-pressed={sort === "relevance"} title="Best matches first. Cards cannot be moved within a column in this order." onclick={() => { sort = "relevance"; }}>Relevance</button>
            <button type="button" class="gp-seg-btn" data-active={sort === "board"} aria-pressed={sort === "board"} title="Matches in the board's own order, where cards can be moved." onclick={() => { sort = "board"; }}>Board order</button>
          </div>
        {/if}
        {#if parsedQuery.problems.length}
          <ul class="query-problems" role="status" aria-live="polite" data-testid="task-query-problems">
            {#each parsedQuery.problems as problem}<li><AlertTriangle size={11} aria-hidden="true" /> {problem}</li>{/each}
          </ul>
        {/if}
        {#if showSearchHelp}
          <dl id="task-search-help" class="query-help" data-testid="task-search-help">
            <div><dt>words</dt><dd>Every word must match: titles, labels, type, owner and repository (word forms, related words, close spellings), and descriptions through the store</dd></div>
            <div><dt>login OR signin</dt><dd>Either word</dd></div>
            <div><dt>"sign in"</dt><dd>That phrase in the title or a label</dd></div>
            {#each QUERY_HELP as row (row.example)}<div><dt><button type="button" class="gp-btn" data-variant="ghost" data-size="sm" title="Add to the search" onclick={() => { search = toggleQueryTerm(search, row.example); }}>{row.example}</button></dt><dd>{row.meaning}</dd></div>{/each}
          </dl>
        {/if}
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
    {#if hiddenRepositoryReport}
      <!-- Like a hidden column: the reader chose to hide the repositories,
           not to lose count of their work. Without a search the Filters bar
           is closed, so the next page of tasks is offered here. -->
      <p class="hidden-note" role="status" data-testid="task-hidden-repository-note">
        <EyeOff size={11} />
        {hiddenRepositoryReport.line}
        {#if moreCandidates && !filtering}
          <button type="button" class="gp-btn" data-variant="ghost" data-size="sm" disabled={loading} onclick={() => void loadMoreCandidates()}>Load more tasks</button>
          {#if loadingAll === "search"}<button type="button" class="gp-btn" data-variant="ghost" data-size="sm" onclick={() => { stopLoadingAll = true; }}>Stop loading</button>
          {:else}<button type="button" class="gp-btn" data-variant="ghost" data-size="sm" disabled={loading} onclick={() => void loadMoreCandidates(true)}>Load all tasks</button>{/if}
        {/if}
        <button type="button" class="gp-btn" data-variant="ghost" data-size="sm" onclick={() => interfaceStore.showAllTaskRepositories()}>Show hidden repositories</button>
      </p>
    {/if}
    {#if hiddenWork}
      <!-- Hiding a column is a layout choice. Hiding the work in it is not one
           this board makes on the reader's behalf, so the cost is named. -->
      <p class="hidden-note" role="status" data-testid="task-hidden-columns">
        <EyeOff size={11} />
        {hiddenWork.summary} hidden from this board.
        <button type="button" class="gp-btn" data-variant="ghost" data-size="sm" onclick={() => interfaceStore.showAllTaskColumns()}>Show all columns</button>
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
          <button type="button" class="gp-btn" data-variant="danger" disabled={deletesRunning > 0 && pendingDelete.running} onclick={() => void commitPendingDelete()}><Trash2 size={12} /> Delete now</button>
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
             same reason. Without it the only bulk end-of-life action on this
             bar was Delete. -->
        <button
          type="button"
          class="gp-btn"
          data-testid="task-archive-selected"
          onclick={() => void archiveCards(selectedCards)}
          disabled={archiveBar.disabled}
          title={archiveBar.title}
        ><Archive size={12} /> Archive</button>
        <!-- The merge gitpulse_merge_tasks runs, offered only for a
             selection it would not refuse on sight (taskMerge.ts). -->
        <button
          type="button"
          class="gp-btn"
          data-testid="task-merge-selected"
          disabled={busy || !selectionMerge.ok}
          title={selectionMerge.ok ? "Merge — fold these into one task and delete the rest" : selectionMerge.reason}
          onclick={() => { if (selectionMerge.ok) mergeDialog = { cards: [...selectedCards], repositoryId: selectionMerge.repositoryId }; }}
        ><Merge size={12} /> Merge…</button>
        <button type="button" class="gp-btn" data-variant="danger" onclick={() => void removeSelected()} disabled={busy}><Trash2 size={12} /> Delete</button>
        <button type="button" class="gp-btn" onclick={() => { selected = new Set(); selectionAnchor = null; }}>Clear</button>
      </div>
    {/if}
    {#if pendingUpdate && !moving}<div class="banner error gp-notice gp-notice-error" role="alert">Confirm the interrupted task update before making another change.<button type="button" class="gp-btn" onclick={() => { if(pendingUpdate) void applyUpdate(pendingUpdate); }}>Try again</button></div>{/if}
    {#if showInbox}<AttentionInbox {scope} {active} onopen={openTask} />{/if}
    {#if showArchive}
      <TaskArchive
        {scope}
        {active}
        {busy}
        refreshToken={archiveToken}
        hiddenIds={pendingDelete?.ids}
        onopen={openTask}
        onaction={(cards, action) => void archiveDockAction(cards, action)}
        onrestoredeleted={restoreDeleted}
        repository={(id) => repositoryById.get(id)}
      />
    {/if}
    {#if relinkPending}<div class="banner error gp-notice gp-notice-error" role="alert" data-testid="repository-relink-uncertain"><span>{relinkPending.message}</span><button type="button" class="gp-btn" disabled={relinking} onclick={() => void retryRelink()}>Try again</button></div>{/if}
    {#if catalogError}<div class="banner error gp-notice gp-notice-error" role="alert">{catalogError}<button type="button" class="gp-btn" onclick={() => initialized ? refresh() : initialize()}>Try again</button></div>{/if}
    {#if issueProgress}<div class="banner gp-notice" data-testid="task-issue-progress"><span>Filing issue {issueProgress.index} of {issueProgress.total}: {issueProgress.title}</span>{#if issueProgress.total > 1}<button type="button" class="gp-btn" disabled={issueStop} onclick={() => { issueStop = true; }}>{issueStop ? "Stopping…" : "Stop"}</button>{/if}</div>{/if}
    {#if liveNotice}<div class="banner gp-notice" role="status" data-testid="task-live-unavailable">{liveNotice}</div>{/if}
    {#if error}<div class="banner error gp-notice gp-notice-error" role="alert">{error}</div>{/if}
    <!-- One card for every board detail that could not be read, so the
         reasons line up instead of trailing off the pane edge. A cause the red
         notice above already states is pointed at, not repeated. -->
    {#if hasReadNotes}
      <ul class="read-notes gp-notice gp-notice-warn" aria-label="Board details not read" data-testid="board-read-notes">
        <!-- Cards carry no agent marks while this stands: an old reading would
             claim agents nobody checked. -->
        {#if verdictError}<li class="agents-unread" role="status" data-testid="board-verdicts-error"><CircleAlert size={12} aria-hidden="true" /><span>{readNote("verification verdicts", verdictError, "Cards show none.")}</span></li>{/if}
        {#if lineageError}<li class="agents-unread" role="status" data-testid="board-lineage-error"><CircleAlert size={12} aria-hidden="true" /><span>{readNote("subtask progress", lineageError, "Cards show none.")}</span></li>{/if}
        {#if $boardAgents.error}<li class="agents-unread" role="status" data-testid="board-agents-error"><CircleAlert size={12} aria-hidden="true" /><span>{readNote("which agents are working on these tasks", $boardAgents.error, "")}</span></li>{/if}
      </ul>
    {/if}
    <!-- A check that could not run is said, not shown as a healthy checkout:
         those rows and cards carry no mark either way. -->
    {#if uncheckedCheckouts.length}
      {@const unknown = unknownState(plural(uncheckedCheckouts.length, "checkout"), `Whether ${uncheckedCheckouts.length === 1 ? "it is" : "they are"} still there is unknown. ${uncheckedCheckouts[0].detail}`)}
      <div class="gp-notice" data-tone="unknown" role="status" data-testid="checkout-unchecked">
        <CircleAlert size={12} aria-hidden="true" />
        <span class="min-w-0 flex-1">{unknown.title}. {unknown.reason}</span>
        <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={() => { checkoutToken += 1; }}>{unknown.actionLabel}</button>
      </div>
    {/if}
    <!-- Its own banner, not part of `error`: a board reload clears `error`,
         and the issue would still exist with nothing on screen to link it. -->
    {#each relinks as entry (entry.taskId)}<div class="banner error gp-notice gp-notice-error" role="alert" data-testid="task-issue-relink"><span>Issue #{entry.number} exists but is not linked to “{entry.title}”.</span><button type="button" class="gp-btn" disabled={busy} onclick={() => void retryIssueLink(entry)}>Link to #{entry.number}</button></div>{/each}
    <div class="sr-only" role="status" aria-live="polite">{announce}</div>
    {#if !initialized && loading}
      <div class="pad"><Skeleton variant="card" count={4} /></div>
    {:else if initialized && error && loadedKey !== boardKey}
      <EmptyState icon={Inbox} title="Tasks unavailable" hint="Try again to load this scope." action={{label:"Try again",onClick:()=>void loadBoard(),variant:"secondary"}} />
    {:else if initialized && !loading && listCards.length === 0 && !filtering && (hiddenRepositoryReport?.tasks ?? 0) > 0}
      <!-- Not "No tasks yet": there are tasks, the reader hid where they live. -->
      <EmptyState icon={EyeOff} title="Every loaded task is in a hidden repository" hint={hiddenRepositoryReport?.line ?? ""} action={moreCandidates ? { label: "Load more tasks", onClick: () => void loadMoreCandidates(), variant: "secondary" } : { label: "Show hidden repositories", onClick: () => interfaceStore.showAllTaskRepositories(), variant: "secondary" }} />
    {:else if initialized && !loading && total === 0 && !filtering}
      <!-- Gated on the same decision as the header button. Offering New task
           here where the header refuses it opened a sheet that could never be
           saved, because nothing had linked a repository. -->
      <EmptyState icon={Inbox} title="No tasks yet" hint={creation.blocked ?? creation.caveat ?? "Create a task in this scope. Cards stay on this board until you delete them."} action={creation.allowed ? { label: "New task", onClick: () => void createTask(), variant: "primary" } : undefined} />
    {:else if initialized && !loading && listCards.length === 0 && filtering}
      <EmptyState icon={Search} title="No tasks match" hint={candidates && !candidatesComplete(candidates) ? `Only ${candidates.broad.loaded} of ${candidates.broad.total} tasks on this board were searched. Load more tasks to search the rest, or clear the search.` : "Nothing on this board matches every word and filter. Clear the search or remove a filter to see the rest."} action={moreCandidates ? { label: "Load more tasks", onClick: () => void loadMoreCandidates(), variant: "secondary" } : { label: "Clear filters", onClick: () => { facet = emptyFacet(); search = ""; }, variant: "secondary" }} />
    {:else if layout === "list"}
      <div class="list" role="group" data-testid="task-columns" data-task-list aria-busy={loading || moving} aria-label="Task list">
        {#if laneList}
          {#each laneList as group (group.key)}
            <div class="list-lane" role="group" aria-label={laneAria(group)} data-task-lane={group.key}>
              <h3 class="lane-title">{group.label}<span>{group.cards.length}</span></h3>
              {#each group.cards as card (card.id)}{@render listRow(card)}{/each}
            </div>
          {/each}
        {:else}
          {#each listCards as card (card.id)}{@render listRow(card)}{/each}
        {/if}
        {#each shown as status (status)}{@render columnPaging(status, true)}{/each}
      </div>
    {:else if laneList}
      <!-- Lanes are rows across the status columns. The column heads, with
           their counts and limits, are drawn once above every lane: a limit
           is about the whole column, not one lane's share of it. -->
      <div class="columns laned" aria-busy={loading || moving} data-testid="task-columns">
        <div class="lane-head" style={laneGrid}>
          {#each shown as status (status)}
            <div class="column-head" class:over-limit={wipOf(status) === "over"} data-task-column-head={status} data-wip={wipOf(status) ?? undefined}>{@render columnTitle(status)}</div>
          {/each}
        </div>
        {#each laneList as group (group.key)}
          <div class="lane" role="group" aria-label={laneAria(group)} data-task-lane={group.key}>
            <h3 class="lane-title">{group.label}<span>{group.cards.length}</span></h3>
            <div class="lane-cells" style={laneGrid}>
              {#each shown as status (status)}
                {@const drawn = group.cards.filter((card) => card.status === status)}
                <section
                  class="column lane-cell"
                  class:drop-target={drag !== null && drag.over === status && drag.lane === group.key}
                  class:over-limit={wipOf(status) === "over"}
                  data-task-column={status}
                  aria-label={`${STATUS_LABELS[status]} — ${group.label}`}
                  oncontextmenu={(e) => onColumnContextMenu(e, status)}
                >
                  <div class="cards">
                    {#each drawn as card (card.id)}
                      {#if insertBefore(status, group.key, drawn, card.id)}<div class="insert" aria-hidden="true"></div>{/if}
                      {@render boardCard(card, status)}
                    {/each}
                    {#if insertAtEnd(status, group.key, drawn)}<div class="insert" aria-hidden="true"></div>{/if}
                  </div>
                </section>
              {/each}
            </div>
          </div>
        {/each}
        <div class="lane-head" style={laneGrid}>
          {#each shown as status (status)}<div>{@render columnPaging(status)}</div>{/each}
        </div>
      </div>
    {:else}
      <!-- Every loaded card is drawn (Load all budgets against MAX_CANDIDATES on
           that premise). Windowing the column with a fixed row height was tried
           and refused: a card's height follows its title, chips and labels, so
           the window drifted off the cards; and the drop, Home/End, nudge and
           move-across code finds cards in the DOM, so an unmounted card lost
           the focus a keyboard move hands back to it. -->
      <div class="columns" aria-busy={loading || moving} data-testid="task-columns">
        {#each shown as status (status)}
          {@const drawn = visibleIn(status)}
          <section
            class="column"
            class:drop-target={drag !== null && drag.over === status}
            class:over-limit={wipOf(status) === "over"}
            data-task-column={status}
            data-testid="task-column"
            data-wip={wipOf(status) ?? undefined}
            aria-label={STATUS_LABELS[status]}
            oncontextmenu={(e) => onColumnContextMenu(e, status)}
          >
            <div class="column-title">{@render columnTitle(status)}</div>
            {#if drawn.length === 0 && drag === null}
              <div class="cards">
                <p class="column-empty">Drop a card here, or add one.</p>
              </div>
            {:else}
              <div class="cards">
                {#each drawn as card (card.id)}
                  {#if insertBefore(status, "", drawn, card.id)}<div class="insert" aria-hidden="true"></div>{/if}
                  {@render boardCard(card, status)}
                {/each}
                {#if insertAtEnd(status, "", drawn)}<div class="insert" aria-hidden="true"></div>{/if}
              </div>
            {/if}
            {@render columnPaging(status)}
          </section>
        {/each}
      </div>
    {/if}
  </main>
  {#if drag}
    <!-- Portaled to body: the board's `.gp-pane` contains layout and paint,
         which makes it the containing block of `position: fixed`, and a
         preview drawn in it trailed the pointer by the pane's own origin.
         Drawn at the card's size and held where it was grabbed. -->
    {@const grab = press?.grab ?? { dx: 0, dy: 0, width: 0, height: 0 }}
    <div use:portal={"body"} class="ghost gp-glass bg-surface shadow-float" style="z-index: {LAYERS.SCROLL_CUE}; width: {grab.width}px; height: {grab.height}px; transform: translate3d({drag.x - grab.dx}px, {drag.y - grab.dy}px, 0)" data-testid="task-drag-ghost">{drag.card.title}</div>
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
      bind:this={enhanceHandle}
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
                class="gp-icon-btn" data-size="sm" data-tone="danger"
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

{#if mergeDialog}<TaskMergeDialog cards={mergeDialog.cards} repositoryId={mergeDialog.repositoryId} onClose={() => { mergeDialog = null; }} onMerged={(result) => { tasksChanged(result.sources.filter((row) => row.outcome !== "not_merged").map((row) => row.item_id)); announce = result.ok ? `Merged ${result.sources.length} ${result.sources.length === 1 ? "task" : "tasks"}.` : "Merged part of the selection; see the merge dialog."; }} />{/if}
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
  /* A docked rail, not a floating card. In flow beside the board it separates
     from it by one tonal step and its own padding: no outline, no radius (the
     pane's rounded corner clips it where it meets the plate), no glass. Glass
     is for the overlay below, because only that one floats over the board. */
  /* `gp-glass` paints only on macOS and its rule is more specific than this
     fill, so the overlay is glass there and the same solid surface elsewhere. */
  .navigator{width:188px;flex-shrink:0;margin:0;padding:10px 8px;overflow:auto;border-radius:0;background:var(--mac-fill-surface,rgb(var(--c-surface)))}
  .rail-overlay{position:absolute;bottom:0;left:0;display:flex;max-width:calc(100% - 24px)}
  .navigator.is-overlay{width:248px;max-width:100%;height:100%;border-radius:0 var(--gp-r-pane) var(--gp-r-pane) 0}
  .navigator .nav-scope{display:flex;align-items:center;gap:6px;flex:1;min-width:0;width:auto;position:relative;text-align:left;font-size:var(--text-xs);font-weight:400;background:transparent;box-shadow:none;color:rgb(var(--c-text))}
  .navigator .nav-scope:hover{background:rgb(var(--c-surface-hover) / 0.7)}
  .navigator .nav-scope.selected{background:color-mix(in srgb,rgb(var(--c-accent)) 13%,transparent);color:rgb(var(--c-accent));font-weight:500}
  .navigator.nav-liquid .nav-scope.selected{background:transparent}
  .navigator .nav-scope:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:-2px}
  .navigator .nav-scope > :not(:global(.gp-liquid-selection)){position:relative}
  .navigator .nav-name{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .navigator .nav-meta{flex-shrink:0;font-size:var(--text-meta);color:rgb(var(--c-text-muted));white-space:nowrap}
  .navigator .nav-meta.checkout-mark{color:rgb(var(--c-warn))}
  /* Every trailing control on a row is one 24px gp-icon-btn, revealed together
     on hover or focus-within. `data-persist` keeps the ones that carry state
     or the row's main action (a hidden repository's Show, a checkout that
     needs relinking); touch has no hover, so it shows them all. */
  .navigator .nav-row{display:flex;align-items:center;min-width:0}
  .navigator .nav-trail{display:inline-flex;flex-shrink:0}
  .navigator .trail-item{display:none;flex-shrink:0}
  .navigator .nav-row:hover .trail-item,.navigator .nav-row:focus-within .trail-item,.navigator .trail-item[data-persist]{display:inline-flex}
  @media (hover:none){.navigator .trail-item{display:inline-flex}}
  .nav-heading{position:relative;display:flex;align-items:center;justify-content:space-between;padding:10px 0 4px 8px;min-height:24px}
  .nav-heading > button,.heading-actions>button{flex-shrink:0}
  .heading-actions{display:inline-flex}
  .navigator .nav-more{display:block;width:100%;text-align:left}
  .navigator .nav-disclosure{gap:2px;width:auto;min-width:0;flex:1;overflow:hidden;font:inherit;color:inherit}
  .navigator .nav-disclosure :global(svg){flex-shrink:0}
  /* Portaled: `fixed` + the popover owner set left/top. Width and height are
     viewport-capped so a long path or a long registered list cannot grow the
     panel off the window the way the old 260px-in-188px absolute menu did.
     `gp-menu`'s overflow:hidden is overridden so the list can scroll. */
  .add-menu{width:min(20rem,calc(100vw - 16px));max-width:calc(100vw - 16px);max-height:min(20rem,calc(100vh - 16px));overflow:hidden auto;box-sizing:border-box}
  .add-menu-label{padding:4px 8px;font-size:var(--text-micro);color:rgb(var(--c-text-muted))}
  .add-menu button{width:100%;height:auto;min-width:0;padding:6px 8px;white-space:normal;overflow:visible;text-align:left}
  .add-item{display:flex;flex-direction:column;align-items:stretch;gap:1px;min-width:0}
  .add-name,.add-path{display:block;min-width:0;overflow-wrap:anywhere;word-break:break-word;white-space:normal}
  .add-path{font-size:var(--text-micro);color:rgb(var(--c-text-muted))}
  .hidden-label{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .archive-pick{padding:0 8px}
  .board-main{flex:1;min-width:0;display:flex;flex-direction:column;overflow:hidden}
  /* The header's box and the title's type are the shared gp-surface-header
     and gp-surface-title (app.css), so Tasks and Agents read as one chrome. */
  h1{display:flex;align-items:baseline;gap:8px}
  h1 span{font-size:var(--text-meta);font-weight:500;color:rgb(var(--c-text-muted))}
  .actions,.facets,.selection,.undo-strip{display:flex;gap:6px;align-items:center;flex-wrap:wrap}
  .facets,.selection{padding:8px 14px}
  .facets .range{display:inline-flex;align-items:center;gap:6px;font-size:var(--text-xs)}
  .selection,.undo-strip{margin:8px 14px 0;padding:8px 10px;border-radius:var(--gp-r-md)}
  .undo-strip>span{flex:1;min-width:0;font-size:var(--text-xs)}
  button,input,select{font-size:var(--text-xs)}
  button:disabled{opacity:.5}
  .hint{font-size:var(--text-meta);color:rgb(var(--c-text-muted))}
  .search{display:flex;align-items:center;gap:6px;color:rgb(var(--c-text-muted))}
  .match-hint{display:block;margin-top:4px;font-size:var(--text-micro);line-height:1.3;color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .nav-search{display:flex;align-items:center;gap:5px;margin:0 0 6px;color:rgb(var(--c-text-muted))}
  .nav-search input{flex:1;min-width:0;font-size:var(--text-meta)}
  .nav-empty{margin:2px 6px 6px;font-size:var(--text-meta);color:rgb(var(--c-text-muted))}
  .query-bar{display:flex;flex-wrap:wrap;gap:6px 10px;align-items:center;padding:0 14px 8px}
  .quick-filters{display:flex;flex-wrap:wrap;gap:4px}
  .quick[data-active="true"]{background:rgb(var(--c-accent) / .18);border-color:rgb(var(--c-accent) / .65)}
  .query-chips{display:flex;flex-wrap:wrap;gap:4px;margin:0;padding:0;list-style:none}
  .query-term{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
  .query-term.negated span{text-decoration:line-through}
  .query-coverage{font-size:var(--text-meta);color:rgb(var(--c-text-muted))}
  .query-problems{flex-basis:100%;margin:0;padding:0;list-style:none;font-size:var(--text-meta);color:rgb(var(--c-warn))}
  .query-help{flex-basis:100%;display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:2px 14px;margin:0;font-size:var(--text-meta)}
  .query-help div{display:flex;gap:8px}
  .query-help dt{min-width:130px;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
  .query-help dd{margin:0;color:rgb(var(--c-text-muted))}
  .columns{display:flex;gap:8px;padding:12px;overflow:auto;flex:1;min-height:0;align-items:stretch}
  .column{width:220px;min-width:196px;flex:1;display:flex;flex-direction:column;border-radius:var(--gp-r-xl);border:1px solid rgb(var(--c-border) / 0.65);overflow:hidden;min-height:0}
  .column.drop-target{border-color:rgb(var(--c-accent))}
  /* Over its work-in-progress limit: the column says so in its border and its head, and still accepts work. */
  .column.over-limit,.column-head.over-limit{border-color:rgb(var(--c-danger))}
  .limit{color:rgb(var(--c-text-muted))}
  /* Lanes: one head row for the columns, then a row of cells per lane. */
  .columns.laned{flex-direction:column;align-items:stretch;gap:10px}
  .lane-head,.lane-cells{display:grid;gap:8px}
  .column-head{display:flex;align-items:center;justify-content:space-between;font-weight:600;font-size:var(--text-xs);padding:8px 10px;border:1px solid rgb(var(--c-border) / 0.65);border-radius:var(--gp-r-lg)}
  .lane{flex-shrink:0;display:flex;flex-direction:column;gap:6px}
  .lane-title{margin:0;font-size:var(--text-meta);font-weight:600;display:flex;gap:6px;align-items:baseline;color:rgb(var(--c-text))}
  .lane-title span{font-weight:400;color:rgb(var(--c-text-muted))}
  .lane-cell{width:auto;min-width:0;flex:none}
  .lane-cell .cards{min-height:48px;overflow:visible}
  .list-lane{display:flex;flex-direction:column;margin-bottom:10px}
  .list-lane .lane-title{padding:4px 2px 6px}
  .column-title{display:flex;align-items:center;justify-content:space-between;font-weight:600;font-size:var(--text-xs);padding:8px 10px}
  .column-meta{display:flex;align-items:center;gap:4px;color:rgb(var(--c-text-muted));font-weight:400}
  .cards{padding:6px;overflow:auto;flex:1;min-height:80px}
  .card{border:1px solid rgb(var(--c-border) / 0.65);border-radius:var(--gp-r-md)}
  .card,.row{width:100%;display:block;text-align:left;padding:8px 9px;margin-bottom:6px;cursor:grab;touch-action:none;user-select:none}
  .row{cursor:pointer;display:grid;grid-template-columns:7rem minmax(0,1fr) auto auto auto;gap:8px;align-items:center}
  .card:focus-visible,.row:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}
  .card.dragging{opacity:.35;cursor:grabbing}
  .card.selected,.row.selected,.card.open,.row.open{border-color:rgb(var(--c-accent));box-shadow:inset 0 0 0 1px rgb(var(--c-accent) / 0.35)}
  .heading{display:flex;align-items:baseline;gap:8px;min-width:0;flex-wrap:wrap}
  .open-mark{flex-shrink:0;margin-top:2px}
  /* Agents working on the card's task; warn when one needs the reader, as in the task's Agents pane.
     The host carries the hooks (testid, data-tone, data-asking) and draws nothing; the chip inside is the mark. */
  .agents-host{display:contents}
  .agents-mark{align-self:flex-start;flex-shrink:0;font-variant-numeric:tabular-nums;white-space:nowrap}
  .card .agents-mark{margin-top:4px}
  /* A task whose primary repository has no checkout here: said on the card, before a launch finds out. */
  .checkout-note{align-self:flex-start;flex-shrink:0;white-space:nowrap}
  .card .checkout-note{margin-top:4px}
  .card-lineage{display:inline-flex;align-items:center;gap:3px;align-self:flex-start;max-width:100%;font-size:var(--text-micro);line-height:1.2;color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .card .card-lineage{display:flex;margin-top:4px}
  .checkout-mark{color:rgb(var(--c-warn));font-size:var(--text-micro)}
  .navigator .gp-icon-btn.urgent{color:rgb(var(--c-warn))}
  .editor-dock{display:flex;flex-direction:column;flex-shrink:0;min-width:0;min-height:0;align-self:stretch}
  /* Same token the sheet below uses (app.css): the strip and the sheet are
     one column and must not be able to disagree about its width. */
  .task-tabs{position:relative;width:var(--gp-task-sheet-w);flex-shrink:0}
  .task-tab-strip{display:flex;align-items:stretch;gap:4px;min-width:0;overflow-x:auto;padding:6px 8px}
  .task-tab-shell{display:flex;align-items:center;gap:2px;flex-shrink:0;max-width:190px;border-radius:var(--gp-r-full);border:1px solid transparent;padding:0 2px 0 8px}
  .task-tab-shell.is-active{border-color:rgb(var(--c-accent) / 0.35);background:color-mix(in srgb,rgb(var(--c-accent)) 12%,transparent)}
  .task-tab-shell.is-draft [role="tab"]{font-style:italic}
  .task-tab-shell [role="tab"]{display:flex;align-items:center;gap:6px;min-width:0;flex:1;padding:5px 0;cursor:pointer;font-size:var(--text-meta)}
  .task-tab-shell .gp-icon-btn{flex-shrink:0}
  .tab-title{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;min-width:0}
  .tab-pip{width:6px;height:6px;border-radius:var(--gp-r-full);flex-shrink:0;background:rgb(var(--c-accent))}
  .task-panel{flex:1;min-height:0;display:flex}
  .task-panel :global(.task-editor){flex:1}
  .card h3,.row-title{font-size:var(--text-xs);line-height:1.4;font-weight:500;margin:0;overflow-wrap:anywhere;min-width:0}
  .card-meta{display:flex;align-items:flex-start;gap:6px}
  .pip{width:7px;height:7px;margin-top:4px;border-radius:var(--gp-r-full);flex-shrink:0;background:rgb(var(--c-accent))}
  .pip[data-priority="0"]{background:rgb(var(--c-danger))}
  .insert{height:2px;margin:2px 4px;border-radius:2px;background:rgb(var(--c-accent))}
  .card-repos,.muted,.status{font-size:var(--text-micro);color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .card-repos{margin-top:4px}
  .card-extra{display:flex;gap:6px;flex-wrap:wrap;margin-top:4px}
  .labels{display:flex;gap:4px;margin-top:6px;flex-wrap:wrap}
  .paging{display:flex;align-items:center;flex-wrap:wrap;gap:5px;padding:6px}
  .paging-count{font-size:var(--text-micro);color:rgb(var(--c-text-muted));font-variant-numeric:tabular-nums;margin-right:auto}
  .paging-note{flex-basis:100%;margin:0;font-size:var(--text-micro);color:rgb(var(--c-text-muted))}
  .column-empty{margin:10px 8px;font-size:var(--text-meta);color:rgb(var(--c-text-muted))}
  .list{padding:12px;overflow:auto;flex:1}
  /* The card skin is gp-notice (app.css); this is only the row inside it. */
  .banner{display:flex;align-items:center;justify-content:space-between;gap:10px}
  .read-notes{list-style:none;display:flex;flex-direction:column;gap:4px;color:rgb(var(--c-text-muted))}
  .read-notes li{display:flex;align-items:flex-start;gap:8px;font-size:var(--text-meta);line-height:1.45}
  .read-notes li :global(svg){flex-shrink:0;margin-top:2px;color:rgb(var(--c-warn))}
  .error{color:rgb(var(--c-danger))}
  .ghost{position:fixed;top:0;left:0;pointer-events:none;box-sizing:border-box;padding:8px 9px;font-size:var(--text-xs);font-weight:500;overflow:hidden;overflow-wrap:anywhere}
  .pad{padding:16px}
  .quick-add-row{padding:8px 14px 0}
  .quick-add-row.dimmed{opacity:.45;pointer-events:none}
  .hidden-note{display:flex;align-items:center;gap:6px;flex-wrap:wrap;margin:0;padding:6px 14px;font-size:var(--text-meta);color:rgb(var(--c-text-muted))}
  /* Compact trades the card's breathing room for roughly a third more cards
     on screen. Only padding and gaps change: nothing is dropped, so the same
     card reads the same way at either density. */
  .is-compact .card,.is-compact .row{padding:5px 7px;margin-bottom:4px}
  .is-compact .card-repos,.is-compact .card-extra{margin-top:2px}
  .is-compact .labels{margin-top:3px}
  .is-compact .columns{gap:6px;padding:8px}
  .is-compact .cards{padding:4px}
</style>
