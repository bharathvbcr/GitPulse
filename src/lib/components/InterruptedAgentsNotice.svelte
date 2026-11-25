<script lang="ts">
  /**
   * App-level notice for agent sessions a crash or reboot interrupted.
   *
   * The per-repository banner lives inside one repo's terminal dock, so after
   * a reboot with agents spread over many repositories most of them were
   * visible only in Health. This speaks for the whole ledger, whichever repo
   * tab is open and whether or not the dock is.
   */
  import { onMount } from "svelte";
  import { RotateCw, X } from "@lucide/svelte";
  import { toastStore } from "../stores/toastStore";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { repoStoreFocus } from "../terminal/sessionFocus";
  import {
    hasUndismissedInterrupted,
    interruptedNoticeText,
    reopenAllSessionLedgerOffers,
    sessionLedgerStore,
    subscribeSessionLedger,
    summarizeInterrupted,
  } from "../terminal/sessionLedger";
  import { formatError } from "../ui/formatError";

  /** Pause between spawns: seventeen `--resume` at once recreates the load that caused the hang. */
  const REOPEN_PACE_MS = 600;

  const summary = $derived(summarizeInterrupted($sessionLedgerStore.offers));
  /** Row ids the user closed the notice for; in memory, so a restart asks again. */
  let dismissed = $state<ReadonlySet<string>>(new Set());
  let reopening = $state(false);
  let failedCount = $state(0);
  const visible = $derived(hasUndismissedInterrupted(summary, dismissed));

  onMount(() => subscribeSessionLedger());

  function dismiss() {
    dismissed = new Set([...dismissed, ...summary.agents.map((offer) => offer.row_id)]);
  }

  async function reopenAll() {
    if (reopening) return;
    reopening = true;
    failedCount = 0;
    try {
      const outcome = await reopenAllSessionLedgerOffers(summary.resumable, { delayMs: REOPEN_PACE_MS });
      failedCount = outcome.failed;
      const parts = [`Reopened ${outcome.reopened} agent${outcome.reopened === 1 ? "" : "s"}.`];
      if (outcome.failed > 0) parts.push(`${outcome.failed} could not be reopened; see Review.`);
      if (outcome.skipped > 0) parts.push(`${outcome.skipped} already reopening.`);
      if (outcome.failed > 0) toastStore.warning(parts.join(" "));
      else toastStore.success(parts.join(" "));
    } catch (error) {
      toastStore.error(`Couldn't reopen agents. ${formatError(error)}`);
    } finally {
      reopening = false;
    }
  }

  async function review() {
    try {
      interfaceStore.setGlobalSurface("repository");
      if (!$repoStore.currentPath) {
        const first = summary.agents.find((offer) => offer.repo_path);
        if (first) await repoStoreFocus.openRepo(first.repo_path);
      }
      repoStore.setActiveTab("insights", "health");
    } catch (error) {
      toastStore.error(`Couldn't open Health. ${formatError(error)}`);
    }
  }
</script>

{#if visible}
  <div
    class="gp-notice flex items-center justify-between gap-2 flex-wrap mx-2 mt-1"
    data-tone={summary.resumable.length > 0 ? "warn" : "unknown"}
    role="region"
    aria-label="Interrupted agent sessions"
    data-testid="interrupted-agents-notice"
  >
    <span class="font-medium text-textPrimary" data-testid="interrupted-agents-text">
      {interruptedNoticeText(summary)}
      {#if failedCount > 0}
        <span class="text-danger"> {failedCount} failed to reopen.</span>
      {/if}
    </span>
    <div class="flex items-center gap-1 shrink-0">
      {#if summary.resumable.length > 0}
        <button
          type="button"
          class="gp-btn"
          data-size="sm"
          data-testid="interrupted-agents-reopen-all"
          disabled={reopening}
          onclick={() => void reopenAll()}
        >
          <RotateCw size={12} /> Reopen all ({summary.resumable.length})
        </button>
      {/if}
      <button type="button" class="gp-btn" data-size="sm" data-testid="interrupted-agents-review" onclick={() => void review()}>
        Review
      </button>
      <button
        type="button"
        class="gp-icon-btn"
        data-size="sm"
        aria-label="Dismiss"
        data-testid="interrupted-agents-dismiss"
        onclick={dismiss}
      >
        <X size={12} />
      </button>
    </div>
  </div>
{/if}
