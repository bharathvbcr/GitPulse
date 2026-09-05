<script lang="ts">
  import { untrack } from "svelte";
  import { repoStore } from "../../stores/repoStore";
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
  } from "lucide-svelte";

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
  let loadedPath = $state<string | null>(null);

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
    const path = $repoStore.currentPath;
    if (path === loadedPath) return;
    loadedPath = path;
    if (!path) {
      pulseStore.reset();
      return;
    }
    void pulseStore.load(path);
    authorFilter = "all";
  });

  // LOC now tracks the repository instead of the tab that opened it: the
  // metric revalidates on every settled write, so the headline line count
  // moves as the user works rather than freezing at whatever it was when this
  // view was first mounted.
  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path) {
      locState = { status: "idle" };
      return;
    }
    return locMetric.subscribe(path, (snap) => {
      locState = toLocState(snap);
      // Only a complete, current measurement is worth recording as a history
      // point; a truncated or stale one would put a false dip in the trend.
      if (snap.value && snap.stale === null && snap.value.truncated !== true) {
        void pulseStore.recordSnapshot(totalCodeLines(snap.value) ?? 0);
      }
    });
  });

  // One shared coverage measurement. This view and CoverageViewer used to
  // invoke `cmd_scan_coverage` separately and could disagree about the result.
  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path || activeTab !== "hotspots") return;
    coverageRequested = true;
    return coverageMetric.subscribe(path, (snap) => {
      coverageReport = snap.value;
      coverageFailed = snap.state === "failed" && snap.value === null;
    });
  });

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
  const doraError = $derived($pulseStore.doraError);

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
  <div class="px-6 py-4 border-b border-border/80 bg-surface/40 shrink-0 flex flex-col sm:flex-row sm:items-center justify-between gap-3">
    <div class="flex items-center gap-3">
      <div class="w-9 h-9 rounded-xl bg-accent/10 border border-accent/25 flex items-center justify-center text-accent shadow-sm">
        <Activity size={18} />
      </div>
      <div>
        <div class="flex items-center gap-2">
          <h1 class="text-sm font-bold text-textPrimary tracking-tight">Repository Pulse</h1>
          {#if report}
            <span class="text-[10px] font-mono px-2 py-0.5 rounded-full bg-surface border border-border text-textMuted">
              {report.total_commits_scanned.toLocaleString()} commits
            </span>
          {/if}
          {#if report?.has_mailmap}
            <span
              class="text-[10px] px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-500 border border-emerald-500/25 flex items-center gap-1"
              title=".mailmap active: author aliases are canonicalized."
            >
              <Mail size={10} />
              <span>.mailmap</span>
            </span>
          {:else if report}
            <span
              class="text-[10px] px-2 py-0.5 rounded-full bg-surface border border-border text-textMuted"
              title="No .mailmap. The same person with work and personal emails counts as two contributors."
            >
              no .mailmap
            </span>
          {/if}
        </div>
        <p class="text-[11px] text-textMuted mt-0.5">
          Local cadence, churn, hotspots, knowledge concentration, and tag-based delivery. All branches, computed on this machine.
        </p>
      </div>
    </div>

    <div class="flex items-center gap-2">
      <div class="flex items-center bg-surface border border-border/70 rounded-lg p-0.5 text-xs">
        <button
          type="button"
          class="px-2.5 py-1 rounded-md transition-colors {activeTab === 'overview' ? 'bg-accent text-white font-medium shadow-xs' : 'text-textMuted hover:text-textPrimary'}"
          onclick={() => (activeTab = "overview")}
        >
          Overview
        </button>
        <button
          type="button"
          class="px-2.5 py-1 rounded-md transition-colors flex items-center gap-1 {activeTab === 'hotspots' ? 'bg-accent text-white font-medium shadow-xs' : 'text-textMuted hover:text-textPrimary'}"
          onclick={() => (activeTab = "hotspots")}
        >
          <Flame size={12} />
          <span>Hotspots</span>
        </button>
        <button
          type="button"
          class="px-2.5 py-1 rounded-md transition-colors flex items-center gap-1 {activeTab === 'knowledge' ? 'bg-accent text-white font-medium shadow-xs' : 'text-textMuted hover:text-textPrimary'}"
          onclick={() => (activeTab = "knowledge")}
        >
          <Users size={12} />
          <span>Knowledge & Age</span>
        </button>
        <button
          type="button"
          class="px-2.5 py-1 rounded-md transition-colors flex items-center gap-1 {activeTab === 'dora' ? 'bg-accent text-white font-medium shadow-xs' : 'text-textMuted hover:text-textPrimary'}"
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
        class="gp-btn !py-1.5 !px-3 text-xs inline-flex items-center gap-1.5"
        title="Export Pulse Summary Card for README"
      >
        <Download size={12} />
        <span class="hidden md:inline">Export Card</span>
      </button>

      <button
        type="button"
        onclick={() => pulseStore.reload()}
        disabled={loading}
        class="gp-btn !py-1.5 !px-3 text-xs inline-flex items-center gap-1.5"
      >
        <RefreshCw size={12} class={loading ? 'animate-spin text-accent' : ''} />
        <span>{loading ? 'Refreshing…' : 'Refresh'}</span>
      </button>
    </div>
  </div>

  {#if report?.truncated}
    <div class="mx-6 mt-4 p-3 rounded-xl bg-amber-500/10 border border-amber-500/30 text-xs text-textPrimary flex items-center justify-between gap-3">
      <div class="flex items-center gap-2 text-amber-500">
        <AlertCircle size={15} class="shrink-0" />
        {#if report.payload_truncated}
          <span class="font-medium">Log output hit the payload budget.</span>
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
          class="gp-btn !py-1 !px-2.5 !text-[11px] shrink-0 font-medium"
        >
          Scan Deeper (25k)
        </button>
      {/if}
    </div>
  {/if}

  {#if error}
    <div class="m-6 p-4 rounded-xl bg-rose-500/10 border border-rose-500/30 text-xs text-rose-500 flex items-start gap-3">
      <AlertCircle size={16} class="shrink-0 mt-0.5" />
      <div class="flex-1">
        <p class="font-semibold">Unable to generate repository pulse metrics</p>
        <p class="mt-1 font-mono text-[11px] opacity-90 break-words">{error}</p>
        <p class="mt-2 text-[11px] text-textMuted">
          Recorded under <span class="font-mono">pulse</span> in Diagnostics, which carries the
          backend log tail — where a backend crash writes its location and backtrace.
        </p>
        <div class="flex items-center gap-2 mt-3">
          <button
            type="button"
            onclick={() => pulseStore.reload()}
            class="gp-btn !py-1 !px-2.5 text-xs"
          >
            Try Again
          </button>
          <button
            type="button"
            onclick={() => window.dispatchEvent(new CustomEvent("gitpulse:diagnostics"))}
            class="gp-btn !py-1 !px-2.5 text-xs"
          >
            Open Diagnostics
          </button>
        </div>
      </div>
    </div>
  {:else if loading && !report}
    <div class="p-6 space-y-4">
      <div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
        {#each [1, 2, 3, 4] as n (n)}
          <div class="h-20 bg-surface/60 rounded-xl border border-border animate-pulse"></div>
        {/each}
      </div>
      <div class="h-44 bg-surface/60 rounded-xl border border-border animate-pulse"></div>
      <div class="h-48 bg-surface/60 rounded-xl border border-border animate-pulse"></div>
    </div>
  {:else if report && report.commits.length === 0}
    <div class="flex-1 flex items-center justify-center p-8">
      <EmptyState
        icon={GitCommit}
        title="No commit activity found"
        hint="This repository does not have any commits yet, or the history could not be reached."
        action={{
          label: "Refresh Pulse",
          onClick: () => pulseStore.reload(),
          icon: RefreshCw,
        }}
      />
    </div>
  {:else if report}
    <div class="p-6 space-y-5 max-w-7xl">
      {#if workspaceLoc.length > 1}
        <div class="gp-card p-3 rounded-xl border border-border/70 bg-surface/40 flex flex-wrap items-center gap-3 text-xs">
          <span class="text-textMuted font-medium uppercase tracking-wider text-[10px]">Open repos</span>
          {#each workspaceLoc as row (row.path)}
            <span
              class="inline-flex items-center gap-1.5 px-2 py-1 rounded-md border {row.path === $repoStore.currentPath ? 'border-accent/40 bg-accent/10 text-textPrimary' : 'border-border/60 text-textMuted'}"
              title={row.path}
            >
              <span class="font-medium">{row.name}</span>
              {#if row.failed}
                <span class="font-mono text-rose-400">LOC unknown</span>
              {:else if row.value !== null}
                <span class="font-mono">{row.value.toLocaleString()} loc{row.truncated ? '*' : ''}</span>
              {/if}
            </span>
          {/each}
        </div>
      {/if}

      {#if activeTab === "overview"}
        <div class="flex items-center justify-between gap-3 flex-wrap">
          <label class="text-[11px] text-textMuted flex items-center gap-2">
            <span>Author scope</span>
            <select
              class="bg-surface border border-border/70 rounded-md px-2 py-1 text-xs text-textPrimary"
              bind:value={authorFilter}
            >
              <option value="all">All contributors (every local and remote branch)</option>
              {#each authors as [email, name] (email)}
                <option value={email}>{name} &lt;{email}&gt;</option>
              {/each}
            </select>
          </label>
          {#if locState.status === "partial"}
            <span class="text-[11px] text-amber-500">LOC is a partial language scan, not a complete count.</span>
          {:else if locState.status === "failed"}
            <span class="text-[11px] text-rose-400">Language scan failed — LOC is not shown as zero.</span>
          {/if}
        </div>

        <PulsePeriodCompare commits={visibleCommits} />
        <PulseRhythm commits={visibleCommits} truncated={report.truncated} />
        <PulseHeatmap commits={visibleCommits} />
        <PulseLineTrend
          commits={visibleCommits}
          totalLoc={locKnown ? locValue : 0}
          locStatus={locState.status}
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
        />

      {:else if activeTab === "knowledge"}
        <PulseKnowledgeMap
          {knowledge}
          error={knowledgeError}
          loading={knowledgeLoading}
          onRefresh={() => pulseStore.loadKnowledge()}
        />
        <PulseCodeAge
          {knowledge}
          error={knowledgeError}
          loading={knowledgeLoading}
        />

      {:else if activeTab === "dora"}
        <PulseDora
          {dora}
          error={doraError}
          loading={doraLoading}
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
