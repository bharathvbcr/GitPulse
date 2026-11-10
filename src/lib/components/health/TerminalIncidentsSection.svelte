<script lang="ts">
  import { onMount } from "svelte";
  import HealthSection from "./HealthSection.svelte";
  import { createListenerTracker } from "../../dom/listenerTracker";
  import { ShieldCheck } from "@lucide/svelte";
  import EmptyState from "../EmptyState.svelte";
  import { describeFailure } from "../../ui/copy/describeFailure";
  import {
    describeSender,
    loadTerminalIncidents,
    mergeIncident,
    watchTerminalIncidents,
  } from "../../health/terminalIncidents";
  import type { TerminalIncident } from "../../terminal/incident";

  /**
   * Terminal sessions that died together from a signal GitPulse did not send.
   *
   * One row per incident, with the sender when the macOS log named one and the
   * reason when it did not. An empty list is a statement about this run only:
   * incidents are kept in memory, not across a restart.
   */
  let incidents = $state<TerminalIncident[]>([]);
  let loadError = $state<string | null>(null);
  let loaded = $state(false);

  function failureLine(verb: string, object: string, cause: unknown): string {
    const failure = describeFailure({ verb, object }, cause);
    return failure.detail ? `${failure.headline} ${failure.detail}` : failure.headline;
  }

  onMount(() => {
    const listeners = createListenerTracker();
    // Live events that arrive while the first read is in flight must survive it.
    void watchTerminalIncidents((incident) => {
      incidents = mergeIncident(incidents, incident);
    })
      .then((stop) => listeners.track(stop))
      .catch((cause) => {
        loadError = failureLine("watch", "terminal incidents", cause);
      });
    loadTerminalIncidents()
      .then((rows) => {
        incidents = rows.reduce((merged, row) => mergeIncident(merged, row), incidents);
      })
      .catch((cause) => {
        loadError = failureLine("read", "terminal incidents", cause);
      })
      .finally(() => {
        loaded = true;
      });
    return () => listeners.dispose();
  });
</script>

<HealthSection id="terminal-incidents">
  {#if loadError}
    <div role="alert" class="gp-notice" data-tone="unknown">
      <div class="min-w-0 flex-1">{loadError}</div>
    </div>
  {/if}
  {#if incidents.length > 0}
    <ul class="space-y-1.5">
      {#each incidents as incident (incident.id)}
        <li class="rounded-xl border border-edge bg-surface px-3 py-2 text-meta">
          <div class="flex items-baseline justify-between gap-3 flex-wrap">
            <span class="font-medium text-textPrimary">
              {incident.session_ids.length} sessions ended by
              <span class="font-mono">{incident.signal}</span>
            </span>
            <time class="font-mono text-textMuted" datetime={new Date(incident.at_ms).toISOString()}
              >{new Date(incident.at_ms).toLocaleString()}</time
            >
          </div>
          <div class="mt-0.5 text-textSecondary">
            Sender:
            <span class="font-mono {incident.sender ? 'text-textPrimary' : 'text-warn'}"
              >{describeSender(incident)}</span
            >
          </div>
          {#if incident.sender}
            <p class="mt-0.5 text-textMuted break-words">{incident.sender_note}</p>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if loaded && !loadError}
    <EmptyState
      icon={ShieldCheck}
      title="No terminal sessions ended from outside"
      hint="Incidents are kept in memory, so this covers the time since GitPulse started."
      compact
    />
  {/if}
</HealthSection>
