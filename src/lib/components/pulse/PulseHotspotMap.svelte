<script lang="ts">
  import { computeHotspotRisks } from "../../pulse/metrics";
  import type { HotspotRiskItem, PulseFileChurn } from "../../pulse/types";
  import type { CoverageReport } from "../../coverage/types";
  import { Flame, FileCode } from "lucide-svelte";

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

  let searchFilter = $state("");
  let selectedLevel = $state<"all" | "critical" | "high" | "medium" | "low">("all");

  const allHotspots = $derived(computeHotspotRisks(topFiles, coverageReport));

  const criticalCount = $derived(allHotspots.filter((h) => h.riskLevel === "critical").length);
  const highCount = $derived(allHotspots.filter((h) => h.riskLevel === "high").length);
  const mediumCount = $derived(allHotspots.filter((h) => h.riskLevel === "medium").length);
  const lowCount = $derived(allHotspots.filter((h) => h.riskLevel === "low").length);

  const filteredHotspots = $derived(
    allHotspots.filter((h) => {
      if (selectedLevel !== "all" && h.riskLevel !== selectedLevel) return false;
      if (searchFilter.trim() && !h.path.toLowerCase().includes(searchFilter.toLowerCase())) {
        return false;
      }
      return true;
    }),
  );

  function getRiskBadge(level: HotspotRiskItem["riskLevel"]) {
    switch (level) {
      case "critical":
        return { bg: "bg-red-500/10 text-red-400 border-red-500/20", label: "Critical" };
      case "high":
        return { bg: "bg-orange-500/10 text-orange-400 border-orange-500/20", label: "High" };
      case "medium":
        return { bg: "bg-amber-500/10 text-amber-400 border-amber-500/20", label: "Medium" };
      default:
        return { bg: "bg-emerald-500/10 text-emerald-400 border-emerald-500/20", label: "Low" };
    }
  }
</script>

<div class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col gap-4">
  <!-- Header & Risk Summary Banner -->
  <div class="flex flex-col sm:flex-row sm:items-center justify-between gap-3 border-b border-border/40 pb-3">
    <div>
      <div class="flex items-center gap-2">
        <Flame size={18} class="text-orange-400" />
        <h3 class="text-sm font-semibold text-textPrimary">Hotspot Risk Map</h3>
        <span class="text-xs px-2 py-0.5 rounded-full bg-surfaceMuted text-textMuted border border-border/60">
          Churn × Coverage
        </span>
      </div>
      <p class="text-xs text-textMuted mt-0.5">
        {#if coveragePending}
          Scanning coverage. Until it lands, ranking is churn-only — not “untested”.
        {:else if coverageFailed}
          Coverage scan failed. Ranking is churn-only; missing coverage is not treated as 0%.
        {:else if !coverageReport}
          Coverage has not been scanned. Ranking is churn-only.
        {:else}
          Files with high modification frequency and low test coverage. A file missing from the coverage report is treated as untested.
        {/if}
      </p>
    </div>

    <!-- Severity Counts -->
    <div class="flex items-center gap-2 text-xs">
      <button
        type="button"
        class="px-2 py-1 rounded-md border text-xs font-medium transition-colors {selectedLevel === 'critical' ? 'bg-red-500/20 border-red-500/50 text-red-300' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
        onclick={() => (selectedLevel = selectedLevel === "critical" ? "all" : "critical")}
      >
        Critical ({criticalCount})
      </button>
      <button
        type="button"
        class="px-2 py-1 rounded-md border text-xs font-medium transition-colors {selectedLevel === 'high' ? 'bg-orange-500/20 border-orange-500/50 text-orange-300' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
        onclick={() => (selectedLevel = selectedLevel === "high" ? "all" : "high")}
      >
        High ({highCount})
      </button>
      <button
        type="button"
        class="px-2 py-1 rounded-md border text-xs font-medium transition-colors {selectedLevel === 'medium' ? 'bg-amber-500/20 border-amber-500/50 text-amber-300' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
        onclick={() => (selectedLevel = selectedLevel === "medium" ? "all" : "medium")}
      >
        Medium ({mediumCount})
      </button>
      <button
        type="button"
        class="px-2 py-1 rounded-md border text-xs font-medium transition-colors {selectedLevel === 'low' ? 'bg-emerald-500/20 border-emerald-500/50 text-emerald-300' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
        onclick={() => (selectedLevel = selectedLevel === "low" ? "all" : "low")}
      >
        Low ({lowCount})
      </button>
    </div>
  </div>

  <!-- Search and Filter Row -->
  <div class="flex items-center gap-3">
    <div class="relative flex-1">
      <input
        type="text"
        placeholder="Filter hotspots by filename..."
        bind:value={searchFilter}
        class="w-full bg-surface text-textPrimary text-xs rounded-lg px-3 py-1.5 border border-border/70 focus:outline-none focus:border-accent"
      />
    </div>
    {#if selectedLevel !== "all" || searchFilter}
      <button
        type="button"
        class="text-xs text-accent hover:underline shrink-0"
        onclick={() => {
          selectedLevel = "all";
          searchFilter = "";
        }}
      >
        Clear filters
      </button>
    {/if}
  </div>

  <!-- Hotspots Table -->
  {#if filteredHotspots.length === 0}
    <div class="py-8 text-center text-textMuted text-xs">
      No hotspots match the current filter.
    </div>
  {:else}
    <div class="overflow-x-auto max-h-80 overflow-y-auto border border-border/50 rounded-lg">
      <table class="w-full text-left text-xs border-collapse">
        <thead class="bg-surfaceMuted/50 text-textMuted uppercase text-[10px] tracking-wider sticky top-0 backdrop-blur-sm">
          <tr class="border-b border-border/60">
            <th class="py-2 px-3 font-semibold">File</th>
            <th class="py-2 px-3 font-semibold">Churn</th>
            <th class="py-2 px-3 font-semibold">Commits</th>
            <th class="py-2 px-3 font-semibold">Coverage</th>
            <th class="py-2 px-3 font-semibold">Risk Score</th>
            <th class="py-2 px-3 font-semibold text-right">Risk Level</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-border/30">
          {#each filteredHotspots as hotspot (hotspot.path)}
            {@const badge = getRiskBadge(hotspot.riskLevel)}
            <tr
              class="hover:bg-surfaceMuted/40 transition-colors group cursor-pointer"
              onclick={() => onSelectFile?.(hotspot.path)}
            >
              <td class="py-2 px-3 font-mono text-textPrimary flex items-center gap-1.5 truncate max-w-xs sm:max-w-md">
                <FileCode size={13} class="text-textMuted shrink-0" />
                <span class="truncate" title={hotspot.path}>{hotspot.path}</span>
              </td>
              <td class="py-2 px-3 whitespace-nowrap">
                <span class="text-emerald-400">+{hotspot.additions.toLocaleString()}</span>
                <span class="text-rose-400 ml-1">-{hotspot.deletions.toLocaleString()}</span>
              </td>
              <td class="py-2 px-3 text-textMuted whitespace-nowrap">
                {hotspot.commitsCount}
              </td>
              <td class="py-2 px-3 whitespace-nowrap">
                {#if hotspot.coveragePercentage !== null}
                  <div class="flex items-center gap-1.5">
                    <span class={hotspot.coveragePercentage < 50 ? "text-rose-400 font-semibold" : hotspot.coveragePercentage < 80 ? "text-amber-400" : "text-emerald-400"}>
                      {hotspot.coveragePercentage}%
                    </span>
                    {#if hotspot.uncoveredLines !== null && hotspot.uncoveredLines > 0}
                      <span class="text-[10px] text-textMuted">({hotspot.uncoveredLines} uncov)</span>
                    {/if}
                  </div>
                {:else if hotspot.coverageStatus === "unscanned"}
                  <span class="text-textMuted italic text-[11px]">Unknown</span>
                {:else}
                  <span class="text-textMuted italic text-[11px]">Not in report</span>
                {/if}
              </td>
              <td class="py-2 px-3 font-mono font-medium text-textPrimary whitespace-nowrap">
                {hotspot.riskScore.toLocaleString()}
              </td>
              <td class="py-2 px-3 text-right whitespace-nowrap">
                <span class="inline-block px-2 py-0.5 rounded-full text-[10px] font-semibold border {badge.bg}">
                  {badge.label}
                </span>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
