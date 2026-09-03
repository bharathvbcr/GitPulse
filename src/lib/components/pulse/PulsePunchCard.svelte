<script lang="ts">
  import { computePunchCard } from "../../pulse/metrics";
  import type { PulseCommitSummary, PunchCardCell } from "../../pulse/types";
  import { Clock, Moon, ShieldCheck } from "lucide-svelte";

  let {
    commits = [],
  }: {
    commits: readonly PulseCommitSummary[];
  } = $props();

  const punch = $derived(computePunchCard(commits));
  let hoveredCell = $state<PunchCardCell | null>(null);

  const dayNames = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const hourLabels = [0, 3, 6, 9, 12, 15, 18, 21];

  function formatHour(h: number): string {
    if (h === 0) return "12am";
    if (h === 12) return "12pm";
    return h < 12 ? `${h}am` : `${h - 12}pm`;
  }
</script>

<div class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col gap-3.5">
  <div class="flex items-center justify-between border-b border-border/50 pb-2.5">
    <div class="flex items-center gap-2">
      <Clock size={15} class="text-accent shrink-0" />
      <span class="text-xs font-semibold text-textPrimary uppercase tracking-wider">Commit Punch Card</span>
      <span class="text-[11px] text-textMuted">(Day of Week × Hour of Day)</span>
    </div>

    <!-- After-hours burnout indicator badge -->
    <div
      class="flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium border {punch.afterHoursPercentage > 40
        ? 'bg-amber-500/10 text-amber-500 border-amber-500/30'
        : 'bg-surface text-textMuted border-border/60'}"
      title="Commits made outside 9am–6pm weekdays. Local-only calculation, never sent off-device."
    >
      <Moon size={12} class="shrink-0" />
      <span>{punch.afterHoursPercentage}% After-Hours</span>
    </div>
  </div>

  <!-- 24-hour grid -->
  <div class="overflow-x-auto pb-1 select-none">
    <div class="inline-flex flex-col gap-1 min-w-[620px]">
      <!-- Hour header labels -->
      <div class="flex text-[10px] text-textMuted h-4">
        <div class="w-8 shrink-0"></div>
        <div class="flex-1 grid grid-cols-24 text-center">
          {#each Array(24) as _, h}
            <span class="truncate {hourLabels.includes(h) ? 'font-medium text-textPrimary' : 'text-transparent'}">
              {formatHour(h)}
            </span>
          {/each}
        </div>
      </div>

      <!-- 7 day rows -->
      <div class="flex flex-col gap-1.5">
        {#each dayNames as dayName, dayIdx}
          <div class="flex items-center gap-2">
            <span class="w-8 text-[10px] font-medium text-textMuted text-right shrink-0">{dayName}</span>
            <div class="flex-1 grid grid-cols-24 gap-1 items-center h-5">
              {#each Array(24) as _, h}
                {@const cell = punch.cells.find((c) => c.dayOfWeek === dayIdx && c.hour === h)}
                {@const count = cell?.count ?? 0}
                {@const radius = punch.maxCount > 0 ? Math.max(3, Math.round((count / punch.maxCount) * 14)) : 3}
                {@const isStandard = dayIdx >= 1 && dayIdx <= 5 && h >= 9 && h < 18}

                <div
                  role="gridcell"
                  tabindex="0"
                  onmouseenter={() => (hoveredCell = cell ?? null)}
                  onmouseleave={() => (hoveredCell = null)}
                  class="flex items-center justify-center h-5 w-full cursor-pointer group"
                >
                  {#if count > 0}
                    <div
                      class="rounded-full bg-accent transition-all group-hover:scale-125 group-hover:ring-2 group-hover:ring-accent/40"
                      style="width: {radius}px; height: {radius}px; opacity: {0.3 + 0.7 * (count / punch.maxCount)};"
                    ></div>
                  {:else}
                    <div
                      class="w-1 h-1 rounded-full {isStandard ? 'bg-border/60' : 'bg-border/30'}"
                    ></div>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
        {/each}
      </div>
    </div>
  </div>

  <!-- Footer Info & Privacy notice -->
  <div class="flex items-center justify-between text-[11px] text-textMuted pt-1 border-t border-border/40">
    <div>
      {#if hoveredCell && hoveredCell.count > 0}
        <span class="font-medium text-textPrimary">{dayNames[hoveredCell.dayOfWeek]} at {formatHour(hoveredCell.hour)}</span>:
        <span class="text-accent font-medium">{hoveredCell.count} commit{hoveredCell.count === 1 ? '' : 's'}</span>
        {#if hoveredCell.churn > 0}
          <span class="text-[10px] text-textMuted ml-1">({hoveredCell.churn.toLocaleString()} lines churned)</span>
        {/if}
      {:else}
        <span>Dot size corresponds to commit frequency for each hour slot</span>
      {/if}
    </div>

    <div class="flex items-center gap-1.5 text-[10px] text-textMuted">
      <ShieldCheck size={12} class="text-accent shrink-0" />
      <span>Computed locally; never leaves your machine</span>
    </div>
  </div>
</div>
