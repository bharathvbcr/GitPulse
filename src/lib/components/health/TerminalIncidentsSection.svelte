<script lang="ts">
  import { onMount } from "svelte";
  import HealthSection from "./HealthSection.svelte";
  import { createListenerTracker } from "../../dom/listenerTracker";
  import { ShieldCheck, RotateCw } from "@lucide/svelte";
  import EmptyState from "../EmptyState.svelte";
  import ListPreviewToggle from "../ListPreviewToggle.svelte";
  import { LIST_PREVIEW_ROWS, previewSlice } from "../../ui/previewList";
  import { describeFailure } from "../../ui/copy/describeFailure";
  import {
    describeEndState,
    describeSender,
    dismissSessionLedgerRow,
    loadTerminalIncidents,
    mergeIncident,
    reopenAllSessionLedgerOffers,
    reopenBannerHeading,
    reopenSessionLedgerOffer,
    reopenableOffers,
    sessionLedgerStore,
    subscribeSessionLedger,
    watchTerminalIncidents,
    type SessionLedgerOffer,
  } from "../../health/terminalIncidents";
  import { launcherLabel, type LauncherKind } from "../../terminal/tabs";
  import type { TerminalIncident } from "../../terminal/incident";

  /**
   * Terminal sessions that died together from a signal GitPulse did not send,
   * plus durable recovery offers persisted across quit, crash, or reboot.
   */
  let incidents = $state<TerminalIncident[]>([]);
  let loadError = $state<string | null>(null);
  let loaded = $state(false);
  let expanded = $state(false);
  let ledgerExpanded = $state(false);
  let reopeningRowId = $state<string | null>(null);
  let reopeningAll = $state(false);
  let ledgerRowErrors = $state<Record<string, string>>({});

  const ledgerOffers = $derived($sessionLedgerStore.offers);
  const ledgerReopenable = $derived(reopenableOffers(ledgerOffers));

  function failureLine(verb: string, object: string, cause: unknown): string {
    const failure = describeFailure({ verb, object }, cause);
    return failure.detail ? `${failure.headline} ${failure.detail}` : failure.headline;
  }

  async function handleReopen(offer: SessionLedgerOffer) {
    reopeningRowId = offer.row_id;
    try {
      const outcome = await reopenSessionLedgerOffer(offer);
      if (!outcome.reopened) {
        ledgerRowErrors = { ...ledgerRowErrors, [offer.row_id]: outcome.error };
      } else if (offer.row_id in ledgerRowErrors) {
        const next = { ...ledgerRowErrors };
        delete next[offer.row_id];
        ledgerRowErrors = next;
      }
    } finally {
      reopeningRowId = null;
    }
  }

  async function handleReopenAll() {
    reopeningAll = true;
    try {
      const summary = await reopenAllSessionLedgerOffers(ledgerOffers);
      const nextErrors = { ...ledgerRowErrors };
      for (const item of summary.results) {
        if (!item.reopened) nextErrors[item.offer.row_id] = item.error;
        else delete nextErrors[item.offer.row_id];
      }
      ledgerRowErrors = nextErrors;
    } finally {
      reopeningAll = false;
    }
  }

  async function handleDismiss(rowId: string) {
    if (rowId in ledgerRowErrors) {
      const next = { ...ledgerRowErrors };
      delete next[rowId];
      ledgerRowErrors = next;
    }
    try {
      await dismissSessionLedgerRow(rowId);
    } catch (cause) {
      ledgerRowErrors = { ...ledgerRowErrors, [rowId]: failureLine("dismiss", "session offer", cause) };
    }
  }

  onMount(() => {
    const listeners = createListenerTracker();
    listeners.track(subscribeSessionLedger());
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
  {#if ledgerOffers.length > 0}
    <div class="mb-3 space-y-2" data-testid="health-session-ledger">
      <div class="flex items-center justify-between gap-2 flex-wrap">
        <span class="font-medium text-textPrimary text-meta" data-testid="health-session-ledger-heading">
          {reopenBannerHeading(ledgerReopenable.length)}
        </span>
        {#if ledgerReopenable.length > 1}
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="health-reopen-all-btn"
            disabled={reopeningAll}
            onclick={() => void handleReopenAll()}
          >
            <RotateCw size={12} /> {reopenBannerHeading(ledgerReopenable.length)}
          </button>
        {/if}
      </div>
      <ul class="space-y-1.5">
        {#each previewSlice(ledgerOffers, ledgerExpanded, LIST_PREVIEW_ROWS) as offer (offer.row_id)}
          {@const rowErr = ledgerRowErrors[offer.row_id] ?? offer.last_error ?? (offer.end_state !== "unreadable" && offer.launcher !== "shell" && !offer.resumable ? offer.reason : null)}
          <li class="rounded-xl border border-edge bg-surface px-3 py-2 text-meta" data-testid="health-ledger-row" data-end-state={offer.end_state}>
            <div class="flex items-start justify-between gap-2">
              <div class="min-w-0 flex-1 space-y-0.5">
                <div class="flex items-center gap-1.5 flex-wrap">
                  <span class="font-medium text-textPrimary">
                    {offer.end_state === "unreadable" ? "Corrupt ledger entry" : launcherLabel(offer.launcher as LauncherKind)}
                  </span>
                  <span class="gp-chip" data-tone={offer.end_state === "unreadable" ? "danger" : "neutral"}>
                    {describeEndState(offer.end_state, offer.signal)}
                  </span>
                  {#if offer.launcher === "shell"}
                    <span class="text-textMuted" data-testid="health-ledger-not-restorable">not restorable</span>
                  {:else if offer.end_state === "unreadable"}
                    <span class="text-danger" data-testid="health-ledger-unreadable">unreadable</span>
                  {/if}
                  {#if offer.mode}
                    <span class="font-mono text-textMuted">{offer.mode}</span>
                  {/if}
                </div>
                {#if offer.repo_path || offer.cwd}
                  <div class="font-mono text-micro text-textMuted truncate" title={offer.cwd || offer.repo_path}>
                    {offer.cwd || offer.repo_path}
                  </div>
                {/if}
                {#if rowErr}
                  <p class="text-danger text-micro break-words" role="alert" data-testid="health-ledger-row-error">{rowErr}</p>
                {/if}
              </div>
              <div class="flex items-center gap-1 shrink-0">
                {#if offer.resumable}
                  <button
                    type="button"
                    class="gp-btn"
                    data-size="sm"
                    data-testid="health-ledger-reopen-btn"
                    disabled={reopeningAll || reopeningRowId === offer.row_id}
                    onclick={() => void handleReopen(offer)}
                  >
                    <RotateCw size={12} /> Reopen
                  </button>
                {/if}
                <button
                  type="button"
                  class="gp-icon-btn"
                  data-size="sm"
                  data-testid="health-ledger-dismiss-btn"
                  aria-label="Dismiss session offer"
                  title="Dismiss"
                  onclick={() => void handleDismiss(offer.row_id)}
                >✕</button>
              </div>
            </div>
          </li>
        {/each}
      </ul>
      <ListPreviewToggle total={ledgerOffers.length} noun="sessions" bind:expanded={ledgerExpanded} />
    </div>
  {/if}
  {#if incidents.length > 0}
    <ul class="space-y-1.5">
      {#each previewSlice(incidents, expanded, LIST_PREVIEW_ROWS) as incident (incident.id)}
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
    <ListPreviewToggle total={incidents.length} noun="incidents" bind:expanded />
  {:else if loaded && !loadError && ledgerOffers.length === 0}
    <EmptyState
      icon={ShieldCheck}
      title="No terminal sessions ended from outside"
      hint="Incidents are kept in memory, so this covers the time since GitPulse started."
      compact
    />
  {/if}
</HealthSection>
