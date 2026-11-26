<script module lang="ts">
  export type { DockAgentRow } from "../terminal/dockAgents";
</script>

<script lang="ts">
  import {
    AlertCircle,
    ArrowRight,
    ArrowUpRight,
    CheckCircle2,
    ChevronDown,
    ChevronRight,
    Eye,
    Layers,
    Loader2,
    Search,
    ShieldAlert,
    X,
  } from "@lucide/svelte";
  import LauncherIcon from "./LauncherIcon.svelte";
  import RepoLogo from "./RepoLogo.svelte";
  import { formatAge } from "../storage/format";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import {
    collectDockAttentionAndDone,
    dockRowSecondary,
    filterDockAgents,
    filterDockAgentsByStatus,
    groupDockAgentsByRepo,
    isDockAgentRunning,
    sortDockTriageRows,
    type DockAgentFilter,
    type DockAgentRow,
  } from "../terminal/dockAgents";
  import type { SessionKind } from "../terminal/tabs";

  interface Props {
    here: DockAgentRow[];
    elsewhere: DockAgentRow[];
    launchers: { kind: SessionKind; label: string; disabled: boolean; title: string }[];
    followAgent: boolean;
    canFollow: boolean;
    currentRepoPath?: string | null;
    currentRepoName?: string | null;
    initialGroupMode?: "auto" | "repo" | "flat";
    initialFilter?: DockAgentFilter;
    onOpenGlobalAgents?: () => void;
    onSelect: (id: string) => void;
    onGoTo: (id: string) => void;
    onLaunch: (kind: SessionKind) => void;
    onToggleFollow: () => void;
  }

  let {
    here,
    elsewhere,
    launchers,
    followAgent,
    canFollow,
    currentRepoPath = null,
    currentRepoName = null,
    initialGroupMode = "auto",
    initialFilter = "all",
    onOpenGlobalAgents = undefined,
    onSelect,
    onGoTo,
    onLaunch,
    onToggleFollow,
  }: Props = $props();

  const ROW_SELECTOR =
    '[data-testid="dock-agents-attention-item"]:not(:disabled), [data-testid="dock-agent-row"]:not(:disabled)';

  let searchQuery = $state("");
  // svelte-ignore state_referenced_locally
  let statusFilter = $state<DockAgentFilter>(initialFilter);
  // svelte-ignore state_referenced_locally
  let groupMode = $state<"auto" | "repo" | "flat">(initialGroupMode);
  let collapsedGroupKeys = $state<Set<string>>(new Set());
  let attentionTrayCollapsed = $state(false);
  let searchInputEl = $state<HTMLInputElement | null>(null);
  let rootEl = $state<HTMLElement | null>(null);
  let now = $state(Date.now());

  const totalCount = $derived(here.length + elsewhere.length);
  const empty = $derived(totalCount === 0);
  const totalDoneCount = $derived(
    here.filter((row) => Boolean(row.done) && !row.attention).length +
      elsewhere.filter((row) => Boolean(row.done) && !row.attention).length,
  );
  const hasActivityTimestamp = $derived(
    here.some((row) => typeof row.lastActivityAt === "number" && row.lastActivityAt > 0) ||
      elsewhere.some((row) => typeof row.lastActivityAt === "number" && row.lastActivityAt > 0),
  );

  $effect(() => {
    if (!hasActivityTimestamp) return;
    now = Date.now();
    return createAdaptiveTimer(() => {
      now = Date.now();
    }, 30_000);
  });

  const trimmedQuery = $derived(searchQuery.trim());
  const searchedHere = $derived(filterDockAgents(here, searchQuery));
  const searchedElsewhere = $derived(filterDockAgents(elsewhere, searchQuery));
  const searchedCount = $derived(searchedHere.length + searchedElsewhere.length);
  const searchedRunningCount = $derived(
    searchedHere.filter(isDockAgentRunning).length + searchedElsewhere.filter(isDockAgentRunning).length,
  );
  const searchedAttentionCount = $derived(
    searchedHere.filter((row) => Boolean(row.attention)).length +
      searchedElsewhere.filter((row) => Boolean(row.attention)).length,
  );
  const searchedDoneCount = $derived(
    searchedHere.filter((row) => Boolean(row.done) && !row.attention).length +
      searchedElsewhere.filter((row) => Boolean(row.done) && !row.attention).length,
  );

  const visibleHere = $derived(filterDockAgentsByStatus(searchedHere, statusFilter));
  const visibleElsewhere = $derived(filterDockAgentsByStatus(searchedElsewhere, statusFilter));
  const triageHere = $derived(sortDockTriageRows(visibleHere));
  const triageElsewhere = $derived(sortDockTriageRows(visibleElsewhere));

  const effectiveGrouped = $derived(
    statusFilter === "attention" || statusFilter === "done"
      ? false
      : groupMode === "repo" || groupMode === "auto",
  );
  const attentionTrayItems = $derived(collectDockAttentionAndDone(visibleHere, visibleElsewhere));
  const showAttentionTray = $derived(
    effectiveGrouped &&
      attentionTrayItems.length > 0 &&
      statusFilter !== "attention" &&
      statusFilter !== "done",
  );
  const trayAttentionCount = $derived(
    attentionTrayItems.filter((item) => Boolean(item.row.attention)).length,
  );
  const trayDangerCount = $derived(
    attentionTrayItems.filter((item) => item.row.attention && /fail|error/i.test(item.row.attention)).length,
  );
  const trayDoneCount = $derived(
    attentionTrayItems.filter((item) => Boolean(item.row.done) && !item.row.attention).length,
  );
  const trayHeading = $derived(
    trayAttentionCount > 0 && trayDoneCount > 0
      ? "Needs attention & finished"
      : trayAttentionCount > 0
        ? "Needs attention"
        : "Finished",
  );

  const repoGroups = $derived(
    effectiveGrouped
      ? groupDockAgentsByRepo(
          visibleHere,
          visibleElsewhere,
          { path: currentRepoPath, name: currentRepoName },
          { collapsedKeys: collapsedGroupKeys },
        )
      : [],
  );
  const noMatches = $derived(!empty && visibleHere.length === 0 && visibleElsewhere.length === 0);

  const filterOptions = $derived.by(() => {
    const base: { id: DockAgentFilter; label: string; count: number }[] = [
      { id: "all", label: "All", count: searchedCount },
      { id: "running", label: "Running", count: searchedRunningCount },
      { id: "attention", label: "Attention", count: searchedAttentionCount },
    ];
    if (totalDoneCount > 0 || statusFilter === "done") {
      base.push({ id: "done", label: "Done", count: searchedDoneCount });
    }
    return base;
  });

  const groupToggleTitle = $derived.by(() => {
    if (statusFilter === "attention" || statusFilter === "done") {
      return "Ungrouped triage view while filtering by attention or finished agents";
    }
    return effectiveGrouped
      ? "Grouped by repository — click to switch to In this repository / Other repositories sections"
      : "Showing In this repository / Other repositories sections — click to group by repository";
  });

  function rowTitle(row: DockAgentRow, isHere: boolean): string {
    if (isHere) return `Show in Shell \u2014 ${row.title}`;
    return row.detail ? `Go to ${row.title} in ${row.detail}` : `Go to ${row.title}`;
  }

  function attentionTone(label: string): "danger" | "warn" {
    return /fail|error/i.test(label) ? "danger" : "warn";
  }

  function toggleGroupMode() {
    groupMode = groupMode === "flat" ? "auto" : "flat";
  }

  function toggleRepoCollapsed(key: string) {
    const next = new Set(collapsedGroupKeys);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    collapsedGroupKeys = next;
  }

  function trayRowContext(row: DockAgentRow, isHere: boolean): string | null {
    const repo = row.repoName?.trim() || (isHere ? currentRepoName?.trim() : "") || "";
    const checkout = dockRowSecondary(row, true);
    if (repo && checkout) return `${repo} · ${checkout}`;
    return repo || checkout || row.detail?.trim() || null;
  }

  function onSectionKeydown(event: KeyboardEvent) {
    if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey) return;
    const target = event.target as HTMLElement | null;
    if (
      target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.tagName === "SELECT" ||
        target.isContentEditable)
    ) {
      return;
    }
    if (!searchInputEl) return;
    event.preventDefault();
    searchInputEl.focus();
  }

  function onSearchKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      if (!searchQuery) return;
      event.preventDefault();
      event.stopPropagation();
      searchQuery = "";
      return;
    }
    if (event.key === "ArrowDown") {
      const firstRow = rootEl?.querySelector<HTMLButtonElement>(ROW_SELECTOR);
      if (!firstRow) return;
      event.preventDefault();
      firstRow.focus();
    }
  }

  /** ArrowUp/ArrowDown/Home/End roving between the tray and list row buttons. */
  function onListKeydown(event: KeyboardEvent) {
    if (
      event.key !== "ArrowDown" &&
      event.key !== "ArrowUp" &&
      event.key !== "Home" &&
      event.key !== "End"
    ) {
      return;
    }
    const root = (event.currentTarget as HTMLElement).closest<HTMLElement>('[data-testid="dock-agents"]');
    if (!root) return;
    const rows = Array.from(root.querySelectorAll<HTMLButtonElement>(ROW_SELECTOR));
    if (rows.length === 0) return;
    if (event.key === "Home") {
      event.preventDefault();
      rows[0]?.focus();
      return;
    }
    if (event.key === "End") {
      event.preventDefault();
      rows[rows.length - 1]?.focus();
      return;
    }
    const at = rows.indexOf(document.activeElement as HTMLButtonElement);
    if (at < 0) return;
    const next = rows[at + (event.key === "ArrowDown" ? 1 : -1)];
    if (!next) return;
    event.preventDefault();
    next.focus();
  }
</script>

{#snippet agentRow(row: DockAgentRow, onPick: (id: string) => void, isHere: boolean, showRepoLogo: boolean, preferCheckout: boolean)}
  {@const running = isDockAgentRunning(row)}
  {@const secondary = dockRowSecondary(row, preferCheckout)}
  <li>
    <button
      type="button"
      class="gp-menu-item group min-w-0 focus-visible:outline-2 focus-visible:outline-accent {row.active ? 'bg-accent/10' : ''} {row.disabled ? 'opacity-50' : ''}"
      data-testid="dock-agent-row"
      data-agent-id={row.id}
      aria-current={row.active ? "true" : undefined}
      disabled={row.disabled}
      title={rowTitle(row, isHere)}
      onclick={() => onPick(row.id)}
    >
      <LauncherIcon kind={row.launcher} size={14} class="shrink-0" />
      {#if showRepoLogo && row.repoPath}
        <RepoLogo path={row.repoPath} name={row.repoName ?? undefined} size={14} class="shrink-0" />
      {/if}
      <span class="flex min-w-0 flex-1 flex-col">
        <span class="flex min-w-0 items-center gap-2">
          <span class="truncate font-medium text-textPrimary">{row.title}</span>
          {#if row.active}
            <span class="gp-chip shrink-0" data-tone="accent" data-kind="label" data-testid="dock-agent-active">Active</span>
          {/if}
          {#if running}
            <span class="gp-live-dot h-1.5 w-1.5 shrink-0 rounded-full bg-accent" aria-hidden="true" data-testid="dock-agent-live-dot"></span>
          {:else if row.unread}
            <span class="h-1.5 w-1.5 shrink-0 rounded-full bg-accent" aria-hidden="true"></span>
            <span class="sr-only">Unread output</span>
          {/if}
          {#if row.unread && running}
            <span class="sr-only">Unread output</span>
          {/if}
          {#if row.attention}
            <span class="gp-chip shrink-0" data-tone={attentionTone(row.attention)} data-kind="label">{row.attention}</span>
          {/if}
        </span>
        {#if secondary}
          <span class="truncate font-mono text-micro text-textMuted">{secondary}</span>
        {/if}
        {#if row.attentionDetail}
          <span class="truncate text-micro text-textMuted" data-testid="dock-agent-attention-detail">{row.attentionDetail}</span>
        {/if}
      </span>
      <span class="ml-auto flex shrink-0 items-center gap-1.5">
        {#if running}
          <Loader2 size={12} class="gp-agent-spinner animate-spin text-accent shrink-0" aria-hidden="true" data-testid="dock-agent-running" />
        {:else if row.done && !row.attention}
          <CheckCircle2 size={12} class="text-ok shrink-0" aria-hidden="true" data-testid="dock-agent-done" />
        {/if}
        <span class="shrink-0 text-micro {row.status ? row.status.cls : 'text-textMuted'}" data-testid="dock-agent-status">{row.status ? row.status.label : "Idle"}</span>
        {#if typeof row.lastActivityAt === "number" && row.lastActivityAt > 0}
          <span class="shrink-0 tabular-nums text-micro text-textMuted" data-testid="dock-agent-age">{formatAge(row.lastActivityAt, now)}</span>
        {/if}
      </span>
      <ArrowRight size={12} class="shrink-0 text-textMuted opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100" aria-hidden="true" />
    </button>
  </li>
{/snippet}

{#snippet rowList(rows: DockAgentRow[], onPick: (id: string) => void, isHere: boolean, showRepoLogo = false)}
  <!-- Justified: the list only hosts ArrowUp/ArrowDown roving between the
       native row buttons inside it; the buttons are the controls and the
       list itself never takes focus. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <ul role="list" class="flex flex-col gap-0.5" onkeydown={onListKeydown}>
    {#each rows as row (row.id)}
      {@render agentRow(row, onPick, isHere, showRepoLogo, false)}
    {/each}
  </ul>
{/snippet}

<!-- Justified: hosts the section-scoped `/` shortcut to focus the search input while interactive children keep their own handlers. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section
  bind:this={rootEl}
  aria-label="Agents"
  data-testid="dock-agents"
  class="flex h-full min-h-0 flex-col overflow-hidden"
  onkeydown={onSectionKeydown}
>
  <div class="shrink-0 px-3 pt-3 pb-2" data-testid="dock-agents-pinned-header">
    <div class="flex flex-wrap items-center gap-1.5 pb-2.5">
      <span class="text-micro text-textMuted">Start an agent</span>
      {#each launchers as launcher (launcher.kind)}
        <button
          type="button"
          class="gp-btn"
          data-size="sm"
          data-testid="dock-agents-launch-{launcher.kind}"
          disabled={launcher.disabled}
          title={launcher.title}
          onclick={() => onLaunch(launcher.kind)}
        >
          <LauncherIcon kind={launcher.kind} size={12} class="shrink-0" />
          {launcher.label}
        </button>
      {/each}
      <div class="ml-auto flex items-center gap-1.5">
        <button
          type="button"
          class="gp-btn {followAgent ? 'text-accent bg-accent/15' : ''}"
          data-size="sm"
          data-testid="dock-agents-follow"
          aria-pressed={followAgent}
          disabled={!canFollow}
          title={canFollow ? "Show the files agents modify in one split pane" : "Open a repository to follow agent edits"}
          onclick={onToggleFollow}
        >
          <Eye size={12} aria-hidden="true" />
          Follow edits
        </button>
        {#if onOpenGlobalAgents}
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="dock-agents-open-global"
            title="Open the full Agents view"
            onclick={onOpenGlobalAgents}
          >
            Open Agents
            <ArrowUpRight size={12} aria-hidden="true" />
          </button>
        {/if}
      </div>
    </div>
    {#if launchers.length > 0 && launchers.every((launcher) => launcher.disabled)}
      <!-- A disabled button cannot take focus, so its tooltip is hover-only:
           say in view why nothing can start. -->
      <p class="mt-1.5 text-micro text-warn" role="status" data-testid="dock-agents-blocked">{launchers[0].title}</p>
    {/if}

    {#if !empty}
      <div class="flex flex-wrap items-center gap-2 pt-0.5" data-testid="dock-agents-toolbar">
        <div class="relative min-w-44 flex-1">
          <Search size={12} class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted" aria-hidden="true" />
          <input
            bind:this={searchInputEl}
            type="text"
            class="gp-field w-full pl-7! pr-7!"
            data-size="sm"
            data-testid="dock-agents-search"
            aria-label="Search dock agents"
            placeholder="Search agents, repositories, branches…"
            bind:value={searchQuery}
            onkeydown={onSearchKeydown}
          />
          {#if searchQuery}
            <button
              type="button"
              class="gp-icon-btn absolute right-1 top-1/2 h-5 w-5 -translate-y-1/2"
              aria-label="Clear search"
              title="Clear search"
              data-testid="dock-agents-search-clear"
              onclick={() => {
                searchQuery = "";
                searchInputEl?.focus();
              }}
            >
              <X size={11} aria-hidden="true" />
            </button>
          {:else}
            <kbd
              class="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 rounded bg-surfaceHover px-1 py-0.5 font-mono text-micro text-textMuted"
              aria-hidden="true"
              data-testid="dock-agents-search-hint"
            >/</kbd>
          {/if}
        </div>

        <div class="gp-segmented shrink-0" data-size="sm" role="group" aria-label="Filter dock agents">
          {#each filterOptions as option (option.id)}
            <button
              type="button"
              class="gp-seg-btn inline-flex items-center gap-1"
              aria-pressed={statusFilter === option.id}
              data-active={statusFilter === option.id ? "true" : "false"}
              data-testid="dock-agents-filter-{option.id}"
              onclick={() => (statusFilter = option.id)}
            >
              <span>{option.label}</span>
              {#if option.count > 0}
                <span class="tabular-nums text-micro opacity-75">{option.count}</span>
              {/if}
            </button>
          {/each}
        </div>

        <button
          type="button"
          class="gp-btn shrink-0 {effectiveGrouped ? 'text-accent bg-accent/15' : ''}"
          data-size="sm"
          data-testid="dock-agents-group-toggle"
          aria-pressed={effectiveGrouped}
          disabled={statusFilter === "attention" || statusFilter === "done"}
          title={groupToggleTitle}
          onclick={toggleGroupMode}
        >
          <Layers size={12} aria-hidden="true" />
          Group by repository
        </button>
      </div>
    {/if}
  </div>

  <div class="flex-1 min-h-0 overflow-y-auto px-3 pb-3" data-testid="dock-agents-list">
    {#if empty}
      <div class="flex flex-col items-center gap-1 py-10 text-center" data-testid="dock-agents-empty">
        <p class="text-meta font-medium text-textPrimary">No agents running</p>
        <p class="text-micro text-textMuted">Start one above — it opens as a tab in Shell, beside your terminals.</p>
      </div>
    {:else if noMatches}
      <div class="flex flex-col items-center gap-2 py-8 text-center" data-testid="dock-agents-no-matches">
        {#if trimmedQuery}
          <p class="text-meta font-medium text-textPrimary">No agents match “{trimmedQuery}”</p>
        {:else}
          <p class="text-meta font-medium text-textPrimary">
            {statusFilter === "running"
              ? "No running agents"
              : statusFilter === "done"
                ? "No finished agents"
                : "No agents need attention"}
          </p>
        {/if}
        <div class="flex items-center gap-2">
          {#if trimmedQuery}
            <button
              type="button"
              class="gp-btn"
              data-size="sm"
              data-testid="dock-agents-clear-search"
              onclick={() => {
                searchQuery = "";
                searchInputEl?.focus();
              }}
            >Clear search</button>
          {/if}
          {#if statusFilter !== "all"}
            <button
              type="button"
              class="gp-btn"
              data-size="sm"
              data-testid="dock-agents-show-all"
              onclick={() => (statusFilter = "all")}
            >Show all</button>
          {/if}
        </div>
      </div>
    {:else if effectiveGrouped}
      {#if showAttentionTray}
        <div data-testid="dock-agents-attention-tray" class="mb-3 rounded-lg bg-surface/70 p-2">
          <div class="flex items-center gap-1.5 px-1">
            {#if trayDangerCount > 0}
              <ShieldAlert size={13} class="shrink-0 text-danger" aria-hidden="true" />
            {:else if trayAttentionCount > 0}
              <AlertCircle size={13} class="shrink-0 text-warn" aria-hidden="true" />
            {:else}
              <CheckCircle2 size={13} class="shrink-0 text-ok" aria-hidden="true" />
            {/if}
            <span class="text-meta font-semibold text-textPrimary">{trayHeading}</span>
            {#if trayAttentionCount > 0}
              <span
                class="gp-chip shrink-0"
                data-tone={trayDangerCount > 0 ? "danger" : "warn"}
                data-kind="label"
                data-testid="dock-agents-tray-attention-count"
              >{trayAttentionCount} attention</span>
            {/if}
            {#if trayDoneCount > 0}
              <span
                class="gp-chip inline-flex shrink-0 items-center gap-1"
                data-tone="info"
                data-kind="label"
                data-testid="dock-agents-tray-done-count"
              >
                <CheckCircle2 size={10} class="text-ok shrink-0" aria-hidden="true" />
                {trayDoneCount} done
              </span>
            {/if}
            <button
              type="button"
              class="gp-icon-btn ml-auto h-5 w-5 shrink-0"
              aria-expanded={!attentionTrayCollapsed}
              aria-label={attentionTrayCollapsed ? "Expand attention tray" : "Collapse attention tray"}
              data-testid="dock-agents-attention-tray-toggle"
              onclick={() => (attentionTrayCollapsed = !attentionTrayCollapsed)}
            >
              {#if attentionTrayCollapsed}
                <ChevronRight size={12} aria-hidden="true" />
              {:else}
                <ChevronDown size={12} aria-hidden="true" />
              {/if}
            </button>
          </div>
          {#if !attentionTrayCollapsed}
            <!-- Justified: hosts ArrowUp/ArrowDown roving across tray items and repo-group rows. -->
            <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
            <ul role="list" class="mt-1.5 flex flex-col gap-0.5" onkeydown={onListKeydown}>
              {#each attentionTrayItems as item (item.row.id)}
                {@const row = item.row}
                {@const trayRepoPath = row.repoPath?.trim() || (item.isHere ? currentRepoPath : "") || ""}
                {@const trayRepoName = row.repoName?.trim() || (item.isHere ? currentRepoName : "") || undefined}
                {@const trayContext = trayRowContext(row, item.isHere)}
                <li>
                  <button
                    type="button"
                    class="gp-menu-item group min-w-0 focus-visible:outline-2 focus-visible:outline-accent {row.active ? 'bg-accent/10' : ''} {row.disabled ? 'opacity-50' : ''}"
                    data-testid="dock-agents-attention-item"
                    data-agent-id={row.id}
                    aria-current={row.active ? "true" : undefined}
                    disabled={row.disabled}
                    title={rowTitle(row, item.isHere)}
                    onclick={() => (item.isHere ? onSelect(row.id) : onGoTo(row.id))}
                  >
                    <LauncherIcon kind={row.launcher} size={14} class="shrink-0" />
                    {#if trayRepoPath}
                      <RepoLogo path={trayRepoPath} name={trayRepoName} size={15} class="shrink-0" />
                    {/if}
                    <span class="flex min-w-0 flex-1 flex-col">
                      <span class="flex min-w-0 items-center gap-2">
                        <span class="truncate font-medium text-textPrimary">{row.title}</span>
                        {#if row.active}
                          <span class="gp-chip shrink-0" data-tone="accent" data-kind="label">Active</span>
                        {/if}
                        {#if row.attention}
                          <span class="gp-chip shrink-0" data-tone={attentionTone(row.attention)} data-kind="label">{row.attention}</span>
                        {:else if row.done}
                          <span class="gp-chip inline-flex shrink-0 items-center gap-1" data-tone="info" data-kind="label">
                            <CheckCircle2 size={10} class="text-ok shrink-0" aria-hidden="true" />
                            Done
                          </span>
                        {/if}
                      </span>
                      {#if trayContext}
                        <span class="truncate font-mono text-micro text-textMuted">{trayContext}</span>
                      {/if}
                      {#if row.attentionDetail}
                        <span class="truncate text-micro text-textMuted">{row.attentionDetail}</span>
                      {/if}
                    </span>
                    {#if typeof row.lastActivityAt === "number" && row.lastActivityAt > 0}
                      <span class="ml-auto shrink-0 tabular-nums text-micro text-textMuted">{formatAge(row.lastActivityAt, now)}</span>
                    {/if}
                    <ArrowRight size={12} class="{typeof row.lastActivityAt === 'number' && row.lastActivityAt > 0 ? '' : 'ml-auto'} shrink-0 text-textMuted opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100" aria-hidden="true" />
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}

      <div class="flex flex-col gap-3">
        {#each repoGroups as group (group.key)}
          <div data-testid="dock-agents-repo-group" data-repo-key={group.key}>
            <button
              type="button"
              class="gp-menu-item w-full min-w-0 justify-start gap-1.5 px-1.5 py-1 text-left focus-visible:outline-2 focus-visible:outline-accent"
              aria-expanded={!group.collapsed}
              data-testid="dock-agents-repo-group-toggle"
              onclick={() => toggleRepoCollapsed(group.key)}
            >
              {#if group.collapsed}
                <ChevronRight size={13} class="shrink-0 text-textMuted" aria-hidden="true" />
              {:else}
                <ChevronDown size={13} class="shrink-0 text-textMuted" aria-hidden="true" />
              {/if}
              {#if group.repoPath}
                <RepoLogo path={group.repoPath} name={group.repoName} size={15} class="shrink-0" />
              {/if}
              <span class="truncate font-semibold text-textPrimary">{group.repoName}</span>
              {#if group.isCurrent}
                <span class="gp-chip shrink-0" data-tone="accent" data-kind="label">This repository</span>
              {/if}
              <span class="gp-chip shrink-0" data-kind="count">{group.items.length}</span>
              <span class="ml-auto flex shrink-0 items-center gap-1">
                {#if group.attentionCount > 0}
                  <span class="gp-chip shrink-0" data-tone="warn" data-kind="label" data-testid="dock-agents-group-attention">
                    {group.attentionCount} attention
                  </span>
                {/if}
                {#if group.doneCount > 0}
                  <span class="gp-chip inline-flex shrink-0 items-center gap-1" data-tone="info" data-kind="label" data-testid="dock-agents-group-done">
                    <CheckCircle2 size={10} class="text-ok shrink-0" aria-hidden="true" />
                    {group.doneCount} done
                  </span>
                {/if}
                {#if group.runningCount > 0}
                  <span class="gp-chip inline-flex shrink-0 items-center gap-1" data-tone="accent" data-kind="label">
                    <Loader2 size={10} class="gp-agent-spinner animate-spin shrink-0" aria-hidden="true" />
                    {group.runningCount} running
                  </span>
                {/if}
              </span>
            </button>
            {#if !group.collapsed}
              <!-- Justified: hosts ArrowUp/ArrowDown roving between row buttons inside the group. -->
              <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
              <ul role="list" class="mt-1 flex flex-col gap-0.5 pl-2" onkeydown={onListKeydown}>
                {#each group.items as item (item.row.id)}
                  {@render agentRow(item.row, item.isHere ? onSelect : onGoTo, item.isHere, false, true)}
                {/each}
              </ul>
            {/if}
          </div>
        {/each}
      </div>
    {:else}
      <div class="mt-1">
        <h3 class="gp-label mb-1.5 flex items-center gap-1.5">
          {#if currentRepoPath}
            <RepoLogo path={currentRepoPath} name={currentRepoName ?? undefined} size={14} class="shrink-0" />
          {/if}
          <span>In this repository</span>
          <span class="gp-chip" data-kind="count">{triageHere.length}</span>
        </h3>
        {#if triageHere.length > 0}
          {@render rowList(triageHere, onSelect, true)}
        {:else}
          <p class="px-2.5 text-micro text-textMuted">None in this repository</p>
        {/if}
      </div>
      {#if triageElsewhere.length > 0}
        <div class="mt-4">
          <h3 class="gp-label mb-1.5 flex items-center gap-1.5">
            <span>Other repositories</span>
            <span class="gp-chip" data-kind="count">{triageElsewhere.length}</span>
          </h3>
          {@render rowList(triageElsewhere, onGoTo, false, true)}
        </div>
      {/if}
    {/if}
  </div>
</section>

<style>
  @media (prefers-reduced-motion: no-preference) {
    .gp-live-dot {
      animation: gp-dock-live-pulse calc(var(--gp-dur-emphasis) * 3) var(--gp-ease-effect) infinite;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .gp-agent-spinner,
    .gp-live-dot {
      animation: none;
    }
  }
  :global(html[data-motion="reduced"]) .gp-agent-spinner,
  :global(html[data-motion="reduced"]) .gp-live-dot {
    animation: none;
  }
  @keyframes gp-dock-live-pulse {
    50% {
      opacity: 0.4;
    }
  }
</style>
