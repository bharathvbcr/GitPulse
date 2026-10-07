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
  import { Bot, CircleAlert, FolderOpen, ListChecks, RefreshCw, Rows3, SquareTerminal, X } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { toastStore } from "../stores/toastStore";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { sessionActivity } from "../terminal/sessionActivity";
  import { focusTerminalSession } from "../terminal/sessionFocus";
  import { attemptNotices, taskTerminalRequests } from "../terminal/taskLaunches";
  import { terminalSessionLimit } from "../terminal/sessionLimit";
  import { createBoardAgents } from "../workbench/boardAgents";
  import { explainError, getTaskRun } from "../workbench/client";
  import { openTaskForRun } from "../workbench/taskOpen";
  import { queuedTerminalNote, showTaskTerminal } from "../workbench/taskTerminal";
  import { agentDirectories } from "../agents/cwd";
  import { agentPlaneStore, sweepTargets } from "../agents/store";
  import { agentKindLabel } from "../work/agentWorktree";
  import { agentsRepositoryScope, setAgentsRepositoryScope } from "../agents/scope";
  import { displayName } from "../repos/paths";
  import { createRegisteredRepositories, repositoryPaths, taskProbeFromBoard } from "../agents/tasks";
  import {
    AGENT_COLUMNS,
    applyAgentFilter,
    isAgentFilter,
    planeHeadline,
    plural,
    projectAgentPlane,
    scopeToRepository,
    type AgentColumnKey,
    type AgentFilter,
    type AgentRow,
  } from "../agents/plane";

  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const board = createBoardAgents();
  let now = $state(Date.now());
  let sweepKey = "";
  /** Registered repositories, so a task names its repository and not its cwd. */
  const registered = createRegisteredRepositories();
  const registeredStatus = registered.status;

  const showing = $derived($interfaceStore.globalSurface === "agents");
  const hidden = $derived(new Set($interfaceStore.agentsHiddenColumns));
  const compact = $derived($interfaceStore.agentsCompact);
  const filter = $derived(isAgentFilter($interfaceStore.agentsFilter) ? $interfaceStore.agentsFilter : "all");

  const terminals = $derived($terminalSessions.map((record) => {
    const activity = record.sessionId ? $sessionActivity.get(record.sessionId) : undefined;
    return {
      key: record.key,
      repoPath: record.repoPath,
      label: record.label,
      title: activity?.title || record.title || record.label,
      status: record.status,
      sessionId: record.sessionId ?? "",
      taskRunId: record.taskRunId ?? "",
      continuesRunId: record.continuesRunId ?? "",
      // Unknown stays unknown. The registry has no directory, and a failed
      // context read must not be filled in with the repository root. The tab
      // chip counts from this same sweep (`agentDirectories`).
      cwd: record.sessionId ? $agentDirectories.get(record.sessionId) ?? null : null,
      attention: activity?.attention?.kind ?? null,
    };
  }));

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
  const scoped = $derived(scopeToRepository(plane.rows, $agentsRepositoryScope, pathOpts));
  const visible = $derived(applyAgentFilter(scoped, filter));
  const scopeLabel = $derived($agentsRepositoryScope ? displayName($agentsRepositoryScope) : "");
  const headline = $derived(planeHeadline(plane, visible.length));
  const scanning = $derived($agentPlaneStore.scanning);
  const notes = $derived([
    ...plane.gaps,
    ...($registeredStatus.error
      ? [{ kind: "partial", repoPath: "", label: "Repositories", reason: `Registered repositories could not be read, so an attempt's repository is not named: ${$registeredStatus.error}` }]
      : !$registeredStatus.complete
        ? [{ kind: "partial", repoPath: "", label: "Repositories", reason: "The registered repository list was capped. Some attempts may not name their repository." }]
        : []),
  ]);

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
      toastStore.error(`The terminal could not be shown: ${explainError(cause)}`);
    }
  }

  async function openTask(row: AgentRow) {
    if (!row.taskRunId) return;
    try {
      await openTaskForRun(row.taskRunId);
    } catch (cause) {
      toastStore.error(`This attempt's task could not be opened: ${explainError(cause)}`);
    }
  }

  function openCheckout(row: AgentRow) {
    if (!row.checkoutPath) return;
    interfaceStore.setGlobalSurface("repository");
    void repoStore.openRepo(row.checkoutPath);
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
    const timer = setInterval(() => {
      now = Date.now();
    }, 1000);
    return () => {
      clearInterval(timer);
      board.stop();
    };
  });

  $effect(() => {
    const open = showing;
    const key = $repoStore.openTabs.map((tab) => `${tab.path}\0${tab.label}\0${tab.family ?? ""}\0${tab.familyRoot ?? ""}`).join("\n");
    if (!open) {
      sweepKey = "";
      agentPlaneStore.cancel();
      return;
    }
    if (key === sweepKey) return;
    sweepKey = key;
    void agentPlaneStore.refresh(sweepTargets($repoStore.openTabs));
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
  <header class="shrink-0 border-b border-border gp-section-edge px-4 py-3 flex flex-col gap-3">
    <div class="flex items-start gap-3">
      <div class="flex items-center gap-2 min-w-0 flex-1">
        <Bot size={16} class="text-accent shrink-0" />
        <div class="min-w-0">
          <h1 class="text-sm font-semibold text-textPrimary">Agents</h1>
          <p class="text-[11px] text-textMuted truncate" data-testid="agents-headline">{headline}</p>
        </div>
      </div>
      <div class="flex items-center gap-1.5 shrink-0">
        <button
          type="button"
          class="gp-btn py-1! px-2! text-[11px]!"
          onclick={refresh}
          disabled={scanning}
        >
          <RefreshCw size={11} class={scanning ? "animate-spin" : ""} />
          <span>Refresh</span>
        </button>
        {#if scanning}
          <button type="button" class="gp-btn py-1! px-2! text-[11px]!" onclick={() => agentPlaneStore.cancel()}>
            Cancel
          </button>
        {/if}
        <button
          type="button"
          class="gp-icon-btn"
          aria-pressed={compact}
          title={compact ? "Comfortable rows" : "Compact rows"}
          aria-label={compact ? "Use comfortable rows" : "Use compact rows"}
          onclick={() => interfaceStore.toggleAgentsCompact()}
        >
          <Rows3 size={14} />
        </button>
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
    </div>

    <div class="flex flex-wrap items-center gap-1.5">
      {#if scopeLabel}
        <span
          class="inline-flex items-center gap-1 rounded-full border border-accent/40 bg-accent/10 pl-2 pr-0.5 py-0.5 text-[11px] text-textPrimary"
          data-testid="agents-scope"
        >
          <span>Only {scopeLabel}</span>
          <button
            type="button"
            class="gp-icon-btn h-4! w-4!"
            aria-label="Show every repository"
            title="Show every repository"
            onclick={() => setAgentsRepositoryScope(null)}
          >
            <X size={10} />
          </button>
        </span>
        <span class="mx-1 h-3 w-px bg-border" aria-hidden="true"></span>
      {/if}
      {#each FILTERS as item (item.id)}
        <button
          type="button"
          class="gp-btn py-0.5! px-2! text-[11px]!"
          aria-pressed={filter === item.id}
          data-testid="agents-filter-{item.id}"
          onclick={() => interfaceStore.setAgentsFilter(item.id)}
        >
          {item.label}
        </button>
      {/each}
      <span class="mx-1 h-3 w-px bg-border" aria-hidden="true"></span>
      {#each AGENT_COLUMNS as column (column.key)}
        <button
          type="button"
          class="gp-btn py-0.5! px-2! text-[11px]!"
          aria-pressed={columnVisible(column.key)}
          onclick={() => interfaceStore.toggleAgentsColumn(column.key)}
        >
          {column.label}
        </button>
      {/each}
      {#if hidden.size > 0}
        <button type="button" class="gp-btn py-0.5! px-2! text-[11px]!" onclick={() => interfaceStore.showAllAgentsColumns()}>
          Show columns
        </button>
      {/if}
    </div>
  </header>

  {#if notes.length > 0}
    <ul class="shrink-0 border-b border-border px-4 py-2 flex flex-col gap-1" data-testid="agents-gaps">
      {#each notes as gap, index (`${gap.kind}:${gap.repoPath}:${index}`)}
        <li class="flex items-start gap-2 text-[11px] text-textSecondary" role="status">
          <CircleAlert size={12} class="shrink-0 mt-0.5 text-amber-500" />
          <span><span class="font-medium text-textPrimary">{gap.label}.</span> {gap.reason}</span>
        </li>
      {/each}
    </ul>
  {/if}

  <div class="flex-1 min-h-0 min-w-0 overflow-auto">
    {#if visible.length === 0}
      <p class="px-4 py-8 text-[12px] text-textMuted max-w-xl" data-testid="agents-empty">
        {#if scopeLabel && plane.rows.length > 0 && scoped.length === 0}
          No agent checkouts, terminals or task attempts in {scopeLabel}. Other repositories have {plural(plane.rows.length, "row", "rows")}; clear the scope to see them.
        {:else if plane.rows.length > 0}
          Nothing matches this filter. The headline still counts the {plural(plane.rows.length, "row", "rows")} it hid.
        {:else if plane.gaps.some((gap) => gap.kind === "failed" || gap.kind === "skipped")}
          Sessions could not be read. The notes above are the reason, not an empty workspace.
        {:else if plane.gaps.length > 0 || scanning}
          Still reading. A note above says what has not come back, and an empty list is not a quiet workspace.
        {:else}
          No agent checkouts, terminals or task attempts in the open repositories. A checkout appears when its path is an agent worktree. A terminal appears when this window started it.
        {/if}
      </p>
    {:else}
      <table class="w-full text-left border-collapse {compact ? 'text-[11px]' : 'text-[12px]'}">
        <caption class="sr-only">Agent checkouts, terminals and task attempts across open repositories</caption>
        <thead class="sticky top-0 bg-background text-textMuted">
          <tr class="border-b border-border">
            <th scope="col" class="px-4 py-2 font-medium">Agent</th>
            {#if columnVisible("checkout")}<th scope="col" class="px-3 py-2 font-medium">Checkout</th>{/if}
            {#if columnVisible("presence")}<th scope="col" class="px-3 py-2 font-medium">Presence</th>{/if}
            {#if columnVisible("attention")}<th scope="col" class="px-3 py-2 font-medium">Attention</th>{/if}
            {#if columnVisible("parallel")}<th scope="col" class="px-3 py-2 font-medium">Parallel</th>{/if}
            {#if columnVisible("changes")}<th scope="col" class="px-3 py-2 font-medium">Changes</th>{/if}
          </tr>
        </thead>
        <tbody>
          {#each visible as row (row.id)}
            <tr class="border-b border-border/60 hover:bg-surfaceHover/60" data-testid="agents-row">
              <th scope="row" class="px-4 py-2 font-normal text-left align-top">
                <span class="block font-medium text-textPrimary">{row.session}</span>
                <span class="block text-textMuted">{agentKindLabel(row.kind)} · {row.repoLabel}</span>
                <span class="mt-1 flex flex-wrap gap-1" data-testid="agents-row-actions">
                  {#if row.liveKey || row.taskRunId}
                    <button
                      type="button"
                      class="gp-btn py-0.5! px-1.5! text-[11px]!"
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
                      class="gp-btn py-0.5! px-1.5! text-[11px]!"
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
                      class="gp-btn py-0.5! px-1.5! text-[11px]!"
                      aria-label="Open checkout {row.checkout} for {row.session}"
                      title={row.checkoutPath}
                      onclick={() => openCheckout(row)}
                    >
                      <FolderOpen size={11} />
                      <span>Open checkout</span>
                    </button>
                  {/if}
                </span>
              </th>
              {#if columnVisible("checkout")}
                <td class="px-3 py-2 align-top text-textSecondary">{row.checkout}</td>
              {/if}
              {#if columnVisible("presence")}
                <td class="px-3 py-2 align-top" title={row.presenceDetail}>
                  {presenceWord(row)}
                </td>
              {/if}
              {#if columnVisible("attention")}
                <td class="px-3 py-2 align-top">{row.attentionLabel || "—"}</td>
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
</div>
