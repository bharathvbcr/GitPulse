<script lang="ts">
  /**
   * Fleet Pulse: how the whole workspace is moving, above the grid.
   *
   * The grid answers "what is the state of each repository". This answers "is
   * anything happening, and where" — the question you actually have when you
   * open a workspace of two dozen repositories on a Monday.
   *
   * Every number here is `fleetPulse`'s, computed by a pure, tested function,
   * and every one of them is rendered next to what it could not count.
   * `describePulseCoverage` returns "" only when the reading really does cover
   * the whole workspace, so the coverage clause below is not decoration: its
   * absence is the claim.
   */
  import {
    Activity,
    ChevronDown,
    ChevronRight,
    Flame,
    Moon,
    TrendingDown,
    TrendingUp,
    Trash2,
    Users,
  } from "lucide-svelte";
  import type { FleetPulse, FleetLanguageMix } from "../fleet/pulse";
  import { describePulseCoverage, TREND_DAYS } from "../fleet/pulse";
  import { foldFleetLanguages } from "../fleet/languages";
  import { COMMIT_WINDOWS, DEFAULT_COMMIT_WINDOW } from "../fleet/types";
  import FleetSparkline from "./FleetSparkline.svelte";
  import FleetLanguageBar from "./FleetLanguageBar.svelte";

  let {
    pulse,
    languages,
    open = true,
    onToggle,
    onSelect,
    onRemove,
    /** The window the current rows were read at, for the segmented control. */
    windowDays = DEFAULT_COMMIT_WINDOW,
    /** Re-sweeps at a new window. The rows are re-read, never relabelled. */
    onWindow,
    /** True while the sweep behind a window change is in flight. */
    loading = false,
  }: {
    pulse: FleetPulse;
    languages: FleetLanguageMix;
    open?: boolean;
    onToggle?: () => void;
    /** Focus a repository named in a callout. */
    onSelect?: (path: string) => void;
    /** Drop a dormant repository from the workspace, from where it is named. */
    onRemove?: (path: string) => void;
    windowDays?: number;
    onWindow?: (days: number) => void;
    loading?: boolean;
  } = $props();

  const coverage = $derived(describePulseCoverage(pulse));
  const languageStats = $derived(foldFleetLanguages(languages.stats));

  /** The trend clause, or why there is not one. */
  const trendLabel = $derived.by(() => {
    const { recent, prior, deltaPct, direction } = pulse.trend;
    if (direction === "new") {
      // A percentage against zero is infinite, and "+100%" for one commit
      // after a silent fortnight is a number that reads like a finding.
      return `${recent.toLocaleString()} in ${TREND_DAYS}d, after none in the ${TREND_DAYS} before`;
    }
    if (deltaPct === null) return `nothing in the last ${TREND_DAYS * 2} days`;
    const sign = deltaPct > 0 ? "+" : "";
    return `${recent.toLocaleString()} in ${TREND_DAYS}d · ${sign}${deltaPct.toFixed(0)}% vs the ${TREND_DAYS} before (${prior.toLocaleString()})`;
  });

  const trendTone = $derived(
    pulse.trend.direction === "up" || pulse.trend.direction === "new"
      ? "text-emerald-600 dark:text-emerald-400"
      : pulse.trend.direction === "down"
        ? "text-amber-600 dark:text-amber-400"
        : "text-textMuted",
  );
</script>

<section
  class="shrink-0 border-b border-border px-4 py-2.5"
  aria-label="Fleet Pulse"
  data-testid="fleet-pulse"
>
  <div class="flex items-center gap-2">
    <button
      type="button"
      class="flex items-center gap-1.5 text-textPrimary hover:text-accent transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 rounded"
      aria-expanded={open}
      onclick={() => onToggle?.()}
      data-testid="fleet-pulse-toggle"
    >
      {#if open}<ChevronDown size={12} />{:else}<ChevronRight size={12} />{/if}
      <Activity size={12} class="text-accent" />
      <span class="text-[11px] font-semibold tracking-wide">Fleet Pulse</span>
    </button>

    {#if pulse.counted > 0}
      <!-- A capped walk read the newest commits and stopped, so every number
           here is a floor. Same mark, same meaning, as the grid's cells: the
           `.gp-floor` underline spans the reading it qualifies. -->
      <span class="text-[10px] text-textMuted tabular-nums {pulse.partial ? 'gp-floor' : ''}">
        {pulse.commits.toLocaleString()}
        {pulse.commits === 1 ? "commit" : "commits"} · last {pulse.windowDays}d
      </span>
      {#if pulse.partial}
        <span
          class="text-amber-600 dark:text-amber-400 text-[10px] font-semibold"
          title="At least one repository's history was capped, so these counts are floors."
          data-testid="fleet-pulse-partial">≥</span
        >
      {/if}
    {/if}

    <span class="flex-1"></span>

    <!-- The window control. Every count on this panel is meaningless without
         the span it was taken over, so the span is a control rather than a
         caption, and changing it re-reads rather than relabels. -->
    <div
      class="flex items-center gap-0.5 shrink-0"
      role="group"
      aria-label="Commit window"
      data-testid="fleet-window"
    >
      {#each COMMIT_WINDOWS as days (days)}
        <button
          type="button"
          class="px-1.5 py-0.5 rounded text-[10px] tabular-nums transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-50 {days ===
          windowDays
            ? 'bg-accent/15 text-accent font-semibold'
            : 'text-textMuted hover:text-textPrimary hover:bg-surfaceHover'}"
          aria-pressed={days === windowDays}
          disabled={loading}
          onclick={() => onWindow?.(days)}
          title="Read commit history over the last {days} days. Costs no extra git — the walk is bounded by commit count, not by date."
        >
          {days}d
        </button>
      {/each}
    </div>

    {#if coverage}
      <span class="text-[10px] text-amber-600 dark:text-amber-400 truncate" title={coverage}>
        {coverage}
      </span>
    {/if}
  </div>

  {#if open}
    {#if pulse.counted === 0}
      <p class="mt-2 text-[11px] text-textMuted" data-testid="fleet-pulse-empty">
        <!-- Never a flat chart here. A baseline drawn over nothing is
             indistinguishable from a workspace that is genuinely quiet. -->
        {coverage || "No commit history has been read yet."}
      </p>
    {:else}
      <div class="mt-2 grid gap-3 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
        <!-- The chart, and the one sentence that qualifies it. -->
        <div class="gp-card rounded-lg px-3 py-2 flex flex-col gap-1.5 min-w-0">
          <div class="flex items-baseline gap-2 flex-wrap">
            <span class="text-[10px] uppercase tracking-wider text-textMuted">Commit activity</span>
            <span class="inline-flex items-center gap-1 text-[10px] {trendTone}">
              {#if pulse.trend.direction === "down"}
                <TrendingDown size={10} />
              {:else if pulse.trend.direction !== "flat"}
                <TrendingUp size={10} />
              {/if}
              {trendLabel}
            </span>
          </div>
          <FleetSparkline
            counts={pulse.daily}
            windowDays={pulse.windowDays}
            total={pulse.commits}
            partial={pulse.partial}
            height={28}
            barWidth={3}
            label="Fleet commits"
          />
          <div class="flex items-center gap-3 text-[10px] text-textMuted tabular-nums flex-wrap">
            <span>{pulse.activeDays} of {pulse.windowDays} days active</span>
            <span class="inline-flex items-center gap-1" title="Authors summed per repository — someone working in three repositories counts three times, because the sweep reports counts, never identities.">
              <Users size={10} />{pulse.authorSlots} author {pulse.authorSlots === 1 ? "slot" : "slots"}
            </span>
            <span>counted across {pulse.counted} of {pulse.eligible}</span>
          </div>
        </div>

        <!-- Who is moving, and who has stopped. -->
        <div class="gp-card rounded-lg px-3 py-2 flex flex-col gap-1.5 min-w-0">
          <span class="text-[10px] uppercase tracking-wider text-textMuted">Movers</span>
          {#if pulse.busiest.length > 0}
            <ul class="flex flex-col gap-0.5">
              {#each pulse.busiest as repo (repo.path)}
                <li>
                  <button
                    type="button"
                    class="w-full flex items-center gap-1.5 text-[11px] text-left rounded px-1 py-0.5 hover:bg-surfaceHover focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
                    onclick={() => onSelect?.(repo.path)}
                    title="{repo.path} — {repo.commits} commits in the window"
                  >
                    <Flame size={10} class="text-amber-500 shrink-0" />
                    <span class="truncate flex-1 text-textPrimary">{repo.label}</span>
                    <span class="tabular-nums text-textMuted shrink-0">{repo.recent}/{TREND_DAYS}d</span>
                  </button>
                </li>
              {/each}
            </ul>
          {:else}
            <p class="text-[11px] text-textMuted">Nothing committed in the window.</p>
          {/if}

          {#if pulse.dormant.length > 0}
            <!-- The reason anyone reads this list is to decide whether a
                 project is finished, parked, or forgotten — so the decision is
                 offered here rather than requiring a hunt down the grid. -->
            <div class="pt-1 border-t border-border/50 mt-0.5 flex flex-col gap-0.5">
              <span class="inline-flex items-center gap-1 text-[10px] text-textMuted">
                <Moon size={10} class="shrink-0" />
                Quiet for {pulse.windowDays}d
              </span>
              <ul class="flex flex-col gap-0.5">
                {#each pulse.dormant as repo (repo.path)}
                  <li class="flex items-center gap-1">
                    <button
                      type="button"
                      class="flex-1 min-w-0 text-left text-[11px] truncate text-textPrimary rounded px-1 py-0.5 hover:bg-surfaceHover focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
                      onclick={() => onSelect?.(repo.path)}
                      title="{repo.path} — no commits in the window. Click to find its row."
                      >{repo.label}</button
                    >
                    <button
                      type="button"
                      class="p-0.5 rounded text-textMuted/60 hover:text-rose-400 hover:bg-surfaceHover transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
                      title="Remove {repo.label} from Fleet (closes the repository)"
                      aria-label="Remove {repo.label} from Fleet"
                      data-testid="fleet-dormant-remove"
                      onclick={() => onRemove?.(repo.path)}
                    >
                      <Trash2 size={10} />
                    </button>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}
        </div>
      </div>

      <!-- The fleet's language mix, drawn only from repositories that have one. -->
      {#if languageStats.length > 0}
        <div class="mt-2 gp-card rounded-lg px-3 py-2 flex flex-col gap-1.5">
          <div class="flex items-baseline gap-2 flex-wrap">
            <span class="text-[10px] uppercase tracking-wider text-textMuted">Fleet languages</span>
            <span
              class="text-[10px] text-textMuted tabular-nums {languages.partial ? 'gp-floor' : ''}"
            >
              {languages.totalLines.toLocaleString()} lines across {languages.counted} of {languages.eligible}
            </span>
            {#if languages.partial}
              <span
                class="text-amber-600 dark:text-amber-400 text-[10px] font-semibold"
                title="At least one language scan stopped at a budget, so this mix is drawn from floors."
                data-testid="fleet-languages-partial">≥</span
              >
            {/if}
            {#if languages.withoutBreakdown > 0}
              <span
                class="text-[10px] text-textMuted"
                title="These repositories have a line count on file but no language breakdown — rescan them to include their mix here."
                >{languages.withoutBreakdown} without a breakdown</span
              >
            {/if}
          </div>
          <FleetLanguageBar stats={languageStats} height={8} label="Fleet language mix" />
          <div class="flex flex-wrap gap-x-3 gap-y-0.5 text-[10px]">
            {#each languageStats as stat (stat.language)}
              <span class="inline-flex items-center gap-1">
                <span
                  class="h-2 w-2 rounded-full shrink-0"
                  style="background-color: {stat.color_hex}"
                ></span>
                <span class="text-textPrimary/90">{stat.language}</span>
                <span class="text-textMuted tabular-nums">{stat.percentage.toFixed(1)}%</span>
              </span>
            {/each}
          </div>
        </div>
      {:else if languages.eligible > 0}
        <p class="mt-2 text-[10px] text-textMuted" data-testid="fleet-languages-empty">
          No language breakdown recorded yet — run the Lines of code scan to fill it in.
        </p>
      {/if}
    {/if}
  {/if}
</section>
