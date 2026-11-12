<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { harnessStore, verdictLabel } from "../stores/harnessStore";
  import { currentRepoPath } from "../stores/repoStore";
  import { copyText } from "../desktop/clipboard";
  import { invoke } from "../ipc/invoke";
  import type { GrantView } from "../grants/types";
  import { activeGrants as selectActiveGrants, grantLifecycle } from "../grants/model";
  import { formatError } from "../ui/formatError";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { redactDiagnosticText } from "../diagnostics/diagnostics";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { manviSectionId } from "../ui/manviFocus";
  import { beginGeneration } from "../async/guard";
  import {
    ADOPT_LEDGER_LABEL,
    NOTICE_TONE,
    activityHistoryPresentations,
    adoptionOffered,
    createRepositoryRefreshTimer,
  } from "../harness/policyPresentation";
  import { ChevronRight, Clipboard, History, ShieldAlert, ShieldCheck } from "@lucide/svelte";

  /**
   * Policy activity, as a disclosure on Work → Overview: the grant ledger and
   * the agent activity journal.
   *
   * The ledger-adoption flow stays here because it only ever answers the
   * journal's own history status ("the activity history is incomplete").
   *
   * The component stays mounted while collapsed and only its body is
   * conditional, so the journal and a deep link's anchor are always there.
   * Grants are read, and refreshed on a timer, only while the disclosure is
   * open: a collapsed card has no list to keep current.
   */
  let { open = $bindable(false) }: { open?: boolean } = $props();

  let logCopied = $state(false);
  let adopting = $state(false);
  let adoptError = $state<string | null>(null);

  let harnessError = $derived($harnessStore.error);
  // Newest first: the question the journal answers is "what just happened".
  let recentActions = $derived($harnessStore.actions.slice().reverse());
  let activityHistory = $derived(
    activityHistoryPresentations(harnessError, $harnessStore.ledger, $harnessStore.catchUp),
  );
  let adoptable = $derived(adoptionOffered($harnessStore.ledger, $harnessStore.catchUp));

  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  /**
   * Asks the backend to adopt this repository's refused ledger. The approval is
   * an OS dialog the backend draws, so this button is a request, not the
   * decision; a declined or refused adoption is reported here, never swallowed.
   */
  async function adoptLedger() {
    const repo = $currentRepoPath;
    if (!repo || adopting) return;
    adopting = true;
    adoptError = null;
    try {
      await invoke<string[]>("cmd_adopt_ledger", { repoPath: repo });
      await harnessStore.catchUp(repo);
    } catch (error: unknown) {
      adoptError = failure("adopt", "the activity history", redactDiagnosticText(formatError(error)));
    } finally {
      adopting = false;
    }
  }

  /**
   * The harness's grant ledger.
   *
   * A verdict whose status is `granted` says a rule fired and someone waived
   * it. Until the ledger was shown, there was no way to see who, why, or
   * until when — so a granted allow was indistinguishable from a clean one to
   * anyone reading the journal.
   */
  let grants = $state<GrantView | null>(null);
  /** The repository `grants` was read for, so a reopen does not show A's ledger under B. */
  let grantsRepo = $state<string | null>(null);
  let grantsLoading = $state(false);
  let grantsLoadError = $state<string | null>(null);
  let grantClock = $state(Date.now());
  const grantRequests = beginGeneration();
  const grantRefreshTimer = createRepositoryRefreshTimer(
    10_000,
    (repo) => {
      grantClock = Date.now();
      if ($currentRepoPath === repo) void refreshGrants(repo, false);
    },
    {
      // Visibility-aware for the same reason the status poll is: this timer
      // re-reads grants every ten seconds, and a hidden window has no grants
      // display to keep current. `createAdaptiveTimer` starts, stops and
      // catches up on its own, so the scheduler seam stays a plain
      // setInterval/clearInterval pair for tests.
      setInterval: (callback, delayMs) => createAdaptiveTimer(callback, delayMs),
      clearInterval: (handle) => (handle as () => void)(),
    },
  );
  onDestroy(() => grantRefreshTimer.dispose());

  async function refreshGrants(repo: string, showLoading: boolean) {
    const generation = grantRequests.next();
    if (showLoading) {
      // Never show A's authority state under B while B's ledger is loading.
      grants = null;
      grantsLoadError = null;
      grantsLoading = true;
    }
    try {
      const view = await invoke<GrantView>("cmd_grants_view", { repoPath: repo });
      if (!grantRequests.isCurrent(generation) || $currentRepoPath !== repo) return;
      grants = view;
      grantsRepo = repo;
      grantsLoadError = null;
      grantClock = Date.now();
    } catch (error: unknown) {
      if (!grantRequests.isCurrent(generation) || $currentRepoPath !== repo) return;
      grantsLoadError = failure("refresh", "grant history", redactDiagnosticText(formatError(error)));
      if (showLoading) grants = null;
    } finally {
      if (grantRequests.isCurrent(generation) && $currentRepoPath === repo) grantsLoading = false;
    }
  }

  // Keyed on the repository and on whether the body is showing, so the ~6s
  // status poll does not reload. Closing stops the timer; reopening reads the
  // ledger again, with the loading state only when it is another repository's.
  $effect(() => {
    const repo = $currentRepoPath;
    const showing = open;
    untrack(() => {
      if (!repo || !showing) {
        grantRefreshTimer.update(null);
        if (!repo) {
          grantRequests.next();
          grants = null;
          grantsRepo = null;
          grantsLoadError = null;
          grantsLoading = false;
        }
        return;
      }
      void refreshGrants(repo, grantsRepo !== repo);
      // Grant consumption and expiry can change while the disclosure stays
      // open. Key the timer by repository rather than this effect's execution:
      // repoStore can publish unrelated same-path status changes more often
      // than the interval, and resetting on each one would starve the refresh.
      grantRefreshTimer.update(repo);
    });
  });

  /** MANVI persists oldest first; retain spent grants as reviewable history. */
  let orderedGrants = $derived((grants?.grants ?? []).slice().reverse());
  let activeGrantCount = $derived(selectActiveGrants(grants?.grants ?? [], grantClock).length);

  function actionTime(ts: number): string {
    return new Date(ts).toLocaleTimeString();
  }

  /** The `gp-chip` tone for a verdict status; an unchecked one is unknown, never neutral. */
  function verdictTone(status: string): string {
    switch (status) {
      case "blocked":
        return "danger";
      case "granted":
        return "accent";
      case "demoted":
      case "warned":
        return "info";
      case "widened":
      case "degraded":
        return "warn";
      case "allowed":
        return "ok";
      case "unchecked":
      default:
        return "unknown";
    }
  }

  let logCopyTimer: number | null = null;

  /** The journal as plain text, for pasting into a bug report or notes file. */
  async function copyLog() {
    const lines = $harnessStore.actions.map((action) => {
      const time = actionTime(action.ts);
      const state = !action.ok
        ? "failed"
        : action.verdict
          ? verdictLabel(action.verdict).replace(/^Policy: /, "")
          : "no gate ran";
      return `${time}\t${action.kind}\t${action.label || "—"}\t[${state}]`;
    });
    if (await copyText(lines.join("\n"))) {
      logCopied = true;
      if (logCopyTimer !== null) window.clearTimeout(logCopyTimer);
      logCopyTimer = window.setTimeout(() => (logCopied = false), 1500);
    }
  }

  $effect(() => {
    return () => {
      if (logCopyTimer !== null) window.clearTimeout(logCopyTimer);
    };
  });
</script>

<section id={manviSectionId("activity")} tabindex="-1" class="mt-4 w-full rounded-xl">
  <button
    type="button"
    class="flex w-full items-center gap-1.5 rounded-xl border border-edge px-3 py-2 text-meta font-medium text-textMuted hover:bg-surfaceHover"
    onclick={() => (open = !open)}
    aria-expanded={open}
    aria-controls="policy-activity-body"
  >
    <ChevronRight size={13} class="shrink-0 transition-transform {open ? 'rotate-90' : ''}" />
    Policy activity
  </button>

  {#if open}
    <div id="policy-activity-body" class="mt-2 space-y-4 text-xs text-textPrimary">
      <!-- Grant ledger: who waived which rule, and until when -->
      {#if grantsLoading || grantsLoadError || grants?.available}
        <section class="gp-card p-4 space-y-2">
          <h3 class="gp-label flex items-center gap-1.5">
            <ShieldAlert size={11} />
            <span>
              {grants?.available
                ? `Grants (${activeGrantCount} active of ${grants.grants.length})`
                : "Grants"}
            </span>
          </h3>
          {#if grantsLoading}
            <div role="status" aria-label="Checking grant history"><Skeleton variant="text" count={2} /></div>
          {:else}
            {#if grantsLoadError}
              <div class="gp-notice" data-tone="unknown" role="alert">
                {grantsLoadError} {grants
                  ? "The last verified snapshot remains below."
                  : "No authority state is inferred."}
              </div>
            {/if}
            {#if grants?.error}
              <!--
                Invalid entries are refused by the backend, while valid entries
                remain visible. The warning and list must therefore be independent.
              -->
              <div class="gp-notice" data-tone="warn" role="status">
                Some grant records were refused, so this list is not the whole
                truth: {redactDiagnosticText(grants.error)}
              </div>
            {/if}
            {#if grants?.grants.length === 0}
              {#if grants.error}
                <div class="gp-notice" data-tone="unknown" role="status">
                  No valid grant record could be displayed.
                </div>
              {:else}
                <EmptyState compact icon={ShieldCheck} title="No rules waived" hint="A grant appears here when someone waives a policy rule in this repository." />
              {/if}
            {:else if grants && grants.grants.length > 0}
              <div class="rounded-xl bg-background p-1 flex flex-col gap-0.5 max-h-40 overflow-y-auto">
                {#each orderedGrants as grant, i (`${grant.id}#${i}`)}
                  <div class="rounded-lg px-2 py-1.5 flex flex-col gap-0.5 text-meta hover:bg-surfaceHover">
                    <div class="flex items-center gap-1.5 min-w-0">
                      <span class="font-mono text-micro text-accent truncate">
                        {grant.scope.rules.length > 0 ? grant.scope.rules.join(", ") : "invalid scope: no policy rule"}
                      </span>
                      {#if grant.scope.once}
                        <span class="shrink-0 text-micro text-textMuted">one use</span>
                      {/if}
                      <span class="ml-auto shrink-0 text-micro text-textMuted">
                        {grantLifecycle(grant, grantClock)}
                      </span>
                    </div>
                    <div class="text-textPrimary truncate">
                      {grant.scope.paths.length > 0 ? grant.scope.paths.join(", ") : "any repository path"}
                      {#if grant.scope.task_id}· task {grant.scope.task_id}{/if}
                    </div>
                    <div class="text-textMuted truncate">
                      {grant.grantor.authority || "invalid authority"}{#if grant.grantor.id}:{grant.grantor.id}{/if}
                      {#if grant.reason}· {grant.reason}{/if}
                      {#if grant.expires_at}· expires {grant.expires_at}{/if}
                      {#if !grant.expires_at}
                        · expiry unavailable
                      {/if}
                    </div>
                  </div>
                {/each}
              </div>
              <p class="text-micro text-textMuted">
                Current Manvi CLI has no grant-revocation command. A grant stops applying after it is consumed or expires.
                GitPulse only reads the ledger at <code class="font-mono break-all">{grants.path}</code>.
              </p>
            {/if}
          {/if}
        </section>
      {/if}

      <!-- Agent activity journal -->
      <section class="gp-card p-4 space-y-2">
        <div class="flex items-center justify-between">
          <h3 class="gp-label flex items-center gap-1.5">
            <History size={11} />
            <span>Agent activity ({recentActions.length})</span>
          </h3>
          <div class="flex items-center gap-1">
            {#if recentActions.length > 0}
              <button
                onclick={() => void copyLog()}
                class="gp-btn"
                data-variant="ghost"
                data-size="sm"
                title="Copy the activity log to the clipboard"
              >
                <Clipboard size={11} />
                <span>{logCopied ? "Copied" : "Copy log"}</span>
              </button>
              <button
                onclick={() => harnessStore.clearActions()}
                class="gp-btn"
                data-variant="ghost"
                data-size="sm"
              >
                Clear
              </button>
            {/if}
          </div>
        </div>
        <div class="grid gap-2 md:grid-cols-2" aria-label="Activity history status">
          {#each activityHistory as status, i (`${status.label}#${i}`)}
            <div class="gp-notice" data-tone={NOTICE_TONE[status.tone]} role="status">
              <div class="min-w-0 flex-1">
                <div class="text-micro font-semibold {status.badgeClass}">
                  {status.label}
                </div>
                <p class="mt-1 text-meta text-textMuted leading-relaxed wrap-break-word">
                  {status.detail}
                </p>
              </div>
            </div>
          {/each}
        </div>
        {#if adoptable}
          <div class="rounded-xl bg-background p-3 flex flex-col gap-2">
            <p class="text-meta text-textMuted leading-relaxed">
              If this checkout is a repository whose activity history was recorded elsewhere (moved
              across devices, restored from a backup, or copied with the original gone), adopt that
              history to keep recording into it. You will be asked to confirm in a system dialog, and
              GitPulse says why if the history cannot be adopted.
            </p>
            <div>
              <button
                onclick={() => void adoptLedger()}
                disabled={adopting}
                class="gp-btn"
                data-size="sm"
              >
                {adopting ? "Adopting…" : ADOPT_LEDGER_LABEL}
              </button>
            </div>
            {#if adoptError}
              <div class="gp-notice" data-tone="danger" role="alert">{adoptError}</div>
            {/if}
          </div>
        {/if}
        {#if recentActions.length === 0}
          <div class="gp-notice" data-tone="unknown" role="status">
            No activity rows are currently available. The history status above says whether this
            means no recorded actions or incomplete data.
          </div>
        {:else}
          <div class="rounded-xl bg-background p-1 flex flex-col gap-0.5 max-h-56 overflow-y-auto">
            {#each recentActions as action, i (`${action.identity}#${i}`)}
              <div class="rounded-lg px-2 py-1.5 flex flex-col gap-0.5 text-meta hover:bg-surfaceHover" title={action.verdict?.detail ?? action.label}>
                <div class="flex items-center gap-2">
                  <span class="font-mono text-micro text-textMuted shrink-0">{actionTime(action.ts)}</span>
                  <span class="gp-chip shrink-0 font-mono" data-kind="label" data-tone={verdictTone(
                    !action.ok ? 'blocked' : (action.verdict?.status ?? 'unchecked')
                  )}>
                    {action.verdict?.status ?? action.kind}
                  </span>
                  <span class="truncate {action.ok ? 'text-textPrimary' : 'text-danger'}">{action.label || "—"}</span>
                  {#if action.verdict?.task_id}
                    <span class="gp-chip ml-auto shrink-0 font-mono" data-tone="neutral" data-kind="label">
                      {action.verdict.task_id}
                    </span>
                  {/if}
                </div>
                {#if action.verdict?.grant_id}
                  <div class="text-micro text-accent font-mono pl-14">
                    Grant {action.verdict.grant_id} by {action.verdict.granted_by || "human"} · {action.verdict.reason || "waived"}
                  </div>
                {:else if action.verdict?.demoted}
                  <div class="text-micro text-info/80 font-mono pl-14">
                    Checks demoted: {action.verdict.demoted}
                  </div>
                {:else if action.verdict?.widened}
                  <div class="text-micro text-warn/80 font-mono pl-14">
                    Scope widened: {action.verdict.widened}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </section>
    </div>
  {/if}
</section>
