<script module lang="ts">
  import type { HarnessPermissionMode } from "../harness/availability";
  import type { CatchUp } from "../ingest/types";
  import type { LedgerStatus } from "../ledger/types";
  import type { MutationTarget } from "../repos/storeTypes";
  import { redactDiagnosticText } from "../diagnostics/diagnostics";

  export type StatusTone = "ready" | "warning" | "error" | "neutral";

  export interface StatusPresentation {
    label: string;
    detail: string;
    tone: StatusTone;
    cardClass: string;
    badgeClass: string;
  }

  export interface RefreshTimerScheduler {
    setInterval(callback: () => void, delayMs: number): unknown;
    clearInterval(handle: unknown): void;
  }

  export interface RepositoryRefreshTimer {
    update(repo: string | null): void;
    dispose(): void;
  }

  /**
   * Keeps one interval for the active repository. Updating a store with the
   * same repository identity is deliberately a no-op, so unrelated status
   * publications cannot postpone expiry/ledger refresh indefinitely.
   */
  export function createRepositoryRefreshTimer(
    intervalMs: number,
    refresh: (repo: string) => void,
    scheduler: RefreshTimerScheduler,
  ): RepositoryRefreshTimer {
    let activeRepo: string | null = null;
    let handle: unknown;

    function clear() {
      if (handle === undefined) return;
      scheduler.clearInterval(handle);
      handle = undefined;
    }

    return {
      update(repo) {
        if (repo === activeRepo) return;
        clear();
        activeRepo = repo;
        if (!repo) return;
        const scheduledRepo = repo;
        handle = scheduler.setInterval(() => {
          if (activeRepo === scheduledRepo) refresh(scheduledRepo);
        }, intervalMs);
      },
      dispose() {
        clear();
        activeRepo = null;
      },
    };
  }

  const STATUS_CLASSES: Record<
    StatusTone,
    Pick<StatusPresentation, "cardClass" | "badgeClass">
  > = {
    ready: {
      cardClass: "border-ok/35 bg-ok/5",
      badgeClass: "text-ok",
    },
    warning: {
      cardClass: "border-warn/35 bg-warn/5",
      badgeClass: "text-warn",
    },
    error: {
      cardClass: "border-danger/35 bg-danger/5",
      badgeClass: "text-danger",
    },
    neutral: {
      cardClass: "border-edge bg-background",
      badgeClass: "text-textMuted",
    },
  };

  /** The `gp-notice` tone for each status tone. "Neutral" here means nothing was determined, which is the unknown tone. */
  export const NOTICE_TONE: Record<StatusTone, "ok" | "warn" | "danger" | "unknown"> = {
    ready: "ok",
    warning: "warn",
    error: "danger",
    neutral: "unknown",
  };

  function presentation(
    label: string,
    detail: string,
    tone: StatusTone,
  ): StatusPresentation {
    return { label, detail, tone, ...STATUS_CLASSES[tone] };
  }

  /** Truthful copy for the action runner's current policy capability. */
  export function scopedRunnerPresentation(
    mode: HarnessPermissionMode,
    refreshError: string | null = null,
  ): StatusPresentation {
    if (refreshError) {
      return presentation(
        "Status stale",
        `Couldn't check Manvi status. ${redactDiagnosticText(refreshError)} No runner capability is claimed until status is checked again.`,
        "error",
      );
    }
    switch (mode) {
      case "connected":
        return presentation(
          "Guarded",
          "Health and coverage commands require a click, a purpose allowlist, direct argv execution, the Manvi policy gate, hard timeouts, bounded output, and stop-on-failure accounting.",
          "ready",
        );
      case "unguarded":
        return presentation(
          "Not checked",
          "User-started health and coverage commands still use a purpose allowlist, direct argv execution, hard timeouts, bounded output, and stop-on-failure accounting. Manvi is not installed, so no policy check runs.",
          "warning",
        );
      case "blocked":
        return presentation(
          "Blocked",
          "Guarded health and coverage commands are refused while Manvi policy checks are failing. Reconnect after the policy gate recovers.",
          "error",
        );
      default:
        return presentation(
          "Status unknown",
          "The Manvi policy capability has not been checked yet. User-started commands remain behind the purpose allowlist; reconnect to establish whether the policy gate can run.",
          "neutral",
        );
    }
  }

  /** The label of the action that keeps a refused ledger. */
  export const ADOPT_LEDGER_LABEL = "Adopt this ledger";

  /**
   * Whether the history has a problem the adoption action might resolve: the
   * ledger reports an error, or catch-up does. The panel does not read which
   * error it is (that is the backend's to say, and it does: it refuses an
   * adoption that does not apply with its own reason). Offering the action on any
   * history error costs a refused request; hiding it behind text matching costs
   * a person the only way out.
   */
  export function adoptionOffered(
    ledger: LedgerStatus | null,
    catchUp: Pick<CatchUp, "error"> | null,
  ): boolean {
    return Boolean(ledger?.error) || Boolean(catchUp?.error);
  }

  /**
   * Separately reports UI refresh, durable recording, and external catch-up.
   * One successful source must never hide a gap in another source.
   */
  export function activityHistoryPresentations(
    refreshError: string | null,
    ledger: LedgerStatus | null,
    catchUp: CatchUp | null,
  ): StatusPresentation[] {
    const statuses: StatusPresentation[] = [];

    if (refreshError) {
      statuses.push(
        presentation(
          "Couldn't check Manvi status",
          `${redactDiagnosticText(refreshError)} Displayed capability status may be stale.`,
          "error",
        ),
      );
    }

    if (!ledger) {
      statuses.push(
        presentation(
          "Activity history not checked",
          "Durable recording status has not loaded for this repository.",
          "neutral",
        ),
      );
    } else if (!ledger.recording || ledger.error) {
      const details = [
        ledger.error
          ? redactDiagnosticText(ledger.error)
          : "Durable activity recording is unavailable.",
        ledger.dropped > 0
          ? `${ledger.dropped} ${ledger.dropped === 1 ? "event was" : "events were"} not recorded.`
          : "",
      ].filter(Boolean);
      statuses.push(presentation("Activity history incomplete", details.join(" "), "error"));
    } else if (ledger.dropped > 0) {
      statuses.push(
        presentation(
          "Activity history incomplete",
          `${ledger.dropped} ${ledger.dropped === 1 ? "event was" : "events were"} not recorded.`,
          "warning",
        ),
      );
    } else {
      statuses.push(
        presentation(
          "Activity history recording",
          ledger.path
            ? `Durable events are recording at ${ledger.path}.`
            : "Durable events are recording for this repository.",
          "ready",
        ),
      );
    }

    if (!catchUp) {
      statuses.push(
        presentation(
          "Catch-up not checked",
          "External transcript and reflog catch-up has not completed for this repository.",
          "neutral",
        ),
      );
    } else if (catchUp.error || catchUp.skipped_lines > 0) {
      const details = [
        catchUp.error ? redactDiagnosticText(catchUp.error) : "",
        catchUp.skipped_lines > 0
          ? `${catchUp.skipped_lines} transcript ${catchUp.skipped_lines === 1 ? "line was" : "lines were"} skipped.`
          : "",
        catchUp.recorded > 0
          ? `${catchUp.recorded} ${catchUp.recorded === 1 ? "event was" : "events were"} still recovered.`
          : "",
      ].filter(Boolean);
      statuses.push(presentation("Catch-up incomplete", details.join(" "), "warning"));
    } else {
      statuses.push(
        presentation(
          "Catch-up complete",
          catchUp.recorded > 0
            ? `${catchUp.recorded} external ${catchUp.recorded === 1 ? "event was" : "events were"} recovered from ${catchUp.transcripts} ${catchUp.transcripts === 1 ? "transcript" : "transcripts"} and ${catchUp.reflog_entries} reflog ${catchUp.reflog_entries === 1 ? "entry" : "entries"}.`
            : "No external transcript or reflog events needed to be recovered.",
          "ready",
        ),
      );
    }

    return statuses;
  }
</script>

<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { harnessStore, verdictLabel, type AiSelection } from "../stores/harnessStore";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { copyText } from "../desktop/clipboard";
  import { invoke } from "../ipc/invoke";
  import type { GrantView } from "../grants/types";
  import {
    activeGrants as selectActiveGrants,
    grantLifecycle,
  } from "../grants/model";
  import {
    contextWindowLabel,
    sweepSummary,
    toolSupportLabel,
    type ScanModel,
    type ScanResult,
  } from "../ai/scan";
  import { formatError } from "../ui/formatError";
  import { describeFailure } from "../ui/copy/describeFailure";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { manviSectionId } from "../ui/manviFocus";
  import { beginGeneration } from "../async/guard";
  import {
    harnessPermissionMode,
    harnessPermissionSummary,
    harnessPostureLabel,
  } from "../harness/availability";
  import {
    RefreshCw,
    ShieldCheck,
    ShieldAlert,
    Server,
    GitBranch,
    Check,
    History,
    Clipboard,
    SquareTerminal,
    Wrench,
    Percent,
    Gauge,
  } from "@lucide/svelte";
  import ExternalToolsPanel from "./ExternalToolsPanel.svelte";
  import ManviClaudeModel from "./ManviClaudeModel.svelte";
  import { openSetupWizard } from "../tools/onboardingStore";

  let branchSuggestion = $state<string>("");
  /** The repository the suggestion was made for; the branch is created there. */
  let branchTarget = $state<MutationTarget | null>(null);
  let branchWarnings = $state<string[]>([]);
  let isSuggesting = $state(false);
  let branchError = $state<string | null>(null);
  let logCopied = $state(false);
  let adopting = $state(false);
  let adoptError = $state<string | null>(null);

  let ai = $derived($harnessStore.ai);
  let harness = $derived($harnessStore.harness);
  let harnessError = $derived($harnessStore.error);
  let permissionMode = $derived(harnessPermissionMode(harness));
  let postureLabel = $derived(harnessPostureLabel(harness?.posture));
  let runnerPresentation = $derived(scopedRunnerPresentation(permissionMode, harnessError));
  let preferred = $derived($harnessStore.preferred);
  // Newest first: the question the journal answers is "what just happened".
  let recentActions = $derived($harnessStore.actions.slice().reverse());
  let activityHistory = $derived(
    activityHistoryPresentations(
      harnessError,
      $harnessStore.ledger,
      $harnessStore.catchUp,
    ),
  );

  let adoptable = $derived(adoptionOffered($harnessStore.ledger, $harnessStore.catchUp));

  /**
   * Asks the backend to adopt this repository's refused ledger. The approval is
   * an OS dialog the backend draws, so this button is a request, not the
   * decision; a declined or refused adoption is reported here, never swallowed.
   */
  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

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
   * it. Until this pane showed the ledger, there was no way to see who, why, or
   * until when — so a granted allow was indistinguishable from a clean one to
   * anyone reading the journal.
   */
  let grants = $state<GrantView | null>(null);
  let grantsLoading = $state(false);
  let grantsLoadError = $state<string | null>(null);
  let grantClock = $state(Date.now());
  const grantRequests = beginGeneration();
  const grantRefreshTimer = createRepositoryRefreshTimer(
    10_000,
    (repo) => {
      grantClock = Date.now();
      if ($repoStore.currentPath === repo) void refreshGrants(repo, false);
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
      if (!grantRequests.isCurrent(generation) || $repoStore.currentPath !== repo) return;
      grants = view;
      grantsLoadError = null;
      grantClock = Date.now();
    } catch (error: unknown) {
      if (!grantRequests.isCurrent(generation) || $repoStore.currentPath !== repo) return;
      grantsLoadError = failure("refresh", "grant history", redactDiagnosticText(formatError(error)));
      if (showLoading) grants = null;
    } finally {
      if (!grantRequests.isCurrent(generation) || $repoStore.currentPath !== repo) return;
      grantsLoading = false;
    }
  }

  // Keyed on the repository alone, so the ~6s status poll does not reload.
  $effect(() => {
    const repo = $currentRepoPath;
    untrack(() => {
      if (!repo) {
        grantRequests.next();
        grants = null;
        grantsLoadError = null;
        grantsLoading = false;
        return;
      }
      void refreshGrants(repo, true);
    });
  });

  // Grant consumption and expiry can change while this pane remains mounted.
  // Key the timer by repository rather than this effect's execution: repoStore
  // can publish unrelated same-path status changes more frequently than the
  // interval, and resetting on each one would starve this refresh forever.
  $effect(() => {
    const repo = $currentRepoPath;
    untrack(() => grantRefreshTimer.update(repo));
  });

  /** MANVI persists oldest first; retain spent grants as reviewable history. */
  let orderedGrants = $derived((grants?.grants ?? []).slice().reverse());
  let activeGrantCount = $derived(selectActiveGrants(grants?.grants ?? [], grantClock).length);

  /**
   * The surfaces this pane links to, as (view, section) pairs.
   *
   * Health and Coverage stopped being views of their own — they are scans of
   * the repository, and they live in Insights beside Pulse and Storage. The
   * pair is what a link needs now: the view alone would land on Insights'
   * default section, which is not the capability the button names.
   */
  const CAPABILITY_TARGETS = {
    health: { tab: "insights", section: "health" },
    coverage: { tab: "insights", section: "coverage" },
    github: { tab: "work", section: "remote" },
  } as const;

  type CapabilityTab = keyof typeof CAPABILITY_TARGETS;

  function openCapability(tab: CapabilityTab) {
    if (!$repoStore.currentPath) return;
    const target = CAPABILITY_TARGETS[tab];
    repoStore.setActiveTab(target.tab, target.section);
  }

  /**
   * The terminal is a dock, not a view, so it opens *over* whatever is on
   * screen instead of navigating away from this pane — which is the point:
   * the grant list stays readable while the shell it describes is running.
   */
  function openTerminal() {
    if (!$repoStore.currentPath) return;
    repoStore.setTerminalOpen(true);
  }

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

  function isSelected(endpointUrl: string, model: string): boolean {
    if (preferred) return preferred.base_url === endpointUrl && preferred.model === model;
    return ai?.selected?.base_url === endpointUrl && ai?.selected?.model === model;
  }

  /**
   * What the local-server sweep found.
   *
   * The endpoint list above reports servers GitPulse already resolved. This
   * answers the question it could not: what each of them actually serves, and
   * whether those models support the features about to be offered on them.
   */
  let scan = $state<ScanResult | null>(null);
  let scanning = $state(false);
  let scanError = $state<string | null>(null);

  async function runScan() {
    scanning = true;
    scanError = null;
    try {
      scan = await invoke<ScanResult>("cmd_local_scan");
    } catch (e) {
      // Reported, not swallowed: a sweep that failed is not a machine with no
      // models on it.
      scanError = failure("scan", "local model servers", e);
      scan = null;
    } finally {
      scanning = false;
    }
  }

  /** Models a chat feature can actually run on, given what the sweep asked. */
  function usable(model: ScanModel): boolean {
    // `capabilities_known` false means nobody asked, so nothing is excluded on
    // the strength of a flag nobody set.
    return !model.capabilities_known || model.supports_completion;
  }

  async function pick(selection: AiSelection | null) {
    await harnessStore.selectModel(selection);
  }

  async function suggestBranch() {
    const path = $repoStore.currentPath;
    if (!path) return;
    isSuggesting = true;
    branchError = null;
    branchSuggestion = "";
    branchTarget = repoStore.captureTarget();
    branchWarnings = [];
    try {
      const result = await harnessStore.suggestBranchName(path);
      branchSuggestion = result.text;
      branchWarnings = result.warnings;
    } catch (err: unknown) {
      branchError = failure("suggest", "a branch name", err);
    } finally {
      isSuggesting = false;
    }
  }

  async function createSuggestedBranch() {
    if (!branchSuggestion) return;
    // createBranch resolves { ok: false } on a refusal or a collision; it never
    // throws, so the suggestion is kept and the reason shown unless it was made.
    const outcome = await repoStore.createBranch(branchSuggestion, undefined, branchTarget);
    if (outcome.ok) {
      branchSuggestion = "";
    } else {
      branchError = outcome.error ?? "Couldn't create the branch.";
    }
  }
</script>

<div class="space-y-4">
  <!-- Policy connection -->
  <section id={manviSectionId("harness")} tabindex="-1" class="gp-card p-4 space-y-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class="gp-label">Manvi policy</h3>
      <button
        onclick={() => harnessStore.reconnect()}
        class="gp-btn"
        data-size="sm"
        title="Restart Manvi and scan for model servers again"
      >
        <RefreshCw size={12} class={$harnessStore.isProbing ? "animate-spin" : ""} />
        <span>Reconnect</span>
      </button>
    </div>
    {#if harnessError}
      <div class="gp-notice" data-tone="unknown" role="alert">
        <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <div class="font-medium">Couldn't check Manvi status</div>
          <p class="text-textMuted leading-relaxed wrap-break-word">
            {redactDiagnosticText(harnessError)} Any previously displayed connection state may be stale;
            press Reconnect to check again.
          </p>
        </div>
      </div>
    {:else if permissionMode === "connected" && harness}
      <div class="gp-notice" data-tone="ok">
        <ShieldCheck size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <div class="font-medium">Connected · protocol {harness.protocol}{#if postureLabel}, {postureLabel}{/if}</div>
          <div class="font-mono text-meta text-textMuted break-all">{harness.binary}</div>
          <p class="text-textMuted leading-relaxed">
            Commits, pushes, merges, rebases, branch deletions, discards and conflict-editor
            saves are put to Manvi's gates before they run. Hard rules — force pushes,
            verification bypass flags, writes to credential paths — are refused here.
          </p>
          <div class="flex flex-wrap gap-1 pt-1">
            {#each harness.ops as op}
              <span class="gp-chip font-mono" data-tone="neutral" data-kind="label">{op}</span>
            {/each}
          </div>
        </div>
      </div>
    {:else if permissionMode === "unguarded"}
      <div class="gp-notice" data-tone="warn">
        <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <div class="font-medium">Manvi is not installed</div>
          <p class="text-textMuted leading-relaxed">
            {harnessPermissionSummary(harness)} Install Manvi, or point
            <span class="font-mono">GITPULSE_MANVI_BIN</span> at the binary, then press Reconnect.
          </p>
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            onclick={() => openSetupWizard("manvi", "explain")}
          >
            Set up Manvi
          </button>
          <ExternalToolsPanel
            only="manvi"
            compact
            onInstalled={() => harnessStore.reconnect()}
          />
        </div>
      </div>
    {:else if permissionMode === "blocked"}
      <div class="gp-notice" data-tone="danger" role="alert">
        <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <div class="font-medium">Policy gate failed — mutations blocked</div>
          <p class="text-textMuted leading-relaxed">{harnessPermissionSummary(harness)}</p>
          <p class="text-textMuted leading-relaxed">
            Press Reconnect after resolving the failure. GitPulse will not treat a timed-out,
            busy, unavailable, or incompatible policy check as approval.
          </p>
        </div>
      </div>
    {:else}
      <div class="gp-notice" data-tone="unknown" role="status">
        {harnessPermissionSummary(harness)}
      </div>
    {/if}
  </section>

  <!-- The live `hello` response is intentionally narrow: GitPulse embeds
       Manvi's policy and local-model planes only. This card keeps that wire
       truth separate from app-owned, user-confirmed command execution. -->
  <section class="gp-card p-4 space-y-3">
    <div class="space-y-1">
      <h3 class="gp-label">Capability boundary</h3>
      <p class="text-textMuted leading-relaxed">
        No autonomous PTY or app-control API is exposed by <span class="font-mono">manvi serve</span>.
        GitPulse adds narrow, explicit controls around the planes Manvi actually provides.
      </p>
    </div>

    <div class="grid gap-2 sm:grid-cols-2">
      <div class="rounded-xl bg-background p-3 space-y-1">
        <div class="flex items-center gap-2 text-textPrimary font-medium">
          <SquareTerminal size={13} class="text-textMuted" />
          <span>Interactive shell</span>
          <span class="gp-chip ml-auto" data-tone="warn" data-kind="label">User only</span>
        </div>
        <p class="text-textMuted leading-relaxed">
          Manvi never receives the PTY handle or keystrokes. Shell commands are typed and owned by you.
        </p>
      </div>
      <div class="gp-notice" data-tone={NOTICE_TONE[runnerPresentation.tone]}>
        <div class="min-w-0 flex-1 space-y-1">
          <div class="flex items-center gap-2 text-textPrimary font-medium">
            {#if runnerPresentation.tone === "ready"}
              <ShieldCheck size={13} class={runnerPresentation.badgeClass} />
            {:else}
              <ShieldAlert size={13} class={runnerPresentation.badgeClass} />
            {/if}
            <span>Scoped action runner</span>
            <span class="ml-auto text-micro {runnerPresentation.badgeClass}">{runnerPresentation.label}</span>
          </div>
          <p class="text-textMuted leading-relaxed">
            {runnerPresentation.detail}
          </p>
        </div>
      </div>
    </div>

    <div class="grid grid-cols-2 sm:grid-cols-4 gap-2">
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("health")}
        title="Scan dependencies, ask Manvi for a remediation plan, and run approved steps"
      >
        <Wrench size={12} /> Health fixes
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("coverage")}
        title="Generate, scan, and analyze coverage reports. Rust needs cargo-llvm-cov; a full run can take several minutes."
      >
        <Percent size={12} /> Coverage
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={openTerminal}
        title="Show the user-owned shell and bounded console in the dock below"
      >
        <SquareTerminal size={12} /> Terminal
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("github")}
        title="Run this repository's bounded local CI pipeline"
      >
        <Gauge size={12} /> CI:local
      </button>
    </div>
  </section>

  <ManviClaudeModel />

  <!-- Model servers -->
  <section id={manviSectionId("model")} tabindex="-1" class="gp-card p-4 space-y-2">
    <h3 class="gp-label">Local model servers</h3>
    {#if ai}
      <!--
        Discovery. Separate from the endpoint list above, which reports servers
        GitPulse resolved; this reports what they serve and what those models
        can do.
      -->
      <div class="flex items-center justify-between gap-2">
        <button
          onclick={() => void runScan()}
          disabled={scanning}
          class="gp-btn"
          data-variant="ghost"
          data-size="sm"
          title="Probe the well-known local endpoints and list what each server serves"
        >
          <RefreshCw size={11} class={scanning ? "animate-spin" : ""} />
          <span>{scanning ? "Scanning…" : "Scan local servers"}</span>
        </button>
        {#if scan}
          <span class="text-micro text-textMuted">{sweepSummary(scan)}</span>
        {/if}
      </div>

      {#if scanError}
        <div class="gp-notice" data-tone="unknown" role="status">
          {scanError} This is not a list of what is running.
        </div>
      {:else if scan && scan.servers.length > 0}
        <div class="space-y-1.5">
          {#each scan.servers as server, i (`${server.base_url}#${i}`)}
            <div class="rounded-xl bg-background p-2.5 space-y-1">
              <div class="flex items-center gap-2 text-meta">
                <Server size={12} class="text-accent shrink-0" />
                <span class="font-mono truncate">{server.base_url}</span>
                <span class="gp-chip shrink-0" data-tone="neutral" data-kind="label">{server.runtime}</span>
                {#if server.version}
                  <span class="shrink-0 text-micro text-textMuted">v{server.version}</span>
                {/if}
              </div>
              {#each server.models as model, i (`${model.id || "model"}#${i}`)}
                <button
                  onclick={() => pick({ base_url: server.base_url, model: model.id })}
                  class="gp-row w-full text-left text-micro {usable(model) ? '' : 'opacity-50'}"
                  data-selected={isSelected(server.base_url, model.id)}
                  title={usable(model)
                    ? `${contextWindowLabel(model)} · ${toolSupportLabel(model)}`
                    : "This model does not generate text — an embedding model answers the same listing as every chat model."}
                >
                  <span class="font-mono truncate flex-1">{model.id}</span>
                  <span class="shrink-0 text-textMuted">{contextWindowLabel(model)}</span>
                  <!--
                    Three states, not two. `capabilities_known` false means
                    nobody asked, and rendering that as "no tools" would make a
                    capable model look incapable.
                  -->
                  <span
                    class="gp-chip shrink-0"
                    data-kind="label"
                    data-tone={!model.capabilities_known ? "unknown" : model.supports_tools ? "accent" : "neutral"}
                  >{toolSupportLabel(model)}</span>
                </button>
              {/each}
            </div>
          {/each}
        </div>
      {/if}

      {#if ai.endpoints.filter((e) => e.reachable).length === 0}
        <div class="gp-notice" data-tone="unknown" role="status">
          <div class="min-w-0 flex-1">
            {ai.detail || "No local model server answered."}
            <p class="mt-1.5 text-textMuted">
              GitPulse only ever talks to a model server on this machine: the transport refuses
              any address that is not loopback, so a diff cannot leave the machine through a
              mistyped setting.
            </p>
          </div>
        </div>
      {:else}
        <div class="space-y-2">
          {#each ai.endpoints.filter((e) => e.reachable) as endpoint}
            <div class="rounded-xl bg-background p-3 space-y-2">
              <div class="flex items-center gap-2 text-textPrimary font-medium">
                <Server size={13} class="text-accent" />
                <span class="font-mono text-meta">{endpoint.base_url}</span>
              </div>
              <div class="grid grid-cols-1 gap-1">
                {#each endpoint.models as model}
                  <button
                    onclick={() => pick({ base_url: endpoint.base_url, model })}
                    class="gp-row justify-between text-left"
                    data-selected={isSelected(endpoint.base_url, model)}
                  >
                    <span class="font-mono text-meta truncate">{model}</span>
                    {#if isSelected(endpoint.base_url, model)}
                      <Check size={13} />
                    {/if}
                  </button>
                {/each}
              </div>
            </div>
          {/each}
          {#if preferred}
            <button
              onclick={() => pick(null)}
              class="gp-btn"
              data-variant="ghost"
              data-size="sm"
            >
              Clear the pinned model and let discovery choose
            </button>
          {/if}
        </div>
      {/if}

      {#if ai.model_info}
        <div class="rounded-xl bg-background p-3 space-y-1">
          <div class="text-textPrimary font-medium">{ai.model_info.model}</div>
          <!-- Provenance, not just a number: a window read off the server
               and one typed into a default produce the same request and
               completely different confidence. -->
          <div class="text-textMuted">
            Context: {ai.model_info.describe}
            {#if !ai.model_info.discovered}
              <span class="text-warn"> (declared, not discovered)</span>
            {/if}
          </div>
          {#if ai.model_info.capabilities_known}
            <div class="flex flex-wrap gap-1 pt-1">
              {#if ai.model_info.supports_tools}<span class="gp-chip" data-tone="neutral" data-kind="label">tools</span>{/if}
              {#if ai.model_info.supports_vision}<span class="gp-chip" data-tone="neutral" data-kind="label">vision</span>{/if}
              {#if ai.model_info.supports_reasoning}<span class="gp-chip" data-tone="neutral" data-kind="label">reasoning</span>{/if}
            </div>
          {:else}
            <div class="text-textMuted">The server published no capability list.</div>
          {/if}
        </div>
      {:else if ai.model_detail}
        <div class="gp-notice" data-tone="warn" role="status">{ai.model_detail}</div>
      {/if}
    {:else}
      <Skeleton variant="text" count={2} />
    {/if}
  </section>

  <!-- Branch naming -->
  {#if $repoStore.currentPath}
    <section class="gp-card p-4 space-y-2">
      <h3 class="gp-label">Name a branch for the work in progress</h3>
      <div class="flex items-center gap-2">
        <button
          onclick={suggestBranch}
          disabled={isSuggesting || !ai?.ready}
          class="gp-btn"
          data-variant="primary"
        >
          <GitBranch size={13} />
          <span>{isSuggesting ? "Thinking…" : "Suggest a name"}</span>
        </button>
        {#if branchSuggestion}
          <input
            bind:value={branchSuggestion}
            aria-label="Suggested branch name"
            class="gp-field flex-1 min-w-0 font-mono"
          />
          <button
            onclick={createSuggestedBranch}
            class="gp-btn"
          >
            Create
          </button>
        {/if}
      </div>
      {#if branchError}
        <div class="gp-notice" data-tone="danger" role="alert">{branchError}</div>
      {/if}
      {#each branchWarnings as warning}
        <div class="gp-notice" data-tone="warn" role="status">{warning}</div>
      {/each}
    </section>
  {/if}

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
  <section id={manviSectionId("activity")} tabindex="-1" class="gp-card p-4 space-y-2">
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
