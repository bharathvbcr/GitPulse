<script lang="ts">
  /**
   * The Agents plane: every session the open repositories are holding.
   *
   * It stays mounted once opened, the same way Fleet and Tasks do. The
   * parent hides it. Unmounting it would drop the sweep and the task
   * watch, and the next open would briefly say there were no attempts
   * because the first read had not happened yet.
   *
   * A checkout, a process this window started, and a task attempt are
   * drawn from one projection (`plane.ts`). This file decides when to
   * look, which columns to keep, and where a row goes when opened.
   */
  import { get } from "svelte/store";
  import { Bot, CircleAlert, RefreshCw, Rows3, X } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { terminalSessions } from "../terminal/sessionRegistry";
  import { sessionActivity } from "../terminal/sessionActivity";
  import { taskTerminalRequests } from "../terminal/taskLaunches";
  import { terminalSessionLimit } from "../terminal/sessionLimit";
  import { createBoardAgents } from "../workbench/boardAgents";
  import { readAgentCwds } from "../agents/cwd";
  import { agentPlaneStore, sweepTargets } from "../agents/store";
  import { taskProbeFromBoard } from "../agents/tasks";
  import {
    AGENT_COLUMNS,
    applyAgentFilter,
    isAgentFilter,
    planeHeadline,
    projectAgentPlane,
    type AgentColumnKey,
    type AgentFilter,
    type AgentRow,
  } from "../agents/plane";

  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const board = createBoardAgents();
  let now = $state(Date.now());
  let sweepKey = "";
  /** Directories `cmd_terminal_context` accepted. Absent means unknown. */
  let directories = $state(new Map<string, string>());
  let cwdGeneration = $state(0);

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
      // context read must not be filled in with the repository root.
      cwd: record.sessionId ? directories.get(record.sessionId) ?? null : null,
      attention: activity?.attention?.kind ?? null,
    };
  }));

  const taskProbe = $derived(taskProbeFromBoard($board, {
    records: $terminalSessions,
    requests: $taskTerminalRequests,
    activity: (sessionId) => $sessionActivity.get(sessionId),
    sessionLimit: $terminalSessionLimit,
    now,
  }));

  const plane = $derived(projectAgentPlane({
    probes: $agentPlaneStore.probes,
    terminals,
    tasks: taskProbe,
    paths: pathOpts,
  }));
  const visible = $derived(applyAgentFilter(plane.rows, filter));
  const headline = $derived(planeHeadline(plane, visible.length));
  const scanning = $derived($agentPlaneStore.scanning);

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
    cwdGeneration += 1;
    void agentPlaneStore.refresh(targets());
  }

  function openRow(row: AgentRow) {
    const liveKey = row.liveKey;
    interfaceStore.setGlobalSurface("repository");
    void repoStore.openRepo(row.worktreePath || row.repoPath, {
      onReady: () => {
        if (!liveKey) return;
        get(terminalSessions).find((record) => record.key === liveKey)?.reveal?.();
      },
    });
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
    const ticket = cwdGeneration;
    const ids = $terminalSessions.map((record) => record.sessionId ?? "");
    let cancelled = false;
    void readAgentCwds(ids).then((found) => {
      if (!cancelled && ticket === cwdGeneration) directories = found;
    });
    return () => {
      cancelled = true;
    };
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

  {#if plane.gaps.length > 0}
    <ul class="shrink-0 border-b border-border px-4 py-2 flex flex-col gap-1" data-testid="agents-gaps">
      {#each plane.gaps as gap, index (`${gap.kind}:${gap.repoPath}:${index}`)}
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
        {#if plane.rows.length > 0}
          Nothing matches this filter. The headline still counts the sessions it hid.
        {:else if plane.gaps.some((gap) => gap.kind === "failed" || gap.kind === "skipped")}
          Sessions could not be read. The notes above are the reason, not an empty workspace.
        {:else if plane.gaps.length > 0 || scanning}
          Still reading. A note above says what has not come back, and an empty list is not a quiet workspace.
        {:else}
          No agent sessions in the open repositories. A checkout appears when its path is an agent worktree. A process appears when this window started it.
        {/if}
      </p>
    {:else}
      <table class="w-full text-left border-collapse {compact ? 'text-[11px]' : 'text-[12px]'}">
        <caption class="sr-only">Agent sessions across open repositories</caption>
        <thead class="sticky top-0 bg-background text-textMuted">
          <tr class="border-b border-border">
            <th scope="col" class="px-4 py-2 font-medium">Session</th>
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
                <button type="button" class="text-left bg-transparent border-0 p-0 text-inherit" onclick={() => openRow(row)}>
                  <span class="block font-medium text-textPrimary">{row.session}</span>
                  <span class="block text-textMuted">{row.kind} · {row.repoLabel}</span>
                </button>
              </th>
              {#if columnVisible("checkout")}
                <td class="px-3 py-2 align-top text-textSecondary">{row.checkout}</td>
              {/if}
              {#if columnVisible("presence")}
                <td class="px-3 py-2 align-top" title={row.presenceDetail}>
                  {row.presence === "live" ? "Live" : row.presence === "on-disk" ? "On disk" : "Not in this window"}
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
