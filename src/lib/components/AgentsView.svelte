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
    FileDiff,
    FolderOpen,
    Inbox,
    LayoutList,
    ListChecks,
    Play,
    RefreshCw,
    Rows3,
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
  let focusedId = $state<string | null>(null);
  let treegrid = $state<HTMLTableElement | null>(null);
  const scoped = $derived(scopeToRepository(plane.roots, $agentsRepositoryScope, pathOpts));
  const visible = $derived(applyAgentTree(scoped, filter));
  const entries = $derived(flattenAgentTree(visible, collapsed));
  /** Every drawn row by id, so a child process's actions can name its parent. */
  const rowsById = $derived(new Map(entries.map((entry) => [entry.row.id, entry.row])));
  const scopeLabel = $derived($agentsRepositoryScope ? displayName($agentsRepositoryScope) : "");
  // Mavi threads are agents too: listed under the plane's rows, in its
  // columns, under the same filter and scope, and counted in the headline.
  const maviRows = $derived(
    maviAgentRows(maviStore.records, (record) => maviStore.summaryOf(record), (record) => maviStore.status(record), {
      filter,
      scope: $agentsRepositoryScope || null,
      sameRepo: (a, b) => sameRepo(a, b, pathOpts),
    }),
  );
  const maviClause = $derived(maviHeadline(maviRows));
  const headline = $derived(
    maviClause ? `${planeHeadline(plane, countAgentRows(visible))} · ${maviClause}` : planeHeadline(plane, countAgentRows(visible)),
  );
  // Exactly one row is in the tab order (a roving tabindex), and it is the one
  // last focused while that row is still drawn.
  const activeId = $derived(
    entries.some((entry) => entry.row.id === focusedId) ? focusedId : (entries[0]?.row.id ?? null),
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

  /** Moves the roving focus to the entry at `index` and puts the browser's focus on it. */
  async function focusEntry(index: number) {
    const target = entries[Math.max(0, Math.min(entries.length - 1, index))];
    if (!target) return;
    focusedId = target.row.id;
    await tick();
    treegrid?.querySelector<HTMLElement>(`[data-row-id="${CSS.escape(target.row.id)}"]`)?.focus();
  }

  /**
   * The treegrid keys. Up and Down move between drawn rows; Right opens a
   * closed row and then steps into it; Left closes an open row and then steps
   * out to its parent. Keys that land on a button inside the row are the
   * button's own.
   */
  function onRowKey(event: KeyboardEvent, entry: AgentTreeEntry, index: number) {
    if (event.target !== event.currentTarget || event.altKey || event.ctrlKey || event.metaKey) return;
    switch (event.key) {
      case "ArrowDown":
        void focusEntry(index + 1);
        break;
      case "ArrowUp":
        void focusEntry(index - 1);
        break;
      case "Home":
        void focusEntry(0);
        break;
      case "End":
        void focusEntry(entries.length - 1);
        break;
      case "ArrowRight":
        if (entry.expandable && !entry.expanded) setExpanded(entry.row.id, true);
        else if (entry.expandable) void focusEntry(index + 1);
        break;
      case "ArrowLeft":
        if (entry.expanded) setExpanded(entry.row.id, false);
        else if (entry.level > 1) {
          let up = index - 1;
          while (up > 0 && entries[up].level >= entry.level) up -= 1;
          void focusEntry(up);
        }
        break;
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
      {#if scopeLabel}
        <span class="gp-chip" data-tone="accent" data-kind="removable" data-testid="agents-scope">
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
      <!-- One filter at a time, so a segmented switch, not toggles. -->
      <div class="flex min-w-0 max-w-full items-center gap-1.5" role="group" aria-label="Show">
        <span class="gp-label shrink-0">Show</span>
        <div class="gp-header-scroll gp-segmented min-w-0 max-w-full flex-nowrap" class:gp-liquid-tabs={macos}>
          {#each FILTERS as item (item.id)}
            <button
              type="button"
              class="gp-seg-btn shrink-0 whitespace-nowrap text-meta!"
              data-active={filter === item.id}
              aria-pressed={filter === item.id}
              data-testid="agents-filter-{item.id}"
              onclick={() => interfaceStore.setAgentsFilter(item.id)}
            >
              {item.label}
            </button>
          {/each}
        </div>
      </div>
      <div class="flex flex-wrap items-center gap-1.5" role="group" aria-label="Columns">
        <span class="gp-label">Columns</span>
        {#each AGENT_COLUMNS as column (column.key)}
          <button
            type="button"
            class="gp-btn" data-size="sm"
            aria-pressed={columnVisible(column.key)}
            onclick={() => interfaceStore.toggleAgentsColumn(column.key)}
          >
            {column.label}
          </button>
        {/each}
        {#if hidden.size > 0}
          <button type="button" class="gp-btn" data-size="sm" onclick={() => interfaceStore.showAllAgentsColumns()}>
            Show columns
          </button>
        {/if}
      </div>
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
        {#if scopeLabel && plane.rows.length > 0 && scoped.length === 0}
          <EmptyState compact icon={Bot} title="No agent work in {scopeLabel}" hint="No agent checkouts, terminals or task runs here. Other repositories have {plural(plane.rows.length, "row", "rows")}; clear the scope to see them." />
        {:else if plane.rows.length > 0}
          <EmptyState compact icon={Bot} title="Nothing matches this filter" hint="The headline still counts the {plural(plane.rows.length, "row", "rows")} it hid." />
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
          {#each entries as entry, index (entry.row.id)}
            {@const row = entry.row}
            {@const under = rollupSummary(row)}
            <tr
              class="hover:bg-surfaceHover/60 focus-visible:outline-2 focus-visible:outline-accent"
              data-testid="agents-row"
              data-row-id={row.id}
              data-level={entry.level}
              data-origin={row.origin}
              aria-level={entry.level}
              aria-posinset={entry.posInSet}
              aria-setsize={entry.setSize}
              aria-expanded={entry.expandable ? entry.expanded : undefined}
              tabindex={row.id === activeId ? 0 : -1}
              onfocus={(event) => {
                if (event.target === event.currentTarget) focusedId = row.id;
              }}
              onkeydown={(event) => onRowKey(event, entry, index)}
            >
              <th scope="row" class="py-2 pr-4 font-normal text-left align-top" style:padding-left="{16 + (entry.level - 1) * 20}px">
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
                <span class="block font-medium text-textPrimary">{row.session}</span>
                <span class="block text-textMuted">{agentKindLabel(row.kind)} · {row.repoLabel}</span>
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
                  {presenceWord(row)}
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
                  {:else if row.presence === "live"}
                    <div class="flex flex-wrap items-center gap-1.5 min-w-0">
                      <span class="gp-chip font-medium" data-tone="ok">
                        <CheckCircle2 size={11} />
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
