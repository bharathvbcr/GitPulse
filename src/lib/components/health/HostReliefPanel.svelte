<script lang="ts">
  import {
    pausedRows,
    reliefBindingLabel,
    reliefHeadline,
    reliefIsCalm,
    reliefModeLabel,
    reliefNowLabel,
    reliefSummary,
    type ReliefOutcome,
    type ReliefStatus,
  } from "../../health/hostRelief";

  /**
   * What the memory-pressure supervisor is doing: its mode, how far it has
   * gone, and each build or test it paused, with a way to resume them now.
   *
   * A status that is stale or could not be read is said so in words and never
   * rendered as "nothing is paused": the supervisor that would have stopped a
   * process may be gone with the process still stopped.
   *
   * Presentation only; `HostReliefSection` reads the status and sends the
   * resume request.
   */
  let {
    outcome,
    now = Date.now(),
    requesting = false,
    requested = false,
    error = "",
    onresume,
  }: {
    outcome: ReliefOutcome | null;
    now?: number;
    requesting?: boolean;
    requested?: boolean;
    error?: string;
    onresume?: () => void;
  } = $props();

  const status = $derived<ReliefStatus | null>(
    outcome && (outcome.state === "fresh" || outcome.state === "stale") ? outcome.status : null,
  );
  const rows = $derived(status ? pausedRows(status, now) : []);
  const summary = $derived(status ? reliefSummary(status) : null);
  const calm = $derived(reliefIsCalm(outcome));
  // A status that could not be verified is the one where the person most needs
  // the button: the request file can be written whatever the status says.
  const unverified = $derived(outcome?.state === "stale" || outcome?.state === "unreadable");
  const canResume = $derived(rows.length > 0 || outcome?.state === "unreadable");
</script>

<div class="mt-4 space-y-2 text-meta" data-testid="host-relief" data-state={outcome?.state ?? "loading"}>
  <h4 class="gp-label">Memory protection</h4>
  <div
    class="gp-notice"
    data-tone={calm ? "ok" : "unknown"}
    role="status"
    data-testid="host-relief-headline"
  >
    <div class="min-w-0 flex-1">{reliefHeadline(outcome)}</div>
  </div>

  {#if status}
    <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5" data-testid="host-relief-summary">
      <dt class="text-textMuted">Mode</dt>
      <dd data-testid="host-relief-mode">{reliefModeLabel(status.mode)}</dd>
      <dt class="text-textMuted">Now</dt>
      <dd data-testid="host-relief-rung">{reliefNowLabel(status)}</dd>
      {#if status.rung !== "none" || (summary && summary.wouldPause.length + summary.wouldSlow > 0)}
        <dt class="text-textMuted">Because</dt>
        <dd data-testid="host-relief-binding">{reliefBindingLabel(status.binding)}</dd>
      {/if}
      {#if status.slowed > 0}
        <dt class="text-textMuted">Slowed</dt>
        <dd data-testid="host-relief-slowed">{status.slowed} agent process(es) at background priority</dd>
      {/if}
      {#if status.cap_hits > 0}
        <dt class="text-textMuted">10-minute limit</dt>
        <dd data-testid="host-relief-cap-hits">
          reached {status.cap_hits} time(s): the process was resumed while memory was still tight
        </dd>
      {/if}
    </dl>

    {#if summary && summary.wouldPause.length > 0}
      <ul class="space-y-1.5" data-testid="host-relief-would-pause">
        {#each summary.wouldPause as tree (`${tree.pid}:${tree.start_us}`)}
          <li class="rounded-xl border border-edge bg-surface px-3 py-2 text-textMuted">
            Would pause <span class="font-mono text-textPrimary break-all">{tree.command}</span>
          </li>
        {/each}
      </ul>
    {/if}

    {#if rows.length > 0}
      <ul class="space-y-1.5" data-testid="host-relief-paused">
        {#each rows as row (row.key)}
          <li class="rounded-xl border border-edge bg-surface px-3 py-2" data-testid="host-relief-paused-row">
            <div class="font-mono text-textPrimary break-all" data-testid="host-relief-command">{row.command}</div>
            <div class="text-textMuted">
              <span title={row.repoPath} data-testid="host-relief-repo">{row.repo}</span>
              · <span data-testid="host-relief-agent">{row.agent}</span>
              · <span data-testid="host-relief-footprint">{row.footprint}</span>
              · paused <span data-testid="host-relief-since">{row.since}</span>
            </div>
          </li>
        {/each}
      </ul>
    {/if}

    {#if status.last_action}
      <p class="text-textMuted" data-testid="host-relief-last-action">
        Last action: {status.last_action.kind.replace("_", " ")}
        {#if status.last_action.detail}: {status.last_action.detail}{/if}
      </p>
    {/if}

    {#if status.failures.length > 0}
      <div data-testid="host-relief-failures">
        <p class="text-warn">Couldn't pause or resume {status.failures.length} process(es):</p>
        <ul class="list-disc pl-5 text-textMuted">
          {#each status.failures as failure (`${failure.at_ms}:${failure.pid}`)}
            <li>pid {failure.pid}: {failure.what}</li>
          {/each}
        </ul>
      </div>
    {/if}
  {/if}

  {#if canResume}
    <div class="flex items-center gap-2">
      <button
        type="button"
        class="gp-btn text-meta px-2 py-0.5"
        data-testid="host-relief-resume"
        disabled={requesting}
        onclick={() => onresume?.()}
      >
        Resume all
      </button>
      {#if requested && unverified}
        <span class="text-warn" data-testid="host-relief-requested-stale">
          Requested, but the memory protection status couldn't be verified. If the supervisor is not running,
          nothing will act on this and a paused process stays paused until it is resumed by hand
          (kill -CONT with its pid).
        </span>
      {:else if requested}
        <span class="text-textMuted" data-testid="host-relief-requested">
          Requested. The supervisor resumes them on its next check.
        </span>
      {/if}
    </div>
  {/if}

  {#if error}
    <p class="text-warn" role="alert" data-testid="host-relief-error">{error}</p>
  {/if}
</div>
