<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type { WorkProjection } from "../work/projection";

  // Survives the per-tab remount so revisiting Work renders the last join
  // instantly; the fetch below then refreshes it in place.
  import { createWorkRefresh } from "../work/refresh";
  const refreshWork = createWorkRefresh();
  const workCache = createRepoPanelCache<{ projection: WorkProjection; loadedAt: number }>();
</script>

<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { openExternal } from "../desktop/openExternal";
  import {
    AlertTriangle,
    ArrowDown,
    ArrowUp,
    ExternalLink,
    GitBranch,
    GitPullRequest,
    LayoutGrid,
    Play,
    RefreshCw,
    Search,
    ShieldCheck,
    Trees,
    Bot,
    FileDiff,
    GitMerge,
    ChevronRight,
    Plug,
    Layers,
  } from "@lucide/svelte";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import RepoPanel from "./RepoPanel.svelte";
  import {
    degradedSummary,
    dirtyCount,
    dirtySummary,
    measuredDirty,
    latestRuns,
    insightSummary,
    noteworthyStatuses,
    openPathFor,
    type WorkRow,
    type WorktreeBinding,
  } from "../work/projection";
  import type { CollisionRisk } from "../insights/types";
  import { formatError } from "../ui/formatError";
  import { headline, kindTitle } from "../repos/operation";
  import { agentKind, agentKindsOn, agentSessionSlug } from "../work/agentWorktree";
  import {
    filterWorkRows,
    hereSummary,
    rowLastActivity,
    type WorkFacet,
  } from "../work/focus";
  import { timestampFormat } from "../ui/timestampFormat";
  import type { PolicyStatus } from "../stores/harnessStore";

  let projection = $state<WorkProjection | null>(null);
  let collisions = $state<CollisionRisk | null>(null);
  let collisionError = $state<string | null>(null);
  let loading = $state(false);
  let loadedAt = $state<number | null>(null);
  let refreshError = $state<string | null>(null);
  let navigationError = $state<string | null>(null);
  let refreshEpoch = 0;
  let disposed = false;

  /**
   * Tone per policy status. Spelled out rather than derived from the name so
   * a status added to the union without a colour here is a compile-time gap,
   * not a chip that silently renders as the default.
   */
  const STATUS_TONE: Record<PolicyStatus, string> = {
    // Each carries both shades: a single fixed one is legible in one theme
    // only, and these chips are the exceptions the reader is here to notice.
    allowed: "text-textMuted bg-surfaceHover",
    demoted: "text-amber-700 dark:text-amber-400 bg-amber-500/10",
    granted: "text-sky-700 dark:text-sky-400 bg-sky-500/10",
    widened: "text-amber-700 dark:text-amber-400 bg-amber-500/10",
    degraded: "text-amber-700 dark:text-amber-400 bg-amber-500/10",
    warned: "text-amber-700 dark:text-amber-400 bg-amber-500/10",
    blocked: "text-rose-700 dark:text-rose-400 bg-rose-500/10",
    unchecked: "text-textMuted bg-surfaceHover",
  };

  function refresh(repo: string): void {
    const epoch = ++refreshEpoch;
    loading = true;
    refreshError = null;
    const live = () => !disposed && $repoStore.currentPath === repo;
    refreshWork.request(repo, {
      projection(result, time) {
        if (!live()) return;
        projection = result;
        loadedAt = time;
        workCache.set(repo, { projection: result, loadedAt: time });
      },
      collisions(result, error) {
        if (!live()) return;
        collisions = result;
        collisionError = error;
      },
      finished(error) {
        if (!live() || epoch !== refreshEpoch) return;
        refreshError = error;
        loading = false;
      },
    });
  }

  let previousRepo: string | null = null;
  let previousGeneration: number | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    const generation = $repoStore.generation;
    if (repo === previousRepo && generation === previousGeneration) return;
    const repoChanged = repo !== previousRepo;
    previousRepo = repo;
    previousGeneration = generation;
    // A repo switch hydrates from cache so the last join is on screen
    // immediately; a generation bump (status poll, mutation, watcher)
    // refreshes in place so "clean" cannot outlive the working tree.
    if (repoChanged) {
      const cached = repo ? workCache.get(repo) : undefined;
      projection = cached?.projection ?? null;
      loadedAt = cached?.loadedAt ?? null;
      navigationError = null;
      refreshError = null;
      collisions = null;
      collisionError = null;
    }
    if (repo) refresh(repo);
    else { refreshWork.cancel(); loading = false; refreshEpoch += 1; }
  });

  $effect(() => () => { disposed = true; refreshWork.cancel(); });

  function rowTitle(row: WorkRow): string {
    // "Not bound to a task" is only meaningful where tasks exist. In a
    // repository with no task store the catch-all holds pull requests and
    // runs whose branch no worktree has checked out — say that instead.
    if (row.kind === "unbound") {
      return hasTasks ? "Not bound to a task" : "Not checked out anywhere";
    }
    return row.title || row.taskId || "(untitled)";
  }

  const hasTasks = $derived(projection?.sources.tasks.present === true);

  function reviewChanges(): void {
    const first = $repoStore.statuses[0];
    if (first) void repoStore.selectFileDiff(first.path, first.is_staged);
    else repoStore.setActiveTab("history", "graph");
  }

  /**
   * Opens a row's worktree as a repository tab.
   *
   * This is what makes the screen a workspace rather than a report: the whole
   * point of showing that a worktree is stuck mid-rebase is being one click
   * from the Resolve view that unsticks it. Rows with several worktrees open
   * the first — the one the row is keyed on in worktree mode.
   */
  async function openWorktree(row: WorkRow): Promise<void> {
    const path = openPathFor(row);
    if (!path) return;
    await openCheckout(path);
  }

  async function openCheckout(path: string): Promise<void> {
    navigationError = null;
    try {
      const opened = await repoStore.openRepo(path, {
        onReady: () => {
          if ($repoStore.operation.operation || $repoStore.statuses.some(status => status.is_conflicted)) repoStore.setActiveTab("work", "resolve");
          else reviewChanges();
        },
      });
      if (!opened) navigationError = $repoStore.error || `Could not open ${path}`;
    } catch (error) { navigationError = formatError(error); }
  }

  async function openGithub(url: string): Promise<void> {
    try { await openExternal(url); }
    catch (error) { navigationError = formatError(error); }
  }

  function agentsOn(row: WorkRow): string[] {
    return agentKindsOn(row.worktrees.map((binding) => binding.worktree.path));
  }

  function agentChipTitle(row: WorkRow, kind: string): string {
    const slugs = row.worktrees
      .filter((binding) => agentKind(binding.worktree.path) === kind)
      .map((binding) => agentSessionSlug(binding.worktree.path))
      .filter(Boolean);
    const sessions = slugs.length > 0 ? ` (${slugs.join(", ")})` : "";
    return `A worktree ${kind} created for a session${sessions}`;
  }

  async function openBinding(binding: WorktreeBinding): Promise<void> {
    await openCheckout(binding.worktree.path);
  }

  const degraded = $derived(projection ? degradedSummary(projection.sources) : "");
  const summary = $derived(projection ? insightSummary(projection) : null);
  const worktreeListKnown = $derived(projection?.sources.worktrees.ok === true || (summary?.worktrees ?? 0) > 0);

  /**
   * Where the reader is standing.
   *
   * Work described every worktree in flight and never the one checked out in
   * front of them — not the branch, not whether it had drifted from its
   * remote, not what was uncommitted in it. Those are the questions asked most
   * often on this screen and they were answered nowhere on it.
   */
  const here = $derived(
    hereSummary($repoStore.currentBranch, $repoStore.branches, $repoStore.statuses, Boolean($repoStore.currentPath) && !$repoStore.isBare),
  );

  /* --- narrowing ---------------------------------------------------------- */

  /**
   * The counts above the rows are doors now.
   *
   * "3 blocked" over a list of forty was a number the reader then had to go
   * find. Selecting a tile filters to exactly the rows it counted, so the
   * strip and the list can never disagree about what "blocked" means.
   */
  let facet = $state<WorkFacet>("all");
  let query = $state("");
  let rowLimit = $state(100);
  $effect(() => { void facet; void query; void $repoStore.currentPath; rowLimit = 100; });

  const visibleRows = $derived(
    projection ? filterWorkRows(projection.rows, facet, query) : [],
  );
  const renderedRows = $derived(visibleRows.slice(0, rowLimit));
  /** True when a filter is on and has hidden every row. */
  const narrowedToNothing = $derived(
    projection !== null && projection.rows.length > 0 && visibleRows.length === 0,
  );

  function toggleFacet(next: WorkFacet) {
    facet = facet === next ? "all" : next;
  }

  /** Tile chrome: pressed tiles read as selected rather than merely hovered. */
  function tileClass(target: WorkFacet): string {
    return facet === target
      ? "border-accent/60 bg-accent/10 ring-1 ring-accent/30"
      : "border-border/70 bg-surface hover:border-accent/40";
  }

  // A repository switch must not leave the previous repository's narrowing
  // on: the reader would be looking at a filtered view of a list they have
  // never seen, with rows missing for a reason that is off screen.
  let facetRepo: string | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    if (repo === facetRepo) return;
    facetRepo = repo;
    facet = "all";
    query = "";
  });

  function openMcpSettings() {
    window.dispatchEvent(new CustomEvent("gitpulse:settings"));
  }

  /**
   * Remotes, submodules and the stash, folded in from what used to be a
   * separate Repo view.
   *
   * They belong on this page — "where does this push to, why is that folder
   * empty, what did I put aside" are questions about the same repository this
   * screen is already describing — but they are reference material, not work
   * in flight, so they start collapsed and never push the rows down.
   */
  let showRepoDetail = $state(false);
</script>

<!-- A scroller that is a column: every band below is `mx-auto w-full max-w-6xl`,
     so the content centres in a wide pane instead of hugging the left edge and
     leaving a third of the window blank, and the empty states take the leftover
     height rather than sitting under the header with the pane empty beneath. -->
<div class="work-overview flex flex-1 min-w-0 flex-col overflow-y-auto p-4 font-sans text-[12px] text-textPrimary">
  <div class="flex items-start justify-between gap-3 mb-3 mx-auto w-full max-w-6xl">
    <div class="min-w-0">
      <h2 class="text-xl font-semibold tracking-tight">Overview</h2>
      <p class="mt-1 text-textMuted text-[12px] leading-relaxed">
        {hasTasks
          ? "Your workspace, from tasks to pull requests."
          : "Your workspace, from local changes to pull requests."}
      </p>
    </div>
    <div class="flex flex-col items-end gap-1 shrink-0">
    <button
      type="button"
      class="gp-btn shrink-0 mt-1"
      disabled={loading || !$repoStore.currentPath}
      onclick={() => $repoStore.currentPath && void refresh($repoStore.currentPath)}
    >
      <RefreshCw size={12} class={loading ? "animate-spin" : ""} />
      {loading ? "Refreshing" : "Refresh"}
    </button>
    <span class="text-[10px] text-textMuted" aria-live="polite">
      {#if loadedAt}Loaded {new Date(loadedAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}{:else if loading}Loading workspace…{/if}
    </span>
    </div>
  </div>
  {#if refreshError || navigationError || $repoStore.error}
    <p role="alert" class="mx-auto mb-3 w-full max-w-6xl text-amber-700 dark:text-amber-300">{refreshError || navigationError || $repoStore.error}</p>
  {/if}

  <!-- Where the reader is standing. First, because every other row on this
       screen is somewhere else. -->
  {#if $repoStore.currentPath}
    <section aria-label="Current repository" class="overview-repository mb-3 mx-auto w-full max-w-6xl rounded-2xl border border-border/70 bg-surface p-4 shadow-card">
      <div class="overview-repository-heading">
        <div class="flex items-center gap-3 min-w-0">
          <div class="rounded-xl bg-accent/10 p-2 text-accent shrink-0"><Layers size={22} /></div>
          <div class="min-w-0">
            <p class="text-[10px] font-semibold uppercase tracking-widest text-textMuted">Current repository</p>
            <h3 class="mt-1 text-[21px] font-semibold tracking-tight truncate" title={$repoStore.currentPath}>
              {$repoStore.currentPath.split(/[\\/]/).filter(Boolean).pop() || $repoStore.currentPath}
            </h3>
            <p class="mt-1 truncate font-mono text-[11px] text-textMuted" title={$repoStore.currentPath}>{$repoStore.currentPath}</p>
          </div>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          {#if (here && here.conflicted > 0) || $repoStore.operation.operation}
            <button type="button" class="gp-btn-primary" onclick={() => repoStore.setActiveTab("work", "resolve")}>
              <GitMerge size={13} /> Open Resolve
            </button>
          {:else if here && here.staged + here.unstaged > 0}
            <button type="button" class="gp-btn-primary" disabled={$repoStore.isLoading || Boolean($repoStore.error)} onclick={reviewChanges}>
              <FileDiff size={13} /> Review changes <ChevronRight size={12} />
            </button>
          {/if}
          <button type="button" class="gp-btn" onclick={() => repoStore.setActiveTab("code", "explorer")}>
            Browse files <ChevronRight size={12} />
          </button>
          <button type="button" class="gp-btn" onclick={() => repoStore.setActiveTab("history", "graph")}>
            View history <ChevronRight size={12} />
          </button>
        </div>
      </div>
      {#if here}
      <div class="overview-branch flex flex-wrap items-center gap-x-3 gap-y-2 mt-3 rounded-xl bg-background/50 px-3 py-2.5">
        <span class="flex items-center gap-1.5 font-medium text-[13px] min-w-0">
          <GitBranch size={14} class="text-accent shrink-0" />
          <span class="truncate">{here.branch ?? "Detached HEAD"}</span>
          <span class="text-[10px] bg-accent/10 text-accent px-1.5 py-0.5 rounded-full font-mono">HEAD</span>
        </span>

        <!-- A branch the stats pass has not reached yet says so. Rendering its
             pre-fetch zeroes would claim it is level with a remote nobody has
             asked about. -->
        {#if !here.branch}
          <span class="text-[11px] text-textMuted">No tracking branch</span>
        {:else if here.unmeasured}
          <span class="text-[11px] text-textMuted font-mono">sync not measured yet</span>
        {:else if here.upstream === null}
          <span class="text-[11px] text-textMuted font-mono" title="No tracking branch is configured">
            no upstream
          </span>
        {:else if here.upstream.gone}
          <span class="gp-pill border-rose-500/30! bg-rose-500/10! text-rose-700! dark:text-rose-300! font-mono"
            title="{here.upstream.name} no longer exists on the remote">
            upstream gone
          </span>
        {:else if here.upstream.ahead > 0 || here.upstream.behind > 0}
          <span class="text-[11px] font-mono text-textMuted inline-flex items-center gap-1.5"
            title="Against {here.upstream.name}">
            {#if here.upstream.ahead > 0}
              <span class="inline-flex items-center gap-0.5 text-emerald-600 dark:text-emerald-400"><ArrowUp size={11} />{here.upstream.ahead}</span>
            {/if}
            {#if here.upstream.behind > 0}
              <span class="inline-flex items-center gap-0.5 text-amber-600 dark:text-amber-400"><ArrowDown size={11} />{here.upstream.behind}</span>
            {/if}
          </span>
        {:else}
          <span class="text-[11px] font-mono text-textMuted">in sync with {here.upstream.name}</span>
        {/if}

        {#if !$repoStore.statsPending && !here.unmeasured && here.behindBase > 0 && here.comparedTo && here.comparedTo !== here.branch}
          <span class="text-[11px] font-mono text-amber-600 dark:text-amber-400"
            title="{here.behindBase} commit{here.behindBase === 1 ? '' : 's'} on {here.comparedTo} that this branch does not have">
            {here.behindBase} behind {here.comparedTo}
          </span>
        {/if}

        <span class="ml-auto flex flex-wrap items-center gap-x-2.5 gap-y-1 text-[11px] font-mono">
          {#if $repoStore.error || $repoStore.isLoading}
            <span>Status {$repoStore.error ? "unavailable" : "loading"}</span>
          {:else}
          {#if here.conflicted > 0}
            <button
              type="button"
              class="text-rose-600 dark:text-rose-400 hover:underline"
              onclick={() => repoStore.setActiveTab("work", "resolve")}
              title="Open Resolve"
            >
              {here.conflicted} conflicted
            </button>
          {/if}
          {#if here.staged > 0}
            <span class="text-emerald-600 dark:text-emerald-400">{here.staged} staged</span>
          {/if}
          {#if here.unstaged > 0}
            <span class="text-amber-600 dark:text-amber-400">{here.unstaged} unstaged</span>
          {/if}
          {#if here.staged + here.unstaged + here.conflicted === 0}
            <span class="text-textMuted">working tree clean</span>
          {/if}
          {/if}
        </span>
      </div>
      {:else}
        <p class="mt-4 text-[11px] text-textMuted">{$repoStore.isBare ? "Bare repository" : "No checked-out branch"}</p>
      {/if}

      <!-- The one thing here that cannot make progress on its own. -->
      {#if $repoStore.operation.operation}
        <button
          type="button"
          class="mt-2 flex w-full items-start gap-2 rounded-xl border border-amber-500/40 bg-amber-500/10 px-2.5 py-1.5 text-left text-[11px] text-amber-600 hover:bg-amber-500/20 dark:text-amber-300"
          onclick={() => repoStore.setActiveTab("work", "resolve")}
          title="Open Resolve to finish or abort the {kindTitle($repoStore.operation.operation.kind).toLowerCase()}"
        >
          <GitMerge size={12} class="mt-px shrink-0" />
          <span class="min-w-0">{headline($repoStore.operation.operation)}</span>
        </button>
      {:else if $repoStore.operation.probeFailed}
        <p class="mt-2 text-[11px] text-amber-600 dark:text-amber-400">
          Could not check for a parked merge or rebase here — this line is not “nothing is parked”.
        </p>
      {/if}
    </section>
  {/if}

  {#if summary && $repoStore.currentPath}
    <!-- Each tile selects the rows it counted. Tiles the projection can only
         ever count as zero would be four doors to an empty room, so a zero
         tile stays readable but is not offered as a filter. -->
    <div aria-label="Workspace summary filters" class="overview-metrics mb-3 mx-auto w-full max-w-6xl grid grid-cols-2 md:grid-cols-4 gap-3">
      <button
        type="button"
        aria-pressed={facet === "all"}
        onclick={() => (facet = "all")}
        class="text-left rounded-xl border px-3 py-2 transition-[border-color,background-color] duration-150 {tileClass('all')}"
      >
        <div class="text-[10px] uppercase tracking-wider text-textMuted flex items-center gap-1">
          <Layers size={11} /> Worktrees
        </div>
        <div class="overview-metric-value text-textPrimary">{worktreeListKnown ? summary.worktrees : "—"}</div>
        <div class="text-[10px] text-textMuted">
          {#if !worktreeListKnown}Worktree list unavailable{:else}
            {#if !projection?.sources.worktrees.ok}Partial data · {/if}{summary.dirtyWorktrees} dirty{#if summary.unscannedDirty > 0}
              · {summary.unscannedDirty} unscanned{/if}
          {/if}
        </div>
      </button>
      <button
        type="button"
        aria-pressed={facet === "agents"}
        disabled={summary.agentSessions === 0}
        onclick={() => toggleFacet("agents")}
        class="text-left rounded-xl border px-3 py-2 transition-[border-color,background-color] duration-150 disabled:cursor-default {tileClass('agents')}"
      >
        <div class="text-[10px] uppercase tracking-wider text-textMuted flex items-center gap-1">
          <Bot size={11} /> Agent worktrees
        </div>
        <div class="overview-metric-value text-textPrimary">{worktreeListKnown ? summary.agentSessions : "—"}</div>
        <div class="text-[10px] text-textMuted truncate">
          {!worktreeListKnown ? "Worktree list unavailable" : summary.agentKinds.length > 0 ? summary.agentKinds.join(", ") : "No agent worktrees detected"}
        </div>
      </button>
      <button
        type="button"
        aria-pressed={facet === "blocked"}
        disabled={summary.blocked === 0}
        onclick={() => toggleFacet("blocked")}
        class="text-left rounded-xl border px-3 py-2 transition-[border-color,background-color] duration-150 disabled:cursor-default {tileClass('blocked')}"
      >
        <div class="text-[10px] uppercase tracking-wider text-textMuted flex items-center gap-1">
          <GitMerge size={11} /> Blocked
        </div>
        <div class="overview-metric-value {summary.blocked > 0 ? 'text-amber-600 dark:text-amber-400' : 'text-textPrimary'}">
          {summary.blocked > 0 || (worktreeListKnown && summary.unscannedOperations === 0) ? summary.blocked : "—"}
        </div>
        <div class="text-[10px] text-textMuted">{!worktreeListKnown ? "Operation status unavailable" : summary.unscannedOperations > 0 ? `${summary.unscannedOperations} worktrees not checked` : summary.blocked > 0 ? "Operations waiting for you" : "Parked merges, rebases & picks"}</div>
      </button>
      <button
        type="button"
        aria-pressed={facet === "pullRequests"}
        disabled={summary.pullRequests === 0}
        onclick={() => toggleFacet("pullRequests")}
        class="text-left rounded-xl border px-3 py-2 transition-[border-color,background-color] duration-150 disabled:cursor-default {tileClass('pullRequests')}"
      >
        <div class="text-[10px] uppercase tracking-wider text-textMuted flex items-center gap-1">
          <GitPullRequest size={11} /> Pull requests
        </div>
        <div class="overview-metric-value text-textPrimary">{projection?.sources.github.present && (projection.sources.github.ok || summary.pullRequests > 0) ? summary.pullRequests : "—"}</div>
        <div class="text-[10px] text-textMuted">{!projection?.sources.github.present ? "GitHub not available" : !projection.sources.github.ok ? "Partial GitHub data" : "Across workspace branches"}</div>
      </button>
    </div>

    <div class="mb-3 mx-auto w-full max-w-6xl flex flex-wrap items-center justify-between gap-2">
      <h3 class="flex items-center gap-2 text-[13px] font-semibold">
        Work in flight
        <span class="rounded-full bg-surfaceHover px-2 py-0.5 text-[10px] font-mono font-normal text-textMuted">{visibleRows.length}{#if projection && visibleRows.length !== projection.rows.length} / {projection.rows.length}{/if}</span>
      </h3>
      <span class="text-[11px] text-textMuted">Blocked work appears first</span>
    </div>
    <div class="mb-3 mx-auto w-full max-w-6xl flex flex-wrap items-center gap-2">
      <label class="relative flex-1 min-w-52">
        <Search size={12} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none" />
        <input
          class="gp-field w-full pl-7! py-2!"
          type="search"
          placeholder="Filter by branch, path, task or pull request"
          aria-label="Filter work rows"
          bind:value={query}
        />
      </label>
      <button type="button" class="inline-flex items-center gap-1.5 rounded-full border px-3 py-2 text-[11px] {tileClass('attention')}"
        aria-pressed={facet === "attention"} onclick={() => toggleFacet("attention")}
        title="Parked operations, local changes, unscanned worktrees, failed CI or requested changes">
        <AlertTriangle size={12} /> Needs attention
      </button>
      <button
        type="button"
        class="overview-dirty-filter inline-flex items-center gap-1.5 rounded-full border px-3 py-2 text-[11px] transition-colors {tileClass('dirty')}"
        aria-pressed={facet === "dirty"}
        onclick={() => toggleFacet("dirty")}
        title="Show worktrees with measured uncommitted changes"
      >
        <FileDiff size={12} class="text-amber-600 dark:text-amber-400" />
        Uncommitted changes
      </button>
      {#if projection && (facet !== "all" || query.trim() !== "")}
        <span class="text-[11px] text-textMuted font-mono">
          {visibleRows.length} of {projection.rows.length}
        </span>
        <button
          type="button"
          class="px-2 py-1 rounded-lg border border-border/70 hover:bg-surfaceHover text-[11px]"
          onclick={() => {
            facet = "all";
            query = "";
          }}
        >
          Clear
        </button>
      {/if}
      <button
        type="button"
        class="inline-flex items-center gap-1.5 px-2 py-1 rounded-lg border border-border/70 hover:bg-surfaceHover text-[11px]"
        onclick={openMcpSettings}
        title="Agents read this same snapshot over MCP 2.0 (gitpulse_insights)"
      >
        <Plug size={12} class="text-accent" />
        Connect an agent
      </button>
    </div>
  {/if}

  {#if collisionError}
    <div
      class="mb-3 mx-auto w-full max-w-6xl flex items-start gap-2 rounded-xl border border-amber-500/30 bg-amber-500/10 p-2.5 text-[11px] text-amber-700 dark:text-amber-300"
    >
      <AlertTriangle size={14} class="shrink-0 mt-px" />
      <span>Could not check overlapping files — {collisionError}. Absence of a list is not “no collisions”.</span>
    </div>
  {/if}
  {#if collisions && (collisions.overlapping_files > 0 || collisions.unscanned_worktrees > 0 || collisions.failed_worktrees > 0 || collisions.truncated)}
    <div
      class="mb-3 mx-auto w-full max-w-6xl rounded-xl border border-amber-500/30 bg-amber-500/10 p-2.5 text-[11px] text-amber-700 dark:text-amber-300"
    >
      <div class="flex items-start gap-2 font-medium">
        <AlertTriangle size={14} class="shrink-0 mt-px" />
        <span>
          {#if collisions.overlapping_files > 0}
            {collisions.overlapping_files} file{collisions.overlapping_files === 1 ? "" : "s"} dirty in more than one worktree
            ({collisions.worktrees_involved} worktrees).
          {:else}
            Overlap results are incomplete.
          {/if}
        </span>
      </div>
      {#if collisions.failed_worktrees > 0 || collisions.unscanned_worktrees > 0 || collisions.truncated}
        <p class="mt-1">{collisions.scanned_worktrees} scanned · {collisions.failed_worktrees} failed · {collisions.unscanned_worktrees} unscanned{collisions.truncated ? " · results truncated" : ""}</p>
      {/if}
      {#if collisions.items.length > 0}
        <ul class="mt-1.5 ml-6 space-y-0.5 font-mono text-[10px]">
          {#each collisions.items.slice(0, 8) as item, i (`${item.path}#${i}`)}
            <li>
              {item.path}
              <span class="text-textMuted">
                — {item.worktrees.map((w) => w.agent_kind || w.branch || w.path).join(", ")}
              </span>
            </li>
          {/each}
        </ul>
        {#if collisions.items.length > 8}<p class="mt-1">Showing 8 of {collisions.items.length} reported overlapping paths.</p>{/if}
      {/if}
    </div>
  {/if}

  <!-- Stated before the rows, not after them. A join assembled from a source
       that could not be read looks exactly like one assembled from a source
       that was empty, and the reader has to know which they are looking at
       before they start reading it. -->
  {#if degraded}
    <div
      class="mb-3 mx-auto w-full max-w-6xl flex items-start gap-2 rounded-xl border border-amber-500/30 bg-amber-500/10 p-2.5 text-[11px] text-amber-700 dark:text-amber-300"
    >
      <AlertTriangle size={14} class="shrink-0 mt-px" />
      <span>{degraded}</span>
    </div>
  {/if}

  {#if !$repoStore.currentPath}
    <div class="flex flex-1 items-center justify-center">
      <EmptyState icon={LayoutGrid} title="No repository open" hint="Open a repository to see the work in it." />
    </div>
  {:else if loading && !projection}
    <div class="mx-auto w-full max-w-6xl space-y-2">
      <Skeleton />
      <Skeleton />
      <Skeleton />
    </div>
  {:else if narrowedToNothing}
    <!-- A filter hiding every row is not the same screen as a repository with
         nothing in flight, and must never borrow its wording. -->
    <div class="flex flex-1 items-center justify-center">
      <EmptyState
        icon={Search}
        title="No row matches this filter"
        hint="{projection?.rows.length ?? 0} rows are loaded; the current filter matches none of them."
        action={{ label: "Clear filter", onClick: () => { facet = "all"; query = ""; } }}
      />
    </div>
  {:else if projection && projection.rows.length === 0}
    <!-- Reaching here means git listed no worktrees at all, which is close to
         impossible for an open repository — every repository has at least its
         own. The old text blamed a missing DevCouncil store, which named a
         system most readers do not run and offered them nothing to do. -->
    <div class="flex flex-1 items-center justify-center">
      <EmptyState
        icon={LayoutGrid}
        title={projection.degraded ? "Workspace data unavailable" : "Nothing in flight"}
        hint={!projection.degraded
          ? "No worktrees, pull requests or runs were found for this repository."
          : "Some workspace sources could not be read. Refresh to try again."}
      />
    </div>
  {:else if projection}
    <div class="mx-auto w-full max-w-6xl space-y-3">
      {#each renderedRows as row, i (row.key || `__unbound:${i}`)}
        {@const chips = noteworthyStatuses(row.verdicts)}
        {@const dirty = dirtySummary(row)}
        {@const activity = rowLastActivity(row, $repoStore.branches)}
        <div
          class="overview-work-row rounded-2xl border border-border/70 bg-surface p-4 shadow-card"
          class:opacity-80={row.kind === "unbound"}
          class:overview-work-row-blocked={row.operation !== null}
          data-work-row
        >
          <div class="overview-row-heading flex items-start justify-between gap-3">
            <div class="min-w-0">
              <div class="flex items-center gap-2 flex-wrap font-medium">
                {#if row.worktrees.length > 0}
                  <button
                    type="button"
                    class="truncate text-left text-[13px] hover:text-accent"
                    title="Open {openPathFor(row)}"
                    onclick={() => void openWorktree(row)}
                  >
                    {rowTitle(row)}
                  </button>
                {:else}
                  <span class="truncate">{rowTitle(row)}</span>
                {/if}
                {#if row.worktrees.some((binding) => binding.worktree.path === $repoStore.currentPath)}
                  <span class="rounded-full bg-accent/10 px-2 py-0.5 text-[10px] text-accent">Current worktree</span>
                {/if}
                {#if row.taskId}
                  <span class="font-mono text-[10px] text-textMuted">{row.taskId}</span>
                {/if}
                <!-- An agent worktree and a hand-made one want opposite
                     remedies — merge or resume, versus prune — so they are
                     never labelled the same. The chip names the agent from
                     the directory layout, not a hard-coded product. -->
                {#each agentsOn(row) as kind (kind)}
                  <span
                    class="inline-flex items-center gap-1 rounded-full bg-accent/10 px-1.5 py-0.5 text-[10px] text-accent"
                    title={agentChipTitle(row, kind)}
                  >
                    <Bot size={10} />{kind}
                  </span>
                {/each}
                {#if row.worktrees.some((b) => b.worktree.is_main)}
                  <span class="rounded-full bg-surfaceHover px-1.5 py-0.5 text-[10px] text-textMuted">
                    main worktree
                  </span>
                {/if}
                {#if row.lease}
                  <span
                    class="rounded-full bg-sky-500/10 px-1.5 py-0.5 text-[10px] text-sky-400"
                    title="Leased by {row.lease.owner}{row.lease.agent
                      ? ` (${row.lease.agent})`
                      : ''}{row.lease.expires_at
                      ? `, expires ${row.lease.expires_at}`
                      : ', never expires'}"
                  >
                    {row.lease.status}
                  </span>
                {/if}
              </div>
              {#if row.kind === "worktree" && row.worktrees.length > 0}
                <p class="mt-1.5 truncate font-mono text-[10px] text-textMuted" title={row.worktrees[0].worktree.path}>
                  {row.worktrees[0].worktree.path}
                </p>
              {/if}
              <div class="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-textMuted">
                <!-- A worktree row IS one worktree; printing "1" beside every
                     one of them is noise that crowds out the counts that vary. -->
                {#if row.kind !== "worktree"}
                  <span class="flex items-center gap-1" title="Worktrees">
                    <Trees size={12} />
                    {row.worktrees.length}
                  </span>
                {/if}
                <span class="flex items-center gap-1" title="Open pull requests">
                  <GitPullRequest size={12} />
                  {#if !projection.sources.github.ok && row.pullRequests.length === 0}
                    PRs unknown
                  {:else if !projection.sources.github.present}
                    PRs unavailable
                  {:else}
                    {row.pullRequests.length} PR{row.pullRequests.length === 1 ? "" : "s"}
                  {/if}
                </span>
                <span class="flex items-center gap-1" title="Workflow runs">
                  <Play size={12} />
                  {#if !projection.sources.github.ok && row.runs.length === 0}
                    Runs unknown
                  {:else if !projection.sources.github.present}
                    Runs unavailable
                  {:else}
                    {row.runs.length} run{row.runs.length === 1 ? "" : "s"}
                  {/if}
                </span>
                <!-- Grants are a DevCouncil concept. Showing "0" of them to a
                     reader who runs no store is a column that can only ever
                     say zero. -->
                {#if row.grants.length > 0}
                  <span class="flex items-center gap-1" title="Grants applied">
                    <ShieldCheck size={12} />
                    {row.grants.length}
                  </span>
                {/if}
                <!-- -1 means the count was never taken (bare, or past the scan
                     cap). Rendering that as 0 would report an unscanned
                     worktree as verified clean. -->
                {#if dirtyCount(row) > 0}
                  <span class="flex items-center gap-1 text-amber-700 dark:text-amber-400" title="Uncommitted files">
                    <FileDiff size={12} />
                    {dirty.files} uncommitted
                  </span>
                {:else if dirtyCount(row) === 0}
                  <span class="flex items-center gap-1" title="No uncommitted changes">
                    <FileDiff size={12} />
                    clean
                  </span>
                {:else if row.worktrees.length > 0 && dirty.total === 0}
                  <span class="text-textMuted">Bare repository</span>
                {:else if row.worktrees.length > 0}
                  <span class="text-textMuted">Changes not scanned</span>
                {/if}
                {#if row.worktrees.some(binding => binding.operationChecked === false)}
                  <span class="text-amber-700 dark:text-amber-400">Operation not checked</span>
                {/if}
                {#if dirty.scanned < dirty.total && dirty.scanned > 0}
                  <span class="text-amber-700 dark:text-amber-400">{dirty.scanned} of {dirty.total} worktrees scanned</span>
                {/if}
                {#if projection.sources.ledger.present}
                  <span class="font-mono" title="Events attributed to this row within the latest 500 ledger records">
                    {row.verdicts.events} recent events
                  </span>
                {/if}
                <!-- How long this row has been sitting. A worktree nobody has
                     touched in three weeks and one from ten minutes ago read
                     identically without it, and they want opposite things
                     done. Absent when no branch on the row has been measured,
                     rather than rendered as the epoch. -->
                {#if activity}
                  <span title="{activity.branch} last moved{activity.author ? ` — ${activity.author}` : ''}">
                    {$timestampFormat.text(activity.timestamp)}
                  </span>
                {/if}
              </div>
            </div>

            <div class="flex flex-wrap items-center justify-end gap-1 shrink-0">
              {#each chips as [status, count] (status)}
                <span class="rounded px-1.5 py-0.5 text-[10px] font-medium {STATUS_TONE[status]}">
                  {status} {count}
                </span>
              {/each}
              <!-- A verdict the ledger recorded and this build could not read.
                   Never folded into `allowed`: that is the exact shape of a
                   check that could not run reading as one that passed. -->
              {#if row.verdicts.unparsed > 0}
                <span
                  class="rounded bg-rose-500/10 px-1.5 py-0.5 text-[10px] font-medium text-rose-700 dark:text-rose-400"
                  title="Verdicts this build could not read. Not counted as allowed."
                >
                  unreadable {row.verdicts.unparsed}
                </span>
              {/if}
            </div>
          </div>

          <!-- A worktree stopped mid-rebase is blocked on a person, and it
               is the only thing on this screen that cannot make progress on
               its own. It gets a line of its own, above the counts. -->
          {#if row.operation}
            <button
              type="button"
              class="mt-2 flex w-full items-start gap-2 rounded-xl border border-amber-500/40 bg-amber-500/10 px-2.5 py-1.5 text-left text-[11px] text-amber-600 hover:bg-amber-500/20 dark:text-amber-300"
              onclick={() => openWorktree(row)}
              title="Open this worktree to finish or abort the {kindTitle(row.operation.kind).toLowerCase()}"
            >
              <GitMerge size={12} class="mt-px shrink-0" />
              <span class="min-w-0">
                {headline(row.operation)}
                {#if row.operation.conflicted_total > 0}
                  — {row.operation.conflicted_total} file{row.operation.conflicted_total === 1
                    ? ""
                    : "s"} still conflicted
                {/if}
              </span>
            </button>
          {/if}

          {#if row.runs.length > 0}
            <div class="mt-2 flex flex-wrap gap-2" aria-label="Latest observed workflow runs">
              {#each latestRuns(row) as run (run.id)}
                <button type="button" class="gp-btn text-[10px]!" onclick={() => void openGithub(run.url)}
                  title={`Open workflow run ${run.id} on GitHub`}>
                  <Play size={11} /> {run.name || "Workflow"}: {run.status.toLowerCase() === "completed" ? run.conclusion || "conclusion unknown" : run.status || "status unknown"}
                </button>
              {/each}
            </div>
          {/if}
          {#if (row.kind !== "worktree" && row.worktrees.length > 0) || row.pullRequests.length > 0}
            <div class="mt-2.5 grid gap-2 border-t border-border/50 pt-2.5 md:grid-cols-2">
              {#if row.kind !== "worktree" && row.worktrees.length > 0}
                <ul class="space-y-1">
                  {#each row.worktrees as binding, i (`${binding.worktree.path}#${i}`)}
                    <li>
                      <button
                        type="button"
                        class="flex w-full items-center gap-1.5 rounded font-mono text-[10px] text-textMuted hover:text-accent"
                        title="Open {binding.worktree.path}"
                        onclick={() => void openBinding(binding)}
                      >
                        <GitBranch size={11} class="shrink-0" />
                        <span class="truncate">{binding.worktree.branch ?? "(detached)"}</span>
                        {#if measuredDirty(binding.worktree.dirty_files) && binding.worktree.dirty_files > 0}
                          <span class="text-amber-500 dark:text-amber-400"
                            >·{binding.worktree.dirty_files} dirty</span
                          >
                        {/if}
                        {#if binding.worktree.is_locked}
                          <span class="text-textMuted">·locked</span>
                        {/if}
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
              {#if row.pullRequests.length > 0}
                <ul class="space-y-1">
                  {#each row.pullRequests as pr (pr.number)}
                    <li class="flex items-center gap-1.5 text-[11px]">
                      <GitPullRequest size={11} class="shrink-0 text-accent" />
                      <span class="min-w-0 flex-1">
                        <span class="block truncate" title={pr.title}>#{pr.number} {pr.title}</span>
                        <span class="text-[10px] text-textMuted">{pr.is_draft ? "Draft · " : ""}CI {pr.ci_status || "unknown"} · {pr.review_decision ? pr.review_decision.toLowerCase().replaceAll("_", " ") : "Review not reported"}</span>
                      </span>
                      <button
                        type="button"
                        class="shrink-0 text-textMuted hover:text-accent"
                        aria-label={`Open PR #${pr.number} on GitHub`}
                        onclick={() => void openGithub(pr.url)}
                      >
                        <ExternalLink size={11} />
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
          {/if}
        </div>
      {/each}
      {#if renderedRows.length < visibleRows.length}
        <button type="button" class="gp-btn" onclick={() => rowLimit += 100}>
          Show more · {renderedRows.length} of {visibleRows.length} matching rows shown
        </button>
      {/if}
    </div>
  {/if}

  {#if $repoStore.currentPath}
    <div class="mt-4 mx-auto w-full max-w-6xl">
      <button
        type="button"
        class="flex w-full items-center gap-1.5 rounded-xl border border-border/70 px-3 py-2 text-[11px] font-medium text-textMuted hover:bg-surfaceHover"
        onclick={() => (showRepoDetail = !showRepoDetail)}
        aria-expanded={showRepoDetail}
      >
        <ChevronRight
          size={13}
          class="shrink-0 transition-transform {showRepoDetail ? 'rotate-90' : ''}"
        />
        Remotes, submodules and stash
      </button>
      {#if showRepoDetail}
        <div class="mt-2">
          <RepoPanel embedded />
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .work-overview {
    container-type: inline-size;
    padding: clamp(16px, 1.6vw, 24px);
  }

  .overview-repository {
    background-image: radial-gradient(ellipse at top left, rgb(var(--c-accent) / 0.07), transparent 65%);
  }

  .overview-repository-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 20px;
  }

  .overview-repository-heading > :first-child {
    flex: 1 1 240px;
  }

  .overview-metrics > button {
    min-width: 0;
    padding: 10px 14px;
    border-radius: 14px;
  }

  .overview-metric-value {
    margin: 4px 0 3px;
    font-size: 24px;
    font-weight: 600;
    line-height: 1.15;
    letter-spacing: -0.04em;
    font-variant-numeric: tabular-nums;
  }

  .overview-work-row-blocked {
    border-color: rgb(245 158 11 / 0.3);
  }

  .overview-row-heading > :first-child {
    flex: 1;
  }

  @container (max-width: 660px) {
    .overview-metrics { grid-template-columns: repeat(2, minmax(0, 1fr)); }
    .overview-row-heading { flex-wrap: wrap; }
    .overview-branch > :last-child { margin-left: 0; width: 100%; }
  }
</style>
