<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type { WorkProjection } from "../work/projection";
  import type { ViewLoader } from "./LazyView.svelte";

  // Survives the per-tab remount so revisiting Work renders the last join
  // instantly; the fetch below then refreshes it in place.
  import { createWorkRefresh } from "../work/refresh";
  const refreshWork = createWorkRefresh();
  const workCache = createRepoPanelCache<{ projection: WorkProjection; loadedAt: number }>();

  /**
   * Stable module-scope loader for the branch-chains card (LazyView keys its
   * cache on loader identity; an inline arrow would remount the view on every
   * parent update).
   */
  const loadStack: ViewLoader = () => import("./CodeStackViewer.svelte");

  /**
   * Overview's two collapsed disclosures, branch and tag cleanup and Policy
   * activity, load as chunks of their own. Imported eagerly they put ~48 KB in
   * the entry chunk and took `main` over the 640 KB production limit
   * (vite.config.ts). Both are bound (`bind:open`), which LazyView's props seam
   * cannot carry, so each is awaited in the template from a memoised loader:
   * one import per session, not one per render.
   */
  // A failed import clears its memo, so Try again (or the next deep link) asks again
  // instead of replaying the same rejection for the rest of the session.
  let cleanupModule: Promise<typeof import("./BranchTagCleanup.svelte")> | null = null;
  const loadBranchTagCleanup = () =>
    (cleanupModule ??= import("./BranchTagCleanup.svelte").catch((error: unknown) => {
      cleanupModule = null;
      throw error;
    }));
  let activityModule: Promise<typeof import("./PolicyActivity.svelte")> | null = null;
  const loadPolicyActivity = () =>
    (activityModule ??= import("./PolicyActivity.svelte").catch((error: unknown) => {
      activityModule = null;
      throw error;
    }));
</script>

<script lang="ts">
  import { tick, untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
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
  import LazyView from "./LazyView.svelte";
  import {
    degradedSummary,
    dirtyCount,
    dirtySummary,
    measuredDirty,
    isStaleRun,
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
  import { harnessStore, type PolicyStatus } from "../stores/harnessStore";
  import { policyActivityVisible } from "../harness/policyActivityVisible";
  import { openConflicts } from "../ui/workConflicts";
  import {
    MANVI_FOCUS_TARGETS,
    manviFocusRequest,
    manviSectionId,
    takeManviFocusFor,
    type ManviFocusId,
  } from "../ui/manviFocus";
  import { toastStore } from "../stores/toastStore";

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
    // Status roles, not palette shades: the role token resolves per theme, and
    // these chips are the exceptions the reader is here to notice.
    allowed: "text-textMuted bg-surfaceHover",
    demoted: "text-warn bg-warn/15",
    granted: "text-info bg-info/15",
    widened: "text-warn bg-warn/15",
    degraded: "text-warn bg-warn/15",
    warned: "text-warn bg-warn/15",
    blocked: "text-danger bg-danger/15",
    unchecked: "text-textMuted bg-surfaceHover",
  };

  function refresh(repo: string, force = false): void {
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
    }, { force });
  }

  // A repo switch hydrates from cache so the last join is on screen
  // immediately; a generation bump refreshes in place so "clean" cannot
  // outlive the working tree. Only activation bumps it (re-activating this
  // tab, or revealing it when a neighbour closes) — `refresh()`, the status
  // poll and watcher events never do (repoStore `bumped()`). Both keys
  // compare by value, so the store's other publishes run neither effect; the
  // hydration is declared first, so a switch hydrates before it refreshes.
  const generation = $derived($repoStore.generation);
  $effect(() => {
    const repo = $currentRepoPath;
    const cached = repo ? workCache.get(repo) : undefined;
    untrack(() => {
      projection = cached?.projection ?? null;
      loadedAt = cached?.loadedAt ?? null;
      navigationError = null;
      refreshError = null;
      collisions = null;
      collisionError = null;
    });
  });
  $effect(() => {
    const repo = $currentRepoPath;
    void generation;
    untrack(() => {
      if (repo) refresh(repo);
      else { refreshWork.cancel(); loading = false; refreshEpoch += 1; }
    });
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

  /**
   * Brings the conflict editor on screen.
   *
   * The editor leads this same section (`WorkOverviewPane`) while a merge needs
   * a person, so "Resolve" is two steps: make sure Overview is the section
   * showing, then scroll the editor to the top of the pane. From another
   * section the switch mounts the pane, which starts at the editor; from
   * Overview itself the reader may be scrolled down among the rows, and doing
   * nothing would read as a dead button.
   */
  function showConflicts(): void {
    openConflicts();
  }

  function reviewChanges(): void {
    void repoStore.previewUncommitted();
  }

  /**
   * Opens a row's worktree as a repository tab.
   *
   * This is what makes the screen a workspace rather than a report: the whole
   * point of showing that a worktree is stuck mid-rebase is being one click
   * from the conflict editor that unsticks it. Rows with several worktrees open
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
          if ($repoStore.operation.operation || $repoStore.statuses.some(status => status.is_conflicted)) showConflicts();
          else reviewChanges();
        },
      });
      if (!opened) navigationError = $repoStore.error || `Couldn't open ${path}.`;
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
  $effect(() => {
    void facet; void query; void $currentRepoPath;
    untrack(() => { rowLimit = 100; });
  });

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
      ? "border-accent bg-accent/10 ring-1 ring-accent/30"
      : "border-edge bg-surface hover:border-edgeStrong";
  }

  // A repository switch must not leave the previous repository's narrowing
  // on: the reader would be looking at a filtered view of a list they have
  // never seen, with rows missing for a reason that is off screen.
  $effect(() => {
    void $currentRepoPath;
    untrack(() => {
      facet = "all";
      query = "";
    });
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
  let showStackDetail = $state(false);
  // Opened by the reader or by a deep link, never on their own: these cards
  // answer questions about the repository rather than work in flight, so they
  // must not push the rows down.
  let showCleanup = $state(false);
  /** Bumped by Try again: re-runs a disclosure's import after a failed chunk load. */
  let cleanupAttempt = $state(0);
  let activityAttempt = $state(0);
  let showActivity = $state(false);

  const showPolicyActivity = $derived(
    policyActivityVisible($harnessStore.harness, $harnessStore.actions.length, $harnessStore.error),
  );

  /* --- deep links ---------------------------------------------------------- */

  /**
   * A deep link from elsewhere in the app (the header chips, the storage
   * panel) names a card, not just this view. Expanding the disclosure is only
   * half the answer: the card still has to be brought on screen and marked, or
   * the reader lands mid-page with no idea which card answered their click.
   */
  let flashTimer: number | null = null;
  let flashed: HTMLElement | null = null;
  let realignTimers: number[] = [];

  function clearFlash() {
    if (flashTimer !== null) {
      window.clearTimeout(flashTimer);
      flashTimer = null;
    }
    // The marked section belongs to a child component, so the class is
    // applied to the node rather than bound in one component's markup.
    flashed?.classList.remove("gp-focus-flash");
    flashed = null;
  }

  function clearRealign() {
    for (const timer of realignTimers) window.clearTimeout(timer);
    realignTimers = [];
  }

  /**
   * Cards above the target keep settling for a moment after the view mounts —
   * the rows resolve, a grant list arrives — and every height change slides
   * the section the reader was sent to, typically far enough to push its
   * heading off the top. Two bounded re-alignments cover the settle; a scroll
   * gesture cancels them, because yanking a reader back is worse than drift.
   */
  const REALIGN_DELAYS_MS = [0, 250];

  function keepAligned(section: HTMLElement) {
    clearRealign();
    for (const delay of REALIGN_DELAYS_MS) {
      realignTimers.push(
        window.setTimeout(() => section.scrollIntoView({ block: "start" }), delay),
      );
    }
    for (const event of ["wheel", "touchstart", "keydown"] as const) {
      window.addEventListener(event, clearRealign, { once: true, passive: true });
    }
  }

  async function revealFocus(id: ManviFocusId) {
    // The disclosure that holds the card is opened here, before looking for it.
    if (id === "cleanup") showCleanup = true;
    else if (id === "activity") showActivity = true;
    // Both disclosures arrive in chunks of their own: wait for the one asked
    // for, or the lookup below runs before its section exists and reports a
    // card that is merely still loading as absent. A failed load is shown in
    // place by its {:catch} notice, so it is not reported here as well.
    try {
      if (id === "cleanup") await loadBranchTagCleanup();
      else if (id === "activity" && showPolicyActivity) await loadPolicyActivity();
    } catch {
      return;
    }
    await tick();
    await tick();
    const section = document.getElementById(manviSectionId(id));
    if (!section) {
      // The card is not offered here (Policy activity with Manvi absent and
      // nothing recorded). The request was taken, so it cannot fire later;
      // say why nothing opened rather than leaving a dead click.
      toastStore.info(`${MANVI_FOCUS_TARGETS[id].label} has nothing to show: Manvi is not installed and nothing has been recorded.`);
      return;
    }
    // Instant, like every other scroll-into-view in the app: a deep link should
    // land deterministically, and a smooth 2000px glide is both a lot of motion
    // and, in a webview that is not compositing, no scroll at all.
    section.scrollIntoView({ block: "start" });
    keepAligned(section);
    // Sections carry tabindex="-1": keyboard and screen-reader users land on
    // the same card the scroll brought into view, not back at the page top.
    section.focus({ preventScroll: true });
    clearFlash();
    section.classList.add("gp-focus-flash");
    flashed = section;
    flashTimer = window.setTimeout(() => clearFlash(), 1600);
  }

  // Runs on mount (the view mounts lazily, after the click) and again whenever
  // a request arrives while the view is already open. Only requests whose card
  // lives here are taken; Settings' are left for Settings.
  $effect(() => {
    if (!$manviFocusRequest) return;
    const requested = takeManviFocusFor("view");
    if (requested) void revealFocus(requested);
  });

  $effect(() => {
    return () => {
      clearFlash();
      clearRealign();
      for (const event of ["wheel", "touchstart", "keydown"] as const) {
        window.removeEventListener(event, clearRealign);
      }
    };
  });
</script>

<!-- A scroller that is a column: every band below is `w-full`, so the content
     spans the whole pane at any window width rather than stopping at a fixed
     cap (it used to centre in max-w-6xl, which on a wide window left a quarter
     of the pane blank on each side), and the empty states take the leftover
     height rather than sitting under the header with the pane empty beneath. -->
<div class="work-overview flex flex-1 min-w-0 flex-col overflow-y-auto font-sans text-xs text-textPrimary">
  <div class="flex items-start justify-between gap-3 mb-3 w-full">
    <div class="min-w-0">
      <h2 class="text-lg font-semibold tracking-tight">Overview</h2>
      <p class="mt-1 text-textMuted text-xs leading-relaxed">
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
      onclick={() => $repoStore.currentPath && void refresh($repoStore.currentPath, true)}
    >
      <RefreshCw size={12} class={loading ? "animate-spin" : ""} />
      {loading ? "Refreshing" : "Refresh"}
    </button>
    <span class="text-micro text-textMuted" aria-live="polite">
      {#if loadedAt}Loaded {new Date(loadedAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}{/if}
    </span>
    </div>
  </div>
  {#if refreshError || navigationError || $repoStore.error}
    <p role="alert" class="mb-3 w-full text-warn">{refreshError || navigationError || $repoStore.error}</p>
  {/if}

  <!-- Where the reader is standing. First, because every other row on this
       screen is somewhere else. -->
  {#if $repoStore.currentPath}
    <section aria-label="Current repository" class="overview-repository mb-3 w-full gp-card p-4">
      <div class="overview-repository-heading">
        <div class="flex items-center gap-3 min-w-0">
          <div class="rounded-xl bg-accent/10 p-2 text-accent shrink-0"><Layers size={22} /></div>
          <div class="min-w-0">
            <p class="gp-label">Current repository</p>
            <h3 class="mt-1 text-lg font-semibold tracking-tight truncate" title={$repoStore.currentPath}>
              {$repoStore.currentPath.split(/[\\/]/).filter(Boolean).pop() || $repoStore.currentPath}
            </h3>
            <p class="mt-1 truncate font-mono text-meta text-textMuted" title={$repoStore.currentPath}>{$repoStore.currentPath}</p>
          </div>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          {#if (here && here.conflicted > 0) || $repoStore.operation.operation}
            <button type="button" class="gp-btn" data-variant="primary" onclick={showConflicts}>
              <GitMerge size={13} /> Resolve conflicts
            </button>
          {:else if here && here.staged + here.unstaged > 0}
            <button type="button" class="gp-btn" data-variant="primary" disabled={$repoStore.isLoading || Boolean($repoStore.error)} onclick={reviewChanges}>
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
        <span class="flex items-center gap-1.5 font-medium text-sm min-w-0">
          <GitBranch size={14} class="text-accent shrink-0" />
          <span class="truncate">{here.branch ?? "Detached HEAD"}</span>
          <span class="text-micro bg-accent/10 text-accent px-1.5 py-0.5 rounded-full font-mono">HEAD</span>
        </span>

        <!-- A branch the stats pass has not reached yet says so. Rendering its
             pre-fetch zeroes would claim it is level with a remote nobody has
             asked about. -->
        {#if !here.branch}
          <span class="text-meta text-textMuted">No tracking branch</span>
        {:else if here.unmeasured}
          <span class="text-meta text-textMuted font-mono">sync not measured yet</span>
        {:else if here.upstream === null}
          <span class="text-meta text-textMuted font-mono" title="No tracking branch is configured">
            no upstream
          </span>
        {:else if here.upstream.gone}
          <span class="gp-chip font-mono" data-tone="danger"
            title="{here.upstream.name} no longer exists on the remote">
            upstream gone
          </span>
        {:else if here.upstream.ahead > 0 || here.upstream.behind > 0}
          <span class="text-meta font-mono text-textMuted inline-flex items-center gap-1.5"
            title="Against {here.upstream.name}">
            {#if here.upstream.ahead > 0}
              <span class="inline-flex items-center gap-0.5 text-ok"><ArrowUp size={11} />{here.upstream.ahead}</span>
            {/if}
            {#if here.upstream.behind > 0}
              <span class="inline-flex items-center gap-0.5 text-warn"><ArrowDown size={11} />{here.upstream.behind}</span>
            {/if}
          </span>
        {:else}
          <span class="text-meta font-mono text-textMuted">in sync with {here.upstream.name}</span>
        {/if}

        {#if !$repoStore.statsPending && !here.unmeasured && here.behindBase > 0 && here.comparedTo && here.comparedTo !== here.branch}
          <span class="text-meta font-mono text-warn"
            title="{here.behindBase} commit{here.behindBase === 1 ? '' : 's'} on {here.comparedTo} that this branch does not have">
            {here.behindBase} behind {here.comparedTo}
          </span>
        {/if}

        <span class="ml-auto flex flex-wrap items-center gap-x-2.5 gap-y-1 text-meta font-mono">
          {#if $repoStore.error || $repoStore.isLoading}
            <span>Status {$repoStore.error ? "unavailable" : "loading"}</span>
          {:else}
          {#if here.conflicted > 0}
            <button
              type="button"
              class="text-danger hover:underline"
              onclick={showConflicts}
              title="Open the conflict editor"
            >
              {here.conflicted} conflicted
            </button>
          {/if}
          {#if here.staged > 0}
            <button type="button" class="text-ok hover:underline" onclick={() => void repoStore.previewUncommitted(undefined, true)} title="Preview staged files and modifications">{here.staged} staged</button>
          {/if}
          {#if here.unstaged > 0}
            <button type="button" class="text-warn hover:underline" onclick={() => void repoStore.previewUncommitted(undefined, false)} title="Preview unstaged files and modifications">{here.unstaged} unstaged</button>
          {/if}
          {#if here.staged + here.unstaged + here.conflicted === 0}
            <span class="text-textMuted">working tree clean</span>
          {/if}
          {/if}
        </span>
      </div>
      {:else}
        <p class="mt-4 text-meta text-textMuted">{$repoStore.isBare ? "Bare repository" : "No checked-out branch"}</p>
      {/if}

      <!-- The one thing here that cannot make progress on its own. -->
      {#if $repoStore.operation.operation}
        <button
          type="button"
          class="mt-2 flex w-full items-start gap-2 rounded-xl bg-warn/15 px-2.5 py-1.5 text-left text-meta text-warn hover:bg-warn/25"
          onclick={showConflicts}
          title="Open the conflict editor to finish or abort the {kindTitle($repoStore.operation.operation.kind).toLowerCase()}"
        >
          <GitMerge size={12} class="mt-px shrink-0" />
          <span class="min-w-0">{headline($repoStore.operation.operation)}</span>
        </button>
      {:else if $repoStore.operation.probeFailed}
        <p class="mt-2 text-meta text-warn">
          Couldn't check for a parked merge or rebase here — this line is not “nothing is parked”.
        </p>
      {/if}
    </section>
  {/if}

  {#if summary && $repoStore.currentPath}
    <!-- Each tile selects the rows it counted. Tiles the projection can only
         ever count as zero would be four doors to an empty room, so a zero
         tile stays readable but is not offered as a filter. -->
    <div aria-label="Workspace summary filters" class="overview-metrics mb-3 w-full grid grid-cols-2 md:grid-cols-4 gap-3">
      <button
        type="button"
        aria-pressed={facet === "all"}
        onclick={() => (facet = "all")}
        class="text-left rounded-xl border px-3 py-2 transition-[border-color,background-color] duration-(--gp-dur-instant) {tileClass('all')}"
      >
        <div class="gp-label flex items-center gap-1">
          <Layers size={11} /> Worktrees
        </div>
        <div class="overview-metric-value text-textPrimary">{worktreeListKnown ? summary.worktrees : "—"}</div>
        <div class="text-micro text-textMuted">
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
        class="text-left rounded-xl border px-3 py-2 transition-[border-color,background-color] duration-(--gp-dur-instant) disabled:cursor-default {tileClass('agents')}"
      >
        <div class="gp-label flex items-center gap-1">
          <Bot size={11} /> Agent worktrees
        </div>
        <div class="overview-metric-value text-textPrimary">{worktreeListKnown ? summary.agentSessions : "—"}</div>
        <div class="text-micro text-textMuted truncate">
          {!worktreeListKnown ? "Worktree list unavailable" : summary.agentKinds.length > 0 ? summary.agentKinds.join(", ") : "No agent worktrees detected"}
        </div>
      </button>
      <button
        type="button"
        aria-pressed={facet === "blocked"}
        disabled={summary.blocked === 0}
        onclick={() => toggleFacet("blocked")}
        class="text-left rounded-xl border px-3 py-2 transition-[border-color,background-color] duration-(--gp-dur-instant) disabled:cursor-default {tileClass('blocked')}"
      >
        <div class="gp-label flex items-center gap-1">
          <GitMerge size={11} /> Blocked
        </div>
        <div class="overview-metric-value {summary.blocked > 0 ? 'text-warn' : 'text-textPrimary'}">
          {summary.blocked > 0 || (worktreeListKnown && summary.unscannedOperations === 0) ? summary.blocked : "—"}
        </div>
        <div class="text-micro text-textMuted">{!worktreeListKnown ? "Operation status unavailable" : summary.unscannedOperations > 0 ? `${summary.unscannedOperations} worktrees not checked` : summary.blocked > 0 ? "Operations waiting for you" : "Parked merges, rebases & picks"}</div>
      </button>
      <button
        type="button"
        aria-pressed={facet === "pullRequests"}
        disabled={summary.pullRequests === 0}
        onclick={() => toggleFacet("pullRequests")}
        class="text-left rounded-xl border px-3 py-2 transition-[border-color,background-color] duration-(--gp-dur-instant) disabled:cursor-default {tileClass('pullRequests')}"
      >
        <div class="gp-label flex items-center gap-1">
          <GitPullRequest size={11} /> Pull requests
        </div>
        <div class="overview-metric-value text-textPrimary">{projection?.sources.github.present && (projection.sources.github.ok || summary.pullRequests > 0) ? summary.pullRequests : "—"}</div>
        <div class="text-micro text-textMuted">{!projection?.sources.github.present ? "GitHub not available" : !projection.sources.github.ok ? "Partial GitHub data" : "Across workspace branches"}</div>
      </button>
    </div>

    <div class="mb-3 w-full flex flex-wrap items-center justify-between gap-2">
      <h3 class="flex items-center gap-2 text-sm font-semibold">
        Work in flight
        <span class="rounded-full bg-surfaceHover px-2 py-0.5 text-micro font-mono font-normal text-textMuted">{visibleRows.length}{#if projection && visibleRows.length !== projection.rows.length} / {projection.rows.length}{/if}</span>
      </h3>
      <span class="text-meta text-textMuted">Blocked work appears first</span>
    </div>
    <div class="mb-3 w-full flex flex-wrap items-center gap-2">
      <label class="relative flex-1 min-w-52">
        <Search size={12} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none" />
        <input
          class="gp-field w-full pl-7!"
          type="search"
          placeholder="Filter by branch, path, task or pull request"
          aria-label="Filter work rows"
          bind:value={query}
        />
      </label>
      <button type="button" class="inline-flex items-center gap-1.5 rounded-full border px-3 py-2 text-meta {tileClass('attention')}"
        aria-pressed={facet === "attention"} onclick={() => toggleFacet("attention")}
        title="Parked operations, local changes, unscanned worktrees, failed CI or requested changes">
        <AlertTriangle size={12} /> Needs attention
      </button>
      <button
        type="button"
        class="overview-dirty-filter inline-flex items-center gap-1.5 rounded-full border px-3 py-2 text-meta transition-colors {tileClass('dirty')}"
        aria-pressed={facet === "dirty"}
        onclick={() => toggleFacet("dirty")}
        title="Show worktrees with measured uncommitted changes"
      >
        <FileDiff size={12} class="text-warn" />
        Uncommitted changes
      </button>
      {#if projection && (facet !== "all" || query.trim() !== "")}
        <span class="text-meta text-textMuted font-mono">
          {visibleRows.length} of {projection.rows.length}
        </span>
        <button
          type="button"
          class="gp-btn" data-size="sm"
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
        class="gp-btn" data-size="sm"
        onclick={openMcpSettings}
        title="Agents read this same snapshot over MCP 2.0"
      >
        <Plug size={12} class="text-accent" />
        Connect an agent
      </button>
    </div>
  {/if}

  {#if collisionError}
    <div
      class="gp-notice mb-3 w-full text-meta"
      data-tone="unknown"
    >
      <AlertTriangle size={14} class="shrink-0 mt-px text-unknown" />
      <span>Couldn't check overlapping files — {collisionError}. Absence of a list is not “no collisions”.</span>
      <button type="button" class="gp-btn shrink-0" data-size="sm" disabled={loading} onclick={() => $repoStore.currentPath && void refresh($repoStore.currentPath)}>Rescan</button>
    </div>
  {/if}
  {#if collisions && collisions.shared_worktrees.length > 0}
    <div
      class="mb-3 w-full rounded-xl border border-warn/35 bg-warn/15 p-2.5 text-meta text-warn"
    >
      <div class="flex items-start gap-2 font-medium">
        <AlertTriangle size={14} class="shrink-0 mt-px" />
        <span>Live agent sessions are sharing a worktree. Every dirty file there is open to all of them.</span>
      </div>
      <ul class="mt-1.5 ml-6 space-y-0.5 font-mono text-micro">
        {#each collisions.shared_worktrees as shared (shared.path)}
          <li>
            {shared.path}
            <span class="text-textMuted">
              — {shared.sessions.length} sessions ·
              {#if shared.scanned}
                {shared.files.length} dirty file{shared.files.length === 1 ? "" : "s"}{shared.truncated ? " (list truncated)" : ""}
              {:else}
                dirty files not scanned
              {/if}
            </span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}
  {#if collisions && (collisions.overlapping_files > 0 || collisions.unscanned_worktrees > 0 || collisions.failed_worktrees > 0 || collisions.truncated)}
    <div
      class="mb-3 w-full rounded-xl border border-warn/35 bg-warn/15 p-2.5 text-meta text-warn"
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
      {#if collisions.items[0]?.entity}
        {@const entity = collisions.items[0].entity}
        <p class="mt-1.5 ml-6">
          {#if entity.kind === "shared_symbol"}
            Shared symbol{entity.shared_symbols.length === 1 ? "" : "s"} on {entity.path}:
            {entity.shared_symbols.join(", ") || "unknown"} — real collision when these branches meet.
          {:else if entity.kind === "contended_line"}
            {entity.path}: edits touch at adjacent or overlapping lines
            {entity.contended_lines?.length ? ` (${entity.contended_lines.map(([s, e]) => (s === e ? `${s}` : `${s}-${e}`)).join(", ")})` : ""}
            — different symbols, but git merges this as a conflict.
          {:else if entity.kind === "disjoint_symbols"}
            {entity.path}: same file, distinct symbols on the old-side ranges — file overlap, not a merge promise.
          {:else}
            {entity.path}: file-level overlap
            {entity.reason ? ` (${entity.reason})` : ""}.
          {/if}
        </p>
      {/if}
      {#if collisions.failed_worktrees > 0 || collisions.unscanned_worktrees > 0 || collisions.truncated}
        <p class="mt-1">{collisions.scanned_worktrees} scanned · {collisions.failed_worktrees} failed · {collisions.unscanned_worktrees} unscanned{collisions.truncated ? " · results truncated" : ""}</p>
      {/if}
      {#if collisions.items.length > 0}
        <ul class="mt-1.5 ml-6 space-y-0.5 font-mono text-micro">
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
      class="gp-notice mb-3 w-full text-meta"
      data-tone="unknown"
    >
      <AlertTriangle size={14} class="shrink-0 mt-px text-unknown" />
      <span>{degraded}</span>
      <button type="button" class="gp-btn shrink-0" data-size="sm" disabled={loading} onclick={() => $repoStore.currentPath && void refresh($repoStore.currentPath)}>Rescan</button>
    </div>
  {/if}

  {#if !$repoStore.currentPath}
    <div class="flex flex-1 items-center justify-center">
      <EmptyState icon={LayoutGrid} title="No repository open" hint="Open a repository to see the work in it." />
    </div>
  {:else if loading && !projection}
    <div class="w-full space-y-2">
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
          : "Some workspace sources couldn't be read. Refresh to try again."}
      />
    </div>
  {:else if projection}
    <div class="w-full space-y-3">
      {#each renderedRows as row, i (row.key || `__unbound:${i}`)}
        {@const chips = noteworthyStatuses(row.verdicts)}
        {@const dirty = dirtySummary(row)}
        {@const changedWorktree = row.worktrees.find(binding => measuredDirty(binding.worktree.dirty_files) && binding.worktree.dirty_files > 0)?.worktree}
        {@const activity = rowLastActivity(row, $repoStore.branches)}
        <div
          class="overview-work-row gp-card p-4"
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
                    class="truncate text-left text-sm hover:text-accent"
                    title="Open {openPathFor(row)}"
                    onclick={() => void openWorktree(row)}
                  >
                    {rowTitle(row)}
                  </button>
                {:else}
                  <span class="truncate">{rowTitle(row)}</span>
                {/if}
                {#if row.worktrees.some((binding) => binding.worktree.path === $repoStore.currentPath)}
                  <span class="rounded-full bg-accent/10 px-2 py-0.5 text-micro text-accent">Current worktree</span>
                {/if}
                {#if row.taskId}
                  <span class="font-mono text-micro text-textMuted">{row.taskId}</span>
                {/if}
                <!-- An agent worktree and a hand-made one want opposite
                     remedies — merge or resume, versus prune — so they are
                     never labelled the same. The chip names the agent from
                     the directory layout, not a hard-coded product. -->
                {#each agentsOn(row) as kind (kind)}
                  <span
                    class="inline-flex items-center gap-1 rounded-full bg-accent/10 px-1.5 py-0.5 text-micro text-accent"
                    title={agentChipTitle(row, kind)}
                  >
                    <Bot size={10} />{kind}
                  </span>
                {/each}
                {#if row.worktrees.some((b) => b.worktree.is_main)}
                  <span class="rounded-full bg-surfaceHover px-1.5 py-0.5 text-micro text-textMuted">
                    main worktree
                  </span>
                {/if}
                {#if row.lease}
                  <span
                    class="rounded-full bg-info/15 px-1.5 py-0.5 text-micro text-info"
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
                <p class="mt-1.5 truncate font-mono text-micro text-textMuted" title={row.worktrees[0].worktree.path}>
                  {row.worktrees[0].worktree.path}
                </p>
              {/if}
              <div class="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-meta text-textMuted">
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
                  <button type="button" class="flex items-center gap-1 text-warn hover:underline" title="Preview uncommitted files in {changedWorktree?.path}"
                    onclick={() => changedWorktree && void repoStore.previewUncommitted(changedWorktree.path)}>
                    <FileDiff size={12} />
                    {dirty.files} uncommitted
                  </button>
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
                  <span class="text-warn">Operation not checked</span>
                {/if}
                {#if dirty.scanned < dirty.total && dirty.scanned > 0}
                  <span class="text-warn">{dirty.scanned} of {dirty.total} worktrees scanned</span>
                {/if}
                {#if projection.sources.ledger.present}
                  <span class="font-mono" title="Events attributed to this row within the latest 500 recorded events">
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
                <span class="rounded px-1.5 py-0.5 text-micro font-medium {STATUS_TONE[status]}">
                  {status} {count}
                </span>
              {/each}
              <!-- A verdict the ledger recorded and this build could not read.
                   Never folded into `allowed`: that is the exact shape of a
                   check that could not run reading as one that passed. -->
              {#if row.verdicts.unparsed > 0}
                <span
                  class="rounded bg-danger/15 px-1.5 py-0.5 text-micro font-medium text-danger"
                  title="Verdicts this build couldn't read. Not counted as allowed."
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
              class="mt-2 flex w-full items-start gap-2 rounded-xl bg-warn/15 px-2.5 py-1.5 text-left text-meta text-warn hover:bg-warn/25"
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
                {@const stale = isStaleRun(row, run)}
                <button type="button" class="gp-btn" data-size="sm" class:opacity-50={stale} data-stale-run={stale ? "true" : undefined} onclick={() => void openGithub(run.url)}
                  title={stale ? `Stale: workflow run ${run.id} ran on ${run.head_sha.slice(0, 7)}, not this worktree's current commit. Open on GitHub` : `Open workflow run ${run.id} on GitHub`}>
                  <Play size={11} /> {run.name || "Workflow"}: {run.status.toLowerCase() === "completed" ? run.conclusion || "conclusion unknown" : run.status || "status unknown"}{#if stale} (stale, {run.head_sha.slice(0, 7)}){/if}
                </button>
              {/each}
            </div>
          {/if}
          {#if (row.kind !== "worktree" && row.worktrees.length > 0) || row.pullRequests.length > 0}
            <div class="mt-3 grid gap-2 md:grid-cols-2">
              {#if row.kind !== "worktree" && row.worktrees.length > 0}
                <ul class="space-y-1">
                  {#each row.worktrees as binding, i (`${binding.worktree.path}#${i}`)}
                    <li class="flex items-center gap-2">
                      <button
                        type="button"
                        class="flex w-full items-center gap-1.5 rounded font-mono text-micro text-textMuted hover:text-accent"
                        title="Open {binding.worktree.path}"
                        onclick={() => void openBinding(binding)}
                      >
                        <GitBranch size={11} class="shrink-0" />
                        <span class="truncate">{binding.worktree.branch ?? "(detached)"}</span>
                        {#if binding.worktree.is_locked}
                          <span class="text-textMuted">·locked</span>
                        {/if}
                      </button>
                      {#if measuredDirty(binding.worktree.dirty_files) && binding.worktree.dirty_files > 0}
                        <button type="button" class="shrink-0 text-micro text-warn hover:underline" title="Preview uncommitted files in {binding.worktree.path}"
                          onclick={() => void repoStore.previewUncommitted(binding.worktree.path)}>{binding.worktree.dirty_files} dirty</button>
                      {/if}
                    </li>
                  {/each}
                </ul>
              {/if}
              {#if row.pullRequests.length > 0}
                <ul class="space-y-1">
                  {#each row.pullRequests as pr (pr.number)}
                    <li class="flex items-center gap-1.5 text-meta">
                      <GitPullRequest size={11} class="shrink-0 text-accent" />
                      <span class="min-w-0 flex-1">
                        <span class="block truncate" title={pr.title}>#{pr.number} {pr.title}</span>
                        <span class="text-micro text-textMuted">{pr.is_draft ? "Draft · " : ""}CI {pr.ci_status || "unknown"} · {pr.review_decision ? pr.review_decision.toLowerCase().replaceAll("_", " ") : "Review not reported"}</span>
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
    <div class="mt-4 w-full">
      <button
        type="button"
        class="flex w-full items-center gap-1.5 rounded-xl border border-edge px-3 py-2 text-meta font-medium text-textMuted hover:bg-surfaceHover"
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

    <div class="mt-4 w-full">
      <button
        type="button"
        class="flex w-full items-center gap-1.5 rounded-xl border border-edge px-3 py-2 text-meta font-medium text-textMuted hover:bg-surfaceHover"
        onclick={() => (showStackDetail = !showStackDetail)}
        aria-expanded={showStackDetail}
      >
        <ChevronRight
          size={13}
          class="shrink-0 transition-transform {showStackDetail ? 'rotate-90' : ''}"
        />
        Branch chains and stacked diffs
      </button>
      {#if showStackDetail}
        <div class="mt-2">
          <LazyView load={loadStack} name="branch chains" />
        </div>
      {/if}
    </div>

    {#key cleanupAttempt}
      {#await loadBranchTagCleanup() then { default: BranchTagCleanup }}
        <BranchTagCleanup bind:open={showCleanup} />
      {:catch error}
        <p class="gp-notice flex items-center gap-2" data-tone="danger" role="alert">
          <span class="min-w-0 flex-1">Couldn't load branch and tag cleanup. {formatError(error)}</span>
          <button type="button" class="gp-btn" onclick={() => cleanupAttempt++}>Try again</button>
        </p>
      {/await}
    {/key}
    {#if showPolicyActivity}
      {#key activityAttempt}
        {#await loadPolicyActivity() then { default: PolicyActivity }}
          <PolicyActivity bind:open={showActivity} />
        {:catch error}
          <p class="gp-notice flex items-center gap-2" data-tone="danger" role="alert">
            <span class="min-w-0 flex-1">Couldn't load Policy activity. {formatError(error)}</span>
            <button type="button" class="gp-btn" onclick={() => activityAttempt++}>Try again</button>
          </p>
        {/await}
      {/key}
    {/if}
  {/if}
</div>

<style>
  .work-overview {
    container-type: inline-size;
    /* Block padding is a band of pane background under the section tabs and
       again under the last block. Inline padding stays. */
    padding-inline: clamp(16px, 1.6vw, 24px);
    padding-block: 0;
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
    border-radius: var(--gp-r-md);
  }

  .overview-metric-value {
    margin: 4px 0 3px;
    font-size:var(--text-display);
    font-weight: 600;
    line-height: 1.15;
    letter-spacing: -0.04em;
    font-variant-numeric: tabular-nums;
  }

  .overview-work-row-blocked {
    border-color: rgb(var(--c-warn) / 0.35);
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
