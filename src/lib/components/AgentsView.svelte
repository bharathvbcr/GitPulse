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
  import { Bot, ChevronDown, LayoutList, ChevronRight, CircleAlert, FolderOpen, Inbox, ListChecks, RefreshCw, Rows3, SquareTerminal, X } from "@lucide/svelte";
  import AgentInbox from "./AgentInbox.svelte";
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
  import { displayName } from "../repos/paths";
  import { createRegisteredRepositories, repositoryPaths, taskProbeFromBoard } from "../agents/tasks";
  import {
    AGENT_COLUMNS,
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
  const activeRepoPath = $derived($agentsRepositoryScope || $repoStore.currentPath || "");

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
  const scopeLabel = $derived($agentsRepositoryScope ? displayName($agentsRepositoryScope) : "");
  const headline = $derived(planeHeadline(plane, countAgentRows(visible)));
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
    { id: "live", label: "Live" },
    { id: "parallel", label: "Parallel" },
  ];

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
  <!-- The same header as Tasks (gp-surface-header): title and what it counts on
       the left; the view switch and actions on the right, where Tasks keeps
       Board/List. -->
  <header class="gp-surface-header">
    <div class="flex items-center gap-2.5 min-w-0 flex-1">
      <Bot size={16} class="text-accent shrink-0" />
      <div class="min-w-0">
        <h1 class="gp-surface-title">Agents</h1>
        <p class="text-meta text-textMuted truncate" data-testid="agents-headline" title={headline}>{headline}</p>
      </div>
    </div>
    <div class="flex items-center gap-1.5 shrink-0">
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
    </div>
  </header>

  <!-- Filters and columns act on the plane's table, so they live with it: the
       Inbox has neither, and showing them there offered controls that did
       nothing. Each group is labelled rather than told apart by a divider. -->
  {#if activeView === "plane"}
    <div class="shrink-0 flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3.5 py-2" data-testid="agents-toolbar">
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
      <div class="flex items-center gap-1.5" role="group" aria-label="Show">
        <span class="gp-label">Show</span>
        <div class="gp-segmented" class:gp-liquid-tabs={macos}>
          {#each FILTERS as item (item.id)}
            <button
              type="button"
              class="gp-seg-btn text-meta!"
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
      <AgentInbox repoPath={activeRepoPath} embedded={true} onopenTask={openInboxTask} />
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
        <div class="flex items-center gap-2 px-3 pt-2 pb-1">
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

  <div class="flex-1 min-h-0 min-w-0 overflow-auto">
    {#if visible.length === 0}
      <div class="px-4 py-6 max-w-xl" data-testid="agents-empty">
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
    {:else}
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
                  <span class="min-w-0">
                <span class="block font-medium text-textPrimary">{row.session}</span>
                <span class="block text-textMuted">{agentKindLabel(row.kind)} · {row.repoLabel}</span>
                <span class="mt-1 flex flex-wrap gap-1" data-testid="agents-row-actions">
                  {#if row.liveKey || row.taskRunId}
                    <button
                      type="button"
                      class="gp-btn" data-size="sm"
                      aria-label="Show terminal for {row.session}"
                      onclick={() => showTerminal(row)}
                    >
                      <SquareTerminal size={11} />
                      <span>Show terminal</span>
                    </button>
                  {/if}
                  {#if row.taskRunId}
                    <button
                      type="button"
                      class="gp-btn" data-size="sm"
                      aria-label="Open task for {row.session}"
                      onclick={() => openTask(row)}
                    >
                      <ListChecks size={11} />
                      <span>Open task</span>
                    </button>
                  {/if}
                  {#if row.checkoutPath}
                    <button
                      type="button"
                      class="gp-btn" data-size="sm"
                      aria-label="Open checkout {row.checkout} for {row.session}"
                      title={row.checkoutPath}
                      onclick={() => openCheckout(row)}
                    >
                      <FolderOpen size={11} />
                      <span>Open checkout</span>
                    </button>
                  {/if}
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
                <td class="px-3 py-2 align-top">
                  {#if !row.attentionObserved}
                    <span title="Nothing in this window watches what this process asks for.">Not observed</span>
                  {:else}
                    {row.attentionLabel || "—"}
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
  </div>
  {/if}
</div>
