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
  import { keyedList } from "../ui/eachKeys";
  import { SvelteSet } from "svelte/reactivity";
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { filterStore } from "../stores/filterStore";
  import { invoke } from "../ipc/invoke";
  import {
    FolderGit2,
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
    ChevronDown,
  } from "@lucide/svelte";
  import { openExternal as openExternalUrl } from "../desktop/openExternal";
  import PullRequestDetail from "./github/PullRequestDetail.svelte";
  import PullRequestCreate from "./github/PullRequestCreate.svelte";
  import FreshnessBadge from "./FreshnessBadge.svelte";
  import FirebasePanel from "./FirebasePanel.svelte";
  import { freshnessStore } from "../provenance/store";
  import { prHeadFreshness, prRevisions } from "../provenance/pullRequests";
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
    RUN_PREVIEW_COUNT,
    WORKFLOW_PREVIEW_COUNT,
    expandLabel,
    overflowsPreview,
    previewSlice,
    useCompactRows,
    workflowStateLabel,
  } from "../github/runActions";
  import { formatReleaseDate } from "../ops/model";
  import { pullRequestCreateUrl } from "../github/compareUrl";
  import {
    checkoutPullRequestInPlace,
    describePrWorktree,
    openPullRequestWorktree,
    withVerdict,
  } from "../github/prActions";
  import {
    filterIssues,
    filterPullRequests,
    filterReleases,
    PR_FACET_LABELS,
    PR_FACETS,
    prFacetCounts,
    relativeAge,
    runsOnBranch,
    type PrFacet,
  } from "../github/remoteFilter";
  import { formatError } from "../ui/formatError";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { reportPanelError } from "../diagnostics/report";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import DeliveryTimeline from "./DeliveryTimeline.svelte";
  import { crossfade } from "svelte/transition";
  import { isMacOS } from "../platform";
  import { liquidSelection } from "../ui/transitions";
  import { runTimelineRow, runTimelineRows } from "../github/runLifecycle";
  import { anyInFlight } from "../delivery/transitions";
  import { createLivePoll, type LiveState } from "../delivery/livePoll";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import type { GitHubRunsReport } from "../github/types";

  const macos = isMacOS();
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

  let ctx = $state<GitHubContext | null>(null);

  /**
   * Runs from the live poll, superseding the ones the full context carried.
   *
   * Null until the first poll returns, so the panel shows the context's runs
   * immediately rather than an empty list waiting for a `gh` call. One derived
   * `runs` below is the single read every consumer uses — two lists of runs on
   * screen at once is a drift waiting to be noticed by a user rather than by
   * a test.
   */
  let liveRuns = $state<GitHubRunsReport | null>(null);
  let liveState = $state<LiveState>({ kind: "idle", reason: "" });
  /** Runs that went red while the panel was watching. */
  let failureNotice = $state<string | null>(null);
  /** Ticks the in-flight duration bars without refetching anything. */
  let timelineNow = $state(Date.now());

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
  /** PR numbers being opened in a new worktree. Either set disables both actions. */
  const openingWorktree = new SvelteSet<number>();
  /** An expanded detail is running its own action (review, merge, checkout or worktree open). */
  let detailBusy = $state(false);
  const prActionsLocked = $derived(checkingOut.size > 0 || openingWorktree.size > 0 || detailBusy);
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
  /** The pull request whose detail (view, review, merge) is open, if any. */
  let expandedPr = $state<number | null>(null);
  let creatingPr = $state(false);
  const currentBranch = $derived($repoStore.branches.find((b) => b.is_current)?.name ?? null);
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
  /**
   * Latest release only, by default. The backend fetches up to fifty and the
   * rail rendered every one of them above the deploy section; what a reader
   * opens this rail for is what shipped last, and the pill beside the heading
   * still gets them the full history.
   */
  let latestReleaseOnly = $state(true);
  const visibleReleases = $derived(filterReleases(ctx?.releases ?? [], latestReleaseOnly));
  /** Narrows the run list to the checked-out branch. */
  let runsThisBranch = $state(false);
  /**
   * Reveal the whole workflow and run listings. Collapsed by default because
   * this rail is a single column: every row above the deploy section pushes
   * Firebase App Hosting further out of reach of anyone who does not already
   * know to keep scrolling.
   */
  let workflowsExpanded = $state(false);
  let runsExpanded = $state(false);
  /** Collapses the CI:local report without discarding it. */
  let ciReportOpen = $state(true);

  const prCounts = $derived(prFacetCounts(ctx?.pull_requests ?? []));
  const visiblePrs = $derived(filterPullRequests(ctx?.pull_requests ?? [], prFacet, prQuery));
  const visibleIssues = $derived(filterIssues(ctx?.issues ?? [], issueQuery));
  /**
   * The canonical run list for everything on screen.
   *
   * A live poll that came back `checked` supersedes the context's snapshot; a
   * poll that failed is ignored here and reported by the badge instead, so a
   * transport error never replaces real rows with an empty list.
   */
  const runsFromPoll = $derived(liveRuns?.checked === true);
  const runs = $derived(runsFromPoll ? (liveRuns?.runs ?? []) : (ctx?.workflow_runs ?? []));
  const runsTruncated = $derived(
    runsFromPoll ? (liveRuns?.truncated ?? false) : (ctx?.runs_truncated ?? false),
  );
  /**
   * Whether the source the rows on screen actually came from ran.
   *
   * Deliberately NOT the last poll's `checked`. A failed poll while the
   * context's rows are still on screen is a stale live view, not an absent
   * listing — reporting it as "could not read runs" would hide rows that were
   * genuinely fetched. The poll's own trouble is the badge's job: it reads
   * "retrying", and "paused" once the session gives up.
   */
  const runsChecked = $derived(runsFromPoll ? true : !ctx?.runs_error);
  const runsError = $derived(runsFromPoll ? null : (ctx?.runs_error ?? null));
  const visibleRuns = $derived(
    runsOnBranch(runs, runsThisBranch ? ($repoStore.currentBranch ?? "") : ""),
  );
  /**
   * Timeline rows for the runs actually on screen, so the branch filter and
   * the visualization cannot disagree about what is being measured.
   */
  const runRows = $derived(runTimelineRows(visibleRuns));
  /**
   * The run cards actually drawn.
   *
   * Sliced from `visibleRuns`, below the branch filter and above nothing else:
   * the preview decides how many cards are drawn, never what is in scope. The
   * timeline above still measures every run the filter admitted, and the poll
   * still watches every run that was fetched — a collapsed section is not a
   * quieter one.
   */
  const previewedRuns = $derived(previewSlice(visibleRuns, runsExpanded, RUN_PREVIEW_COUNT));
  const visibleWfs = $derived(
    previewSlice(workflows?.workflows ?? [], workflowsExpanded, WORKFLOW_PREVIEW_COUNT),
  );
  /**
   * Density follows what is on screen, not what was fetched: the preview stays
   * comfortable and only the list the reader deliberately opened tightens.
   */
  const compactWfRows = $derived(useCompactRows(visibleWfs.length));
  const compactRunRows = $derived(useCompactRows(previewedRuns.length));
  /** True when a filter is on and has hidden every row that was fetched. */
  const prsNarrowedToNothing = $derived(
    (ctx?.pull_requests.length ?? 0) > 0 && visiblePrs.length === 0,
  );
  const issuesNarrowedToNothing = $derived(
    (ctx?.issues.length ?? 0) > 0 && visibleIssues.length === 0,
  );
  const releasesNarrowedToNothing = $derived(
    (ctx?.releases.length ?? 0) > 0 && visibleReleases.length === 0,
  );
  const prFilterOn = $derived(prFacet !== "all" || prQuery.trim() !== "");

  /** When the context on screen was fetched; drives the staleness stamp. */
  let fetchedAt = $state<number | null>(null);
  /** Ticks the relative stamp without refetching anything. */
  let clockTick = $state(Date.now());
  // Visibility-aware: a ticker whose only job is to re-render a relative
  // stamp must not wake the renderer behind other windows.
  $effect(() => createAdaptiveTimer(() => (clockTick = Date.now()), 30_000));
  const fetchedAgo = $derived(
    fetchedAt === null ? "" : relativeAge(new Date(fetchedAt).toISOString(), clockTick),
  );

  /**
   * One poll of just the run listing.
   *
   * `cmd_github_runs` rather than `cmd_github_context`: one `gh` call instead
   * of four, because this is on a repeating timer. Returns whether the poll
   * succeeded, which is what the scheduler backs off on.
   *
   * A repository switch mid-poll is handled by discarding the answer: the
   * component is keyed on `currentPath`, but a resolve can still land after
   * the path moved, and applying it would show one repository's runs under
   * another's name.
   */
  async function pollRunsOnce(repo: string): Promise<boolean> {
    try {
      const report = await invoke<GitHubRunsReport>("cmd_github_runs", { repoPath: repo });
      if ($repoStore.currentPath !== repo) return false;
      liveRuns = report;
      if (report.checked) {
        // Which runs just went red is the native observer's call, the one the
        // CI watcher also acts on (`github/outcome.rs`). The rule: a run is
        // announced only if it is red in this listing and the panel's own
        // previous listing showed it in flight, once. The observer keeps that
        // previous listing per repository (not what the watcher saw, and it
        // counts this listing even when the watcher's newer request came back
        // first, since the panel stops polling once nothing is in flight), drops it
        // after the 15-minute live-poll session ceiling, and announces nothing
        // on the panel's first look, so mounting the panel cannot replay a
        // failure it was not watching, however recently the watcher logged it.
        // The panel only words it.
        const wentRed = report.went_red.map(runTimelineRow);
        if (wentRed.length > 0) {
          failureNotice =
            wentRed.length === 1
              ? `${wentRed[0].label} — ${wentRed[0].stateLabel}`
              : `${wentRed.length} runs failed`;
        }
      }
      // `checked: false` is a failed poll even though the call resolved: the
      // report carries its own error rather than rejecting.
      return report.checked;
    } catch {
      return false;
    }
  }

  /**
   * The live poll, torn down and rebuilt per repository.
   *
   * Depends only on `currentPath` — `runs` is fed in through `sync` below
   * instead, because a driver rebuilt on every change to the run list would
   * lose the session budget every time a run's timestamp ticked over.
   */
  let livePoll: ReturnType<typeof createLivePoll> | null = null;
  $effect(() => {
    const repo = $currentRepoPath;
    liveRuns = null;
    failureNotice = null;
    liveState = { kind: "idle", reason: "" };
    if (!repo) return;
    const driver = createLivePoll({
      poll: () => pollRunsOnce(repo),
      onState: (next) => (liveState = next),
    });
    livePoll = driver;
    return () => {
      driver.dispose();
      if (livePoll === driver) livePoll = null;
    };
  });

  /**
   * Tell the driver whether anything is still moving.
   *
   * Reads `runs` — the unfiltered canonical list — deliberately. Gating the
   * poll on `visibleRuns` would stop watching a running job the moment someone
   * filtered it off screen, and the branch filter is a view, not a decision
   * about what to monitor.
   */
  $effect(() => {
    const moving = anyInFlight(runTimelineRows(runs));
    livePoll?.sync(moving);
  });

  /**
   * Advances the in-flight duration bars.
   *
   * Only while something is actually in flight: a settled row's bar is fixed,
   * so a ticking clock would invalidate the whole timeline every second to
   * redraw identical numbers.
   */
  $effect(() => {
    if (liveState.kind !== "live") return;
    // Also visibility-aware, and for a sharper reason than the stamp above:
    // this one fires every second, and `liveState` stays "live" while the
    // window is hidden — the poll's own timer stops, but this ticker would
    // not have.
    return createAdaptiveTimer(() => (timelineNow = Date.now()), 1_000);
  });

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
      prs_error: null,
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
      // gh reports a rate limit or an outage as a context carrying `error`. On
      // a panel already showing a good read, that is a failed refresh, not the
      // new truth: the listings stay, the failure is said, and the stamp keeps
      // describing what is on screen. An error never overwrites the cache.
      if (next.error && ctx && !ctx.error) {
        reportRefreshFailure(next.error);
        return;
      }
      ctx = next;
      // The full context is the newer read, so the poll's older snapshot must
      // stop superseding it. Without this, a Refresh after a paused poll shows
      // fresh pull requests beside runs from whenever the poll last managed a
      // call — one panel, two ages, no way to tell.
      liveRuns = null;
      // An explicit fetch is the human action that revives a session which
      // gave up. Nothing else may.
      livePoll?.reset();
      fetchedAt = Date.now();
      // One clock per fetch, so two pull requests opened in the same second
      // cannot disagree about their age.
      velocityNow = Date.now();
      ctxCache.set(repo, next);
    } catch (err) {
      if (!guard.isLive()) return;
      if (ctx && !ctx.error) reportRefreshFailure(formatError(err));
      else ctx = emptyContext(formatError(err));
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  /** "Couldn't <verb> <object>. <Reason>" for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  /** A failed refresh over data already on screen: keep the data, say what failed and how old it is. */
  function reportRefreshFailure(message: string) {
    actionError = `${failure("refresh", "the remote", message)} — showing ${fetchedAgo ? `data fetched ${fetchedAgo}` : "the last data read"}`;
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

  /**
   * Reloads, then confirms what an action did.
   *
   * `loadAll` clears the notice before its first await, so a notice set before
   * it is wiped; and the row that triggered the action may leave the list with
   * the reload (a merged pull request does), taking any confirmation kept on
   * the row with it. The confirmation is therefore the panel's, and set last.
   * A repository switched to mid-reload is not the one the action was on.
   */
  async function reloadThenNotice(message: string) {
    const repo = $repoStore.currentPath;
    await loadAll();
    if ($repoStore.currentPath !== repo) return;
    actionNotice = message;
  }

  // A pull request that has left the list (merged, closed, no longer fetched)
  // must not leave its detail open for the next one to inherit the number.
  // Judged against the fetched list, not the filtered one: narrowing the list
  // hides a row without forgetting which detail the reader had open.
  $effect(() => {
    const open = ctx?.pull_requests;
    if (!open) return;
    untrack(() => {
      if (expandedPr !== null && !open.some((p) => p.number === expandedPr)) expandedPr = null;
    });
  });

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
   *
   * The effect reads values compared by value, never `$repoStore` itself, so
   * a store emission that changed neither the repository nor its default
   * branch — a status poll, a diff selection — is not another batch.
   */
  const defaultBranchName = $derived(
    $repoStore.branches.find((b) => b.is_default && !b.is_remote)?.name ?? null,
  );

  const freshness = freshnessStore.consumer("pull-requests");
  $effect(() => {
    const repo = $currentRepoPath;
    const prs = ctx?.pull_requests ?? [];
    const base = defaultBranchName;
    if (!repo || prs.length === 0) return;
    void freshness.load(repo, prRevisions(prs), base);
  });

  // Reload context + workflows only when the real dependencies change: the
  // repository, and the checked-out branch for the dispatch-ref default. Both
  // compare by value, so a status poll or stats drain neither re-runs this
  // nor — through the teardown below — cancels a load already in flight,
  // which used to leave the spinner on with no load left to clear it.
  const checkedOutBranch = $derived($repoStore.currentBranch);
  $effect(() => {
    const repo = $currentRepoPath;
    const branch = checkedOutBranch;
    return untrack(() => {
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
      latestReleaseOnly = true;
      workflowsExpanded = false;
      runsExpanded = false;
      ciReportOpen = true;
      checkingOut.clear();
      ciReport = null;
      ciError = null;
      ciRunning = false;
      actionNotice = null;
      actionError = null;
      // Dispatch ref is repo-scoped: default to the checked-out branch.
      dispatchRef = branch ?? "";
      if (!repo) {
        inflight?.cancel();
        workflowsInflight?.cancel();
        ciInflight?.cancel();
        loading = false;
        workflowsLoading = false;
        return undefined;
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
  });

  // Status roles on every verdict: they are defined per theme, so the label
  // keeps its contrast on the light theme's near-white card as well as the
  // dark one — on exactly the labels a reader opened this page to check.
  function ciClass(status: string): string {
    const s = status.toLowerCase();
    if (s === "success" || s === "completed") return "text-ok";
    if (s === "failure" || s === "cancelled" || s === "timed_out") return "text-danger";
    // `waiting`/`requested` sit behind deployment protections/approvals —
    // in flight, never a verdict.
    if (s === "pending" || s === "in_progress" || s === "queued" || s === "waiting" || s === "requested")
      return "text-warn";
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
  function fileVerdict(result: Guarded<unknown> | null, repoPath: string) {
    harnessStore.recordVerdict(result?.policy ?? null, repoPath);
  }

  /** Builds the action notice from gh's output, with verdict context. */
  function actionMessage(result: Guarded<string>, fallback: string): string {
    const base = result.output || fallback;
    return `${base}${result.policy ? ` — ${verdictLabel(result.policy)}` : ""}`;
  }

  async function openPrWorktree(number: number) {
    const repo = $repoStore.currentPath;
    if (!repo || prActionsLocked) return;
    openingWorktree.add(number);
    actionError = null;
    actionNotice = null;
    try {
      const result = await openPullRequestWorktree(repo, number);
      // No repoStore refresh: the checkout this panel shows did not change, and
      // a refresh reloads its commit graph (the github-panel harness pins that).
      if ($repoStore.currentPath !== repo) return;
      actionNotice = withVerdict(describePrWorktree(result.output), result.policy);
    } catch (err: unknown) {
      if ($repoStore.currentPath === repo) actionError = failure("open", "the pull request in a worktree", err);
    } finally {
      openingWorktree.delete(number);
    }
  }

  async function checkoutPr(number: number) {
    const repo = $repoStore.currentPath;
    if (!repo || prActionsLocked) return;
    // Track per-id so concurrent checkouts cannot clobber each other's
    // spinner state; while any is running every button is disabled.
    checkingOut.add(number);
    try {
      const result = await checkoutPullRequestInPlace(repo, number);
      if ($repoStore.currentPath !== repo) return;
      await repoStore.refresh(repo);
      if ($repoStore.currentPath !== repo) return;
      // Post-mutation reload keeps the visible filter context: a bare
      // loadGraph(repo) reset the graph to query=""/HEAD while FilterBar
      // still showed the selection. The backend applies every query term,
      // so the reload answers exactly what the scheduler keys on.
      await graphStore.loadGraph(repo, $filterStore.searchQuery, $filterStore.selectedBranch);
      actionNotice = withVerdict(result.output || `Checked out pull request #${number} in place.`, result.policy);
    } catch (err: unknown) {
      if ($repoStore.currentPath === repo) {
        repoStore.setError(failure("check out", `pull request #${number}`, err));
      }
    } finally {
      // Clear only this id unconditionally: this component remounts per repo
      // ({#key} on currentPath), so a stale settle after a switch cannot
      // clobber a newer instance — but a path-gated reset here would strand
      // the spinner when the user switches repos mid-checkout.
      checkingOut.delete(number);
    }
  }

  /**
   * A worktree opened from the detail: say where it landed. The checkout this
   * panel shows did not change, so nothing here is reloaded — a repoStore
   * refresh would reload this checkout's commit graph for nothing (the
   * github-panel harness pins that), and the Worktrees panel lists worktrees
   * itself.
   */
  function openedInWorktree(message: string) {
    if (!$repoStore.currentPath) return;
    actionError = null;
    actionNotice = message;
  }

  /** In-place checkout from the detail: the checkout already happened. */
  async function checkedOutInPlace(message: string) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    actionError = null;
    try {
      await repoStore.refresh(repo);
      if ($repoStore.currentPath !== repo) return;
      await graphStore.loadGraph(repo, $filterStore.searchQuery, $filterStore.selectedBranch);
      actionNotice = message;
    } catch (err: unknown) {
      if ($repoStore.currentPath === repo) repoStore.setError(failure("refresh", "the checkout", err));
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
      actionError = failure("dispatch", workflow.name || "the workflow", err);
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
      actionError = failure("re-run", "the workflow run", err);
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
      actionError = failure("cancel", "the workflow run", err);
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
      ciError = failure("start", "CI:local", err);
    } finally {
      if (guard.isLive()) ciRunning = false;
    }
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans p-4 overflow-auto">
  <div class="flex items-start justify-between gap-3 mb-3">
    <div class="min-w-0">
      <h2 class="flex items-center gap-2 text-sm font-semibold text-textPrimary min-w-0">
        <FolderGit2 size={16} class="text-accent shrink-0" />
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
        <p class="mt-1 text-micro text-textMuted font-mono">fetched {fetchedAgo}</p>
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
      <!-- Busy whenever a read is out, not only on the first one: a refresh over
           data already on screen shows nothing else changing, so without this the
           click looks like it did nothing. Disabled too, so a second click does
           not start a read the first is still making. It re-enables when the read
           settles, failed or not — it is also the retry for the error card. -->
      <button type="button" onclick={loadAll} class="gp-btn" disabled={loading || workflowsLoading}>
        <span class="inline-flex items-center gap-1.5">
          {#if loading || workflowsLoading}
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
    <div class="mb-3 p-3 rounded-xl border border-edge bg-surface text-textMuted text-xs max-w-xl">
      Running this repository's CI pipeline locally (type-check, tests, build, fmt, clippy, cargo test)…
    </div>
  {:else if ciError}
    <div class="gp-notice mb-3! max-w-xl" data-tone="danger" role="alert">{ciError}</div>
  {:else if ciReport}
    <div class="mb-3 rounded-xl border border-edge bg-surface max-w-4xl">
      <div class="flex items-center justify-between gap-3 px-3 pt-3 {ciReportOpen ? 'pb-1' : 'pb-1'}">
        <button
          type="button"
          class="flex min-w-0 items-center gap-2 text-left"
          aria-expanded={ciReportOpen}
          onclick={() => (ciReportOpen = !ciReportOpen)}
          title={ciReportOpen ? "Hide the steps" : "Show the steps"}
        >
          <ChevronRight size={13} class="shrink-0 text-textMuted transition-transform {ciReportOpen ? 'rotate-90' : ''}" />
          <span class="font-medium {ciReport.failed > 0 ? 'text-danger' : 'text-ok'}">
            CI:local {ciLocalVerdict(ciReport)}
          </span>
          <span class="text-meta text-textMuted font-mono">
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
      {#if ciReport.test_scope?.reason}
        <p
          class="mx-3 mb-1 text-meta font-mono leading-snug {ciReport.test_scope.fail_closed
            ? 'text-warn'
            : 'text-textMuted'}"
          title={ciReport.test_scope.reason}
        >
          {#if ciReport.test_scope.fail_closed}
            full suite — {ciReport.test_scope.reason}
          {:else}
            affected: {ciReport.test_scope.test_files.length} test file(s)
            {#if ciReport.test_scope.seeds.length > 0}
              from {ciReport.test_scope.seeds.length} changed path(s)
            {/if}
          {/if}
        </p>
      {/if}
      {#if ciReportOpen}
        <div class="space-y-1 px-3">
          {#each keyedList(ciReport.steps, (step) => step.name) as { item: step, key: renderKey } (renderKey)}
            <details class="group rounded-lg px-2 py-1 hover:bg-surfaceHover/60">
              <summary class="flex cursor-pointer list-none items-center justify-between gap-3">
                <span class="flex min-w-0 items-center gap-2">
                  <span class="{ciStepClass(step.status)} shrink-0 w-14">{step.status}</span>
                  <span class="truncate text-textPrimary">{step.name}</span>
                  <span class="truncate font-mono text-micro text-textMuted hidden md:inline">{step.command}</span>
                </span>
                <span class="shrink-0 text-meta text-textMuted font-mono">{(step.duration_ms / 1000).toFixed(1)}s</span>
              </summary>
              {#if step.detail || step.command}
                <pre class="mt-1 mb-1 ml-16 whitespace-pre-wrap break-all rounded bg-background p-2 text-micro leading-relaxed text-textMuted overflow-x-auto">{step.detail || ""}</pre>
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
      <div class="mt-3 mx-3 mb-3 text-meta font-mono">
        {#if ciReport.recorded_commit}
          <span class="text-ok" title="Written to refs/notes/gitpulse/verification">
            recorded on {ciReport.recorded_commit.slice(0, 8)}
          </span>
        {:else}
          <span class="text-textMuted" title={ciReport.not_recorded_reason}>
            not recorded — {ciReport.not_recorded_reason || "no reason was given"}
          </span>
        {/if}
        {#if ciReport.signal_error}
          <!-- The task of an agent attempt was not told. Stated apart from the
               note above: the two are separate records and either can fail. -->
          <div class="mt-1 text-warn" role="status" data-testid="ci-signal-error">
            {ciReport.signal_error}
          </div>
        {/if}
      </div>
    </div>
  {/if}

  {#if (loading && !ctx) || (workflowsLoading && !workflows)}
    <div class="space-y-4 max-w-4xl">
      <Skeleton variant="card" count={2} hold={false} />
      <div class="space-y-2 pt-2">
        <Skeleton variant="text" count={4} height="2.5rem" hold={false} />
      </div>
    </div>
  {:else if ctx?.error}
    <div class="gp-notice max-w-xl" data-tone="unknown" role="status">
      <div class="min-w-0 flex-1">
        <div>Couldn't read the remote.</div>
        <div>{ctx.error}</div>
        {#if ctx.html_url}
          <div class="mt-2">
            <button class="text-accent underline" onclick={() => ctx && openExternal(ctx.html_url)}>{ctx.html_url}</button>
          </div>
        {/if}
      </div>
      <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={loadAll} disabled={loading || workflowsLoading}>Try again</button>
    </div>
  {:else if ctx}
    {#if actionNotice || actionError}
      <div
        class="gp-notice mb-4! max-w-3xl flex-col! gap-0!"
        data-tone={actionError ? "danger" : "ok"}
      >
        <!-- Both when both are true: a merge that went through and a reload that
             then failed are two facts, and the error must not hide the first. -->
        {#if actionError && actionNotice}<div>{actionNotice}</div>{/if}
        <div>{actionError ?? actionNotice}</div>
      </div>
    {/if}
    {#if (ctx.warnings?.length ?? 0) > 0}
      <div class="gp-notice mb-4! max-w-3xl flex-col! gap-1!" data-tone="warn">
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
    <div class="grid grid-cols-1 xl:grid-cols-3 gap-5 items-start">
      <div class="xl:col-span-2 space-y-5 min-w-0">
        <section>
          <div class="flex items-center justify-between gap-3 mb-2">
            <h3 class="gp-label">Open pull requests</h3>
            <div class="flex items-center gap-2">
              {#if prFilterOn}
                <span class="text-meta text-textMuted font-mono">
                  {visiblePrs.length} of {ctx.pull_requests.length}
                </span>
              {/if}
              <button
                type="button"
                class="gp-btn flex items-center gap-1"
                data-size="sm"
                aria-expanded={creatingPr}
                onclick={() => (creatingPr = !creatingPr)}
              ><Plus size={12} /> New pull request</button>
            </div>
          </div>
          {#if creatingPr && $repoStore.currentPath}
            <PullRequestCreate
              repoPath={$repoStore.currentPath}
              slug="{ctx.owner}/{ctx.repo}"
              headBranch={currentBranch}
              defaultBase={$repoStore.defaultBranch ?? "main"}
              onCreated={(message) => { creatingPr = false; void reloadThenNotice(message); }}
              onCancel={() => (creatingPr = false)}
            />
          {/if}
          {#if ctx.prs_error && ctx.pull_requests.length === 0}
            <div class="gp-notice" data-tone="unknown" role="status">
              Pull request listing unavailable: {ctx.prs_error}
            </div>
          {:else if ctx.pull_requests.length === 0 && !ctx.prs_truncated}
            <EmptyState icon={GitPullRequest} title="No open pull requests" compact />
          {:else if ctx.pull_requests.length === 0}
            <EmptyState icon={GitPullRequest} title="No open pull requests shown" compact />
          {:else}
            <!-- Median rather than mean: one PR left open for a year should not
                 become the headline number for the queue. Drafts are excluded
                 because they are not waiting on anyone. -->
            <div class="mb-2 text-meta text-textMuted font-mono flex flex-wrap gap-x-3">
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
              <div class="gp-segmented" class:gp-liquid-tabs={macos} role="tablist" aria-label="Pull request filter">
                {#each PR_FACETS as candidate (candidate)}
                  <button
                    type="button"
                    role="tab"
                    aria-selected={prFacet === candidate}
                    data-active={prFacet === candidate ? "true" : "false"}
                    disabled={candidate !== "all" && prCounts[candidate] === 0}
                    onclick={() => (prFacet = candidate)}
                    class="gp-seg-btn disabled:opacity-40 disabled:cursor-default"
                  >
                    {#if macos && prFacet === candidate}
                      <span
                        class="gp-liquid-selection gp-gpu"
                        aria-hidden="true"
                        in:receiveSelection={{ key: "pr-facet" }}
                        out:sendSelection={{ key: "pr-facet" }}
                      ></span>
                    {/if}
                    <span>{PR_FACET_LABELS[candidate]}<span class="ml-1 font-mono text-micro opacity-70">{prCounts[candidate]}</span></span>
                  </button>
                {/each}
              </div>
              <label class="relative flex-1 min-w-44">
                <Search size={12} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none" />
                <input
                  class="gp-field w-full pl-7!"
                  type="search"
                  placeholder="Filter by number, title or branch"
                  aria-label="Filter pull requests"
                  bind:value={prQuery}
                />
              </label>
              {#if prFilterOn}
                <button type="button" class="gp-btn" data-size="sm" onclick={clearPrFilter}>
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
                  <div class="gp-card p-3.5 transition-colors hover:border-accent/35">
                  <div class="flex items-start justify-between gap-3">
                    <div class="min-w-0">
                      <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                        <GitPullRequest size={14} class="text-accent shrink-0" />
                        <span class="truncate">#{pr.number} {pr.title}</span>
                        <FreshnessBadge freshness={prHeadFreshness($freshnessStore.byRevision, pr)} />
                        {#if pr.is_draft}
                          <span class="gp-chip" data-tone="neutral">draft</span>
                        {/if}
                      </div>
                      <div class="mt-1 text-meta text-textMuted font-mono">
                        {pr.head_ref} → {pr.base_ref}
                        <span class="{ciClass(pr.ci_status)} ml-2">CI {pr.ci_status}</span>
                        <span class="ml-2" title="Open for {formatAge(openHours(pr, velocityNow))}">
                          open {formatAge(openHours(pr, velocityNow))}
                        </span>
                        {#if hoursToFirstReview(pr) !== null}
                          <span class="ml-2 text-ok" title="First review came {formatAge(hoursToFirstReview(pr))} after opening">
                            reviewed +{formatAge(hoursToFirstReview(pr))}
                          </span>
                        {:else if !pr.is_draft}
                          <span class="ml-2 text-warn" title="No review submitted yet">awaiting review</span>
                        {/if}
                      </div>
                    </div>
                    <div class="flex items-center gap-1 shrink-0">
                      <button
                        type="button"
                        onclick={() => (expandedPr = expandedPr === pr.number ? null : pr.number)}
                        class="gp-icon-btn hover:text-accent"
                        aria-expanded={expandedPr === pr.number}
                        title={expandedPr === pr.number ? "Hide details" : "Details, review and merge"}
                        aria-label={`${expandedPr === pr.number ? "Hide" : "Show"} details for pull request #${pr.number}`}
                      >
                        {#if expandedPr === pr.number}<ChevronDown size={14} />{:else}<ChevronRight size={14} />{/if}
                      </button>
                      <button
                        type="button"
                        onclick={() => void openPrWorktree(pr.number)}
                        disabled={prActionsLocked}
                        class="gp-icon-btn hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed"
                        title={openingWorktree.has(pr.number) ? "Opening worktree…" : "Open in new worktree"}
                        aria-label={openingWorktree.has(pr.number) ? `Opening pull request #${pr.number} in a worktree` : `Open pull request #${pr.number} in a new worktree`}
                      >
                        <FolderGit2 size={14} />
                      </button>
                      <button
                        type="button"
                        onclick={() => void checkoutPr(pr.number)}
                        disabled={prActionsLocked}
                        class="gp-icon-btn hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed"
                        title={checkingOut.has(pr.number) ? "Checking out…" : "Checkout in place"}
                        aria-label={checkingOut.has(pr.number) ? `Checking out PR #${pr.number}` : `Checkout pull request #${pr.number} in place`}
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
                  {#if expandedPr === pr.number && $repoStore.currentPath}
                    <PullRequestDetail
                      repoPath={$repoStore.currentPath}
                      number={pr.number}
                      slug="{ctx.owner}/{ctx.repo}"
                      locked={prActionsLocked}
                      onChanged={(message) => void reloadThenNotice(message)}
                      onCheckedOut={(message) => void checkedOutInPlace(message)}
                      onWorktreeOpened={(message) => openedInWorktree(message)}
                      onBusyChange={(busy) => (detailBusy = busy)}
                    />
                  {/if}
                  </div>
                {/each}
              </div>
            {/if}
            {#if ctx.prs_truncated}
              <div class="mt-2 text-warn text-meta">Showing {ctx.pull_requests.length} pull requests; more open PRs exist. This is not complete coverage.</div>
            {/if}
          {/if}
        </section>

        <section>
          <div class="flex items-center justify-between gap-3 mb-2">
            <h3 class="gp-label">Open issues</h3>
            {#if ctx.issues.length > 3}
              <label class="relative w-48">
                <Search size={12} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none" />
                <input
                  class="gp-field w-full pl-7! py-0.5!"
                  type="search"
                  placeholder="Filter issues"
                  aria-label="Filter issues"
                  bind:value={issueQuery}
                />
              </label>
            {/if}
          </div>
          {#if ctx.issues_error}
            <div class="gp-notice" data-tone="unknown" role="status">
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
                <div class="gp-card p-3.5 flex items-start justify-between gap-3 transition-colors hover:border-accent/35">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap">
                      <CircleDot size={14} class="text-accent shrink-0" />
                      <span class="truncate">#{issue.number} {issue.title}</span>
                    </div>
                    <div class="mt-1 text-meta text-textMuted font-mono">
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
            <div class="mt-2 text-warn text-meta">Showing {ctx.issues.length} issues; more open issues exist. This is not complete coverage.</div>
          {/if}
        </section>
      </div>

      <!-- The CI rail: what can be dispatched, what it produced, and what was
           shipped. Workflows and their runs used to sit a grid row apart. -->
      <div class="space-y-5 min-w-0">
        <section>
          <h3 class="gp-label mb-2">Workflows</h3>
          {#if workflows?.error}
            <div class="gp-notice" data-tone="unknown" role="status">
              Workflow listing unavailable: {workflows.error}
            </div>
          {:else if !workflows}
            {#if workflowsLoading}
              <Skeleton variant="text" count={2} height="2.5rem" hold={false} />
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
            <div class={compactWfRows ? "space-y-1" : "space-y-2"}>
              {#each visibleWfs as wf (wf.id)}
                <!-- The compact row tightens padding and leading only. The
                     path stays on screen at every density because it is the
                     dispatch selector and the only thing that tells two
                     workflows with the same `name:` apart. -->
                <div class="{compactWfRows ? 'px-2.5 py-1.5 rounded-xl' : 'p-3 rounded-2xl'} bg-surface border border-edge shadow-card flex items-start justify-between gap-3 transition-colors hover:border-accent/35">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2 text-textPrimary font-medium flex-wrap {compactWfRows ? 'text-sm leading-tight' : ''}">
                      <Workflow size={14} class="text-accent shrink-0" />
                      <span class="truncate">{wf.name}</span>
                      <span class="gp-chip" data-tone={isWorkflowDispatchable(wf.state) ? "ok" : "neutral"}>
                        {workflowStateLabel(wf.state)}
                      </span>
                    </div>
                    <div class="{compactWfRows ? 'mt-0 leading-tight' : 'mt-1'} text-meta text-textMuted font-mono truncate">{wf.path}</div>
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
            {#if overflowsPreview(workflows.workflows.length, WORKFLOW_PREVIEW_COUNT)}
              <!-- Reports the fetched count, which is what this control can
                   actually reveal. Whether the backend capped the listing is a
                   separate fact, and the notice below still states it. -->
              <button
                type="button"
                class="gp-btn mt-2 w-full" data-variant="ghost" data-size="sm"
                aria-expanded={workflowsExpanded}
                onclick={() => (workflowsExpanded = !workflowsExpanded)}
              >
                {expandLabel(workflows.workflows.length, workflowsExpanded, "workflows")}
              </button>
            {/if}
            {#if workflows.truncated}
              <div class="mt-2 text-warn text-meta">Showing {workflows.workflows.length} workflows; more exist.</div>
            {/if}
          {/if}
        </section>

        <!-- Deploys sit in the CI rail beside the workflows and releases that
             produce them, rather than in a section of their own: a Firebase
             section would be empty for every repository that does not deploy
             to Firebase, which is the shape the Insights registry already
             records as a mistake.

             Above the runs and releases, not below them, because those two are
             the rail's long listings and this is the only one that is a
             destination. It used to sit last, which on a repository with
             twenty runs and ten releases meant a section that loads fine was
             unreachable without scrolling past everything that does not need
             reading. Both listings now preview, and this sits above them
             anyway: a reader who expands one should not lose their deploys. -->
        <FirebasePanel repoPath={$repoStore.currentPath} />

        <section>
          <div class="flex items-center justify-between gap-2 mb-2">
            <h3 class="gp-label">Workflow runs</h3>
            {#if $repoStore.currentBranch && runs.length > 0}
              <button
                type="button"
                class="gp-chip cursor-pointer"
                data-tone={runsThisBranch ? "accent" : "neutral"}
                aria-pressed={runsThisBranch}
                onclick={() => (runsThisBranch = !runsThisBranch)}
                title="Only runs whose head branch is {$repoStore.currentBranch}"
              >
                this branch
              </button>
            {/if}
          </div>
          {#if failureNotice}
            <!-- Something went red while we were watching. Dismissible, and
                 never auto-cleared: a notice that disappears on the next poll
                 is one nobody standing up from their desk will ever see. -->
            <div
              class="gp-notice mb-2! justify-between"
              data-tone="danger"
              role="status"
            >
              <span class="min-w-0 wrap-break-word">
                Failed while watching: {failureNotice}
              </span>
              <button
                type="button"
                class="gp-btn shrink-0"
                data-size="sm"
                onclick={() => (failureNotice = null)}
              >
                Dismiss
              </button>
            </div>
          {/if}
          <!--
            Gated on the same list it measures, not on `runs`.

            `DeliveryTimeline` documents an empty `rows` with `checked: true`
            as a real, measured absence, and says so on screen: "No runs
            recorded for this repository." Gating on the unfiltered `runs`
            while measuring `visibleRuns` hands it a filter artifact instead —
            with runs fetched but none on this branch it would claim none were
            recorded, directly above the truthful "No run on this branch" that
            counts them. The filter case already has its own empty state, with
            an action to clear the filter, so let that one speak alone.
          -->
          {#if visibleRuns.length > 0}
            <div class="mb-2 min-w-0">
              <DeliveryTimeline
                title="Run duration and outcome"
                rows={runRows}
                now={timelineNow}
                checked={runsChecked}
                truncated={runsTruncated}
                error={runsError}
                sampleNoun="runs"
                live={liveState}
                onRescan={() => void loadAll()}
              />
            </div>
          {/if}
          {#if ctx.runs_error && runs.length === 0}
            <div class="gp-notice" data-tone="unknown" role="status">
              Run listing unavailable: {ctx.runs_error}
            </div>
          {:else if runs.length === 0}
            <EmptyState icon={Play} title="No recent Actions runs" compact />
          {:else if visibleRuns.length === 0}
            <EmptyState
              icon={Play}
              title="No run on this branch"
              hint="{runs.length} recent runs are loaded, none of them for {$repoStore.currentBranch}."
              compact
              action={{ label: "Show all runs", onClick: () => (runsThisBranch = false) }}
            />
          {:else}
            <div class={compactRunRows ? "space-y-1" : "space-y-2"}>
              {#each previewedRuns as run (run.id)}
                <div class="{compactRunRows ? 'px-2.5 py-1.5 rounded-xl' : 'p-3 rounded-2xl'} bg-surface border border-edge shadow-card flex items-start justify-between gap-3 transition-colors hover:border-accent/35">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2 text-textPrimary font-medium {compactRunRows ? 'text-sm leading-tight' : ''}">
                      <Play size={14} class="text-accent shrink-0" />
                      <span class="truncate">{run.title || run.name}</span>
                    </div>
                    <div class="{compactRunRows ? 'mt-0 leading-tight' : 'mt-1'} text-meta text-textMuted font-mono">
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
                        class="gp-icon-btn" data-tone="danger"
                        title={busyRunAction === `cancel:${run.id}` ? "Canceling…" : "Cancel run"}
                        aria-label={busyRunAction === `cancel:${run.id}` ? `Canceling run ${run.title || run.name}` : `Cancel run ${run.title || run.name}`}
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
            {#if overflowsPreview(visibleRuns.length, RUN_PREVIEW_COUNT)}
              <!-- Counts what this control can actually reveal: the runs left
                   after the branch filter, not everything fetched. Whether the
                   backend withheld older runs is a separate fact, and the
                   notice below still states it. -->
              <button
                type="button"
                class="gp-btn mt-2 w-full" data-variant="ghost" data-size="sm"
                aria-expanded={runsExpanded}
                onclick={() => (runsExpanded = !runsExpanded)}
              >
                {expandLabel(visibleRuns.length, runsExpanded, "runs")}
              </button>
            {/if}
          {/if}
          {#if runsTruncated}
            <div class="mt-2 text-warn text-meta">Showing the {runs.length} most recent runs; older runs exist.</div>
          {/if}
        </section>

        <section>
          <div class="flex items-center justify-between gap-2 mb-2">
            <div class="flex items-center gap-2">
              <h3 class="gp-label">Releases</h3>
              {#if (ctx.releases?.length ?? 0) > 0}
                <span class="gp-chip" data-kind="count" data-tone="neutral">{ctx.releases.length}</span>
              {/if}
            </div>
            {#if (ctx.releases?.length ?? 0) > 0}
              <button
                type="button"
                class="gp-chip cursor-pointer"
                data-tone={latestReleaseOnly ? "accent" : "neutral"}
                aria-pressed={latestReleaseOnly}
                onclick={() => (latestReleaseOnly = !latestReleaseOnly)}
                title="Only releases marked latest"
              >
                latest only
              </button>
            {/if}
          </div>
          {#if ctx.releases_error}
            <div class="gp-notice" data-tone="unknown" role="status">
              Release listing unavailable: {ctx.releases_error}
            </div>
          {:else if !ctx.releases || ctx.releases.length === 0}
            <EmptyState icon={Tag} title="No releases found" compact />
          {:else if releasesNarrowedToNothing}
            <EmptyState
              icon={Tag}
              title="No latest release found"
              hint="{ctx.releases.length} releases are loaded, none of them marked latest."
              compact
              action={{ label: "Show all releases", onClick: () => (latestReleaseOnly = false) }}
            />
          {:else}
            <div class="space-y-2">
              {#each visibleReleases as release, i (`${release.tag_name || release.name || "release"}#${i}`)}
                <div class="gp-card p-3 flex items-start justify-between gap-3 transition-colors hover:border-accent/35">
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
                        <span class="gp-chip" data-tone="ok">latest</span>
                      {/if}
                      {#if release.is_prerelease}
                        <span class="gp-chip" data-tone="warn">pre-release</span>
                      {/if}
                      {#if release.is_draft}
                        <span class="gp-chip" data-tone="neutral">draft</span>
                      {/if}
                    </div>
                    {#if release.published_at || release.created_at}
                      <div class="mt-1 text-meta text-textMuted">
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
              <div class="mt-2 text-warn text-meta">Showing {ctx.releases.length} releases; more releases exist.</div>
            {/if}
          {/if}
        </section>
      </div>
    </div>
  {/if}
</div>
