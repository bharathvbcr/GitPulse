<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import {
    formatAge,
    hoursToFirstReview,
    openHours,
    summarizeVelocity,
  } from "../github/prVelocity";
  import type { WorkflowsReport, GitHubContext } from "../github/types";



  // Survive the per-tab remount so revisiting the GitHub view renders the
  // last-known context and workflow listing instantly; the fetches then
  // refresh them in place.
  const ctxCache = createRepoPanelCache<GitHubContext>();
  const workflowsCache = createRepoPanelCache<WorkflowsReport>();
</script>

<script lang="ts">
  import { SvelteSet } from "svelte/reactivity";
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { filterStore } from "../stores/filterStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    Github,
    GitPullRequest,
    ExternalLink,
    Play,
    GitBranch,
    Tag,
    Workflow,
    RotateCcw,
    Square,
    Terminal,
    LoaderCircle,
    CircleDot,
    ChevronRight,
    Plus,
    Search,
    X,
  } from "lucide-svelte";
  import { openExternal as openExternalUrl } from "../desktop/openExternal";
  import FreshnessBadge from "./FreshnessBadge.svelte";
  import { freshnessStore } from "../provenance/store";
  import { prFreshness, prRevisions } from "../provenance/pullRequests";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { harnessStore, verdictLabel, type Guarded } from "../stores/harnessStore";
  import type {
    WorkflowInfo,
    WorkflowRunInfo,
    CiLocalReport,
  } from "../github/types";
  import {
    canCancelRun,
    canRerunRun,
    ciLocalVerdict,
    ciStepClass,
    isWorkflowDispatchable,
    workflowStateLabel,
  } from "../github/runActions";
  import { formatReleaseDate } from "../ops/model";
  import { pullRequestCreateUrl } from "../github/compareUrl";
  import {
    filterIssues,
    filterPullRequests,
    PR_FACET_LABELS,
    PR_FACETS,
    prFacetCounts,
    relativeAge,
    runsOnBranch,
    type PrFacet,
  } from "../github/remoteFilter";
  import { formatError } from "../ui/formatError";
  import { reportPanelError } from "../diagnostics/report";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";

  let ctx = $state<GitHubContext | null>(null);

  // Read once per render pass: a fresh Date.now() per row would make two PRs
  // opened in the same second disagree about their age.
  let velocityNow = $state(Date.now());
  const velocity = $derived(summarizeVelocity(ctx?.pull_requests ?? [], velocityNow));
  const prCreateUrl = $derived(
    pullRequestCreateUrl(
      ctx?.html_url ?? "",
      $repoStore.defaultBranch ?? "main",
      $repoStore.currentBranch ?? "",
    ),
  );
  let loading = $state(false);
  /** PR numbers with a checkout in flight; any in-flight checkout disables all. */
  const checkingOut = new SvelteSet<number>();
  let inflight: AsyncGuard | null = null;

  // --- Actions workflows (CI/CD) ---
  let workflows = $state<WorkflowsReport | null>(null);
  let workflowsLoading = $state(false);
  let workflowsInflight: AsyncGuard | null = null;
  /** Ref a dispatched workflow runs at; prefilled from the current branch. */
  let dispatchRef = $state("");
  /** Selector of the one trigger in flight; non-null disables every action. */
  let triggering = $state(false);

  // --- Run actions (re-run / cancel) ---
  let busyRunAction = $state<string | null>(null);

  // --- CI:local ---
  let ciRunning = $state(false);
  let ciReport = $state<CiLocalReport | null>(null);
  let ciError = $state<string | null>(null);
  /** A CI run outlives branch switches; the guard retires superseded runs. */
  let ciInflight: AsyncGuard | null = null;

  /** Panel-scoped feedback for the last attempted action; cleared on reload. */
  let actionNotice = $state<string | null>(null);
  let actionError = $state<string | null>(null);

  /* --- narrowing ----------------------------------------------------------
     Five listings on one screen with no way into any of them: thirty open
     pull requests rendered thirty cards and the reader scrolled. The counts
     that matter were already being computed for the velocity strip and
     connected to nothing. Facet and list share one predicate, so a chip's
     number and the rows under it cannot disagree. */
  let prFacet = $state<PrFacet>("all");
  let prQuery = $state("");
  let issueQuery = $state("");
  /** Narrows the run list to the checked-out branch. */
  let runsThisBranch = $state(false);
  /** Collapses the CI:local report without discarding it. */
  let ciReportOpen = $state(true);

  const prCounts = $derived(prFacetCounts(ctx?.pull_requests ?? []));
  const visiblePrs = $derived(filterPullRequests(ctx?.pull_requests ?? [], prFacet, prQuery));
  const visibleIssues = $derived(filterIssues(ctx?.issues ?? [], issueQuery));
  const visibleRuns = $derived(
    runsOnBranch(ctx?.workflow_runs ?? [], runsThisBranch ? ($repoStore.currentBranch ?? "") : ""),
  );
  /** True when a filter is on and has hidden every row that was fetched. */
  const prsNarrowedToNothing = $derived(
    (ctx?.pull_requests.length ?? 0) > 0 && visiblePrs.length === 0,
  );
  const issuesNarrowedToNothing = $derived(
    (ctx?.issues.length ?? 0) > 0 && visibleIssues.length === 0,
  );
  const prFilterOn = $derived(prFacet !== "all" || prQuery.trim() !== "");

  /** When the context on screen was fetched; drives the staleness stamp. */
  let fetchedAt = $state<number | null>(null);
  /** Ticks the relative stamp without refetching anything. */
  let clockTick = $state(Date.now());
  $effect(() => {
    if (typeof setInterval === "undefined") return;
    const timer = setInterval(() => (clockTick = Date.now()), 30_000);
    return () => clearInterval(timer);
  });
  const fetchedAgo = $derived(
    fetchedAt === null ? "" : relativeAge(new Date(fetchedAt).toISOString(), clockTick),
  );

  function clearPrFilter() {
    prFacet = "all";
    prQuery = "";
  }

  function emptyContext(error: string): GitHubContext {
    return {
      available: false,
      cli_present: false,
      host: "",
      owner: "",
      repo: "",
      html_url: "",
      pull_requests: [],
      prs_truncated: false,
      issues: [],
      issues_truncated: false,
      issues_error: null,
      workflow_runs: [],
      runs_error: null,
      runs_truncated: false,
      releases: [],
      releases_truncated: false,
      releases_error: null,
      warnings: [],
      error,
    };
  }

  async function loadFor(repo: string) {
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    try {
      const next = await invoke<GitHubContext>("cmd_github_context", { repoPath: repo });
      if (!guard.isLive()) return;
      ctx = next;
      fetchedAt = Date.now();
      // One clock per fetch, so two pull requests opened in the same second
      // cannot disagree about their age.
      velocityNow = Date.now();
      ctxCache.set(repo, next);
    } catch (err) {
      if (!guard.isLive()) return;
      ctx = emptyContext(formatError(err));
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  async function loadWorkflowsFor(repo: string) {
    workflowsInflight?.cancel();
    const guard = createAsyncGuard();
    workflowsInflight = guard;
    workflowsLoading = true;
    try {
      const next = await invoke<WorkflowsReport>("cmd_github_workflows", { repoPath: repo });
      if (!guard.isLive()) return;
      workflows = next;
      workflowsCache.set(repo, next);
    } catch (err) {
      if (!guard.isLive()) return;
      workflows = {
        available: false,
        cli_present: true,
        workflows: [],
        truncated: false,
        error: formatError(err),
      };
    } finally {
      if (guard.isLive()) workflowsLoading = false;
    }
  }

  async function loadAll() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    actionNotice = null;
    actionError = null;
    await Promise.all([loadFor(repo), loadWorkflowsFor(repo)]);
  }

  $effect(() => {
    return () => {
      inflight?.cancel();
      workflowsInflight?.cancel();
      ciInflight?.cancel();
    };
  });

  /**
   * Provenance freshness for every open pull request's head, in one batched
   * call.
   *
   * One base for the whole batch, so it is the repository's default branch
   * rather than each PR's own `base_ref` — which all but a stacked PR targets
   * anyway. The badge is claiming how far the mainline has moved past the
   * point where the head was verified, and for a stacked PR that is still a
   * true statement, just not the tightest one available.
   *
   * Each head is asked about under both `origin/<ref>` and `<ref>`; a head
   * that resolves under neither reads as *unknown*, never as unverified.
   */
  const defaultBranchName = $derived(
    $repoStore.branches.find((b) => b.is_default && !b.is_remote)?.name ?? null,
  );

  $effect(() => {
    const repo = $repoStore.currentPath;
    const prs = ctx?.pull_requests ?? [];
    const base = defaultBranchName;
    if (!repo || prs.length === 0) return;
    void freshnessStore.load(repo, prRevisions(prs), base);
  });

  // Reload context + workflows only when the real dependencies change (repo,
  // checked-out branch for the dispatch-ref default). Every status-poll /
  // stats-drain emission re-runs this effect otherwise, blanking ctx
  // mid-flight, clearing checkout spinners, and clobbering the dispatchRef
  // input the user may be editing.
  let prevRepo: string | null = null;
  let prevBranch: string | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    const branch = $repoStore.currentBranch;
    if (repo === prevRepo && branch === prevBranch) return;
    prevRepo = repo;
    prevBranch = branch;
    // Hydrate last-known data synchronously so a revisit renders instantly;
    // the fetches below then refresh it in place.
    ctx = ctxCache.get(repo ?? "") ?? null;
    workflows = workflowsCache.get(repo ?? "") ?? null;
    // A hydrated context is from whenever it was fetched, not from now; the
    // refetch below stamps it. Narrowing does not survive a repository or
    // branch switch either — rows missing for a reason that scrolled away
    // with the previous repository is worse than showing all of them.
    fetchedAt = null;
    clearPrFilter();
    issueQuery = "";
    runsThisBranch = false;
    ciReportOpen = true;
    checkingOut.clear();
    ciReport = null;
    ciError = null;
    ciRunning = false;
    actionNotice = null;
    actionError = null;
    // Dispatch ref is repo-scoped: default to the checked-out branch.
    dispatchRef = $repoStore.currentBranch ?? "";
    if (!repo) {
      inflight?.cancel();
      workflowsInflight?.cancel();
      ciInflight?.cancel();
      loading = false;
      workflowsLoading = false;
      return;
    }
    void loadFor(repo);
    void loadWorkflowsFor(repo);
    const startedCtx = inflight;
    const startedWf = workflowsInflight;
    return () => {
      if (inflight === startedCtx) startedCtx?.cancel();
      if (workflowsInflight === startedWf) startedWf?.cancel();
    };
  });

  // Both shades on every verdict. A single `-400` is tuned for the dark theme
  // and sits at roughly 2:1 against the light theme's near-white card — on
  // exactly the labels a reader opened this page to check.
  function ciClass(status: string): string {
    const s = status.toLowerCase();
    if (s === "success" || s === "completed") return "text-green-700 dark:text-green-400";
    if (s === "failure" || s === "cancelled" || s === "timed_out") return "text-red-700 dark:text-red-400";
    // `waiting`/`requested` sit behind deployment protections/approvals —
    // in flight, never a verdict.
    if (s === "pending" || s === "in_progress" || s === "queued" || s === "waiting" || s === "requested")
      return "text-amber-700 dark:text-amber-400";
    return "text-textMuted";
  }

  function runLabel(run: WorkflowRunInfo): string {
    return run.conclusion || run.status || "unknown";
  }

  /**
   * Opens advisory/GitHub URLs through the canonical OS opener. The shared
   * opener throws on failure (no window.open fallback: it could navigate the
   * app shell); here the failure is surfaced in-place via the action banner
   * and the persistent diagnostics ring.
   */
  async function openExternal(url: string) {
    try {
      await openExternalUrl(url);
    } catch (err) {
      actionError = reportPanelError("github", err);
    }
  }

  /**
   * Files a gated action's policy verdict with the harness store so the
   * journal records what the gate decided — including "no gate present".
   * These actions bypass repoStore.runMutating, so without this the verdict
   * would be dropped on the floor.
   */
  function fileVerdict(result: Guarded<string> | null, repoPath: string) {
    harnessStore.recordVerdict(result?.policy ?? null, repoPath);
  }

  /** Builds the action notice from gh's output, with verdict context. */
  function actionMessage(result: Guarded<string>, fallback: string): string {
    const base = result.output || fallback;
    return `${base}${result.policy ? ` — ${verdictLabel(result.policy)}` : ""}`;
  }

  async function checkoutPr(number: number) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    // Track per-id so concurrent checkouts cannot clobber each other's
    // spinner state; while any is running every button is disabled.
    checkingOut.add(number);
    try {
      const result = await invoke<Guarded<string>>("cmd_github_checkout_pr", {
        repoPath: repo,
        number,
      });
      fileVerdict(result, repo);
      if ($repoStore.currentPath !== repo) return;
      await repoStore.refresh(repo);
      if ($repoStore.currentPath !== repo) return;
      // Post-mutation reload keeps the visible filter context: a bare
      // loadGraph(repo) reset the graph to query=""/HEAD while FilterBar
      // still showed the selection. The backend applies every query term,
      // so the reload answers exactly what the scheduler keys on.
      await graphStore.loadGraph(repo, $filterStore.searchQuery, $filterStore.selectedBranch);
    } catch (err: unknown) {
      if ($repoStore.currentPath === repo) {
        repoStore.setError(formatError(err));
      }
    } finally {
      // Clear only this id unconditionally: this component remounts per repo
      // ({#key} on currentPath), so a stale settle after a switch cannot
      // clobber a newer instance — but a path-gated reset here would strand
      // the spinner when the user switches repos mid-checkout.
      checkingOut.delete(number);
    }
  }

  async function triggerWorkflow(workflow: WorkflowInfo) {
    const repo = $repoStore.currentPath;
    if (!repo || triggering || busyRunAction) return;
    const refName = dispatchRef.trim();
    if (!refName) {
      actionError = "Enter a branch or tag to dispatch against.";
      return;
    }
    triggering = true;
    actionError = null;
    actionNotice = null;
    try {
      // The backend returns { policy, output }; treating that object as a
      // string used to render "[object Object]" in the success banner.
      const result = await invoke<Guarded<string>>("cmd_github_trigger_workflow", {
        repoPath: repo,
        workflow: workflow.path,
        gitRef: refName,
      });
      fileVerdict(result, repo);
      actionNotice = actionMessage(
        result,
        `Dispatched ${workflow.name || workflow.path} at ${refName}. It may take a moment to appear.`,
      );
      await Promise.all([loadFor(repo), loadWorkflowsFor(repo)]);
    } catch (err: unknown) {
      actionError = formatError(err);
    } finally {
      triggering = false;
    }
  }

  async function rerunRun(run: WorkflowRunInfo) {
    const repo = $repoStore.currentPath;
    if (!repo || busyRunAction || triggering) return;
    busyRunAction = `rerun:${run.id}`;
    actionError = null;
    actionNotice = null;
    try {
      const result = await invoke<Guarded<string>>("cmd_github_rerun_run", {
        repoPath: repo,
        runId: run.id,
      });
      fileVerdict(result, repo);
      actionNotice = actionMessage(result, `Re-running “${run.title || run.name}”.`);
      await loadFor(repo);
    } catch (err: unknown) {
      actionError = formatError(err);
    } finally {
      busyRunAction = null;
    }
  }

  async function cancelRun(run: WorkflowRunInfo) {
    const repo = $repoStore.currentPath;
    if (!repo || busyRunAction || triggering) return;
    busyRunAction = `cancel:${run.id}`;
    actionError = null;
    actionNotice = null;
    try {
      const result = await invoke<Guarded<string>>("cmd_github_cancel_run", {
        repoPath: repo,
        runId: run.id,
      });
      fileVerdict(result, repo);
      actionNotice = actionMessage(
        result,
        `Cancellation requested for “${run.title || run.name}”.`,
      );
      await loadFor(repo);
    } catch (err: unknown) {
      actionError = formatError(err);
    } finally {
      busyRunAction = null;
    }
  }

  async function runCiLocally() {
    const repo = $repoStore.currentPath;
    if (!repo || ciRunning) return;
    // A CI run outlives branch switches (a PR checkout re-fires the mount
    // effect); the guard makes a superseded run's late result inert instead
    // of letting two pipelines race to write ciReport last.
    ciInflight?.cancel();
    const guard = createAsyncGuard();
    ciInflight = guard;
    ciRunning = true;
    ciReport = null;
    ciError = null;
    try {
      const report = await invoke<CiLocalReport>("cmd_ci_local", { repoPath: repo });
      if (!guard.isLive()) return;
      ciReport = report;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      ciError = formatError(err);
    } finally {
      if (guard.isLive()) ciRunning = false;
    }
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans p-4 overflow-auto">
  <div class="flex items-start justify-between gap-3 mb-3">
    <div class="min-w-0">
      <h2 class="flex items-center gap-2 text-sm font-semibold text-textPrimary min-w-0">
        <Github size={16} class="text-accent shrink-0" />
        Remote
        {#if ctx?.owner}
          {#if ctx.html_url}
            <button
              type="button"
              class="text-textMuted font-mono font-normal truncate hover:text-accent"
              onclick={() => ctx && openExternal(ctx.html_url)}
              title="Open {ctx.owner}/{ctx.repo} on GitHub"
            >
              {ctx.owner}/{ctx.repo}
            </button>
          {:else}
            <span class="text-textMuted font-mono font-normal truncate">{ctx.owner}/{ctx.repo}</span>
          {/if}
        {/if}
      </h2>
      <!-- How old what is on screen is. A listing carrying no timestamp reads
           as current however long it has been sitting there. -->
      {#if fetchedAgo}
        <p class="mt-1 text-[10px] text-textMuted font-mono">fetched {fetchedAgo}</p>
      {/if}
    </div>
    <div class="flex flex-wrap items-center justify-end gap-1.5 shrink-0">
      {#if prCreateUrl}
        <button
          type="button"
          class="gp-btn"
          onclick={() => prCreateUrl && openExternal(prCreateUrl)}
          title="Open GitHub's new-pull-request form for {$repoStore.currentBranch} onto {$repoStore.defaultBranch ?? "main"}"
        >
          <span class="inline-flex items-center gap-1.5">
            <Plus size={13} />
            New pull request
          </span>
        </button>
      {/if}
      {#if ctx?.html_url}
        <button type="button" class="gp-btn" onclick={() => ctx && openExternal(`${ctx.html_url}/actions`)}>
          Actions
        </button>
      {/if}
      <button type="button" class="gp-btn" onclick={runCiLocally} disabled={ciRunning}>
        <span class="inline-flex items-center gap-1.5">
          <Terminal size={13} />
          {ciRunning ? "Running…" : "Run CI locally"}
        </span>
      </button>
      <button type="button" onclick={loadAll} class="gp-btn">
        <span class="inline-flex items-center gap-1.5">
          {#if (loading && !ctx) || (workflowsLoading && !workflows)}
            <LoaderCircle size={13} class="animate-spin" />
          {/if}
          Refresh
        </span>
      </button>
    </div>
  </div>
  <!-- The local pipeline's own result. It used to be a full-width banner at
       the top of the page that pushed every listing down and could not be put
       away; it is a card the reader folds up or dismisses when done with it. -->
  {#if ciRunning}
    <div class="mb-3 p-3 rounded-xl border border-border/70 bg-surface text-textMuted text-xs max-w-xl">
      Running this repository's CI pipeline locally (type-check, tests, build, fmt, clippy, cargo test)…
    </div>
  {:else if ciError}
    <div class="mb-3 p-3 rounded-xl border border-red-500/30 bg-red-500/10 text-red-700 dark:text-red-300 text-xs max-w-xl">
      CI:local could not start: {ciError}
    </div>
  {:else if ciReport}
    <div class="mb-3 rounded-xl border border-border/70 bg-surface max-w-4xl">
      <div class="flex items-center justify-between gap-3 px-3 pt-3 {ciReportOpen ? 'pb-1' : 'pb-1'}">
        <button
          type="button"
          class="flex min-w-0 items-center gap-2 text-left"
          aria-expanded={ciReportOpen}
          onclick={() => (ciReportOpen = !ciReportOpen)}
          title={ciReportOpen ? "Hide the steps" : "Show the steps"}
        >
          <ChevronRight size={13} class="shrink-0 text-textMuted transition-transform {ciReportOpen ? 'rotate-90' : ''}" />
          <span class="font-medium {ciReport.failed > 0 ? 'text-red-600 dark:text-red-400' : 'text-green-600 dark:text-green-400'}">
            CI:local {ciLocalVerdict(ciReport)}
          </span>
          <span class="text-[11px] text-textMuted font-mono">
            {(ciReport.total_duration_ms / 1000).toFixed(1)}s total
          </span>
        </button>
        <button
          type="button"
          class="gp-icon-btn shrink-0"
          aria-label="Dismiss the CI:local report"
          title="Dismiss"
          onclick={() => (ciReport = null)}
        >
          <X size={13} />
        </button>
      </div>
      {#if ciReportOpen}
        <div class="space-y-1 px-3">
          {#each ciReport.steps as step (step.name)}
            <details class="group rounded-lg px-2 py-1 hover:bg-surfaceHover/60">
              <summary class="flex cursor-pointer list-none items-center justify-between gap-3">
                <span class="flex min-w-0 items-center gap-2">
                  <span class="{ciStepClass(step.status)} shrink-0 w-14">{step.status}</span>
                  <span class="truncate text-textPrimary">{step.name}</span>
                  <span class="truncate font-mono text-[10px] text-textMuted hidden md:inline">{step.command}</span>
                </span>
                <span class="shrink-0 text-[11px] text-textMuted font-mono">{(step.duration_ms / 1000).toFixed(1)}s</span>
              </summary>
              {#if step.detail || step.command}
                <pre class="mt-1 mb-1 ml-16 whitespace-pre-wrap break-all rounded bg-background p-2 text-[10px] leading-relaxed text-textMuted overflow-x-auto">{step.detail || ""}</pre>
              {/if}
            </details>
          {/each}
        </div>
      {/if}
      <!-- Whether this run became a durable, git-native claim or stayed a
           number on a screen. Both states are stated: a run that was not
           recorded and a run nobody looked at leave the same empty space.
           Outside the fold, because a reader who has put the steps away still
           needs to know which of the two they have. -->
      <div class="mt-2 mx-3 mb-3 pt-2 border-t border-border/50 text-[11px] font-mono">
        {#if ciReport.recorded_commit}
          <span class="text-emerald-600 dark:text-emerald-400" title="Written to refs/notes/gitpulse/verification">
            recorded on {ciReport.recorded_commit.slice(0, 8)}
          </span>
        {:else}
          <span class="text-textMuted" title={ciReport.not_recorded_reason}>
            not recorded — {ciReport.not_recorded_reason || "no reason was given"}
          </span>
        {/if}
      </div>
    </div>
  {/if}

  {#if (loading && !ctx) || (workflowsLoading && !workflows)}
    <div class="space-y-4 max-w-4xl">
      <Skeleton variant="card" count={2} />
      <div class="space-y-2 pt-2">
        <Skeleton variant="text" count={4} height="2.5rem" />
      </div>
    </div>
  {:else if ctx?.error}
    <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-300 text-xs max-w-xl">
      {ctx.error}
      {#if ctx.html_url}
        <div class="mt-2">
          <button class="text-accent underline" onclick={() => ctx && openExternal(ctx.html_url)}>{ctx.html_url}</button>
        </div>
      {/if}
    </div>
  {:else if ctx}
    {#if actionNotice || actionError}
      <div
        class="mb-4 p-3 rounded-xl border text-xs max-w-3xl {actionError
          ? 'border-red-500/30 bg-red-500/10 text-red-300'
          : 'border-emerald-500/30 bg-emerald-500/10 text-emerald-300'}"
      >
        {actionError ?? actionNotice}
      </div>
    {/if}
    {#if (ctx.warnings?.length ?? 0) > 0}
      <div class="mb-4 p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-300 text-xs max-w-3xl space-y-1">
        {#each ctx.warnings as warning}
          <div>{warning}</div>
        {/each}
      </div>
    {/if}
    <!-- Two columns rather than a five-cell grid. The old layout put the
         listings in one grid whose rows are as tall as their tallest cell, so
         a repository with twenty pull requests left a screen of white space
         beside a three-line releases card — and it separated Workflows from
         the runs they produce by a row. The pull requests, which are what a
         reader acts on, get the wide column; everything about CI sits
         together in the rail. -->
    <div class="grid grid-cols-1 xl:grid-cols-3 gap-5 max-w-7xl items-start">
      <div class="xl:col-span-2 space-y-5 min-w-0">
        <section>
          <div class="flex items-center justify-between gap-3 mb-2">
            <h3 class="text-[11px] uppercase tracking-wider text-textMuted">Open pull requests</h3>
            {#if prFilterOn}
              <span class="text-[11px] text-textMuted font-mono">
                {visiblePrs.length} of {ctx.pull_requests.length}
              </span>
            {/if}
          </div>
          {#if ctx.pull_requests.length === 0 && !ctx.prs_truncated}
            <EmptyState icon={GitPullRequest} title="No open pull requests" compact />
          {:else if ctx.pull_requests.length === 0}
            <EmptyState icon={GitPullRequest} title="No open pull requests shown" compact />
          {:else}
            <!-- Median rather than mean: one PR left open for a year should not
                 become the headline number for the queue. Drafts are excluded
                 because they are not waiting on anyone. -->
            <div class="mb-2 text-[11px] text-textMuted font-mono flex flex-wrap gap-x-3">
              <span title="Median time the {velocity.considered} non-draft open pull requests have been open">
                median open {formatAge(velocity.medianOpenHours)}
              </span>
              <span title="Median time from opening to the first submitted review, across pull requests that have one">
                median to first review {formatAge(velocity.medianFirstReviewHours)}
              </span>
              <span title="Longest-open non-draft pull request">
                oldest {formatAge(velocity.oldestOpenHours)}
              </span>
            </div>

            <!-- The queue's own counts, as the way into it. "4 awaiting
                 review" used to be a number the reader then had to go find. -->
            <div class="mb-2 flex flex-wrap items-center gap-2">
              <div class="gp-segmented" role="tablist" aria-label="Pull request filter">
                {#each PR_FACETS as candidate (candidate)}
                  <button
                    type="button"
                    role="tab"
                    aria-selected={prFacet === candidate}
                    data-active={prFacet === candidate ? "true" : "false"}
                    disabled={candidate !== "all" && prCounts[candidate] === 0}
                    onclick={() => (prFacet = candidate)}
                    class="gp-seg-btn !text-[11px] !py-0.5 disabled:opacity-40 disabled:cursor-default"
                  >
                    {PR_FACET_LABELS[candidate]}
                    <span class="ml-1 font-mono text-[10px] opacity-70">{prCounts[candidate]}</span>
                  </button>
                {/each}
              </div>
              <label class="relative flex-1 min-w-[11rem]">
                <Search size={12} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none" />
                <input
                  class="gp-field w-full !pl-7"
                  type="search"
                  placeholder="Filter by number, title or branch"
                  aria-label="Filter pull requests"
                  bind:value={prQuery}
                />
              </label>
              {#if prFilterOn}
                <button type="button" class="gp-btn !py-1 !px-2.5 !text-[11px]" onclick={clearPrFilter}>
                  Clear
                </button>
              {/if}
            </div>

            {#if prsNarrowedToNothing}
              <!-- A filter matching nothing is not a repository with no open
                   pull requests, and must not borrow its wording. -->
              <EmptyState
                icon={Search}
                title="No pull request matches this filter"
                hint="{ctx.pull_requests.length} open pull requests are loaded."
                compact
                action={{ label: "Clear filter", onClick: clearPrFilter }}
              />
            {:else}
              <div class="space-y-2">
                {#each visiblePrs as pr (pr.number)}
                  <div class="p-3.5 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                    <div class="min-w-0">
                      <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                        <GitPullRequest size={14} class="text-accent shrink-0" />
                        <span class="truncate">#{pr.number} {pr.title}</span>
                        <FreshnessBadge freshness={prFreshness($freshnessStore.byRevision, pr.head_ref)} />
                        {#if pr.is_draft}
                          <span class="gp-pill">draft</span>
                        {/if}
                      </div>
                      <div class="mt-1 text-[11px] text-textMuted font-mono">
                        {pr.head_ref} → {pr.base_ref}
                        <span class="{ciClass(pr.ci_status)} ml-2">CI {pr.ci_status}</span>
                        <span class="ml-2" title="Open for {formatAge(openHours(pr, velocityNow))}">
                          open {formatAge(openHours(pr, velocityNow))}
                        </span>
                        {#if hoursToFirstReview(pr) !== null}
                          <span class="ml-2 text-emerald-600 dark:text-emerald-400" title="First review came {formatAge(hoursToFirstReview(pr))} after opening">
                            reviewed +{formatAge(hoursToFirstReview(pr))}
                          </span>
                        {:else if !pr.is_draft}
                          <span class="ml-2 text-amber-600 dark:text-amber-400" title="No review submitted yet">awaiting review</span>
                        {/if}
                      </div>
                    </div>
                    <div class="flex items-center gap-1 shrink-0">
                      <button
                        type="button"
                        onclick={() => void checkoutPr(pr.number)}
                        disabled={checkingOut.size > 0}
                        class="gp-icon-btn hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed"
                        title={checkingOut.has(pr.number) ? "Checking out…" : "Checkout pull request"}
                        aria-label={checkingOut.has(pr.number) ? `Checking out PR #${pr.number}` : `Checkout pull request #${pr.number}`}
                      >
                        <GitBranch size={14} />
                      </button>
                      <button
                        type="button"
                        onclick={() => openExternal(pr.url)}
                        class="gp-icon-btn"
                        title="Open on GitHub"
                        aria-label={`Open PR #${pr.number} on GitHub`}
                      >
                        <ExternalLink size={14} />
                      </button>
                    </div>
                  </div>
                {/each}
              </div>
            {/if}
            {#if ctx.prs_truncated}
              <div class="mt-2 text-amber-600 dark:text-amber-400 text-[11px]">Showing {ctx.pull_requests.length} pull requests; more open PRs exist. This is not complete coverage.</div>
            {/if}
          {/if}
        </section>

        <section>
          <div class="flex items-center justify-between gap-3 mb-2">
            <h3 class="text-[11px] uppercase tracking-wider text-textMuted">Open issues</h3>
            {#if ctx.issues.length > 3}
              <label class="relative w-48">
                <Search size={12} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none" />
                <input
                  class="gp-field w-full !pl-7 !py-0.5"
                  type="search"
                  placeholder="Filter issues"
                  aria-label="Filter issues"
                  bind:value={issueQuery}
                />
              </label>
            {/if}
          </div>
          {#if ctx.issues_error}
            <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs">
              Issue listing unavailable: {ctx.issues_error}
            </div>
          {:else if ctx.issues.length === 0 && !ctx.issues_truncated}
            <EmptyState icon={CircleDot} title="No open issues" compact />
          {:else if ctx.issues.length === 0}
            <EmptyState icon={CircleDot} title="No open issues shown" compact />
          {:else if issuesNarrowedToNothing}
            <EmptyState
              icon={Search}
              title="No issue matches this filter"
              hint="{ctx.issues.length} open issues are loaded."
              compact
              action={{ label: "Clear filter", onClick: () => (issueQuery = "") }}
            />
          {:else}
            <div class="grid gap-2 md:grid-cols-2">
              {#each visibleIssues as issue (issue.number)}
                <div class="p-3.5 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                      <CircleDot size={14} class="text-accent shrink-0" />
                      <span class="truncate">#{issue.number} {issue.title}</span>
                    </div>
                    <div class="mt-1 text-[11px] text-textMuted font-mono">
                      {issue.author || "unknown author"}
                      {#if relativeAge(issue.updated_at, clockTick)}
                        · updated {relativeAge(issue.updated_at, clockTick)}
                      {/if}
                      {#if issue.labels.length > 0}
                        · {issue.labels.slice(0, 4).join(", ")}
                      {/if}
                    </div>
                  </div>
                  <button
                    type="button"
                    onclick={() => openExternal(issue.url)}
                    class="gp-icon-btn shrink-0"
                    title="Open on GitHub"
                    aria-label={`Open issue #${issue.number} on GitHub`}
                  >
                    <ExternalLink size={14} />
                  </button>
                </div>
              {/each}
            </div>
          {/if}
          {#if ctx.issues_truncated}
            <div class="mt-2 text-amber-600 dark:text-amber-400 text-[11px]">Showing {ctx.issues.length} issues; more open issues exist. This is not complete coverage.</div>
          {/if}
        </section>
      </div>

      <!-- The CI rail: what can be dispatched, what it produced, and what was
           shipped. Workflows and their runs used to sit a grid row apart. -->
      <div class="space-y-5 min-w-0">
        <section>
          <h3 class="text-[11px] uppercase tracking-wider text-textMuted mb-2">Workflows</h3>
          {#if workflows?.error}
            <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs">
              Workflow listing unavailable: {workflows.error}
            </div>
          {:else if !workflows}
            {#if workflowsLoading}
              <div class="flex items-center gap-1.5 text-textMuted text-[11px]">
                <LoaderCircle size={12} class="animate-spin" />
                Loading workflows…
              </div>
            {:else}
              <EmptyState icon={Workflow} title="No Actions workflows" compact />
            {/if}
          {:else if workflows.workflows.length === 0}
            <EmptyState icon={Workflow} title="No Actions workflows" compact />
          {:else}
            <div class="flex items-center gap-2 mb-2">
              <input
                class="gp-field flex-1 min-w-0"
                placeholder="branch or tag (dispatch ref)"
                bind:value={dispatchRef}
                aria-label="Dispatch ref"
              />
            </div>
            <div class="space-y-2">
              {#each workflows.workflows as wf (wf.id)}
                <div class="p-3 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                      <Workflow size={14} class="text-accent shrink-0" />
                      <span class="truncate">{wf.name}</span>
                      <span class="gp-pill {isWorkflowDispatchable(wf.state) ? '!border-emerald-500/30 !bg-emerald-500/10 !text-emerald-600 dark:!text-emerald-400' : ''}">
                        {workflowStateLabel(wf.state)}
                      </span>
                    </div>
                    <div class="mt-1 text-[11px] text-textMuted font-mono truncate">{wf.path}</div>
                  </div>
                  <button
                    type="button"
                    onclick={() => void triggerWorkflow(wf)}
                    disabled={!isWorkflowDispatchable(wf.state) || triggering || busyRunAction !== null}
                    class="gp-icon-btn hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed shrink-0"
                    title={isWorkflowDispatchable(wf.state)
                      ? `Dispatch ${wf.name} at ${dispatchRef.trim() || "<ref>"}`
                      : "Only active workflows can be dispatched"}
                    aria-label={isWorkflowDispatchable(wf.state)
                      ? `Dispatch ${wf.name} at ${dispatchRef.trim() || "<ref>"}`
                      : `Workflow ${wf.name} cannot be dispatched`}
                  >
                    <Play size={14} />
                  </button>
                </div>
              {/each}
            </div>
            {#if workflows.truncated}
              <div class="mt-2 text-amber-600 dark:text-amber-400 text-[11px]">Showing {workflows.workflows.length} workflows; more exist.</div>
            {/if}
          {/if}
        </section>

        <section>
          <div class="flex items-center justify-between gap-2 mb-2">
            <h3 class="text-[11px] uppercase tracking-wider text-textMuted">Workflow runs</h3>
            {#if $repoStore.currentBranch && ctx.workflow_runs.length > 0}
              <button
                type="button"
                class="gp-pill hover:text-accent {runsThisBranch ? '!border-accent/50 !bg-accent/10 !text-accent' : ''}"
                aria-pressed={runsThisBranch}
                onclick={() => (runsThisBranch = !runsThisBranch)}
                title="Only runs whose head branch is {$repoStore.currentBranch}"
              >
                this branch
              </button>
            {/if}
          </div>
          {#if ctx.runs_error}
            <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs">
              Run listing unavailable: {ctx.runs_error}
            </div>
          {:else if ctx.workflow_runs.length === 0}
            <EmptyState icon={Play} title="No recent Actions runs" compact />
          {:else if visibleRuns.length === 0}
            <EmptyState
              icon={Play}
              title="No run on this branch"
              hint="{ctx.workflow_runs.length} recent runs are loaded, none of them for {$repoStore.currentBranch}."
              compact
              action={{ label: "Show all runs", onClick: () => (runsThisBranch = false) }}
            />
          {:else}
            <div class="space-y-2">
              {#each visibleRuns as run (run.id)}
                <div class="p-3 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2 text-textPrimary font-medium">
                      <Play size={14} class="text-accent shrink-0" />
                      <span class="truncate">{run.title || run.name}</span>
                    </div>
                    <div class="mt-1 text-[11px] text-textMuted font-mono">
                      {run.name}
                      {#if run.head_branch}
                        · {run.head_branch}
                      {/if}
                      {#if relativeAge(run.created_at, clockTick)}
                        · {relativeAge(run.created_at, clockTick)}
                      {/if}
                      <span class="{ciClass(runLabel(run))} ml-2">{runLabel(run)}</span>
                    </div>
                  </div>
                  <div class="flex items-center gap-1 shrink-0">
                    {#if canCancelRun(run)}
                      <button
                        type="button"
                        onclick={() => void cancelRun(run)}
                        disabled={busyRunAction !== null || triggering}
                        class="gp-icon-btn hover:text-red-400 disabled:opacity-40 disabled:cursor-not-allowed"
                        title={busyRunAction === `cancel:${run.id}` ? "Cancelling…" : "Cancel run"}
                        aria-label={busyRunAction === `cancel:${run.id}` ? `Cancelling run ${run.title || run.name}` : `Cancel run ${run.title || run.name}`}
                      >
                        <Square size={13} />
                      </button>
                    {:else if canRerunRun(run)}
                      <button
                        type="button"
                        onclick={() => void rerunRun(run)}
                        disabled={busyRunAction !== null || triggering}
                        class="gp-icon-btn hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed"
                        title={busyRunAction === `rerun:${run.id}` ? "Re-running…" : "Re-run workflow"}
                        aria-label={busyRunAction === `rerun:${run.id}` ? `Re-running run ${run.title || run.name}` : `Re-run workflow ${run.title || run.name}`}
                      >
                        <RotateCcw size={13} />
                      </button>
                    {/if}
                    <button
                      type="button"
                      onclick={() => openExternal(run.url)}
                      class="gp-icon-btn"
                      title="Open workflow run"
                      aria-label={`Open workflow run ${run.title || run.name}`}
                    >
                      <ExternalLink size={14} />
                    </button>
                  </div>
                </div>
              {/each}
            </div>
          {/if}
          {#if ctx.runs_truncated}
            <div class="mt-2 text-amber-600 dark:text-amber-400 text-[11px]">Showing the {ctx.workflow_runs.length} most recent runs; older runs exist.</div>
          {/if}
        </section>

        <section>
          <div class="flex items-center justify-between mb-2">
            <h3 class="text-[11px] uppercase tracking-wider text-textMuted">Releases</h3>
            {#if (ctx.releases?.length ?? 0) > 0}
              <span class="gp-pill">{ctx.releases.length}</span>
            {/if}
          </div>
          {#if ctx.releases_error}
            <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs">
              Release listing unavailable: {ctx.releases_error}
            </div>
          {:else if !ctx.releases || ctx.releases.length === 0}
            <EmptyState icon={Tag} title="No releases found" compact />
          {:else}
            <div class="space-y-2">
              {#each ctx.releases as release (release.tag_name || release.name)}
                <div class="p-3 bg-surface border border-border/70 rounded-2xl shadow-card flex items-start justify-between gap-3 transition-[border-color,box-shadow] duration-150 hover:border-accent/40">
                  <div class="min-w-0 flex-1">
                    <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                      <Tag size={14} class="text-accent shrink-0" />
                      {#if release.tag_name}
                        <span class="font-mono text-accent">{release.tag_name}</span>
                      {/if}
                      {#if release.name && release.name !== release.tag_name}
                        <span class="truncate">{release.name}</span>
                      {/if}
                      {#if release.is_latest}
                        <span class="gp-pill !border-emerald-500/30 !bg-emerald-500/10 !text-emerald-600 dark:!text-emerald-400">latest</span>
                      {/if}
                      {#if release.is_prerelease}
                        <span class="gp-pill !border-amber-500/30 !bg-amber-500/10 !text-amber-600 dark:!text-amber-400">pre-release</span>
                      {/if}
                      {#if release.is_draft}
                        <span class="gp-pill">draft</span>
                      {/if}
                    </div>
                    {#if release.published_at || release.created_at}
                      <div class="mt-1 text-[11px] text-textMuted">
                        {formatReleaseDate(release.published_at || release.created_at)}
                      </div>
                    {/if}
                  </div>
                  {#if release.url}
                    <button
                      type="button"
                      onclick={() => openExternal(release.url)}
                      class="gp-icon-btn shrink-0"
                      title="Open release on GitHub"
                      aria-label={`Open release ${release.tag_name || release.name} on GitHub`}
                    >
                      <ExternalLink size={14} />
                    </button>
                  {/if}
                </div>
              {/each}
            </div>
            {#if ctx.releases_truncated}
              <div class="mt-2 text-amber-600 dark:text-amber-400 text-[11px]">Showing {ctx.releases.length} releases; more releases exist.</div>
            {/if}
          {/if}
        </section>
      </div>
    </div>
  {/if}
</div>
