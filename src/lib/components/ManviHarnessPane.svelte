<script module lang="ts">
  import type { HarnessPermissionMode } from "../harness/availability";
  import type { CatchUp } from "../ingest/types";
  import type { LedgerStatus } from "../ledger/types";
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
      cardClass: "border-emerald-500/25 bg-emerald-500/5",
      badgeClass: "text-emerald-300",
    },
    warning: {
      cardClass: "border-amber-500/30 bg-amber-500/5",
      badgeClass: "text-amber-300",
    },
    error: {
      cardClass: "border-rose-500/30 bg-rose-500/5",
      badgeClass: "text-rose-300",
    },
    neutral: {
      cardClass: "border-border/70 bg-background",
      badgeClass: "text-textMuted",
    },
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
        `The latest MANVI status request failed: ${redactDiagnosticText(refreshError)} No runner capability is claimed until status is checked again.`,
        "error",
      );
    }
    switch (mode) {
      case "connected":
        return presentation(
          "Guarded",
          "Health and coverage commands require a click, a purpose allowlist, direct argv execution, the MANVI policy gate, hard timeouts, bounded output, and stop-on-failure accounting.",
          "ready",
        );
      case "unguarded":
        return presentation(
          "Not checked",
          "User-started health and coverage commands still use a purpose allowlist, direct argv execution, hard timeouts, bounded output, and stop-on-failure accounting. MANVI is not installed, so no policy check runs.",
          "warning",
        );
      case "blocked":
        return presentation(
          "Blocked",
          "Guarded health and coverage commands are refused while MANVI policy checks are failing. Reconnect after the policy gate recovers.",
          "error",
        );
      default:
        return presentation(
          "Status unknown",
          "The MANVI policy capability has not been checked yet. User-started commands remain behind the purpose allowlist; reconnect to establish whether the policy gate can run.",
          "neutral",
        );
    }
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
          "Latest MANVI status request failed",
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
  import { onDestroy } from "svelte";
  import { createVisibleInterval } from "../dom/visibleInterval";
  import { harnessStore, verdictLabel, type AiSelection } from "../stores/harnessStore";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { copyText } from "../desktop/clipboard";
  import { invoke } from "@tauri-apps/api/core";
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
  import { manviSectionId } from "../ui/manviFocus";
  import { beginGeneration } from "../async/guard";
  import {
    harnessPermissionMode,
    harnessPermissionSummary,
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
  } from "lucide-svelte";

  let branchSuggestion = $state<string>("");
  let branchWarnings = $state<string[]>([]);
  let isSuggesting = $state(false);
  let branchError = $state<string | null>(null);
  let logCopied = $state(false);

  let ai = $derived($harnessStore.ai);
  let harness = $derived($harnessStore.harness);
  let harnessError = $derived($harnessStore.error);
  let permissionMode = $derived(harnessPermissionMode(harness));
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
  let grantsRepo: string | null = null;
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
      // display to keep current. `createVisibleInterval` starts, stops and
      // catches up on its own, so the scheduler seam stays a plain
      // setInterval/clearInterval pair for tests.
      setInterval: (callback, delayMs) => createVisibleInterval(callback, delayMs),
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
      grantsLoadError = redactDiagnosticText(formatError(error));
      if (showLoading) grants = null;
    } finally {
      if (!grantRequests.isCurrent(generation) || $repoStore.currentPath !== repo) return;
      grantsLoading = false;
    }
  }

  $effect(() => {
    const repo = $repoStore.currentPath;
    // Guarded on a genuine repo switch: this effect re-runs on every store
    // emission, and the ~6s status poll is one.
    if (repo === grantsRepo) return;
    grantsRepo = repo;
    if (!repo) {
      grantRequests.next();
      grants = null;
      grantsLoadError = null;
      grantsLoading = false;
      return;
    }
    void refreshGrants(repo, true);
  });

  // Grant consumption and expiry can change while this pane remains mounted.
  // Key the timer by repository rather than this effect's execution: repoStore
  // can publish unrelated same-path status changes more frequently than the
  // interval, and resetting on each one would starve this refresh forever.
  $effect(() => {
    const repo = $repoStore.currentPath;
    grantRefreshTimer.update(repo);
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
    interfaceStore.setTerminalDockOpen(true);
  }

  function actionTime(ts: number): string {
    return new Date(ts).toLocaleTimeString();
  }

  function verdictChip(status: string): string {
    switch (status) {
      case "blocked":
        return "bg-rose-500/20 text-rose-300 border-rose-500/40";
      case "granted":
        return "bg-purple-500/20 text-purple-300 border-purple-500/40";
      case "demoted":
        return "bg-blue-500/20 text-blue-300 border-blue-500/40";
      case "widened":
        return "bg-amber-500/20 text-amber-300 border-amber-500/40";
      case "degraded":
        return "bg-yellow-500/20 text-yellow-300 border-yellow-500/40";
      case "warned":
        return "bg-sky-500/15 text-sky-300 border-sky-500/30";
      case "allowed":
        return "bg-emerald-500/10 text-emerald-300 border-emerald-500/25";
      case "unchecked":
      default:
        return "bg-zinc-500/15 text-zinc-400 border-zinc-500/30";
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
      scanError = formatError(e);
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
    branchWarnings = [];
    try {
      const result = await harnessStore.suggestBranchName(path);
      branchSuggestion = result.text;
      branchWarnings = result.warnings;
    } catch (err: unknown) {
      branchError = formatError(err);
    } finally {
      isSuggesting = false;
    }
  }

  async function createSuggestedBranch() {
    if (!branchSuggestion) return;
    try {
      await repoStore.createBranch(branchSuggestion);
      branchSuggestion = "";
    } catch (err: unknown) {
      branchError = formatError(err);
    }
  }
</script>

<div class="space-y-4">
  <!-- Harness -->
  <section id={manviSectionId("harness")} tabindex="-1" class="gp-card p-4 space-y-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">MANVI harness</h3>
      <button
        onclick={() => harnessStore.reconnect()}
        class="gp-btn !py-1 !text-[11px]"
        title="Restart the MANVI sidecar and sweep for model servers again"
      >
        <RefreshCw size={12} class={$harnessStore.isProbing ? "animate-spin" : ""} />
        <span>Reconnect</span>
      </button>
    </div>
    {#if harnessError}
      <div class="rounded-xl border border-rose-500/30 bg-rose-500/5 p-3 space-y-1.5">
        <div class="flex items-center gap-2 text-rose-400 font-medium">
          <ShieldAlert size={14} />
          <span>Latest MANVI status request failed</span>
        </div>
        <p class="text-textMuted leading-relaxed break-words">
          {redactDiagnosticText(harnessError)} Any previously displayed connection state may be stale;
          press Reconnect to check again.
        </p>
      </div>
    {:else if permissionMode === "connected" && harness}
      <div class="rounded-xl border border-emerald-500/25 bg-emerald-500/5 p-3 space-y-1.5">
        <div class="flex items-center gap-2 text-emerald-400 font-medium">
          <ShieldCheck size={14} />
          <span>Connected — protocol {harness.protocol}, posture {harness.posture}</span>
        </div>
        <div class="font-mono text-[11px] text-textMuted break-all">{harness.binary}</div>
        <p class="text-textMuted leading-relaxed">
          Commits, pushes, merges, rebases, branch deletions, discards and conflict-editor
          saves are put to the harness's gates before they run. Hard rules — force pushes,
          verification bypass flags, writes to credential paths — are refused here.
        </p>
        <div class="flex flex-wrap gap-1 pt-1">
          {#each harness.ops as op}
            <span class="px-1.5 py-0.5 rounded-full bg-surfaceHover border border-border/80 font-mono text-[10px] text-textMuted">{op}</span>
          {/each}
        </div>
      </div>
    {:else if permissionMode === "unguarded"}
      <div class="rounded-xl border border-amber-500/25 bg-amber-500/5 p-3 space-y-1.5">
        <div class="flex items-center gap-2 text-amber-400 font-medium">
          <ShieldAlert size={14} />
          <span>MANVI is not installed</span>
        </div>
        <p class="text-textMuted leading-relaxed">
          {harnessPermissionSummary(harness)} Install MANVI, or point
          <span class="font-mono">GITPULSE_MANVI_BIN</span> at the binary, then press Reconnect.
        </p>
      </div>
    {:else if permissionMode === "blocked"}
      <div class="rounded-xl border border-rose-500/30 bg-rose-500/5 p-3 space-y-1.5">
        <div class="flex items-center gap-2 text-rose-400 font-medium">
          <ShieldAlert size={14} />
          <span>Policy gate failed — mutations blocked</span>
        </div>
        <p class="text-textMuted leading-relaxed">{harnessPermissionSummary(harness)}</p>
        <p class="text-textMuted leading-relaxed">
          Press Reconnect after resolving the sidecar failure. GitPulse will not treat a timed-out,
          busy, unavailable, or incompatible policy check as approval.
        </p>
      </div>
    {:else}
      <div class="rounded-xl border border-border/70 bg-background p-3 text-textMuted">
        {harnessPermissionSummary(harness)}
      </div>
    {/if}
  </section>

  <!-- The live `hello` response is intentionally narrow: GitPulse embeds
       MANVI's policy and local-model planes only. This card keeps that wire
       truth separate from app-owned, user-confirmed command execution. -->
  <section class="gp-card p-4 space-y-3">
    <div class="space-y-1">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Capability boundary</h3>
      <p class="text-textMuted leading-relaxed">
        No autonomous PTY or app-control API is exposed by <span class="font-mono">manvi serve</span>.
        GitPulse adds narrow, explicit controls around the planes the sidecar actually provides.
      </p>
    </div>

    <div class="grid gap-2 sm:grid-cols-2">
      <div class="rounded-xl border border-border/70 bg-background p-3 space-y-1">
        <div class="flex items-center gap-2 text-textPrimary font-medium">
          <SquareTerminal size={13} class="text-textMuted" />
          <span>Interactive shell</span>
          <span class="ml-auto text-[10px] uppercase text-amber-300">User only</span>
        </div>
        <p class="text-textMuted leading-relaxed">
          MANVI never receives the PTY handle or keystrokes. Shell commands are typed and owned by you.
        </p>
      </div>
      <div class="rounded-xl border p-3 space-y-1 {runnerPresentation.cardClass}">
        <div class="flex items-center gap-2 text-textPrimary font-medium">
          {#if runnerPresentation.tone === "ready"}
            <ShieldCheck size={13} class={runnerPresentation.badgeClass} />
          {:else}
            <ShieldAlert size={13} class={runnerPresentation.badgeClass} />
          {/if}
          <span>Scoped action runner</span>
          <span class="ml-auto text-[10px] uppercase {runnerPresentation.badgeClass}">{runnerPresentation.label}</span>
        </div>
        <p class="text-textMuted leading-relaxed">
          {runnerPresentation.detail}
        </p>
      </div>
    </div>

    <div class="grid grid-cols-2 sm:grid-cols-4 gap-2">
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("health")}
        title="Scan dependencies, ask MANVI for a remediation plan, and run approved steps"
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

  <!-- Model servers -->
  <section id={manviSectionId("model")} tabindex="-1" class="gp-card p-4 space-y-2">
    <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Local model servers</h3>
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
          class="text-[10px] text-textMuted hover:text-accent underline inline-flex items-center gap-1 disabled:opacity-50"
          title="Probe the well-known local endpoints and list what each server serves"
        >
          <RefreshCw size={11} class={scanning ? "animate-spin" : ""} />
          <span>{scanning ? "Scanning…" : "Scan local servers"}</span>
        </button>
        {#if scan}
          <span class="text-[10px] text-textMuted">{sweepSummary(scan)}</span>
        {/if}
      </div>

      {#if scanError}
        <div class="rounded-xl border border-amber-500/40 bg-amber-500/5 p-2.5 text-[11px] text-amber-400">
          The scan did not complete, so this is not a list of what is running: {scanError}
        </div>
      {:else if scan && scan.servers.length > 0}
        <div class="space-y-1.5">
          {#each scan.servers as server (server.base_url)}
            <div class="rounded-xl border border-border/70 bg-background p-2.5 space-y-1">
              <div class="flex items-center gap-2 text-[11px]">
                <Server size={12} class="text-accent shrink-0" />
                <span class="font-mono truncate">{server.base_url}</span>
                <span class="shrink-0 text-[9px] uppercase rounded-full bg-surfaceHover border border-border/80 px-1.5 text-textMuted">
                  {server.runtime}
                </span>
                {#if server.version}
                  <span class="shrink-0 text-[9px] text-textMuted">v{server.version}</span>
                {/if}
              </div>
              {#each server.models as model (model.id)}
                <button
                  onclick={() => pick({ base_url: server.base_url, model: model.id })}
                  class="w-full px-2 py-1 rounded-lg flex items-center gap-2 text-left text-[10px] transition-colors
                    {isSelected(server.base_url, model.id)
                      ? 'bg-accent/15 text-accent'
                      : 'hover:bg-surfaceHover text-textPrimary'}
                    {usable(model) ? '' : 'opacity-50'}"
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
                    class="shrink-0 text-[9px] rounded-full px-1.5 border
                      {!model.capabilities_known
                        ? 'text-textMuted border-border/80'
                        : model.supports_tools
                          ? 'text-accent border-accent/40'
                          : 'text-textMuted border-border/80'}"
                  >{toolSupportLabel(model)}</span>
                </button>
              {/each}
            </div>
          {/each}
        </div>
      {/if}

      {#if ai.endpoints.filter((e) => e.reachable).length === 0}
        <div class="rounded-xl border border-border/70 bg-background p-3 text-textMuted leading-relaxed">
          {ai.detail || "No local model server answered."}
          <p class="mt-1.5">
            GitPulse only ever talks to a model server on this machine: the transport refuses
            any address that is not loopback, so a diff cannot leave the machine through a
            mistyped setting.
          </p>
        </div>
      {:else}
        <div class="space-y-2">
          {#each ai.endpoints.filter((e) => e.reachable) as endpoint}
            <div class="rounded-xl border border-border/70 bg-background p-3 space-y-2">
              <div class="flex items-center gap-2 text-textPrimary font-medium">
                <Server size={13} class="text-accent" />
                <span class="font-mono text-[11px]">{endpoint.base_url}</span>
              </div>
              <div class="grid grid-cols-1 gap-1">
                {#each endpoint.models as model}
                  <button
                    onclick={() => pick({ base_url: endpoint.base_url, model })}
                    class="px-2.5 py-1.5 rounded-full flex items-center justify-between gap-2 text-left transition-colors
                      {isSelected(endpoint.base_url, model)
                        ? 'bg-accent/15 text-accent border border-accent/40'
                        : 'hover:bg-surfaceHover border border-transparent text-textPrimary'}"
                  >
                    <span class="font-mono text-[11px] truncate">{model}</span>
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
              class="text-[11px] text-textMuted hover:text-textPrimary underline"
            >
              Clear the pinned model and let discovery choose
            </button>
          {/if}
        </div>
      {/if}

      {#if ai.model_info}
        <div class="rounded-xl border border-border/70 bg-background p-3 space-y-1">
          <div class="text-textPrimary font-medium">{ai.model_info.model}</div>
          <!-- Provenance, not just a number: a window read off the server
               and one typed into a default produce the same request and
               completely different confidence. -->
          <div class="text-textMuted">
            Context: {ai.model_info.describe}
            {#if !ai.model_info.discovered}
              <span class="text-amber-400"> (declared, not discovered)</span>
            {/if}
          </div>
          {#if ai.model_info.capabilities_known}
            <div class="flex flex-wrap gap-1 pt-1">
              {#if ai.model_info.supports_tools}<span class="px-1.5 py-0.5 rounded-full bg-surfaceHover border border-border/80 text-[10px]">tools</span>{/if}
              {#if ai.model_info.supports_vision}<span class="px-1.5 py-0.5 rounded-full bg-surfaceHover border border-border/80 text-[10px]">vision</span>{/if}
              {#if ai.model_info.supports_reasoning}<span class="px-1.5 py-0.5 rounded-full bg-surfaceHover border border-border/80 text-[10px]">reasoning</span>{/if}
            </div>
          {:else}
            <div class="text-textMuted">The server published no capability list.</div>
          {/if}
        </div>
      {:else if ai.model_detail}
        <div class="text-amber-400">{ai.model_detail}</div>
      {/if}
    {:else}
      <div class="text-textMuted">Probing…</div>
    {/if}
  </section>

  <!-- Branch naming -->
  {#if $repoStore.currentPath}
    <section class="gp-card p-4 space-y-2">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Name a branch for the work in progress</h3>
      <div class="flex items-center gap-2">
        <button
          onclick={suggestBranch}
          disabled={isSuggesting || !ai?.ready}
          class="gp-btn-primary"
        >
          <GitBranch size={13} />
          <span>{isSuggesting ? "Thinking…" : "Suggest a name"}</span>
        </button>
        {#if branchSuggestion}
          <input
            bind:value={branchSuggestion}
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
        <div class="text-rose-400">{branchError}</div>
      {/if}
      {#each branchWarnings as warning}
        <div class="text-amber-400">{warning}</div>
      {/each}
    </section>
  {/if}

  <!-- Grant ledger: who waived which rule, and until when -->
  {#if grantsLoading || grantsLoadError || grants?.available}
    <section class="gp-card p-4 space-y-2">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted flex items-center gap-1.5">
        <ShieldAlert size={11} />
        <span>
          {grants?.available
            ? `Grants (${activeGrantCount} active of ${grants.grants.length})`
            : "Grants"}
        </span>
      </h3>
      {#if grantsLoading}
        <div class="rounded-xl border border-border/70 bg-background p-3 text-textMuted leading-relaxed" role="status">
          Checking grant history…
        </div>
      {:else}
        {#if grantsLoadError}
          <div class="rounded-xl border border-rose-500/40 bg-rose-500/5 p-3 text-[11px] text-rose-400 leading-relaxed" role="alert">
            Grant history could not be refreshed. {grants
              ? "The last verified snapshot remains below."
              : "No authority state is inferred."} {grantsLoadError}
          </div>
        {/if}
        {#if grants?.error}
          <!--
            Invalid entries are refused by the backend, while valid entries
            remain visible. The warning and list must therefore be independent.
          -->
          <div class="rounded-xl border border-amber-500/40 bg-amber-500/5 p-3 text-[11px] text-amber-400 leading-relaxed">
            Some grant records were refused, so this list is not the whole
            truth: {redactDiagnosticText(grants.error)}
          </div>
        {/if}
        {#if grants?.grants.length === 0}
          <div class="rounded-xl border border-border/70 bg-background p-3 text-textMuted leading-relaxed">
            {grants.error
              ? "No valid grant record could be displayed."
              : "No rule has been waived in this repository."}
          </div>
        {:else if grants && grants.grants.length > 0}
          <div class="rounded-xl border border-border/70 bg-background divide-y divide-border/40 max-h-40 overflow-y-auto">
            {#each orderedGrants as grant (grant.id)}
              <div class="px-3 py-1.5 flex flex-col gap-0.5 text-[11px]">
                <div class="flex items-center gap-1.5 min-w-0">
                  <span class="font-mono text-[10px] text-accent truncate">
                    {grant.scope.rules.length > 0 ? grant.scope.rules.join(", ") : "invalid scope: no policy rule"}
                  </span>
                  {#if grant.scope.once}
                    <span class="shrink-0 text-[9px] uppercase text-textMuted">one use</span>
                  {/if}
                  <span class="ml-auto shrink-0 text-[9px] uppercase text-textMuted">
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
          <p class="text-[10px] text-textMuted">
            Current MANVI CLI has no grant-revocation command. A grant stops applying after it is consumed or expires.
            GitPulse only reads the ledger at <code class="font-mono break-all">{grants.path}</code>.
          </p>
        {/if}
      {/if}
    </section>
  {/if}

  <!-- Agent activity journal -->
  <section id={manviSectionId("activity")} tabindex="-1" class="gp-card p-4 space-y-2">
    <div class="flex items-center justify-between">
      <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted flex items-center gap-1.5">
        <History size={11} />
        <span>Agent activity ({recentActions.length})</span>
      </h3>
      <div class="flex items-center gap-3">
        {#if recentActions.length > 0}
          <button
            onclick={() => void copyLog()}
            class="text-[10px] text-textMuted hover:text-textPrimary underline inline-flex items-center gap-1"
            title="Copy the activity log to the clipboard"
          >
            <Clipboard size={11} />
            <span>{logCopied ? "Copied" : "Copy log"}</span>
          </button>
          <button
            onclick={() => harnessStore.clearActions()}
            class="text-[10px] text-textMuted hover:text-textPrimary underline"
          >
            Clear
          </button>
        {/if}
      </div>
    </div>
    <div class="grid gap-2 md:grid-cols-2" aria-label="Activity history status">
      {#each activityHistory as status (status.label)}
        <div class="rounded-xl border p-3 {status.cardClass}" role="status">
          <div class="text-[10px] font-semibold uppercase tracking-wider {status.badgeClass}">
            {status.label}
          </div>
          <p class="mt-1 text-[11px] text-textMuted leading-relaxed break-words">
            {status.detail}
          </p>
        </div>
      {/each}
    </div>
    {#if recentActions.length === 0}
      <div class="rounded-xl border border-border/70 bg-background p-3 text-textMuted leading-relaxed">
        No activity rows are currently available. The history status above says whether this
        means no recorded actions or incomplete data.
      </div>
    {:else}
      <div class="rounded-xl border border-border/70 bg-background divide-y divide-border/40 max-h-56 overflow-y-auto">
          {#each recentActions as action (action.identity)}
          <div class="px-3 py-1.5 flex flex-col gap-0.5 text-[11px]" title={action.verdict?.detail ?? action.label}>
            <div class="flex items-center gap-2">
              <span class="font-mono text-[10px] text-textMuted shrink-0">{actionTime(action.ts)}</span>
              <span class="shrink-0 px-1.5 py-0.5 rounded-full font-mono text-[9px] uppercase border {verdictChip(
                !action.ok ? 'blocked' : (action.verdict?.status ?? 'unchecked')
              )}">
                {action.verdict?.status ?? action.kind}
              </span>
              <span class="truncate {action.ok ? 'text-textPrimary' : 'text-rose-400'}">{action.label || "—"}</span>
              {#if action.verdict?.task_id}
                <span class="ml-auto font-mono text-[9px] px-1.5 py-0.2 rounded bg-surfaceHover text-textMuted shrink-0">
                  {action.verdict.task_id}
                </span>
              {/if}
            </div>
            {#if action.verdict?.grant_id}
              <div class="text-[10px] text-purple-300/80 font-mono pl-14">
                Grant {action.verdict.grant_id} by {action.verdict.granted_by || "human"} · {action.verdict.reason || "waived"}
              </div>
            {:else if action.verdict?.demoted}
              <div class="text-[10px] text-blue-300/80 font-mono pl-14">
                Posture demoted: {action.verdict.demoted}
              </div>
            {:else if action.verdict?.widened}
              <div class="text-[10px] text-amber-300/80 font-mono pl-14">
                Scope widened: {action.verdict.widened}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </section>
</div>
