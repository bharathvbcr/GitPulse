<script lang="ts">
  import { computeHotspotRisks } from "../../pulse/metrics";
  import type { HotspotRiskItem, PulseFileChurn } from "../../pulse/types";
  import type { CoverageReport } from "../../coverage/types";
  import EmptyState from "../EmptyState.svelte";
  import SettingSegment from "../SettingSegment.svelte";
  import { LAYERS } from "../../ui/layers";
  import { Flame, FileCode, TriangleAlert } from "@lucide/svelte";

  let {
    topFiles = [],
    coverageReport = null,
    coverageFailed = false,
    coveragePending = false,
    onSelectFile,
  }: {
    topFiles: readonly PulseFileChurn[];
    coverageReport: CoverageReport | null;
    coverageFailed?: boolean;
    coveragePending?: boolean;
    onSelectFile?: (path: string) => void;
  } = $props();

  type RiskFilter = "all" | HotspotRiskItem["riskLevel"];

  let searchFilter = $state("");
  let selectedLevel = $state<RiskFilter>("all");

  const allHotspots = $derived(computeHotspotRisks(topFiles, coverageReport));

  const criticalCount = $derived(allHotspots.filter((h) => h.riskLevel === "critical").length);
  const highCount = $derived(allHotspots.filter((h) => h.riskLevel === "high").length);
  const mediumCount = $derived(allHotspots.filter((h) => h.riskLevel === "medium").length);
  const lowCount = $derived(allHotspots.filter((h) => h.riskLevel === "low").length);

  const levelOptions = $derived([
    { value: "critical" as const, label: `Critical (${criticalCount})` },
    { value: "high" as const, label: `High (${highCount})` },
    { value: "medium" as const, label: `Medium (${mediumCount})` },
    { value: "low" as const, label: `Low (${lowCount})` },
  ]);

  const filteredHotspots = $derived(
    allHotspots.filter((h) => {
      if (selectedLevel !== "all" && h.riskLevel !== selectedLevel) return false;
      if (searchFilter.trim() && !h.path.toLowerCase().includes(searchFilter.toLowerCase())) {
        return false;
      }
      return true;
    }),
  );

  /** Choosing the level that is already chosen clears the filter, as the buttons always did. */
  function toggleLevel(next: RiskFilter) {
    selectedLevel = selectedLevel === next ? "all" : next;
  }

  /** Risk tiers wear the status roles; medium is neutral because the roles have no fifth severity. */
  function getRiskBadge(level: HotspotRiskItem["riskLevel"]) {
    switch (level) {
      case "critical":
        return { tone: "danger", label: "Critical" };
      case "high":
        return { tone: "warn", label: "High" };
      case "medium":
        return { tone: "neutral", label: "Medium" };
      default:
        return { tone: "ok", label: "Low" };
    }
  }
</script>

<div class="gp-card p-4 flex flex-col gap-4">
  <!-- Header & risk summary -->
  <div class="flex flex-col sm:flex-row sm:items-center justify-between gap-3">
    <div class="min-w-0">
      <div class="flex items-center gap-2">
        <Flame size={18} class="text-warn" />
        <h3 class="text-sm font-semibold text-textPrimary">Hotspot risk map</h3>
        <span class="gp-chip" data-tone="neutral">Churn × coverage</span>
      </div>
      {#if coverageFailed}
        <!-- A scan that could not run says so in the unknown tone: the ranking
             below is churn-only and must not read as a coverage verdict. -->
        <div class="gp-notice mt-1" data-tone="unknown" role="status">
          <TriangleAlert size={14} class="mt-px shrink-0" style="color: var(--gp-notice-ink)" />
          <div class="min-w-0 flex-1">
            Couldn't scan coverage. Ranking is churn-only; missing coverage is not treated as 0%.
          </div>
        </div>
      {:else}
        <p class="text-xs text-textMuted mt-0.5">
          {#if coveragePending}
            Scanning coverage. Until it lands, ranking is churn-only — not “untested”.
          {:else if !coverageReport}
            Coverage has not been scanned. Ranking is churn-only.
          {:else}
            Files with high modification frequency and low test coverage. A file missing from the coverage report is treated as untested.
          {/if}
        </p>
      {/if}
    </div>

    <!-- Severity counts -->
    <SettingSegment
      ariaLabel="Risk level"
      options={levelOptions}
      value={selectedLevel}
      onselect={toggleLevel}
    />
  </div>

  <!-- Search and filter row -->
  <div class="flex items-center gap-3">
    <div class="relative flex-1">
      <input
        type="text"
        placeholder="Filter hotspots by file name…"
        aria-label="Filter hotspots by file name"
        bind:value={searchFilter}
        class="gp-field w-full"
      />
    </div>
    {#if selectedLevel !== "all" || searchFilter}
      <button
        type="button"
        class="gp-btn shrink-0"
        data-variant="ghost"
        data-size="sm"
        onclick={() => {
          selectedLevel = "all";
          searchFilter = "";
        }}
      >
        Clear filters
      </button>
    {/if}
  </div>

  <!-- Hotspots table -->
  {#if allHotspots.length === 0}
    <EmptyState
      compact
      icon={Flame}
      title="No hotspots yet"
      hint="Files with churn appear here once commits change them."
    />
  {:else if filteredHotspots.length === 0}
    <div class="py-8 text-center text-textMuted text-xs">
      No hotspots match the current filter.
    </div>
  {:else}
    <div class="overflow-x-auto max-h-80 overflow-y-auto rounded-lg">
      <table class="w-full text-left text-xs border-collapse">
        <thead class="bg-surfaceHover text-textMuted sticky top-0" style="z-index: {LAYERS.STICKY}">
          <tr>
            <th class="gp-label py-2 px-3">File</th>
            <th class="gp-label py-2 px-3">Churn</th>
            <th class="gp-label py-2 px-3">Commits</th>
            <th class="gp-label py-2 px-3">Coverage</th>
            <th class="gp-label py-2 px-3">Risk score</th>
            <th class="gp-label py-2 px-3 text-right">Risk level</th>
          </tr>
        </thead>
        <tbody>
          {#each filteredHotspots as hotspot (hotspot.path)}
            {@const badge = getRiskBadge(hotspot.riskLevel)}
            <tr
              class="hover:bg-surfaceHover/60 transition-colors duration-(--gp-dur-instant) group cursor-pointer"
              onclick={() => onSelectFile?.(hotspot.path)}
            >
              <td class="py-2 px-3 font-mono text-textPrimary flex items-center gap-1.5 truncate max-w-xs sm:max-w-md">
                <FileCode size={13} class="text-textMuted shrink-0" />
                <span class="truncate" title={hotspot.path}>{hotspot.path}</span>
              </td>
              <td class="py-2 px-3 whitespace-nowrap">
                <span class="text-ok">+{hotspot.additions.toLocaleString()}</span>
                <span class="text-danger ml-1">-{hotspot.deletions.toLocaleString()}</span>
              </td>
              <td class="py-2 px-3 text-textMuted whitespace-nowrap">
                {hotspot.commitsCount}
              </td>
              <td class="py-2 px-3 whitespace-nowrap">
                {#if hotspot.coveragePercentage !== null}
                  <div class="flex items-center gap-1.5">
                    <span class={hotspot.coveragePercentage < 50 ? "text-danger font-semibold" : hotspot.coveragePercentage < 80 ? "text-warn" : "text-ok"}>
                      {hotspot.coveragePercentage}%
                    </span>
                    {#if hotspot.uncoveredLines !== null && hotspot.uncoveredLines > 0}
                      <span class="text-micro text-textMuted">({hotspot.uncoveredLines} uncovered)</span>
                    {/if}
                  </div>
                {:else if hotspot.coverageStatus === "unscanned"}
                  <span class="gp-chip" data-tone="unknown" title="Coverage was not scanned for this file">Unknown</span>
                {:else}
                  <span class="text-textMuted text-meta">Not in report</span>
                {/if}
              </td>
              <td class="py-2 px-3 font-mono font-medium text-textPrimary whitespace-nowrap">
                {hotspot.riskScore.toLocaleString()}
              </td>
              <td class="py-2 px-3 text-right whitespace-nowrap">
                <span class="gp-chip" data-tone={badge.tone}>{badge.label}</span>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <!-- The table lists the files the report ranked, not the repository, so
         say how many survive the filters rather than letting a short list
         read as the whole picture. -->
    <p class="m-0 text-meta text-textMuted">
      Showing {filteredHotspots.length} of {allHotspots.length} ranked {allHotspots.length === 1 ? "file" : "files"}
    </p>
  {/if}
</div>
