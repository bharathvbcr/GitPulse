<script lang="ts">
  import { onMount } from "svelte";
  import HealthSection from "./HealthSection.svelte";
  import { createListenerTracker } from "../../dom/listenerTracker";
  import { formatError } from "../../ui/formatError";
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

  onMount(() => {
    const listeners = createListenerTracker();
    // Live events that arrive while the first read is in flight must survive it.
    void watchTerminalIncidents((incident) => {
      incidents = mergeIncident(incidents, incident);
    })
      .then((stop) => listeners.track(stop))
      .catch((cause) => {
        loadError = `Live updates unavailable: ${formatError(cause)}`;
      });
    loadTerminalIncidents()
      .then((rows) => {
        incidents = rows.reduce((merged, row) => mergeIncident(merged, row), incidents);
      })
      .catch((cause) => {
        loadError = `Could not read terminal incidents: ${formatError(cause)}`;
      })
      .finally(() => {
        loaded = true;
      });
    return () => listeners.dispose();
  });
</script>

<HealthSection id="terminal-incidents">
  {#if loadError}
    <p role="alert" class="text-[11px] text-amber-300">{loadError}</p>
  {/if}
  {#if incidents.length > 0}
    <ul class="space-y-1.5">
      {#each incidents as incident (incident.id)}
        <li class="rounded-xl border border-border/70 bg-surface px-3 py-2 text-[11px]">
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
            <span class="font-mono {incident.sender ? 'text-textPrimary' : 'text-amber-300'}"
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
    <p class="text-[11px] text-textMuted">
      No terminal sessions have been killed from outside since GitPulse started.
    </p>
  {/if}
</HealthSection>
