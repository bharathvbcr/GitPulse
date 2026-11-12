<script lang="ts">
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../../stores/repoStore";
  import { pulseStore } from "../../pulse/pulseStore";
  import PulseHeatmap from "./PulseHeatmap.svelte";
  import PulseRhythm from "./PulseRhythm.svelte";
  import PulsePunchCard from "./PulsePunchCard.svelte";
  import PulseLineTrend from "./PulseLineTrend.svelte";
  import PulseHygiene from "./PulseHygiene.svelte";
  import PulseHotspotMap from "./PulseHotspotMap.svelte";
  import PulseKnowledgeMap from "./PulseKnowledgeMap.svelte";
  import PulseCodeAge from "./PulseCodeAge.svelte";
  import PulseDora from "./PulseDora.svelte";
  import PulsePeriodCompare from "./PulsePeriodCompare.svelte";
  import PulseExportModal from "./PulseExportModal.svelte";
  import PulseExtensionChurn from "./PulseExtensionChurn.svelte";
  import EmptyState from "../EmptyState.svelte";
  import Skeleton from "../Skeleton.svelte";
  import { describeFailure } from "../../ui/copy/describeFailure";
  import type { CoverageReport } from "../../coverage/types";
  import type { LanguageStatsReport } from "../../language/barStats";
  import { computeCommitWindow, computeHygiene } from "../../pulse/metrics";
  import { coverageMetric, locMetric, totalCodeLines } from "../../metrics/repoMetrics";
  import type { MetricSnapshot } from "../../metrics/freshness";
  import {
    Activity,
    AlertCircle,
    Download,
    Flame,
    GitCommit,
    Mail,
    RefreshCw,
    Rocket,
    Users,
  } from "@lucide/svelte";

  type LocState =
    | { status: "idle" | "loading" }
    | { status: "ok" | "partial"; value: number; truncated: boolean }
    | { status: "failed" };

  let locState = $state<LocState>({ status: "idle" });
  let coverageReport = $state<CoverageReport | null>(null);
  let coverageFailed = $state(false);
  let coverageRequested = $state(false);
  let activeTab = $state<"overview" | "hotspots" | "knowledge" | "dora">("overview");
  let exportModalOpen = $state(false);
  let authorFilter = $state("all");
  let workspaceLoc = $state<
    { path: string; name: string; value: number | null; truncated: boolean; failed: boolean }[]
  >([]);

  // The tab set as a VALUE, not a reference. repoStore rebuilds openTabs with
  // .map() on every status poll (~6s), so an effect that depends on the array
  // itself re-runs on every emission and tore down and rebuilt every LOC
  // subscription each time. The joined key changes only when the tabs do.
  const workspaceTabs = $derived(
    $repoStore.openTabs.map((tab) => ({ path: tab.path, name: tab.label || tab.name })),
  );
  const workspaceKey = $derived(
    workspaceTabs.map((tab) => `${tab.path}\u0000${tab.name}`).join("\u001f"),
  );

  /** Identifies the live workspace-LOC run. A plain `let`, never $state:
   *  the subscription callback reads it, and a reactive read there would
   *  recreate exactly the self-dependency this effect was fixed to avoid. */
  let workspaceRun = 0;

  /** Maps a LOC metric snapshot onto this view's display state. */
  function toLocState(snap: MetricSnapshot<LanguageStatsReport>): LocState {
    if (snap.value === null) {
      return snap.state === "failed"
        ? { status: "failed" }
        : { status: snap.state === "loading" ? "loading" : "idle" };
    }
    const value = totalCodeLines(snap.value) ?? 0;
    // `partial` covers both halves of "this is a floor, not a total": the
    // backend truncated the scan, or the repository has changed since it ran.
    const truncated = snap.value.truncated === true || snap.stale !== null;
    return { status: truncated ? "partial" : "ok", value, truncated };
  }

  $effect(() => {
    const path = $currentRepoPath;
    untrack(() => {
      if (!path) {
        pulseStore.reset();
        return;
      }
      void pulseStore.load(path);
      authorFilter = "all";
    });
  });

  // LOC now tracks the repository instead of the tab that opened it: the
  // metric revalidates on every settled write, so the headline line count
  // moves as the user works rather than freezing at whatever it was when this
  // view was first mounted. Keyed on the repository alone: a subscription
  // delivers its snapshot synchronously and may record it, so resubscribing
  // per store publish was a history write per publish.
  $effect(() => {
    const path = $currentRepoPath;
    if (!path) {
      locState = { status: "idle" };
      return;
    }
    return untrack(() => locMetric.subscribe(path, (snap) => {
      locState = toLocState(snap);
      // Only a complete, current measurement is worth recording as a history
      // point; a truncated or stale one would put a false dip in the trend.
      if (snap.value && snap.stale === null && snap.value.truncated !== true) {
        void pulseStore.recordSnapshot(totalCodeLines(snap.value) ?? 0);
      }
    }));
  });

  // One shared coverage measurement. This view and CoverageViewer used to
  // invoke `cmd_scan_coverage` separately and could disagree about the result.
  $effect(() => {
    const path = $currentRepoPath;
    if (!path || activeTab !== "hotspots") return;
    coverageRequested = true;
    return untrack(() =>
      coverageMetric.subscribe(path, (snap) => {
        coverageReport = snap.value;
        coverageFailed = snap.state === "failed" && snap.value === null;
      }),
    );
  });

  // Rescan goes through the same shared metric as the subscription above, so
  // this view and CoverageViewer still see one measurement.
  function rescanCoverage() {
    const path = $currentRepoPath;
    if (path) void coverageMetric.refresh(path, { force: true });
  }

  // Per-tab line counts for the workspace comparison strip. Same metric, so a
  // repository open in two places is measured once.
  $effect(() => {
    // Depend on the tab set's value; read the tabs themselves untracked so a
    // fresh-but-equal openTabs array does not re-run this.
    void workspaceKey;
    const tabs = untrack(() => workspaceTabs);
    // Claim this run before anything can publish. freshness.publish() iterates
    // a COPY of the listener set, so a listener can still be invoked after its
    // own unsubscribe — a superseded run would otherwise merge into the rows
    // of a tab set that is no longer open and publish them over the current
    // ones. Comparing row.path to tab.path cannot catch that: both come from
    // the same run, so that test is true by construction.
    const run = ++workspaceRun;
    if (tabs.length < 2) {
      workspaceLoc = [];
      return;
    }
    // `rows` is a plain, non-reactive array and is the source of truth the
    // subscription callbacks merge into. Reading `workspaceLoc` here instead
    // is what crashed this pane: locMetric delivers the current snapshot
    // SYNCHRONOUSLY from inside subscribe(), so that read registered
    // `workspaceLoc` as a dependency of the very effect that writes it, and
    // each write re-invalidated the effect until Svelte gave up with
    // effect_update_depth_exceeded. Writing without reading cannot loop.
    const rows = tabs.map((tab) => ({
      path: tab.path,
      name: tab.name,
      value: null as number | null,
      truncated: false,
      failed: false,
    }));
    workspaceLoc = [...rows];
    const unsubscribes = tabs.map((tab, index) =>
      locMetric.subscribe(tab.path, (snap) => {
        if (run !== workspaceRun) return;
        const row = rows[index];
        rows[index] = {
          ...row,
          value: snap.value ? (totalCodeLines(snap.value) ?? 0) : null,
          truncated: snap.value?.truncated === true || snap.stale !== null,
          failed: snap.state === "failed" && snap.value === null,
        };
        workspaceLoc = [...rows];
      }),
    );
    return () => {
      for (const off of unsubscribes) off();
    };
  });

  function handleDeepenScan() {
    if ($repoStore.currentPath) {
      void pulseStore.setLimit(25_000);
    }
  }

  const report = $derived($pulseStore.report);
  const knowledge = $derived($pulseStore.knowledge);
  const dora = $derived($pulseStore.dora);
  const loading = $derived($pulseStore.loading);
  const knowledgeLoading = $derived($pulseStore.knowledgeLoading);
  const doraLoading = $derived($pulseStore.doraLoading);
  const error = $derived($pulseStore.error);
  const knowledgeError = $derived($pulseStore.knowledgeError);
  const pulseFailure = $derived(
    describeFailure({ verb: "generate", object: "repository pulse metrics" }, error),
  );
  const doraError = $derived($pulseStore.doraError);
  const knowledgeMeasuredAt = $derived($pulseStore.knowledgeMeasuredAt);
  const doraMeasuredAt = $derived($pulseStore.doraMeasuredAt);

  const authors = $derived.by(() => {
    if (!report) return [];
    const map = new Map<string, string>();
    for (const c of report.commits) {
      if (!map.has(c.author_email)) {
        map.set(c.author_email, c.author_name || c.author_email);
      }
    }
    return [...map.entries()].sort((a, b) => a[1].localeCompare(b[1]));
  });

  const visibleCommits = $derived.by(() => {
    if (!report) return [];
    if (authorFilter === "all") return report.commits;
    return report.commits.filter((c) => c.author_email === authorFilter);
  });

  const locValue = $derived(locState.status === "ok" || locState.status === "partial" ? locState.value : 0);
  const locKnown = $derived(locState.status === "ok" || locState.status === "partial");
  const hygiene = $derived(computeHygiene(visibleCommits));

  /**
   * Everything the export card says, taken from one population.
   *
   * The card is read without the app beside it, so an unmeasured metric must
   * reach it as null and be rendered as such — a blame scan that has not
   * finished is not a bus factor of zero, and a failed language scan is not an
   * empty repository.
   */
  const cardWindow = $derived(computeCommitWindow(visibleCommits));
  const blameMeasured = $derived(Boolean(knowledge) && (knowledge?.scanned_files ?? 0) > 0);
  const scopedAuthor = $derived(
    authorFilter === "all"
      ? null
      : (authors.find(([email]) => email === authorFilter)?.[1] ?? authorFilter),
  );

  const repoName = $derived(
    $repoStore.currentPath ? $repoStore.currentPath.split("/").pop() || "repository" : "repository",
  );

  const canDeepen = $derived(
    Boolean(report?.truncated) &&
      !report?.payload_truncated &&
      $pulseStore.maxCommits < 25_000,
  );
</script>

<div class="flex-1 flex flex-col min-h-0 bg-background overflow-y-auto">
  <div class="px-6 py-4 bg-surface shrink-0 flex flex-col sm:flex-row sm:items-center justify-between gap-3">
    <div class="flex items-center gap-3">
      <div class="w-9 h-9 rounded-xl bg-accent/10 flex items-center justify-center text-accent shadow-xs">
        <Activity size={18} />
      </div>
      <div>
        <div class="flex items-center gap-2">
          <h1 class="text-sm font-semibold text-textPrimary tracking-tight">Repository pulse</h1>
          {#if report}
            <span class="gp-chip font-mono" data-kind="label" data-tone="neutral">
              {report.total_commits_scanned.toLocaleString()} commits
            </span>
          {/if}
          {#if report?.has_mailmap}
            <span
              class="gp-chip"
              data-kind="label"
              data-tone="ok"
              title=".mailmap active: author aliases are canonicalized."
            >
              <Mail size={10} />
              <span>.mailmap</span>
            </span>
          {:else if report}
            <span
              class="gp-chip"
              data-kind="label"
              data-tone="neutral"
              title="No .mailmap. The same person with work and personal emails counts as two contributors."
            >
              no .mailmap
            </span>
          {/if}
        </div>
        <p class="text-meta text-textMuted mt-0.5">
          Local cadence, churn, hotspots, knowledge concentration, and tag-based delivery. All branches, computed on this machine.
        </p>
      </div>
    </div>

    <div class="flex items-center gap-2">
      <div class="gp-segmented" role="group" aria-label="Pulse section">
        <button
          type="button"
          class="gp-seg-btn"
          aria-pressed={activeTab === "overview"}
          data-active={activeTab === "overview" ? "true" : "false"}
          onclick={() => (activeTab = "overview")}
        >
          Overview
        </button>
        <button
          type="button"
          class="gp-seg-btn inline-flex items-center gap-1"
          aria-pressed={activeTab === "hotspots"}
          data-active={activeTab === "hotspots" ? "true" : "false"}
          onclick={() => (activeTab = "hotspots")}
        >
          <Flame size={12} />
          <span>Hotspots</span>
        </button>
        <button
          type="button"
          class="gp-seg-btn inline-flex items-center gap-1"
          aria-pressed={activeTab === "knowledge"}
          data-active={activeTab === "knowledge" ? "true" : "false"}
          onclick={() => (activeTab = "knowledge")}
        >
          <Users size={12} />
          <span>Knowledge and age</span>
        </button>
        <button
          type="button"
          class="gp-seg-btn inline-flex items-center gap-1"
          aria-pressed={activeTab === "dora"}
          data-active={activeTab === "dora" ? "true" : "false"}
          onclick={() => (activeTab = "dora")}
        >
          <Rocket size={12} />
          <span>DORA</span>
        </button>
      </div>

      <button
        type="button"
        onclick={() => (exportModalOpen = true)}
        disabled={!report}
        class="gp-btn"
        data-variant="secondary"
        data-size="md"
        title="Export the pulse summary card for your README"
      >
        <Download size={12} />
        <span class="hidden md:inline">Export card</span>
      </button>

      <button
        type="button"
        onclick={() => pulseStore.reload()}
        disabled={loading}
        class="gp-btn"
        data-variant="secondary"
        data-size="md"
      >
        <RefreshCw size={12} class={loading ? 'animate-spin text-accent' : ''} />
        <span>{loading ? 'Refreshing…' : 'Refresh'}</span>
      </button>
    </div>
  </div>

  {#if report?.truncated}
    <div class="mx-6 mt-4">
      <div class="gp-notice" data-tone="warn" role="status">
        <AlertCircle size={15} class="shrink-0 mt-px" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 flex flex-wrap items-center gap-x-2">
          {#if report.payload_truncated}
            <!-- Named, not assumed: the flag is also set by a stream we could
                 not read to the end, which is not a budget problem. -->
            <span class="font-medium"
              >Log output {report.payload_truncation_reason ?? "is incomplete"}.</span
            >
            <span class="text-textMuted hidden sm:inline">Tiles below are a prefix, not the full history. Raising the commit cap would make this worse.</span>
          {:else}
            <span class="font-medium">History bounded at {report.total_commits_scanned.toLocaleString()} commits.</span>
            <span class="text-textMuted hidden sm:inline">Older commits exist in this repository. Longest-gap is only as old as this scan.</span>
          {/if}
        </div>
        {#if canDeepen}
          <button
            type="button"
            onclick={handleDeepenScan}
            disabled={loading}
            class="gp-btn shrink-0"
            data-variant="secondary"
            data-size="sm"
          >
            Scan deeper (25k)
          </button>
        {/if}
      </div>
    </div>
  {/if}

  {#if error}
    <div class="m-6">
      <div class="gp-notice" data-tone="danger" role="alert">
        <AlertCircle size={16} class="shrink-0 mt-0.5" style="color: var(--gp-notice-ink)" />
        <div class="flex-1 min-w-0">
          <p class="font-semibold text-danger">{pulseFailure.headline}</p>
          <p class="mt-1 font-mono text-meta wrap-break-word">{error}</p>
          <p class="mt-2 text-meta text-textMuted">
            Recorded under <span class="font-mono">pulse</span> in Diagnostics, which carries the
            backend log tail — where a backend crash writes its location and backtrace.
          </p>
          <div class="flex items-center gap-2 mt-3">
            <button
              type="button"
              onclick={() => pulseStore.reload()}
              class="gp-btn"
              data-variant="secondary"
              data-size="sm"
            >
              Try again
            </button>
            <button
              type="button"
              onclick={() => window.dispatchEvent(new CustomEvent("gitpulse:diagnostics"))}
              class="gp-btn"
              data-variant="secondary"
              data-size="sm"
            >
              Open Diagnostics
            </button>
          </div>
        </div>
      </div>
    </div>
  {:else if loading && !report}
    <div class="p-6 space-y-4">
      <div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
        {#each [1, 2, 3, 4] as n (n)}
          <Skeleton variant="rect" height="5rem" />
        {/each}
      </div>
      <Skeleton variant="rect" height="11rem" />
      <Skeleton variant="rect" height="12rem" />
    </div>
  {:else if report && report.commits.length === 0}
    <div class="flex-1 flex items-center justify-center p-8">
      <EmptyState
        icon={GitCommit}
        title="No commit activity found"
        hint="This repository does not have any commits yet, or the history couldn't be reached."
        action={{
          label: "Refresh",
          onClick: () => pulseStore.reload(),
          icon: RefreshCw,
        }}
      />
    </div>
  {:else if report}
    <div class="p-6 space-y-5">
      {#if workspaceLoc.length > 1}
        <div class="gp-card p-3 flex flex-wrap items-center gap-3 text-xs">
          <span class="gp-label">Open repositories</span>
          {#each workspaceLoc as row (row.path)}
            <span
              class="gp-chip"
              data-kind="label"
              data-tone={row.path === $repoStore.currentPath ? "accent" : "neutral"}
              title={row.path}
            >
              <span class="font-medium">{row.name}</span>
              {#if row.failed}
                <span class="font-mono text-unknown">LOC unknown</span>
              {:else if row.value !== null}
                <span class="font-mono">{row.value.toLocaleString()} LOC{row.truncated ? '*' : ''}</span>
              {/if}
            </span>
          {/each}
        </div>
      {/if}

      {#if activeTab === "overview"}
        <div class="flex items-center justify-between gap-3 flex-wrap">
          <label class="text-meta text-textMuted flex items-center gap-2">
            <span>Author scope</span>
            <select class="gp-select" bind:value={authorFilter}>
              <option value="all">All contributors (every local and remote branch)</option>
              {#each authors as [email, name] (email)}
                <option value={email}>{name} &lt;{email}&gt;</option>
              {/each}
            </select>
          </label>
          {#if locState.status === "partial"}
            <span class="text-meta text-warn">LOC is a partial language scan, not a complete count.</span>
          {:else if locState.status === "failed"}
            <span class="text-meta text-danger">Couldn't scan languages. LOC is not shown as zero.</span>
          {/if}
        </div>

        <PulsePeriodCompare commits={visibleCommits} truncated={report.truncated} />
        <PulseRhythm commits={visibleCommits} truncated={report.truncated} />
        <PulseHeatmap commits={visibleCommits} />
        <PulseLineTrend
          commits={visibleCommits}
          totalLoc={locKnown ? locValue : 0}
          locStatus={locState.status}
          scoped={authorFilter !== "all"}
        />
        <PulseExtensionChurn extensions={report.extensions} />
        <PulsePunchCard commits={visibleCommits} scoped={authorFilter !== "all"} />
        <PulseHygiene commits={visibleCommits} />

      {:else if activeTab === "hotspots"}
        <PulseHotspotMap
          topFiles={report.top_files_by_churn}
          {coverageReport}
          coverageFailed={coverageFailed}
          coveragePending={coverageRequested && coverageReport === null && !coverageFailed}
          onRescanCoverage={rescanCoverage}
        />

      {:else if activeTab === "knowledge"}
        <PulseKnowledgeMap
          {knowledge}
          error={knowledgeError}
          loading={knowledgeLoading}
          measuredAt={knowledgeMeasuredAt}
          onRefresh={() => pulseStore.loadKnowledge()}
        />
        <PulseCodeAge
          {knowledge}
          error={knowledgeError}
          loading={knowledgeLoading}
          measuredAt={knowledgeMeasuredAt}
          staleNotice={false}
          onRefresh={() => pulseStore.loadKnowledge()}
        />

      {:else if activeTab === "dora"}
        <PulseDora
          {dora}
          error={doraError}
          loading={doraLoading}
          measuredAt={doraMeasuredAt}
          onRefresh={() => pulseStore.loadDora()}
        />
      {/if}
    </div>
  {/if}

  {#if report}
    <PulseExportModal
      open={exportModalOpen}
      options={{
        repoName,
        totalCommits: cardWindow.commits,
        activeDays: cardWindow.activeDays,
        windowStart: cardWindow.firstDay,
        windowEnd: cardWindow.lastDay,
        authorScope: scopedAuthor,
        truncated: report.truncated,
        totalLoc: locKnown ? locValue : null,
        locPartial: locState.status === "partial",
        busFactor: blameMeasured ? (knowledge?.bus_factor ?? null) : null,
        halfLifeDays: blameMeasured ? (knowledge?.half_life_days ?? null) : null,
        blamePartial: blameMeasured && knowledge?.truncated === true,
        conventionalPct: visibleCommits.length > 0 ? hygiene.conventionalPercentage : null,
        signedPct: visibleCommits.length > 0 ? hygiene.signedPercentage : null,
      }}
      onClose={() => (exportModalOpen = false)}
    />
  {/if}
</div>
