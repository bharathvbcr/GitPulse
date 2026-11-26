<script lang="ts">
  /**
   * The Agents plane: the agent checkouts, terminals and task attempts the
   * open repositories are holding.
   *
   * It stays mounted once opened, the same way Fleet and Tasks do. The
   * parent hides it. Unmounting it would drop the sweep and the task
   * watch, and the next open would briefly say there were no attempts
   * because the first read had not happened yet.
   *
   * A checkout, a process this window started, and a task attempt are
   * drawn from one projection (`plane.ts`). This file decides when to
   * look, which columns to keep, and where a row's actions go. Each action
   * goes through the owner of that move: a terminal is shown by
   * `focusTerminalSession` (surface, then the tab that hosts it, then its
   * dock, then the terminal), an attempt's terminal by `showTaskTerminal`,
   * and an attempt's task by `openTaskForRun`.
   */
  import { get } from "svelte/store";
  import {
    AlertCircle,
    Bell,
    Bot,
    Check,
    CheckCircle2,
    ChevronDown,
    ChevronRight,
    CircleAlert,
    CircleHelp,
    Columns3,
    FileDiff,
    FolderGit2,
    FolderOpen,
    Inbox,
    LayoutList,
    ListChecks,
    Loader2,
    Play,
    RefreshCw,
    Rows3,
    Search,
    ShieldAlert,
    SquareTerminal,
    X,
  } from "@lucide/svelte";
  import { agentRowActions, type AgentRowAction } from "../agents/rowActions";
  import { terminalLaunchRequests } from "../terminal/launchRequests";
  import type { SessionKind, TabKind } from "../terminal/tabs";
  import AgentInbox from "./AgentInbox.svelte";
  import LauncherIcon from "./LauncherIcon.svelte";
  import PageHeader from "./PageHeader.svelte";
  import RepoLogo from "./RepoLogo.svelte";
  import { formatAge } from "../storage/format";
  import { tick, untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { toastStore } from "../stores/toastStore";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { sessionActivity } from "../terminal/sessionActivity";
  import { focusTerminalSession } from "../terminal/sessionFocus";
  import { attemptNotices, taskTerminalRequests } from "../terminal/taskLaunches";
  import { terminalSessionLimit } from "../terminal/sessionLimit";
  import { createBoardAgents } from "../workbench/boardAgents";
  import { getTaskRun } from "../workbench/client";
  import { describeFailure } from "../ui/copy/describeFailure";
  import EmptyState from "./EmptyState.svelte";
  import Toggle from "./Toggle.svelte";
  import { LAYERS } from "../ui/layers";
  import { openTaskForRun, requestTaskOpen } from "../workbench/taskOpen";
  import { queuedTerminalNote, showTaskTerminal } from "../workbench/taskTerminal";
  import { agentDirectories } from "../agents/cwd";
  import { planeTerminals } from "../agents/repoAttention";
  import { agentPlaneStore, sweepTargets } from "../agents/store";
  import { isMacOS } from "../platform";
  import { gapGroupTitle, gapsBlockReading, groupGaps, type GapNote } from "../agents/gapGroups";
  import { agentKindLabel } from "../work/agentWorktree";
  import { agentsRepositoryScope, setAgentsRepositoryScope } from "../agents/scope";
  import { inboxFocusRequest, takeInboxFocus } from "../agents/inboxFocus";
  import { displayName, sameRepo } from "../repos/paths";
  import { maviStore } from "../mavi/store.svelte";
  import { maviAgentRows, maviHeadline } from "../mavi/agentsRows";
  import MaviAgentsTable from "./mavi/MaviAgentsTable.svelte";
  import { createRegisteredRepositories, repositoryPaths, taskProbeFromBoard } from "../agents/tasks";
  import {
    AGENT_COLUMNS,
    NOTIFICATION_REASONS,
    UNREAD_REASONS,
    applyAgentTree,
    countAgentRows,
    flattenAgentTree,
    isAgentFilter,
    planeHeadline,
    plural,
    projectAgentPlane,
    rollupSummary,
    scopeToRepository,
    type AgentColumnKey,
    type AgentFilter,
    type AgentRow,
    type AgentTreeEntry,
  } from "../agents/plane";
  import {
    attentionTrayHeading,
    collectAttentionAndDoneRows,
    countAgentFilters,
    groupAgentTreeByRepo,
    isRowActivelyRunning,
    isRowDone,
    searchAgentTree,
    searchMaviRows,
    type AgentRepoGroup,
  } from "../agents/viewModel";

  /** "Couldn't <verb> <object>. <Reason>." for a toast or an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const board = createBoardAgents();
  let now = $state(Date.now());
  /** Registered repositories, so a task names its repository and not its cwd. */
  const registered = createRegisteredRepositories();
  const registeredStatus = registered.status;

  const showing = $derived($interfaceStore.globalSurface === "agents");
  const hidden = $derived(new Set($interfaceStore.agentsHiddenColumns));
  const compact = $derived($interfaceStore.agentsCompact);
  const filter = $derived(isAgentFilter($interfaceStore.agentsFilter) ? $interfaceStore.agentsFilter : "all");
  let activeView = $state<"plane" | "inbox">("plane");
  let focusedInboxThreadKey = $state<string | null>(null);
  let inboxFocusSeq = $state(0);
  const activeRepoPath = $derived($agentsRepositoryScope || $repoStore.currentPath || "");

  $effect(() => {
    if (!$inboxFocusRequest) return;
    const req = takeInboxFocus();
    if (!req) return;
    setAgentsRepositoryScope(req.repoPath ?? null);
    focusedInboxThreadKey = req.threadKey;
    inboxFocusSeq = req.seq;
    activeView = "inbox";
  });

  // The one mapping the header shares (`repoAttention.ts`): unknown stays
  // unknown there, and the tab chip counts from this same sweep.
  const terminals = $derived(planeTerminals($terminalSessions, $sessionActivity, $agentDirectories));

  const repoPaths = $derived(repositoryPaths($registered, $repoStore.openTabs, pathOpts));

  const taskProbe = $derived(taskProbeFromBoard($board, {
    records: $terminalSessions,
    requests: $taskTerminalRequests,
    activity: (sessionId) => $sessionActivity.get(sessionId),
    notices: (runId) => $attemptNotices.get(runId),
    sessionLimit: $terminalSessionLimit,
    now,
    repositoryPath: (id) => repoPaths.get(id) ?? null,
  }));

  const plane = $derived(projectAgentPlane({
    probes: $agentPlaneStore.probes,
    terminals,
    tasks: taskProbe,
    paths: pathOpts,
  }));
  // The plane is a tree: a terminal that continues an attempt, and a child
  // agent process, sit under the row they came from. The scope and the filter
  // pick roots, and a kept root keeps everything under it; `collapsed` only
  // hides what is drawn. The headline counts rows (nested ones too), so a
  // nested row is never a row the filter quietly hid.
  let collapsed = $state<ReadonlySet<string>>(new Set());
  let collapsedRepos = $state<ReadonlySet<string>>(new Set());
  let focusedId = $state<string | null>(null);
  let treegrid = $state<HTMLTableElement | null>(null);
  let searchQuery = $state("");
  let searchInput = $state<HTMLInputElement | null>(null);
  let columnMenuOpen = $state(false);
  let columnsAnchor = $state<HTMLElement | null>(null);
  let columnsToggleBtn = $state<HTMLButtonElement | null>(null);
  let trayCollapsed = $state(false);

  const scoped = $derived(scopeToRepository(plane.roots, $agentsRepositoryScope, pathOpts));
  const filterCounts = $derived(countAgentFilters(scoped));
  const filtered = $derived(applyAgentTree(scoped, filter));
  const visible = $derived(searchAgentTree(filtered, searchQuery));
  const scopedRowCount = $derived(countAgentRows(scoped));
  const visibleRowCount = $derived(countAgentRows(visible));
  const entries = $derived(flattenAgentTree(visible, collapsed));
  const scopeLabel = $derived($agentsRepositoryScope ? displayName($agentsRepositoryScope) : "");
  // Mavi threads are agents too: listed under the plane's rows, in its
  // columns, under the same filter and scope, and counted in the headline.
  const rawMaviRows = $derived(
    maviAgentRows(maviStore.records, (record) => maviStore.summaryOf(record), (record) => maviStore.status(record), {
      filter,
      scope: $agentsRepositoryScope || null,
      sameRepo: (a, b) => sameRepo(a, b, pathOpts),
    }),
  );
  const maviRows = $derived(searchMaviRows(rawMaviRows, searchQuery));
  const groupMode = $derived($interfaceStore.agentsGroupByRepo);
  const effectiveGrouped = $derived(
    filter !== "attention" &&
      filter !== "notifications" &&
      !$agentsRepositoryScope &&
      (groupMode === "repo" || groupMode === "auto"),
  );
  const attentionTrayRows = $derived(collectAttentionAndDoneRows(visible));
  const showAttentionTray = $derived(effectiveGrouped && attentionTrayRows.length > 0);
  const trayHeading = $derived(attentionTrayHeading(attentionTrayRows));
  const trayCounts = $derived.by(() => {
    let blocking = 0;
    let error = 0;
    let done = 0;
    for (const row of attentionTrayRows) {
      if (row.attentionUrgency === "error" || row.attention.includes("error")) {
        blocking += 1;
        error += 1;
      } else if (
        row.attentionUrgency === "blocking" ||
        row.attention.some((reason) => BLOCKING_REASONS.has(reason))
      ) {
        blocking += 1;
      } else {
        done += 1;
      }
    }
    return { blocking, error, done };
  });
  const trayInformationalRows = $derived(attentionTrayRows.filter((row) => isInformationalRow(row)));
  const repoGroups = $derived(effectiveGrouped ? groupAgentTreeByRepo(visible, pathOpts) : []);

  type NavItem =
    | { kind: "group"; id: string; group: AgentRepoGroup; expanded: boolean; posInSet: number; setSize: number }
    | { kind: "row"; id: string; entry: AgentTreeEntry; level: number; groupKey: string | null };

  const navItems = $derived.by((): NavItem[] => {
    if (!effectiveGrouped) {
      return entries.map((entry) => ({
        kind: "row",
        id: entry.row.id,
        entry,
        level: entry.level,
        groupKey: null,
      }));
    }
    const items: NavItem[] = [];
    repoGroups.forEach((group, groupIndex) => {
      const expanded = !collapsedRepos.has(group.key);
      items.push({
        kind: "group",
        id: `repo-group:${group.key}`,
        group,
        expanded,
        posInSet: groupIndex + 1,
        setSize: repoGroups.length,
      });
      if (expanded) {
        for (const entry of flattenAgentTree(group.roots, collapsed)) {
          items.push({
            kind: "row",
            id: entry.row.id,
            entry,
            level: entry.level + 1,
            groupKey: group.key,
          });
        }
      }
    });
    return items;
  });

  /** Every drawn row by id, so a child process's actions can name its parent. */
  const rowsById = $derived(
    new Map(
      navItems
        .filter((item): item is Extract<NavItem, { kind: "row" }> => item.kind === "row")
        .map((item) => [item.id, item.entry.row]),
    ),
  );
  const maviClause = $derived(maviHeadline(maviRows));
  const headline = $derived(
    maviClause ? `${planeHeadline(plane, countAgentRows(visible))} · ${maviClause}` : planeHeadline(plane, countAgentRows(visible)),
  );
  // Exactly one row is in the tab order (a roving tabindex), and it is the one
  // last focused while that row is still drawn.
  const activeId = $derived(
    navItems.some((item) => item.id === focusedId) ? focusedId : (navItems[0]?.id ?? null),
  );
  const visibleColumnCount = $derived(
    1 +
      (columnVisible("checkout") ? 1 : 0) +
      (columnVisible("presence") ? 1 : 0) +
      (columnVisible("attention") ? 1 : 0) +
      (columnVisible("parallel") ? 1 : 0) +
      (columnVisible("changes") ? 1 : 0),
  );
  const scanning = $derived($agentPlaneStore.scanning);
  const macos = isMacOS();
  const notes = $derived<GapNote[]>([
    ...plane.gaps,
    ...($registeredStatus.error
      ? [{ kind: "partial" as const, repoPath: "", label: "Repositories", reason: `Couldn't read the registered repositories, so a run's repository is not named. ${$registeredStatus.error}` }]
      : !$registeredStatus.complete
        ? [{ kind: "partial" as const, repoPath: "", label: "Repositories", reason: "The registered repository list was capped. Some runs may not name their repository." }]
        : []),
  ]);
  // The same failure in every repository is one line naming them all, not
  // one line each; see agents/gapGroups.ts.
  const noteGroups = $derived(groupGaps(notes));
  const notesBlock = $derived(gapsBlockReading(noteGroups));

  const FILTERS: { id: AgentFilter; label: string }[] = [
    { id: "all", label: "All" },
    { id: "attention", label: "Needs action" },
    { id: "notifications", label: "Notifications" },
    { id: "gaps", label: "Gaps" },
    { id: "live", label: "Live" },
    { id: "parallel", label: "Parallel" },
  ];

  type ChipTone = "danger" | "warn" | "info" | "unknown" | "ok" | "neutral";

  const BLOCKING_REASONS = new Set<string>([
    "needs-you",
    "error",
    "pending",
    "blocked",
    "collision",
    "disconnected",
    "unstarted",
    "signalled",
  ]);

  function launcherKindFor(kind: string): TabKind {
    const k = kind.trim().toLowerCase();
    if (k === "claude" || k === "claude code" || k === "claude-code") return "claude";
    if (k === "agy" || k === "antigravity") return "agy";
    if (k === "codex" || k === "grok" || k === "manvi" || k === "shell" || k === "browser" || k === "file") {
      return k;
    }
    return "shell";
  }

  function attentionTone(row: AgentRow): ChipTone {
    if (
      row.attentionUrgency === "error" ||
      row.attention.includes("error") ||
      row.attention.includes("blocked") ||
      row.attention.includes("collision")
    ) {
      return "danger";
    }
    if (
      row.attentionUrgency === "blocking" ||
      row.attention.some((reason) => BLOCKING_REASONS.has(reason))
    ) {
      return "warn";
    }
    if (row.attention.some((reason) => NOTIFICATION_REASONS.has(reason))) {
      return "info";
    }
    if (row.attention.some((reason) => UNREAD_REASONS.has(reason))) {
      return "unknown";
    }
    return "neutral";
  }

  function isInformationalRow(row: AgentRow): boolean {
    if (!row.sessionId) return false;
    if (row.attentionUrgency === "blocking" || row.attentionUrgency === "error") return false;
    if (row.attention.some((reason) => BLOCKING_REASONS.has(reason))) return false;
    return row.attention.some((reason) => NOTIFICATION_REASONS.has(reason));
  }

  const MAX_DATE_MS = 8.64e15;

  function safeIsoDatetime(atMs: number | null): string | undefined {
    if (typeof atMs !== "number" || !Number.isFinite(atMs) || Math.abs(atMs) > MAX_DATE_MS) {
      return undefined;
    }
    return new Date(atMs).toISOString();
  }

  function attentionTimestamp(row: AgentRow): number | null {
    if (row.attentionAt !== null && row.attentionAt > 0) return row.attentionAt;
    if (!row.sessionId) return null;
    const outputAt = $sessionActivity.get(row.sessionId)?.lastOutputAt ?? null;
    return outputAt !== null && outputAt > 0 ? outputAt : null;
  }

  function dismissNotice(row: AgentRow) {
    if (!row.sessionId) return;
    sessionActivity.dismiss(row.sessionId);
  }

  function collectInformational(roots: readonly AgentRow[], out: AgentRow[] = []): AgentRow[] {
    for (const row of roots) {
      if (isInformationalRow(row)) out.push(row);
      if (row.children.length > 0) collectInformational(row.children, out);
    }
    return out;
  }

  const informationalRows = $derived(collectInformational(visible));

  function dismissAllNotices() {
    for (const row of informationalRows) {
      if (row.sessionId) sessionActivity.dismiss(row.sessionId);
    }
  }

  function toggleGrouping() {
    if (groupMode === "flat") {
      interfaceStore.setAgentsGroupByRepo("auto");
    } else {
      interfaceStore.setAgentsGroupByRepo("flat");
    }
  }

  const groupTooltip = $derived.by(() => {
    if ($agentsRepositoryScope) return "Showing one repository";
    if (filter === "attention" || filter === "notifications") {
      return "Filtered attention and notification rows are shown ungrouped";
    }
    if (effectiveGrouped) {
      return "Grouped by repository with Needs attention and finished tray above. Click to switch to flat list.";
    }
    return "Flat list. Click to group by repository.";
  });

  function columnVisible(key: AgentColumnKey): boolean {
    return !hidden.has(key);
  }

  function targets() {
    return sweepTargets(get(repoStore).openTabs);
  }

  function refresh() {
    agentDirectories.refresh();
    registered.refresh();
    void agentPlaneStore.refresh(targets());
  }

  /** The run behind a row, from the board's read when it has it. */
  async function runOf(runId: string) {
    return get(board).runs.find((run) => run.id === runId) ?? await getTaskRun(runId);
  }

  /**
   * Brings the row's terminal on screen in the tab that hosts it. An attempt
   * whose terminal is not here goes through `showTaskTerminal`, the owner of
   * starting or focusing an attempt's terminal.
   */
  async function showTerminal(row: AgentRow) {
    try {
      const record = row.liveKey ? get(terminalSessions).find((item) => item.key === row.liveKey) : undefined;
      if (record) {
        const outcome = await focusTerminalSession(record);
        if (outcome.ok) return;
        if (!row.taskRunId) {
          toastStore.error("That terminal can no longer be shown. It may have just ended.");
          return;
        }
      }
      if (!row.taskRunId) return;
      const run = await runOf(row.taskRunId);
      if ((await showTaskTerminal(run)) === "queued") toastStore.info(queuedTerminalNote(run.cwd));
    } catch (cause) {
      toastStore.error(failure("show", "the terminal", cause));
    }
  }

  async function openTask(row: AgentRow) {
    if (!row.taskRunId) return;
    try {
      await openTaskForRun(row.taskRunId);
    } catch (cause) {
      toastStore.error(failure("open", "this run's task", cause));
    }
  }

  async function openInboxTask(taskId: string) {
    try {
      requestTaskOpen(taskId);
    } catch (cause) {
      toastStore.error(failure("open", "this ask's task", cause));
    }
  }

  function openCheckout(row: AgentRow) {
    if (!row.checkoutPath) return;
    interfaceStore.setGlobalSurface("repository");
    void repoStore.openRepo(row.checkoutPath);
  }

  /** The row whose start is in flight; one at a time, as the launch channel holds one request. */
  let starting = $state<string | null>(null);

  /**
   * Starts `launcher` in the row's checkout, the way the Worktrees panel's
   * fork does: trust, open the checkout as the active tab, show its dock, then
   * hand the launch to that dock's panel (`terminalLaunchRequests`).
   */
  async function startIn(row: AgentRow, launcher: SessionKind) {
    const path = row.checkoutPath;
    if (!path || starting !== null) return;
    starting = row.id;
    try {
      if (!(await repoStore.trustRepo(path))) return;
      interfaceStore.setGlobalSurface("repository");
      let readyPath: string | null = null;
      const opened = await repoStore.openRepo(path, { activate: true, onReady: (canonical) => { readyPath = canonical; } });
      const at = opened ? (readyPath ?? path) : null;
      if (!at) {
        toastStore.error(`Couldn't open ${path}, so nothing was started.`);
        return;
      }
      const launched = terminalLaunchRequests.request(at, launcher);
      repoStore.setTerminalOpen(true);
      await launched;
    } catch (cause) {
      toastStore.error(failure("start", launcher === "shell" ? "the terminal" : agentKindLabel(launcher), cause));
    } finally {
      starting = null;
    }
  }

  async function reviewChanges(row: AgentRow) {
    if (!row.checkoutPath) return;
    try {
      await repoStore.previewUncommitted(row.checkoutPath);
      interfaceStore.setGlobalSurface("repository");
    } catch (cause) {
      toastStore.error(failure("open", "the changes", cause));
    }
  }

  function runAction(action: AgentRowAction) {
    const row = action.target;
    switch (action.kind) {
      case "terminal":
        return void showTerminal(row);
      case "task":
        return void openTask(row);
      case "start":
        return void startIn(row, action.launcher ?? "shell");
      case "review":
        return void reviewChanges(row);
      case "checkout":
        return openCheckout(row);
    }
  }

  function setExpanded(id: string, open: boolean) {
    const next = new Set(collapsed);
    if (open) next.delete(id);
    else next.add(id);
    collapsed = next;
  }

  function setRepoExpanded(key: string, open: boolean) {
    const next = new Set(collapsedRepos);
    if (open) next.delete(key);
    else next.add(key);
    collapsedRepos = next;
  }

  const allReposCollapsed = $derived(
    repoGroups.length > 0 && repoGroups.every((group) => collapsedRepos.has(group.key)),
  );

  function toggleCollapseAllRepos() {
    collapsedRepos = allReposCollapsed ? new Set() : new Set(repoGroups.map((group) => group.key));
  }

  /** Moves the roving focus to the item at `index` and puts the browser's focus on it. */
  async function focusNav(index: number) {
    const target = navItems[Math.max(0, Math.min(navItems.length - 1, index))];
    if (!target) return;
    focusedId = target.id;
    await tick();
    treegrid?.querySelector<HTMLElement>(`[data-row-id="${CSS.escape(target.id)}"]`)?.focus();
  }

  function isEditableTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    if (target.isContentEditable) return true;
    const tag = target.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
  }

  function onWindowKey(event: KeyboardEvent) {
    if (!showing || activeView !== "plane") return;
    if (columnMenuOpen && event.key === "Escape") {
      columnMenuOpen = false;
      event.preventDefault();
      columnsToggleBtn?.focus();
      return;
    }
    if (event.key !== "/" || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
    if (isEditableTarget(event.target)) return;
    event.preventDefault();
    searchInput?.focus();
  }

  function onWindowPointerDown(event: PointerEvent) {
    if (!columnMenuOpen) return;
    if (columnsAnchor && event.target instanceof Node && !columnsAnchor.contains(event.target)) {
      columnMenuOpen = false;
    }
  }

  function onGroupKey(event: KeyboardEvent, item: Extract<NavItem, { kind: "group" }>, index: number) {
    if (event.target !== event.currentTarget || event.altKey || event.ctrlKey || event.metaKey) return;
    switch (event.key) {
      case "ArrowDown":
        void focusNav(index + 1);
        break;
      case "ArrowUp":
        void focusNav(index - 1);
        break;
      case "Home":
        void focusNav(0);
        break;
      case "End":
        void focusNav(navItems.length - 1);
        break;
      case "ArrowRight":
        if (!item.expanded) setRepoExpanded(item.group.key, true);
        else if (navItems[index + 1]?.kind === "row") void focusNav(index + 1);
        break;
      case "ArrowLeft":
        if (item.expanded) setRepoExpanded(item.group.key, false);
        break;
      case "Enter":
      case " ":
        setRepoExpanded(item.group.key, !item.expanded);
        break;
      default:
        return;
    }
    event.preventDefault();
  }

  /**
   * The treegrid keys. Up and Down move between drawn rows; Right opens a
   * closed row and then steps into it; Left closes an open row and then steps
   * out to its parent; Enter runs the primary action. Keys that land on a
   * button inside the row are the button's own.
   */
  function onRowKey(event: KeyboardEvent, item: Extract<NavItem, { kind: "row" }>, index: number) {
    if (event.target !== event.currentTarget || event.altKey || event.ctrlKey || event.metaKey) return;
    const entry = item.entry;
    switch (event.key) {
      case "ArrowDown":
        void focusNav(index + 1);
        break;
      case "ArrowUp":
        void focusNav(index - 1);
        break;
      case "Home":
        void focusNav(0);
        break;
      case "End":
        void focusNav(navItems.length - 1);
        break;
      case "ArrowRight":
        if (entry.expandable && !entry.expanded) setExpanded(entry.row.id, true);
        else if (entry.expandable) void focusNav(index + 1);
        break;
      case "ArrowLeft":
        if (entry.expanded) setExpanded(entry.row.id, false);
        else if (entry.level > 1) {
          let up = index - 1;
          while (up > 0) {
            const prev = navItems[up];
            if (prev.kind !== "row" || prev.entry.level < entry.level) break;
            up -= 1;
          }
          void focusNav(up);
        } else if (item.groupKey) {
          const groupIdx = navItems.findIndex(
            (candidate) => candidate.kind === "group" && candidate.group.key === item.groupKey,
          );
          if (groupIdx >= 0) void focusNav(groupIdx);
        }
        break;
      case "Enter": {
        const parent = entry.row.parentId ? rowsById.get(entry.row.parentId) ?? null : null;
        const primary = agentRowActions(entry.row, parent, $interfaceStore.terminalLauncher)[0];
        if (primary) runAction(primary);
        break;
      }
      default:
        return;
    }
    event.preventDefault();
  }

  function presenceWord(row: AgentRow): string {
    if (row.presence === "live") return "Live";
    if (row.presence === "exited") return "Exited";
    if (row.presence === "on-disk") return "On disk";
    return "Not in this window";
  }

  $effect(() => {
    if (!showing) return;
    board.start();
    // The "x ago" clock stops while the window is hidden and catches up when
    // it returns, rather than waking the renderer every second behind it.
    const stopClock = createAdaptiveTimer(() => {
      now = Date.now();
    }, 1000);
    return () => {
      stopClock();
      board.stop();
    };
  });

  // The tab set as a value: repoStore rebuilds openTabs on every publish, so
  // the sweep depends on this key and reads the tabs themselves untracked.
  const openTabs = $derived($repoStore.openTabs);
  const sweepTabsKey = $derived(
    openTabs.map((tab) => `${tab.path}\0${tab.label}\0${tab.family ?? ""}\0${tab.familyRoot ?? ""}`).join("\n"),
  );
  $effect(() => {
    const open = showing;
    void sweepTabsKey;
    untrack(() => {
      if (!open) agentPlaneStore.cancel();
      else void agentPlaneStore.refresh(sweepTargets(openTabs));
    });
  });

  $effect(() => {
    if (!showing) return;
    // A run naming a repository the last read did not have asks again, so a
    // repository registered after the plane opened is still named.
    registered.want($board.runs.map((run) => run.repository_id));
  });
</script>

<svelte:window onkeydown={onWindowKey} onpointerdown={onWindowPointerDown} />

<div
  class="gp-workspace flex-1 flex flex-col min-h-0 min-w-0 bg-background"
  data-testid="agents-view"
  role="region"
  aria-label="Agents"
>
  <!-- The same header as every page (PageHeader): title and what it counts on
       the left; the view switch and actions on the right, where Tasks keeps
       Board/List. -->
  <PageHeader title="Agents" level={1} description={headline} descriptionTestId="agents-headline">
    {#snippet actions()}
      <div class="gp-segmented" class:gp-liquid-tabs={macos} role="group" aria-label="Agents view">
        <button
          type="button"
          class="gp-seg-btn inline-flex items-center gap-1.5"
          data-active={activeView === "plane"}
          aria-pressed={activeView === "plane"}
          onclick={() => (activeView = "plane")}
          data-testid="agents-view-mode-plane"
        >
          <LayoutList size={12} />
          <span>Plane</span>
        </button>
        <button
          type="button"
          class="gp-seg-btn inline-flex items-center gap-1.5"
          data-active={activeView === "inbox"}
          aria-pressed={activeView === "inbox"}
          onclick={() => (activeView = "inbox")}
          data-testid="agents-view-mode-inbox"
        >
          <Inbox size={12} />
          <span>Inbox</span>
        </button>
      </div>
        <button
          type="button"
          class="gp-btn" data-size="sm"
          onclick={refresh}
          disabled={scanning}
        >
          <RefreshCw size={11} class={scanning ? "animate-spin" : ""} />
          <span>Refresh</span>
        </button>
        {#if scanning}
          <button type="button" class="gp-btn" data-size="sm" onclick={() => agentPlaneStore.cancel()}>
            Cancel
          </button>
        {/if}
        <button
          type="button"
          class="gp-icon-btn"
          onclick={() => interfaceStore.setAgentsOpen(false)}
          title="Close Agents"
          aria-label="Close Agents"
        >
          <X size={14} />
        </button>
    {/snippet}
  </PageHeader>

  <!-- Filters and columns act on the plane's table, so they live with it: the
       Inbox has neither, and showing them there offered controls that did
       nothing. Each group is labelled rather than told apart by a divider. -->
  {#if activeView === "plane"}
    <div class="shrink-0 min-w-0 flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3.5 py-2" data-testid="agents-toolbar">
      {#if scopeLabel && $agentsRepositoryScope}
        <span class="gp-chip" data-tone="accent" data-kind="removable" data-testid="agents-scope">
          <RepoLogo path={$agentsRepositoryScope} name={scopeLabel} size={12} />
          <span>Only {scopeLabel}</span>
          <button
            type="button"
            class="gp-icon-btn"
            data-size="sm"
            aria-label="Show every repository"
            title="Show every repository"
            onclick={() => setAgentsRepositoryScope(null)}
          >
            <X size={10} />
          </button>
        </span>
      {/if}
      <div class="relative min-w-64 flex-1 max-w-80" data-width-cap="toolbar search field">
        <Search size={12} class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted" aria-hidden="true" />
        <input
          bind:this={searchInput}
          type="text"
          role="searchbox"
          class="gp-field w-full pl-7 pr-8"
          data-testid="agents-search"
          placeholder="Search agents, repositories, branches…"
          aria-label="Search agents, repositories, branches"
          bind:value={searchQuery}
          onkeydown={(event) => {
            if (event.key === "Escape" && searchQuery !== "") {
              searchQuery = "";
              event.preventDefault();
              event.stopPropagation();
            } else if (event.key === "ArrowDown" && navItems.length > 0) {
              event.preventDefault();
              void focusNav(0);
            }
          }}
        />
        {#if searchQuery}
          <button
            type="button"
            class="gp-icon-btn absolute right-1 top-1/2 -translate-y-1/2"
            data-size="sm"
            aria-label="Clear search"
            title="Clear search"
            onclick={() => {
              searchQuery = "";
              searchInput?.focus();
            }}
          >
            <X size={11} />
          </button>
        {:else}
          <kbd
            class="gp-keycap pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-micro text-textMuted"
            aria-hidden="true"
          >/</kbd>
        {/if}
      </div>
      {#if searchQuery.trim() !== ""}
        <span class="text-meta text-textMuted tabular-nums shrink-0" data-testid="agents-search-count">
          {visibleRowCount} of {scopedRowCount}
        </span>
      {/if}
      <!-- One filter at a time, so a segmented switch, not toggles. -->
      <div class="flex min-w-0 max-w-full items-center gap-1.5" role="group" aria-label="Show">
        <span class="gp-label shrink-0">Show</span>
        <div class="gp-segmented gp-header-scroll min-w-0 max-w-full flex-nowrap" class:gp-liquid-tabs={macos}>
          {#each FILTERS as item (item.id)}
            {@const count = filterCounts[item.id]}
            <button
              type="button"
              class="gp-seg-btn inline-flex items-center gap-1.5 shrink-0 whitespace-nowrap text-meta!"
              data-active={filter === item.id}
              aria-pressed={filter === item.id}
              data-testid="agents-filter-{item.id}"
              onclick={() => interfaceStore.setAgentsFilter(item.id)}
            >
              <span>{item.label}</span>
              {#if count > 0}
                <span
                  class="gp-chip"
                  data-kind="count"
                  data-size="inline"
                  data-tone={item.id === "attention" ? "warn" : "neutral"}
                >
                  {count}
                </span>
              {/if}
            </button>
          {/each}
        </div>
      </div>
      <div class="relative shrink-0" bind:this={columnsAnchor}>
        <button
          bind:this={columnsToggleBtn}
          type="button"
          class="gp-btn inline-flex items-center gap-1.5"
          data-size="sm"
          data-testid="agents-columns-toggle"
          aria-expanded={columnMenuOpen}
          aria-haspopup="true"
          title="Choose which columns the table shows"
          onclick={() => (columnMenuOpen = !columnMenuOpen)}
        >
          <Columns3 size={12} aria-hidden="true" />
          <span>Columns</span>
          {#if hidden.size > 0}
            <span class="tabular-nums text-textMuted">({AGENT_COLUMNS.length - hidden.size}/{AGENT_COLUMNS.length})</span>
          {/if}
        </button>
        {#if columnMenuOpen}
          <div
            class="absolute right-0 top-full mt-1 gp-menu gp-pop p-2 w-48 flex flex-col gap-0.5"
            style="z-index: {LAYERS.MENU}"
            role="group"
            aria-label="Columns"
            data-testid="agents-columns-menu"
          >
            {#each AGENT_COLUMNS as column (column.key)}
              <Toggle
                label={column.label}
                checked={columnVisible(column.key)}
                onchange={() => interfaceStore.toggleAgentsColumn(column.key)}
              />
            {/each}
            <span class="gp-menu-sep" aria-hidden="true"></span>
            <button
              type="button"
              class="gp-btn self-start"
              data-variant="ghost"
              data-size="sm"
              disabled={hidden.size === 0}
              onclick={() => interfaceStore.showAllAgentsColumns()}
            >
              Show all columns
            </button>
          </div>
        {/if}
      </div>
      <button
        type="button"
        class="gp-btn inline-flex items-center gap-1.5 {effectiveGrouped ? 'text-accent bg-accent/15' : ''}"
        data-size="sm"
        data-testid="agents-group-toggle"
        data-group-mode={groupMode}
        aria-pressed={effectiveGrouped}
        disabled={Boolean($agentsRepositoryScope) || filter === "attention" || filter === "notifications"}
        title={groupTooltip}
        onclick={toggleGrouping}
      >
        <FolderGit2 size={12} aria-hidden="true" />
        <span>By repository</span>
      </button>
      {#if effectiveGrouped && repoGroups.length > 1}
        <button
          type="button"
          class="gp-btn"
          data-size="sm"
          data-testid="agents-groups-collapse-all"
          onclick={toggleCollapseAllRepos}
        >
          {allReposCollapsed ? "Expand all" : "Collapse all"}
        </button>
      {/if}
      {#if informationalRows.length > 0}
        <button
          type="button"
          class="gp-btn inline-flex items-center gap-1.5"
          data-size="sm"
          data-testid="agents-dismiss-all"
          title="Dismiss all informational notifications without stopping their terminals"
          onclick={dismissAllNotices}
        >
          <Check size={11} aria-hidden="true" />
          <span>Dismiss all</span>
        </button>
      {/if}
      <button
        type="button"
        class="gp-icon-btn ml-auto"
        aria-pressed={compact}
        title={compact ? "Comfortable rows" : "Compact rows"}
        aria-label={compact ? "Use comfortable rows" : "Use compact rows"}
        onclick={() => interfaceStore.toggleAgentsCompact()}
      >
        <Rows3 size={14} />
      </button>
    </div>
  {/if}

  {#if activeView === "inbox"}
    <!-- No inset: the Inbox spans the pane like the Plane's table does. -->
    <div class="flex-1 min-h-0 min-w-0 overflow-hidden flex flex-col" data-testid="agents-inbox-container">
      <AgentInbox
        repoPath={activeRepoPath}
        embedded={true}
        focusThreadKey={focusedInboxThreadKey}
        focusSeq={inboxFocusSeq}
        onopenTask={openInboxTask}
      />
    </div>
  {:else}
    {#if noteGroups.length > 0}
      <!-- One card, above the table so a hidden column never hides it. A
           failure every repository shares is one line naming them all. -->
      <section
        class="gp-notice mx-3! mt-2! shrink-0 flex-col! gap-0! p-0!"
        data-tone="unknown"
        data-testid="agents-gaps"
        aria-label="What the plane couldn't read"
      >
        <div class="flex flex-wrap items-center gap-2 px-3 pt-2 pb-1">
          <CircleAlert size={13} class="shrink-0" style="color: var(--gp-notice-ink)" />
          <span class="text-meta font-semibold text-textPrimary">
            {notesBlock ? "Not everything could be read" : "Some of this is partial"}
          </span>
          <span class="text-meta text-textMuted">{plural(notes.length, "note", "notes")}</span>
          <button
            type="button"
            class="gp-btn ml-auto" data-size="sm"
            onclick={refresh}
            disabled={scanning}
            data-testid="agents-gaps-retry"
          >
            <RefreshCw size={11} class={scanning ? "animate-spin" : ""} />
            <span>{scanning ? "Reading…" : "Try again"}</span>
          </button>
        </div>
        <ul class="flex flex-col gap-1.5 px-3 pb-2.5 pl-[33px] max-h-56 overflow-y-auto">
          {#each noteGroups as group (group.key)}
            <li class="text-meta text-textSecondary min-w-0" role="status" data-gap-kind={group.kind}>
              <span class="font-medium text-textPrimary">{gapGroupTitle(group)}.</span> {group.reason}
              {#if group.labels.length > 1}
                <!-- Every name, wrapped: the card scrolls, so none is dropped. -->
                <span class="mt-0.5 block text-textMuted" data-gap-names>{group.labels.join(", ")}</span>
              {/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if showAttentionTray}
      <section
        data-testid="agents-attention-tray"
        aria-label={trayHeading}
        class="mx-3 mt-2 shrink-0 rounded-lg bg-surface/70 p-2.5"
      >
        <div class="flex flex-wrap items-center gap-2 min-w-0">
          {#if trayCounts.error > 0}
            <AlertCircle size={13} class="text-danger shrink-0" aria-hidden="true" />
          {:else if trayCounts.blocking > 0}
            <ShieldAlert size={13} class="text-warn shrink-0" aria-hidden="true" />
          {:else}
            <CheckCircle2 size={13} class="text-ok shrink-0" aria-hidden="true" />
          {/if}
          <span class="text-meta font-semibold text-textPrimary">{trayHeading}</span>
          {#if trayCounts.blocking > 0}
            <span
              class="gp-chip"
              data-tone={trayCounts.error === trayCounts.blocking ? "danger" : "warn"}
              data-testid="agents-tray-blocking-count"
            >
              {trayCounts.blocking} need action
            </span>
          {/if}
          {#if trayCounts.done > 0}
            <span class="gp-chip" data-tone="info" data-testid="agents-tray-done-count">
              {trayCounts.done} finished
            </span>
          {/if}
          {#if trayInformationalRows.length > 0}
            <button
              type="button"
              class="gp-btn"
              data-size="sm"
              data-testid="agents-tray-dismiss-all"
              title="Dismiss all informational notifications without stopping their terminals"
              onclick={dismissAllNotices}
            >
              <Check size={11} aria-hidden="true" />
              <span>Dismiss all</span>
            </button>
          {/if}
          <button
            type="button"
            class="gp-icon-btn ml-auto"
            data-size="sm"
            data-testid="agents-attention-tray-toggle"
            aria-expanded={!trayCollapsed}
            aria-label="{trayCollapsed ? 'Expand' : 'Collapse'} {trayHeading}"
            title={trayCollapsed ? "Expand" : "Collapse"}
            onclick={() => (trayCollapsed = !trayCollapsed)}
          >
            {#if trayCollapsed}<ChevronRight size={12} />{:else}<ChevronDown size={12} />{/if}
          </button>
        </div>
        {#if !trayCollapsed}
          <ul role="list" class="mt-2 flex flex-col gap-1 max-h-56 overflow-y-auto">
            {#each attentionTrayRows as row (row.id)}
              {@const tone = attentionTone(row)}
              {@const attTs = attentionTimestamp(row)}
              {@const primaryAction = agentRowActions(row, row.parentId ? rowsById.get(row.parentId) ?? null : null, $interfaceStore.terminalLauncher)[0]}
              <li
                class="flex flex-wrap items-center gap-2 rounded-md px-2 py-1.5 hover:bg-surfaceHover/60"
                data-testid="agents-attention-item"
                data-row-id={row.id}
              >
                {#if tone === "danger"}
                  <AlertCircle size={12} class="text-danger shrink-0" aria-hidden="true" />
                {:else if tone === "warn"}
                  <ShieldAlert size={12} class="text-warn shrink-0" aria-hidden="true" />
                {:else if isRowDone(row) && row.attention.includes("finished")}
                  <CheckCircle2 size={12} class="text-ok shrink-0" aria-hidden="true" />
                {:else}
                  <Bell size={12} class="text-info shrink-0" aria-hidden="true" />
                {/if}
                {#if row.repoPath}
                  <RepoLogo path={row.repoPath} name={row.repoLabel} size={16} />
                {/if}
                <LauncherIcon kind={launcherKindFor(row.launcher || row.kind)} size={13} class="shrink-0" />
                <div class="min-w-0 flex-1">
                  <div class="flex flex-wrap items-center gap-1.5 min-w-0">
                    <span class="font-medium text-textPrimary truncate">{row.session}</span>
                    <span class="text-meta text-textMuted truncate">
                      {row.repoLabel}{row.checkout ? ` · ${row.checkout}` : ""}
                    </span>
                    <span
                      class="gp-chip font-medium"
                      data-tone={tone}
                      data-urgency={row.attentionUrgency ?? undefined}
                    >
                      <span>{row.attentionLabel || "—"}</span>
                    </span>
                    {#if attTs !== null}
                      <time class="text-micro text-textMuted tabular-nums shrink-0" datetime={safeIsoDatetime(attTs)}>
                        {formatAge(attTs, now)}
                      </time>
                    {/if}
                  </div>
                  {#if row.attentionDetail}
                    <p class="m-0 mt-0.5 truncate text-meta text-textMuted" title={row.attentionDetail}>
                      {row.attentionDetail}
                    </p>
                  {/if}
                </div>
                <div class="flex items-center gap-1 shrink-0 ml-auto">
                  {#if primaryAction}
                    <button
                      type="button"
                      class="gp-btn"
                      data-size="sm"
                      data-action={primaryAction.kind}
                      aria-label={primaryAction.ariaLabel}
                      title={primaryAction.title}
                      disabled={primaryAction.kind === "start" && starting !== null}
                      onclick={() => runAction(primaryAction)}
                    >
                      {#if primaryAction.kind === "terminal"}<SquareTerminal size={11} />
                      {:else if primaryAction.kind === "task"}<ListChecks size={11} />
                      {:else if primaryAction.kind === "start"}<Play size={11} />
                      {:else if primaryAction.kind === "review"}<FileDiff size={11} />
                      {:else}<FolderOpen size={11} />{/if}
                      <span>{primaryAction.kind === "start" && starting === primaryAction.target.id ? "Starting…" : primaryAction.label}</span>
                    </button>
                  {/if}
                  {#if isInformationalRow(row)}
                    <button
                      type="button"
                      class="gp-btn"
                      data-size="sm"
                      data-action="dismiss"
                      aria-label="Dismiss notification for {row.session}"
                      title="Dismiss this notification without stopping the terminal"
                      onclick={() => dismissNotice(row)}
                    >
                      <Check size={11} />
                      <span>Dismiss</span>
                    </button>
                  {/if}
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    {/if}

  <div class="gp-page">
    {#if visible.length === 0 && maviRows.length > 0}
      <!-- Mavi threads below are agent work, so no "nothing here" state; an
           unread or failed plane still says so rather than looking quiet. -->
      {#if plane.gaps.some((gap) => gap.kind === "failed" || gap.kind === "skipped")}
        <div class="gp-page-body"><div class="gp-notice" data-tone="unknown" role="status">Sessions couldn't be read. The notes above are the reason, not an empty workspace.</div></div>
      {:else if plane.gaps.length > 0 || scanning}
        <div class="gp-page-body"><div class="gp-notice" data-tone="unknown" role="status">Still reading. A note above says what has not come back, and an empty list is not a quiet workspace.</div></div>
      {/if}
    {:else if visible.length === 0}
      <div class="gp-page-body" data-testid="agents-empty">
        {#if searchQuery.trim() !== ""}
          <EmptyState
            compact
            icon={Search}
            title={`No agents match “${searchQuery.trim()}”`}
            hint={filter !== "all" ? "Active filter may also hide matching rows." : "Try another session, repository, or branch."}
            action={{
              label: "Clear search",
              onClick: () => {
                searchQuery = "";
                searchInput?.focus();
              },
              icon: X,
            }}
          />
          {#if filter !== "all"}
            <div class="mt-2 flex justify-center">
              <button
                type="button"
                class="gp-btn"
                data-size="sm"
                data-testid="agents-empty-show-all"
                onclick={() => interfaceStore.setAgentsFilter("all")}
              >
                Show all
              </button>
            </div>
          {/if}
        {:else if scopeLabel && plane.rows.length > 0 && scoped.length === 0}
          <EmptyState
            compact
            icon={Bot}
            title="No agent work in {scopeLabel}"
            hint="No agent checkouts, terminals or task runs here. Other repositories have {plural(plane.rows.length, "row", "rows")}; clear the scope to see them."
            action={{
              label: "Show all repositories",
              onClick: () => setAgentsRepositoryScope(null),
              icon: X,
            }}
          />
        {:else if plane.rows.length > 0}
          <EmptyState
            compact
            icon={Bot}
            title="Nothing matches this filter"
            hint="The headline still counts the {plural(plane.rows.length, "row", "rows")} it hid."
            action={{
              label: "Show all",
              onClick: () => interfaceStore.setAgentsFilter("all"),
              icon: X,
            }}
          />
        {:else if plane.gaps.some((gap) => gap.kind === "failed" || gap.kind === "skipped")}
          <div class="gp-notice" data-tone="unknown" role="status">Sessions couldn't be read. The notes above are the reason, not an empty workspace.</div>
        {:else if plane.gaps.length > 0 || scanning}
          <div class="gp-notice" data-tone="unknown" role="status">Still reading. A note above says what has not come back, and an empty list is not a quiet workspace.</div>
        {:else}
          <EmptyState compact icon={Bot} title="No agent work yet" hint="No agent checkouts, terminals or task runs in the open repositories. A checkout appears when its path is an agent worktree. A terminal appears when this window started it." />
        {/if}
      </div>
    {:else if visible.length > 0}
      <!-- A table whose rows nest is a treegrid (ARIA authoring practices); the
           rows carry their level and open state, and the arrow keys move through
           them (harness/agents.html drives them). -->
      <!-- Justified: Svelte's rule only knows the flat table, not the treegrid. -->
      <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
      <table
        bind:this={treegrid}
        role="treegrid"
        aria-label="Agent checkouts, terminals, task runs and the child sessions under them"
        class="w-full text-left border-collapse {compact ? 'text-meta' : 'text-xs'}"
      >
        <caption class="sr-only">Agent checkouts, terminals and task runs across open repositories</caption>
        <thead class="sticky top-0 bg-background text-textMuted">
          <tr>
            <th scope="col" class="px-4 py-2 font-medium">Agent</th>
            {#if columnVisible("checkout")}<th scope="col" class="px-3 py-2 font-medium">Checkout</th>{/if}
            {#if columnVisible("presence")}<th scope="col" class="px-3 py-2 font-medium">Presence</th>{/if}
            {#if columnVisible("attention")}<th scope="col" class="px-3 py-2 font-medium">Attention</th>{/if}
            {#if columnVisible("parallel")}<th scope="col" class="px-3 py-2 font-medium">Parallel</th>{/if}
            {#if columnVisible("changes")}<th scope="col" class="px-3 py-2 font-medium">Changes</th>{/if}
          </tr>
        </thead>
        <tbody>
          {#each navItems as item, index (item.id)}
            {#if item.kind === "group"}
              {@const group = item.group}
              <tr
                class="bg-surface/60 hover:bg-surfaceHover/60 cursor-pointer focus-visible:outline-2 focus-visible:outline-accent"
                data-testid="agents-repo-group"
                data-repo-path={group.repoPath}
                data-row-id={item.id}
                aria-level={1}
                aria-posinset={item.posInSet}
                aria-setsize={item.setSize}
                aria-expanded={item.expanded}
                tabindex={item.id === activeId ? 0 : -1}
                onclick={() => setRepoExpanded(group.key, !item.expanded)}
                onfocus={(event) => {
                  if (event.target === event.currentTarget) focusedId = item.id;
                }}
                onkeydown={(event) => onGroupKey(event, item, index)}
              >
                <td colspan={visibleColumnCount} class="px-4 py-1.5">
                  <div class="flex flex-wrap items-center gap-2 min-w-0">
                    <button
                      type="button"
                      class="gp-icon-btn shrink-0"
                      data-size="sm"
                      tabindex="-1"
                      aria-label="{item.expanded ? 'Collapse' : 'Expand'} {group.repoLabel}"
                      title={item.expanded ? "Collapse" : "Expand"}
                      data-testid="agents-repo-group-toggle"
                      onclick={(event) => {
                        event.stopPropagation();
                        setRepoExpanded(group.key, !item.expanded);
                      }}
                    >
                      {#if item.expanded}<ChevronDown size={12} />{:else}<ChevronRight size={12} />{/if}
                    </button>
                    {#if group.repoPath}
                      <RepoLogo path={group.repoPath} name={group.repoLabel} size={16} />
                    {:else}
                      <Bot size={16} class="shrink-0 text-textMuted" aria-hidden="true" />
                    {/if}
                    <span class="font-semibold text-textPrimary truncate">{group.repoLabel}</span>
                    <span class="gp-chip" data-kind="count" data-tone="neutral">{group.total}</span>
                    {#if group.running > 0}
                      <span class="gp-chip" data-tone="accent" data-testid="agents-repo-group-running">
                        <Loader2 size={11} class="gp-agent-spinner animate-spin shrink-0" aria-hidden="true" />
                        <span>{group.running} running</span>
                      </span>
                    {/if}
                    {#if group.attention > 0}
                      <span class="gp-chip" data-tone="warn" data-testid="agents-repo-group-attention">
                        <ShieldAlert size={11} aria-hidden="true" />
                        <span>{group.attention} {group.attention === 1 ? "needs" : "need"} attention</span>
                      </span>
                    {/if}
                    {#if group.done > 0}
                      <span class="gp-chip" data-tone="info" data-testid="agents-repo-group-done">
                        <CheckCircle2 size={11} class="text-ok shrink-0" aria-hidden="true" />
                        <span>{group.done} done</span>
                      </span>
                    {/if}
                    {#if repoGroups.length > 1 && group.repoPath}
                      <button
                        type="button"
                        class="gp-btn ml-auto"
                        data-size="sm"
                        data-testid="agents-repo-focus"
                        aria-label="Focus repository {group.repoLabel}"
                        onclick={(event) => {
                          event.stopPropagation();
                          setAgentsRepositoryScope(group.repoPath);
                        }}
                      >
                        Focus repository
                      </button>
                    {/if}
                  </div>
                </td>
              </tr>
            {:else}
            {@const entry = item.entry}
            {@const row = entry.row}
            {@const under = rollupSummary(row)}
            {@const activelyRunning = isRowActivelyRunning(row)}
            {@const rowDone = isRowDone(row)}
            <tr
              class="hover:bg-surfaceHover/60 focus-visible:outline-2 focus-visible:outline-accent"
              data-testid="agents-row"
              data-row-id={row.id}
              data-level={item.level}
              data-origin={row.origin}
              aria-level={item.level}
              aria-posinset={entry.posInSet}
              aria-setsize={entry.setSize}
              aria-expanded={entry.expandable ? entry.expanded : undefined}
              tabindex={row.id === activeId ? 0 : -1}
              onfocus={(event) => {
                if (event.target === event.currentTarget) focusedId = row.id;
              }}
              onkeydown={(event) => onRowKey(event, item, index)}
            >
              <th scope="row" class="py-2 pr-4 font-normal text-left align-top" style:padding-left="{16 + (item.level - 1) * 20}px">
                <span class="flex items-start gap-1">
                  {#if entry.expandable}
                    <button
                      type="button"
                      class="gp-icon-btn mt-0.5 shrink-0" data-size="sm"
                      tabindex="-1"
                      aria-label="{entry.expanded ? 'Collapse' : 'Expand'} {row.session}"
                      title={entry.expanded ? "Collapse" : "Expand"}
                      data-testid="agents-row-toggle"
                      onclick={() => setExpanded(row.id, !entry.expanded)}
                    >
                      {#if entry.expanded}<ChevronDown size={11} />{:else}<ChevronRight size={11} />{/if}
                    </button>
                  {:else if entry.level > 1}
                    <span class="w-4 shrink-0" aria-hidden="true"></span>
                  {/if}
                  <LauncherIcon kind={launcherKindFor(row.launcher || row.kind)} size={14} class="mt-0.5 shrink-0" />
                  <span class="min-w-0">
                <span class="flex items-center gap-1.5 font-medium text-textPrimary">
                  {#if activelyRunning && !columnVisible("attention")}
                    <Loader2 size={11} class="gp-agent-spinner animate-spin text-accent shrink-0" aria-hidden="true" />
                  {/if}
                  <span class="truncate">{row.session}</span>
                </span>
                <span class="mt-0.5 flex items-center gap-1.5 text-textMuted min-w-0">
                  <span class="shrink-0">{agentKindLabel(row.kind)}</span>
                  {#if !effectiveGrouped && !$agentsRepositoryScope}
                    <span aria-hidden="true">·</span>
                    {#if row.repoPath}
                      <RepoLogo path={row.repoPath} name={row.repoLabel} size={14} />
                    {/if}
                    <span class="truncate">{row.repoLabel}</span>
                  {/if}
                </span>
                <span class="mt-1 flex flex-wrap gap-1" data-testid="agents-row-actions">
                  {#each agentRowActions(row, row.parentId ? rowsById.get(row.parentId) ?? null : null, $interfaceStore.terminalLauncher) as action (action.kind)}
                    <button
                      type="button"
                      class="gp-btn" data-size="sm"
                      data-action={action.kind}
                      aria-label={action.ariaLabel}
                      title={action.title}
                      disabled={action.kind === "start" && starting !== null}
                      onclick={() => runAction(action)}
                    >
                      {#if action.kind === "terminal"}<SquareTerminal size={11} />
                      {:else if action.kind === "task"}<ListChecks size={11} />
                      {:else if action.kind === "start"}<Play size={11} />
                      {:else if action.kind === "review"}<FileDiff size={11} />
                      {:else}<FolderOpen size={11} />{/if}
                      <span>{action.kind === "start" && starting === action.target.id ? "Starting…" : action.label}</span>
                    </button>
                  {/each}
                </span>
                  </span>
                </span>
              </th>
              {#if columnVisible("checkout")}
                <td class="px-3 py-2 align-top text-textSecondary">{row.checkout}</td>
              {/if}
              {#if columnVisible("presence")}
                <td class="px-3 py-2 align-top" title={row.presenceDetail}>
                  <span class="inline-flex items-center gap-1.5">
                    {#if row.presence === "live"}
                      <span class="gp-live-dot" aria-hidden="true"></span>
                    {/if}
                    <span>{presenceWord(row)}</span>
                  </span>
                  {#if under.presence}
                    <span class="block text-textMuted" data-testid="agents-row-rollup-presence">{under.presence}</span>
                  {/if}
                </td>
              {/if}
              {#if columnVisible("attention")}
                {@const attTs = attentionTimestamp(row)}
                <td class="px-3 py-2 align-top min-w-0">
                  {#if !row.attentionObserved}
                    <span
                      class="gp-chip font-medium"
                      data-tone="unknown"
                      title="Nothing in this window watches what this process asks for."
                    >
                      <CircleHelp size={11} />
                      <span>Not observed</span>
                    </span>
                  {:else if row.attention.length > 0 || row.attentionLabel}
                    {@const tone = attentionTone(row)}
                    <div class="flex flex-wrap items-center gap-1.5 min-w-0">
                      <span
                        class="gp-chip font-medium"
                        data-tone={tone}
                        data-urgency={row.attentionUrgency ?? undefined}
                      >
                        {#if tone === "danger"}
                          <AlertCircle size={11} />
                        {:else if tone === "warn"}
                          <ShieldAlert size={11} />
                        {:else if rowDone && row.attention.includes("finished")}
                          <CheckCircle2 size={11} class="text-ok shrink-0" />
                        {:else if tone === "info"}
                          <Bell size={11} />
                        {:else if tone === "unknown"}
                          <CircleHelp size={11} />
                        {:else}
                          <FileDiff size={11} />
                        {/if}
                        <span>{row.attentionLabel || "—"}</span>
                      </span>
                      {#if attTs !== null}
                        <time
                          class="text-micro text-textMuted tabular-nums shrink-0"
                          datetime={safeIsoDatetime(attTs)}
                        >
                          {formatAge(attTs, now)}
                        </time>
                      {/if}
                      {#if isInformationalRow(row)}
                        <button
                          type="button"
                          class="gp-btn"
                          data-size="sm"
                          data-action="dismiss"
                          aria-label="Dismiss notification for {row.session}"
                          title="Dismiss this notification without stopping the terminal"
                          onclick={() => dismissNotice(row)}
                        >
                          <Check size={11} />
                          <span>Dismiss</span>
                        </button>
                      {/if}
                    </div>
                    {#if row.attentionDetail}
                      <p
                        class="m-0 mt-1 min-w-0 max-w-[min(28rem,32cqw)] line-clamp-2 wrap-anywhere text-meta text-textMuted"
                        data-width-cap="truncating attention detail cell"
                        title={row.attentionDetail}
                      >{row.attentionDetail}</p>
                    {/if}
                  {:else if activelyRunning}
                    <div class="flex flex-wrap items-center gap-1.5 min-w-0">
                      <span class="gp-chip font-medium" data-tone="accent" data-testid="agents-running-indicator">
                        <Loader2 size={11} class="gp-agent-spinner animate-spin shrink-0" aria-hidden="true" />
                        <span>Working</span>
                      </span>
                      {#if attTs !== null}
                        <time
                          class="text-micro text-textMuted tabular-nums shrink-0"
                          datetime={safeIsoDatetime(attTs)}
                        >
                          {formatAge(attTs, now)}
                        </time>
                      {/if}
                    </div>
                  {:else if row.presence === "live"}
                    <div class="flex flex-wrap items-center gap-1.5 min-w-0">
                      <span class="gp-chip font-medium" data-tone="neutral">
                        <CheckCircle2 size={11} class="text-ok shrink-0" aria-hidden="true" />
                        <span>Idle</span>
                      </span>
                      {#if attTs !== null}
                        <time
                          class="text-micro text-textMuted tabular-nums shrink-0"
                          datetime={safeIsoDatetime(attTs)}
                        >
                          {formatAge(attTs, now)}
                        </time>
                      {/if}
                    </div>
                  {:else}
                    <span class="text-textMuted">—</span>
                  {/if}
                  {#if under.attention}
                    <span class="block text-textMuted" data-testid="agents-row-rollup-attention">{under.attention}</span>
                  {/if}
                </td>
              {/if}
              {#if columnVisible("parallel")}
                <td class="px-3 py-2 align-top tabular-nums">
                  {row.parallelFloor ? `at least ${row.parallelCount}` : row.parallelCount}
                </td>
              {/if}
              {#if columnVisible("changes")}
                <td class="px-3 py-2 align-top tabular-nums">
                  {row.dirtyKnown ? row.dirtyFiles : "Not read"}
                </td>
              {/if}
            </tr>
            {/if}
          {/each}
        </tbody>
      </table>
    {/if}
    {#if maviRows.length > 0}
      <MaviAgentsTable rows={maviRows} {compact} {columnVisible} />
    {/if}
  </div>
  {/if}
</div>

<style>
  .gp-live-dot {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background-color: rgb(var(--c-ok));
    flex-shrink: 0;
  }
  @media (prefers-reduced-motion: no-preference) {
    .gp-live-dot {
      animation: gp-live-pulse calc(var(--gp-dur-emphasis) * 3) var(--gp-ease-effect) infinite;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    :global(.gp-agent-spinner),
    .gp-live-dot {
      animation: none;
    }
  }
  :global(html[data-motion="reduced"]) :global(.gp-agent-spinner),
  :global(html[data-motion="reduced"]) .gp-live-dot {
    animation: none;
  }
  @keyframes gp-live-pulse {
    50% {
      opacity: 0.4;
    }
  }
</style>
