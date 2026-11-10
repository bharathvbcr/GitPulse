<script lang="ts">
  import { computeHeatmap } from "../../pulse/metrics";
  import type { HeatmapDay, PulseCommitSummary } from "../../pulse/types";
  import { repoStore } from "../../stores/repoStore";
  import { filterStore } from "../../stores/filterStore";
  import SettingSegment from "../SettingSegment.svelte";
  import { Activity } from "@lucide/svelte";

  let {
    commits = [],
    now = Date.now(),
  }: {
    commits: readonly PulseCommitSummary[];
    now?: number;
  } = $props();

  let mode = $state<"count" | "churn">("count");
  let hoveredDay = $state<HeatmapDay | null>(null);

  const weeks = $derived(computeHeatmap(commits, 53, now, mode));

  // Determine month labels across the top
  const monthLabels = $derived.by(() => {
    const labels: { name: string; weekIndex: number }[] = [];
    let lastMonth = -1;

    for (const week of weeks) {
      // Look at the first valid day of the week
      const day = week.days.find((d) => d !== null);
      if (day) {
        const d = new Date(day.timestamp);
        const m = d.getMonth();
        if (m !== lastMonth) {
          labels.push({
            name: d.toLocaleString("default", { month: "short" }),
            weekIndex: week.weekIndex,
          });
          lastMonth = m;
        }
      }
    }
    return labels;
  });

  function cellColor(level: number): string {
    switch (level) {
      case 1:
        return "bg-accent/30 hover:ring-1 hover:ring-accent";
      case 2:
        return "bg-accent/55 hover:ring-1 hover:ring-accent";
      case 3:
        return "bg-accent/80 hover:ring-1 hover:ring-accent";
      case 4:
        return "bg-accent hover:ring-1 hover:ring-onAccent";
      default:
        return "bg-surfaceHover hover:bg-border/60";
    }
  }

  function handleDayClick(day: HeatmapDay) {
    if (day.count === 0) return;
    // `date:` is a first-class filter prefix (parseQuery + CommitFilter).
    filterStore.setSearch(`date:${day.date}`);
    repoStore.setActiveTab("history");
  }

  function handleDayKey(day: HeatmapDay, event: KeyboardEvent) {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      handleDayClick(day);
    }
  }
</script>

<div class="gp-card p-4 flex flex-col gap-3">
  <div class="flex items-center justify-between gap-3">
    <div class="flex items-center gap-2">
      <Activity size={15} class="text-accent shrink-0" />
      <span class="text-xs font-semibold text-textPrimary">Contribution calendar</span>
      <span class="text-meta text-textMuted">(Past 53 weeks)</span>
    </div>

    <SettingSegment
      ariaLabel="Calendar measure"
      options={[
        { value: "count", label: "Commits" },
        { value: "churn", label: "Lines changed" },
      ]}
      value={mode}
      onselect={(next) => (mode = next)}
    />
  </div>

  <!-- Heatmap Calendar Grid -->
  <div class="overflow-x-auto pb-1 select-none">
    <div class="inline-flex flex-col gap-1 min-w-[720px]">
      <!-- Month headers -->
      <div class="flex text-micro text-textMuted h-4 relative">
        <div class="w-6 shrink-0"></div>
        <div class="flex-1 flex relative">
          {#each monthLabels as month}
            <span
              class="absolute truncate"
              style="left: {month.weekIndex * 13.5}px;"
            >
              {month.name}
            </span>
          {/each}
        </div>
      </div>

      <!-- 7 rows x 53 columns -->
      <div class="flex gap-1">
        <!-- Day labels column -->
        <div class="flex flex-col justify-between text-micro text-textMuted w-6 pt-0.5 pb-0.5 shrink-0">
          <span class="h-2.5">Sun</span>
          <span class="h-2.5">Tue</span>
          <span class="h-2.5">Thu</span>
          <span class="h-2.5">Sat</span>
        </div>

        <!-- Weeks -->
        <div class="flex gap-[3px] flex-1">
          {#each weeks as week (week.weekIndex)}
            <div class="flex flex-col gap-[3px]">
              {#each week.days as day, dIdx (dIdx)}
                {#if day}
                  <!-- Justified: interactive calendar cell navigates to history on click -->
                  <!-- svelte-ignore a11y_click_events_have_key_events -->
                  <div
                    role="gridcell"
                    tabindex="0"
                    onclick={() => handleDayClick(day)}
                    onkeydown={(e) => handleDayKey(day, e)}
                    onmouseenter={() => (hoveredDay = day)}
                    onmouseleave={() => (hoveredDay = null)}
                    class="w-[10.5px] h-[10.5px] rounded-xs transition-colors duration-(--gp-dur-instant) ease-(--gp-ease-effect) cursor-pointer {cellColor(day.level)}"
                    title="{day.date}: {day.count} commit{day.count === 1 ? '' : 's'} (+{day.additions} / -{day.deletions})"
                  ></div>
                {:else}
                  <div class="w-[10.5px] h-[10.5px]"></div>
                {/if}
              {/each}
            </div>
          {/each}
        </div>
      </div>
    </div>
  </div>

  <!-- Footer Info & Legend -->
  <div class="flex items-center justify-between gap-3 text-meta text-textMuted pt-1">
    <div class="min-h-4 text-textPrimary">
      {#if hoveredDay}
        <span class="font-medium text-textPrimary">{hoveredDay.date}</span>:
        <span class="text-accent font-medium">{hoveredDay.count} commit{hoveredDay.count === 1 ? '' : 's'}</span>
        {#if hoveredDay.churn > 0}
          <span class="text-micro text-textMuted ml-1">
            (<span class="text-ok font-mono">+{hoveredDay.additions}</span> /
            <span class="text-danger font-mono">-{hoveredDay.deletions}</span>)
          </span>
        {/if}
        {#if hoveredDay.count > 0}
          <span class="text-micro text-accent ml-2">Click to view in the graph</span>
        {/if}
      {:else}
        <span>Hover a day for details. Click it to filter the graph.</span>
      {/if}
    </div>

    <div class="flex items-center gap-1.5 text-micro">
      <span>Less</span>
      <span class="w-2.5 h-2.5 rounded-xs bg-surfaceHover border border-edgeSoft"></span>
      <span class="w-2.5 h-2.5 rounded-xs bg-accent/30"></span>
      <span class="w-2.5 h-2.5 rounded-xs bg-accent/55"></span>
      <span class="w-2.5 h-2.5 rounded-xs bg-accent/80"></span>
      <span class="w-2.5 h-2.5 rounded-xs bg-accent"></span>
      <span>More</span>
    </div>
  </div>
</div>
