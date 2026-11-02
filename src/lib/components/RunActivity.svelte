<script lang="ts">
  /**
   * What one agent run did and how its time was spent, read from the ledger
   * rows it left (`ledger/runTimeline.ts`): the commands, tests and edits with
   * how each ended and how long it took, the state bar (working, waiting on a
   * tool, waiting for you, idle, ended), and the tokens it used.
   *
   * Read when the reader opens it, and again while it stays open if the run
   * moves on. A span nobody observed is drawn as "Not observed" and never as
   * idle; a command whose result was never read has no outcome and no
   * duration; a read that filled its row limit says it was capped.
   */
  import { untrack } from "svelte";
  import { barWidthPct, formatDuration } from "../delivery/timeline";
  import { formatRelativeTime } from "../format";
  import { readRunRows, runWindowOf, RUN_ACTIVITY_ROWS } from "../ledger/runActivity";
  import {
    SEGMENT_LABELS,
    formatTokens,
    projectRunTimeline,
    segmentShares,
    segmentTitle,
    spentSummary,
    type RunTimeline,
  } from "../ledger/runTimeline";
  import { explainError, type TaskRun } from "../workbench/client";

  let { run, now, active = true }: { run: TaskRun; now: number; active?: boolean } = $props();

  /** Items drawn before the reader asks for the rest. */
  const PREVIEW_ITEMS = 12;

  let open = $state(false);
  let showAll = $state(false);
  let loading = $state(false);
  let error = $state("");
  let timeline = $state<RunTimeline | null>(null);
  /** When the rows were read, so "last activity" and a live span are measured from a stated instant. */
  let readAt = $state(0);
  let ticket = 0;

  async function load(): Promise<void> {
    const mine = ++ticket;
    loading = true;
    try {
      const rows = await readRunRows(run);
      if (mine !== ticket) return;
      const instant = Date.now();
      timeline = projectRunTimeline(rows, runWindowOf(run), instant, RUN_ACTIVITY_ROWS);
      readAt = instant;
      error = "";
    } catch (cause) {
      if (mine !== ticket) return;
      error = explainError(cause);
    } finally {
      if (mine === ticket) loading = false;
    }
  }

  // Opened, or the run moved on while it is open: read again.
  $effect(() => {
    const revision = run.revision;
    const state = run.state;
    void revision;
    void state;
    if (open && active) untrack(() => void load());
  });

  const shares = $derived(timeline ? segmentShares(timeline.segments) : []);
  const longest = $derived(
    timeline
      ? timeline.items.reduce<number | null>(
          (best, item) => (item.durationMs !== null && (best === null || item.durationMs > best) ? item.durationMs : best),
          null,
        )
      : null,
  );
  const shown = $derived(timeline ? (showAll ? timeline.items : timeline.items.slice(-PREVIEW_ITEMS)) : []);
  const summary = $derived(timeline ? spentSummary(timeline.spentMs) : "");
</script>

<details class="activity" data-testid="agent-activity" ontoggle={(event) => { open = (event.currentTarget as HTMLDetailsElement).open; }}>
  <summary>
    Activity{#if timeline && summary} · <span class="spent" data-testid="agent-activity-summary">{summary}</span>{/if}
  </summary>
  {#if loading && !timeline}<p class="meta" role="status">Reading this attempt's activity…</p>{/if}
  {#if error}<p class="problem" role="alert" data-testid="agent-activity-error">Activity could not be read: {error}</p>{/if}
  {#if timeline}
    {#if timeline.segments.length}
      <div class="bar" role="img" aria-label={summary || "No time observed"} data-testid="agent-activity-bar">
        {#each timeline.segments.filter((s) => s.durationMs !== null && s.state !== "ended") as segment, index (index)}
          {@const share = shares[index]?.pct ?? 0}
          <span style:width="{share}%" data-state={segment.state} data-bound={segment.bound} title={segmentTitle(segment)} data-testid="agent-activity-segment"></span>
        {/each}
      </div>
      <ul class="legend" aria-label="Time by state">
        {#each Object.entries(timeline.spentMs) as [state, ms] (state)}
          <li data-state={state}><span class="swatch" aria-hidden="true"></span>{SEGMENT_LABELS[state as keyof typeof SEGMENT_LABELS]} {formatDuration(ms ?? null)}</li>
        {/each}
      </ul>
      {#if timeline.segments.some((s) => s.durationMs === null && s.state !== "ended")}<p class="meta" data-testid="agent-activity-unbounded">How long the last stretch lasted was not observed, so it is not drawn.</p>{/if}
      {#if timeline.segments.some((s) => s.bound === "now")}<p class="meta">The newest span is still open and counted up to the last read.</p>{/if}
    {:else}
      <p class="meta" data-testid="agent-activity-no-lifecycle">No lifecycle events were recorded for this attempt, so how its time was spent is not known.</p>
    {/if}

    <p class="meta" data-testid="agent-activity-tokens">
      {#if timeline.tokens}
        {formatTokens(timeline.tokens.input)} in · {formatTokens(timeline.tokens.output)} out · {formatTokens(timeline.tokens.cacheRead + timeline.tokens.cacheCreation)} cached · {timeline.tokens.responses} {timeline.tokens.responses === 1 ? "response" : "responses"}
      {:else}
        Tokens were not recorded.
      {/if}
      {#if timeline.lastActivityMs !== null} · last activity {formatRelativeTime(Math.floor(timeline.lastActivityMs / 1000), Math.floor((readAt || now) / 1000))}{/if}
    </p>

    {#if timeline.capped}<p class="meta" data-testid="agent-activity-capped">Showing the newest {RUN_ACTIVITY_ROWS} rows; earlier activity is not shown, so the totals above cover only what is.</p>{/if}

    {#if timeline.items.length}
      <ul class="items" aria-label="What the agent did" data-testid="agent-activity-items">
        {#each shown as item (item.id)}
          {@const width = barWidthPct(item.durationMs, longest)}
          <li data-kind={item.kind} data-outcome={item.outcome} data-testid="agent-activity-item">
            <span class="kind">{item.kind}</span>
            <span class="label" title={item.label}>{item.label}</span>
            {#if item.subagentId}<span class="sub" title="Done by a subagent">subagent {item.subagentId.slice(0, 7)}</span>{/if}
            <span class="outcome">{item.outcome === "failed" ? "failed" : item.outcome === "unobserved" ? "no result seen" : "ok"}</span>
            <span class="took" data-testid="agent-activity-duration">{formatDuration(item.durationMs)}</span>
            {#if width !== null}<span class="track" aria-hidden="true"><span style:width="{width}%"></span></span>{/if}
          </li>
        {/each}
      </ul>
      {#if timeline.items.length > PREVIEW_ITEMS}
        <button class="gp-btn" type="button" onclick={() => { showAll = !showAll; }}>{showAll ? "Show fewer" : `Show all ${timeline.items.length}`}</button>
      {/if}
      {#if timeline.failed || timeline.unobserved}
        <p class="meta" data-testid="agent-activity-counts">{timeline.failed} failed · {timeline.unobserved} with no result seen</p>
      {/if}
    {:else}
      <p class="meta">No edits or commands were recorded for this attempt yet.</p>
    {/if}
  {/if}
</details>

<style>
  .activity{margin-top:8px;font-size:11.5px}
  summary{cursor:pointer;font-weight:600;color:rgb(var(--c-text))}
  summary:focus-visible{outline:2px solid rgb(var(--c-accent)/.6);outline-offset:2px}
  .spent{font-weight:450;color:rgb(var(--c-text-muted))}
  .meta{color:rgb(var(--c-text-muted));font-size:11px;margin:6px 0 0}
  .problem{color:#dc6565;margin:6px 0 0}
  .bar{display:flex;height:8px;margin-top:8px;border-radius:4px;overflow:hidden;background:rgb(var(--c-text-muted)/.12)}
  .bar span,.swatch{background:rgb(var(--c-text-muted)/.5)}
  .bar [data-state="working"],li[data-state="working"] .swatch{background:#3fb950}
  .bar [data-state="waiting_tool"],li[data-state="waiting_tool"] .swatch{background:rgb(var(--c-accent))}
  .bar [data-state="waiting_you"],li[data-state="waiting_you"] .swatch{background:#d29922}
  .bar [data-state="idle"],li[data-state="idle"] .swatch{background:rgb(var(--c-text-muted)/.55)}
  .bar [data-state="unknown"],li[data-state="unknown"] .swatch{background:repeating-linear-gradient(45deg,rgb(var(--c-text-muted)/.25) 0 3px,transparent 3px 6px)}
  .legend{list-style:none;margin:6px 0 0;padding:0;display:flex;flex-wrap:wrap;gap:4px 12px;color:rgb(var(--c-text-muted))}
  .legend li{display:flex;align-items:center;gap:5px}
  .swatch{width:8px;height:8px;border-radius:2px;display:inline-block}
  .items{list-style:none;margin:8px 0 0;padding:0;display:flex;flex-direction:column;gap:3px}
  .items li{display:grid;grid-template-columns:auto minmax(0,1fr) auto auto;align-items:baseline;gap:2px 8px;min-width:0}
  .kind{font-size:10px;text-transform:uppercase;letter-spacing:.04em;color:rgb(var(--c-text-muted))}
  .label{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
  .sub{font-size:10px;color:rgb(var(--c-text-muted))}
  .outcome{font-size:10.5px;color:rgb(var(--c-text-muted))}
  li[data-outcome="failed"] .outcome,li[data-outcome="failed"] .label{color:#dc6565}
  .took{font-variant-numeric:tabular-nums;color:rgb(var(--c-text-muted));text-align:right}
  .track{grid-column:2 / -1;height:2px;background:rgb(var(--c-text-muted)/.12);border-radius:1px;overflow:hidden}
  .track span{display:block;height:100%;background:rgb(var(--c-accent)/.7)}
  button{margin-top:6px}
</style>
