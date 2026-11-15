<script lang="ts">
  /**
   * Build slots and idle sessions in Health (`docs/LOAD_GOVERNOR.md`, card 1d).
   *
   * Reads the lease directory and the activity classifier while it is on
   * screen. Nothing here writes except Park, which is one click here and then a
   * Yes in a system dialog this page cannot answer; the backend checks the
   * session again before and after that dialog.
   */
  import { onDestroy, onMount } from "svelte";
  import { keyedList } from "../../ui/eachKeys";
  import HealthTable from "./HealthTable.svelte";
  import {
    loadIdleReport,
    loadLeaseStatus,
    parkSession,
    type IdleReport,
    type LeaseStatus,
  } from "../../stores/hostLoadStore";
  import { bytes } from "../../health/hostLoad";
  import {
    activityLine,
    capacityLine,
    footprintOf,
    holderCells,
    missesLine,
    parkOffered,
    unleasedLine,
    waiterCells,
  } from "../../health/leases";
  import { describeFailure, type FailureCopy } from "../../ui/copy/describeFailure";
  import { createAdaptiveTimer } from "../../runtime/adaptiveTimer";

  const REFRESH_MS = 4_000;
  const HOLDER_COLUMNS = Object.freeze([
    { label: "Slot" },
    { label: "Build" },
    { label: "Session" },
    { label: "Held for" },
  ]);
  const WAITER_COLUMNS = Object.freeze([
    { label: "Place" },
    { label: "Build" },
    { label: "Session" },
    { label: "Waiting" },
  ]);
  const IDLE_COLUMNS = Object.freeze([
    { label: "Agent" },
    { label: "Directory" },
    { label: "Memory" },
    { label: "State" },
    { label: "Action", hideLabel: true, width: "w-24" },
  ]);

  let leases = $state<LeaseStatus | null>(null);
  let idle = $state<IdleReport | null>(null);
  let error = $state<FailureCopy | null>(null);
  let parking = $state<string | null>(null);
  let outcome = $state("");
  let stopRefresh: (() => void) | null = null;

  async function refresh() {
    try {
      [leases, idle] = await Promise.all([loadLeaseStatus(), loadIdleReport()]);
      error = null;
    } catch (cause) {
      error = describeFailure({ verb: "read", object: "build slots" }, cause);
    }
  }

  onMount(() => {
    void refresh();
    stopRefresh = createAdaptiveTimer(() => void refresh(), REFRESH_MS);
  });
  onDestroy(() => {
    stopRefresh?.();
  });

  async function park(terminalSession: string) {
    parking = terminalSession;
    outcome = "";
    try {
      outcome = await parkSession(terminalSession);
    } catch (cause) {
      outcome = cause instanceof Error ? cause.message : String(cause);
    } finally {
      parking = null;
      void refresh();
    }
  }
</script>

<div class="space-y-2" data-testid="host-load-leases">
  {#if error}
    <p class="text-warn">{error.headline}{#if error.detail} {error.detail}{/if}</p>
  {/if}

  {#if leases}
    <h4 class="gp-label">Build slots</h4>
    <p class="{leases.capacity.refuses ? 'text-warn' : 'text-textMuted'}" data-testid="host-load-lease-capacity">
      {capacityLine(leases)}
    </p>
    {#if leases.error}
      <p class="text-warn" data-testid="host-load-lease-error">
        The slot directory couldn't be read: {leases.error}. Builds are not being counted.
      </p>
    {/if}
    {#each leases.unreadable as why (why)}
      <p class="text-warn">{why}</p>
    {/each}
    {#if leases.links_missing}
      <p class="text-warn" data-testid="host-load-lease-links">
        The cargo and go wrappers are not in place: {leases.links_missing}
      </p>
    {/if}

    {#if leases.holders.length > 0}
      <HealthTable caption="Builds holding a slot" columns={HOLDER_COLUMNS}>
        {#each leases.holders as holder (holder.slot)}
          <tr>
            {#each holderCells(holder, leases.now_mono_ns) as cell, at (at)}
              <td class="px-3 py-1.5 break-all {at === 0 ? 'text-textPrimary' : 'text-textMuted'}">{cell}</td>
            {/each}
          </tr>
        {/each}
      </HealthTable>
    {/if}
    {#if leases.queue.length > 0}
      <HealthTable caption="Builds waiting for a slot" columns={WAITER_COLUMNS}>
        {#each keyedList(leases.queue, (w) => w.name) as { item: waiter, key }, place (key)}
          <tr>
            {#each waiterCells(waiter, place, leases.now_mono_ns) as cell, at (at)}
              <td class="px-3 py-1.5 break-all {at === 0 ? 'text-textPrimary' : 'text-textMuted'}">{cell}</td>
            {/each}
          </tr>
        {/each}
      </HealthTable>
    {/if}

    <p class="{leases.misses === null || leases.misses > 0 ? 'text-warn' : 'text-textMuted'}" data-testid="host-load-lease-misses">
      {missesLine(leases)}
    </p>
    <p class="{leases.unleased_running === null || leases.unleased_running > 0 ? 'text-warn' : 'text-textMuted'}" data-testid="host-load-lease-unleased">
      {unleasedLine(leases)}
    </p>
  {/if}

  {#if idle}
    <h4 class="gp-label">Idle sessions</h4>
    {#if idle.error}
      <p class="text-warn" data-testid="host-load-idle-error">Sessions couldn't be classified: {idle.error}.</p>
    {:else}
      <p class="text-textMuted" data-testid="host-load-idle-summary">
        {idle.idle} idle for {Math.round(idle.idle_after_s / 60)} min or more, {idle.unknown} not known either way.
        A session is idle only when it reported that it is waiting, its processes are nearly still and its terminal
        is quiet; anything GitPulse couldn't read counts as not known.
      </p>
    {/if}
    {#if idle.sessions.length > 0}
      <HealthTable caption="Agent sessions and whether they are idle" columns={IDLE_COLUMNS}>
        {#each idle.sessions as session (`${session.pid}:${session.start_us}`)}
          <tr>
            <td class="px-3 py-1.5 text-textPrimary">{session.kind}</td>
            <td class="px-3 py-1.5 font-mono text-textMuted break-all">{session.cwd ?? "unknown"}</td>
            <td class="px-3 py-1.5 text-textMuted">{footprintOf(session, bytes)}</td>
            <td class="px-3 py-1.5 text-textMuted">
              {activityLine(session)}
              {#if session.activity.state === "idle" && session.park_blocked}
                <span class="block">Cannot be parked: {session.park_blocked}.</span>
              {/if}
            </td>
            <td class="px-3 py-1.5">
              {#if parkOffered(session, idle)}
                <button
                  type="button"
                  class="gp-btn"
                  disabled={parking !== null}
                  onclick={() => session.terminal_session && void park(session.terminal_session)}
                  data-testid="host-load-park"
                >
                  {parking === session.terminal_session ? "Parking" : "Park"}
                </button>
              {/if}
            </td>
          </tr>
        {/each}
      </HealthTable>
      <p class="text-textMuted">
        Park is offered once a session has been idle for {Math.round(idle.park_after_s / 60)} min. It closes the
        terminal after you confirm in a system dialog, and keeps the conversation so Resume can reopen it. Nothing
        is parked on its own.
      </p>
    {/if}
    {#if outcome}
      <p class="text-textMuted" data-testid="host-load-park-outcome">{outcome}</p>
    {/if}
  {/if}
</div>
