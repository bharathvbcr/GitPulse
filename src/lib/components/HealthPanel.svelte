<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type {
    DepsHealthReport,
    DependabotReport,
  } from "../health/types";

  // Survives the per-tab remount so revisiting the Health view renders the
  // last scan instantly; the fetch then refreshes it in place.
  const healthCache = createRepoPanelCache<{
    deps: DepsHealthReport;
    dependabot: DependabotReport;
  }>();
</script>

<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { openExternal as openExternalUrl } from "../desktop/openExternal";
  import {
    ShieldAlert,
    RefreshCw,
    ExternalLink,
    Package,
    AlertTriangle,
    Clipboard,
    LoaderCircle,
    Sparkles,
    Play,
    Check,
    Terminal,
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import {
    harnessStore,
    type AiGeneration,
    type PolicyVerdict,
  } from "../stores/harnessStore";
  import { copyText } from "../desktop/clipboard";
  import { formatHealthReport, observedTotal, skippedAudits } from "../health/report";
  import { buildRunnablePlanSteps } from "../terminal/tokenize";
  import type {
    Vulnerability,
  } from "../health/types";
  import {
    dependabotBadgeClass as badgeClassFor,
    formatAuditCounts,
    issueClass,
    severityClass,
    updateKind,
    updateKindClass,
  } from "../health/format";
  import { formatError } from "../ui/formatError";
  import { reportPanelError } from "../diagnostics/report";
  import Skeleton from "./Skeleton.svelte";

  let report = $state<DepsHealthReport | null>(null);
  let dependabot = $state<DependabotReport | null>(null);
  let loading = $state(false);
  let errorMsg = $state<string | null>(null);
  /**
   * Failures of in-panel actions (opening an advisory link). Deliberately NOT
   * `errorMsg`: that one guards the "load failed" branch, so reusing it made
   * one failed link click swap the whole report for a bare banner.
   */
  let actionError = $state<string | null>(null);
  let filter = $state<"all" | "direct">("all");
  let copied = $state(false);
  let fixing = $state(false);
  let plan = $state<AiGeneration | null>(null);
  let planError = $state<string | null>(null);
  let planCopied = $state(false);

  interface RunnableStep {
    id: string;
    number: number | null;
    text: string;
    command: string | null;
    argv: string[] | null;
    error?: string;
  }

  let planSteps = $derived.by<RunnableStep[]>(() => {
    if (!plan?.text) return [];
    return buildRunnablePlanSteps(plan.text);
  });

  let stepResults = $state<
    Record<
      string,
      {
        running: boolean;
        status?: "passed" | "failed";
        detail?: string;
        duration_ms?: number;
      }
    >
  >({});
  let runningAll = $state(false);

  let visibleVulns = $derived.by(() => {
    const current = report;
    if (!current) return [] as Vulnerability[];
    if (filter === "direct") {
      return current.vulnerabilities.filter((v) => v.is_direct);
    }
    return current.vulnerabilities;
  });

  /** Audits that could not run because their CLI was missing. */
  let skipped = $derived(report ? skippedAudits(report) : []);

  /** A clean result is valid only after every discovered supported target ran. */
  let auditComplete = $derived(report?.audit_complete === true);
  let auditsRan = $derived((report?.scanners_ran ?? []).length > 0);

  /** Open Dependabot alerts, for the header badge. */
  let openDependabotCount = $derived(
    dependabot?.available ? dependabot.alerts.length : 0,
  );
  let dependabotBadgeClass = $derived(
    dependabot?.available ? badgeClassFor(dependabot.alerts) : "",
  );
  // Observed totals, not surviving-row counts: a capped table that prints only
  // what it kept reads as complete coverage. Shared with the copied report so
  // the screen and the clipboard cannot disagree.
  let outdatedTotal = $derived(
    report ? observedTotal(report, "outdated npm packages", report.outdated.length) : 0,
  );
  let issuesTotal = $derived(
    report ? observedTotal(report, "health issues", report.issues.length) : 0,
  );
  let vulnerabilitiesTotal = $derived(
    report ? Math.max(report.audit.total, report.vulnerabilities.length) : 0,
  );

  const scanned = { path: "" };
  let inflight: AsyncGuard | null = null;
  let fixInflight: AsyncGuard | null = null;
  /**
   * Guards the sequential step runner. Destroying this component does not
   * abort its in-flight async loops: without a cancellation check between
   * steps, "Run all" kept executing the remaining commands against whatever
   * repository became current after a switch.
   */
  let stepsInflight: AsyncGuard | null = null;

  function beginSteps(): AsyncGuard {
    stepsInflight?.cancel();
    const guard = createAsyncGuard();
    stepsInflight = guard;
    return guard;
  }

  async function scan(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    errorMsg = null;
    actionError = null;
    // Both sources run together so the Health page fills in as one picture,
    // and each settles independently: a failed Dependabot fetch must not
    // erase a finished local scan, or the reverse.
    const [deps, alerts] = await Promise.allSettled([
      invoke<DepsHealthReport>("cmd_scan_deps_health", { repoPath }),
      invoke<DependabotReport>("cmd_github_dependabot_alerts", { repoPath }),
    ]);
    if (!guard.isLive()) return;
    if (deps.status === "fulfilled") {
      report = deps.value;
      // A new scan supersedes whatever a plan was written against.
      plan = null;
      planError = null;
    } else {
      errorMsg = formatError(deps.reason);
      report = null;
      // A failed scan must not mark the repo as scanned, or the effect above
      // would refuse to rescan it after something changes.
      scanned.path = "";
    }
    // The Dependabot command reports its own unavailable/error states; only
    // an IPC-level failure is folded into that same shape here, so "could
    // not check" never renders as a clean bill of health.
    dependabot =
      alerts.status === "fulfilled"
        ? alerts.value
        : {
            available: false,
            cli_present: false,
            is_github_remote: true,
            slug: "",
            alerts: [],
            truncated: false,
            error: formatError(alerts.reason),
          };
    if (deps.status === "fulfilled") {
      healthCache.set(repoPath, { deps: deps.value, dependabot });
    }
    if (guard.isLive()) loading = false;
  }

  /** The rendered text behind both "Copy report" and "Fix with MANVI". */
  function renderedReport(): string | null {
    const current = report;
    const repoPath = $repoStore.currentPath;
    if (!current || !repoPath) return null;
    return formatHealthReport(current, repoPath, dependabot);
  }

  let copyTimer: number | null = null;
  let planCopyTimer: number | null = null;

  async function copyReport() {
    const text = renderedReport();
    if (!text) return;
    if (await copyText(text)) {
      copied = true;
      if (copyTimer !== null) window.clearTimeout(copyTimer);
      copyTimer = window.setTimeout(() => (copied = false), 1500);
    }
  }

  /**
   * Sends the health report through the harness's local-AI plane for a
   * remediation plan.
   */
  async function fixWithManvi() {
    const text = renderedReport();
    if (!text || fixing) return;
    fixInflight?.cancel();
    const guard = createAsyncGuard();
    fixInflight = guard;
    fixing = true;
    planError = null;
    plan = null;
    stepResults = {};
    try {
      const next = await harnessStore.fixHealth($repoStore.currentPath!, text);
      if (!guard.isLive()) return;
      plan = next;
    } catch (err) {
      if (!guard.isLive()) return;
      planError = formatError(err);
    } finally {
      if (guard.isLive()) fixing = false;
    }
  }

  async function runStep(step: RunnableStep, guard: AsyncGuard): Promise<boolean> {
    const repoPath = $repoStore.currentPath;
    if (!step.argv || step.argv.length === 0 || !repoPath) return false;
    stepResults[step.id] = { running: true };

    try {
      // Wire shape of `crate::terminal::TerminalRunResult`.
      const res = await invoke<{
        command: string;
        gated: boolean;
        policy?: PolicyVerdict | null;
        timed_out: boolean;
        exit_code: number | null;
        stdout_tail: string;
        stderr_tail: string;
        truncated: boolean;
        duration_ms: number;
      }>("cmd_manvi_run_action", {
        repoPath,
        args: step.argv,
        actionKind: "health",
        // Long enough for a cold install/build; the backend clamps to [1s, 30min].
        timeoutSecs: 600,
      });
      if (!guard.isLive()) return false;

      const passed = !res.timed_out && res.exit_code === 0;
      stepResults[step.id] = {
        running: false,
        status: passed ? "passed" : "failed",
        detail: res.timed_out
          ? "Timed out and was killed."
          : passed
            ? res.stdout_tail || "Command completed successfully (exit 0)"
            : res.stderr_tail || res.stdout_tail || `Command failed (exit ${res.exit_code ?? "?"})`,
        duration_ms: res.duration_ms,
      };

      harnessStore.recordAction({
        kind: "remediation-step",
        label: step.command ?? step.text,
        ok: passed,
        verdict: res.policy ?? null,
      });

      return passed;
    } catch (err) {
      if (!guard.isLive()) return false;
      const msg = formatError(err);
      stepResults[step.id] = {
        running: false,
        status: "failed",
        detail: msg,
      };

      harnessStore.recordAction({
        kind: "remediation-step",
        label: step.command ?? step.text,
        ok: false,
      });

      return false;
    }
  }

  async function runAllSteps() {
    if (runningAll) return;
    runningAll = true;
    const guard = beginSteps();
    try {
      for (const step of planSteps) {
        if (!guard.isLive()) break;
        if (step.argv && step.argv.length > 0) {
          const ok = await runStep(step, guard);
          if (!ok) {
            // Stop sequential execution on failure
            break;
          }
        }
      }
    } finally {
      runningAll = false;
    }
  }

  async function copyPlan() {
    if (!plan?.text) return;
    if (await copyText(plan.text)) {
      planCopied = true;
      if (planCopyTimer !== null) window.clearTimeout(planCopyTimer);
      planCopyTimer = window.setTimeout(() => (planCopied = false), 1500);
    }
  }

  let aiReady = $derived($harnessStore.ai?.ready ?? false);

  $effect(() => {
    return () => {
      inflight?.cancel();
      fixInflight?.cancel();
      stepsInflight?.cancel();
      if (copyTimer !== null) window.clearTimeout(copyTimer);
      if (planCopyTimer !== null) window.clearTimeout(planCopyTimer);
    };
  });

  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path) {
      inflight?.cancel();
      fixInflight?.cancel();
      stepsInflight?.cancel();
      scanned.path = "";
      report = null;
      dependabot = null;
      errorMsg = null;
      actionError = null;
      loading = false;
      plan = null;
      planError = null;
      return;
    }
    if (path === scanned.path) return;
    scanned.path = path;
    // Hydrate last-known data synchronously so a revisit renders instantly
    // (the placeholder below only fires when there is no cached report).
    const cached = healthCache.get(path);
    if (cached) {
      report = cached.deps;
      dependabot = cached.dependabot;
    }
    void scan(path);
  });

  async function openExternal(url: string) {
    // The shared opener throws on failure (no window.open fallback: inside a
    // Tauri webview it can navigate the app shell itself). Surface it
    // in-place on the panel error banner and in the diagnostics ring.
    try {
      await openExternalUrl(url);
    } catch (err) {
      actionError = reportPanelError("health", err);
    }
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans overflow-hidden">
  {#snippet dependabotSection()}
    {#if dependabot && (dependabot.available || dependabot.error)}
      <section class="space-y-2">
        <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
          GitHub Dependabot{dependabot.available
            ? ` (${dependabot.alerts.length}${dependabot.truncated ? "+" : ""})`
            : ""}
        </h3>
        {#if !dependabot.cli_present}
          <p class="text-textMuted max-w-2xl">
            Install the <span class="font-mono">gh</span> CLI and run
            <span class="font-mono">gh auth login</span> to fetch Dependabot alerts for
            {dependabot.slug || "this repository"}.
          </p>
        {:else if dependabot.error}
          <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-200 max-w-2xl">
            Could not fetch Dependabot alerts: {dependabot.error}
          </div>
        {:else if dependabot.alerts.length === 0}
          <p class="text-textMuted">No open Dependabot alerts on {dependabot.slug}.</p>
        {:else}
          {#if dependabot.truncated}
            <p class="text-amber-300">Showing the first {dependabot.alerts.length} alerts.</p>
          {/if}
          <div class="border border-border/70 rounded-2xl overflow-hidden max-w-5xl shadow-card">
            <table class="w-full text-left">
              <thead class="bg-surface text-[10px] uppercase text-textMuted">
                <tr>
                  <th class="px-3 py-2 font-medium">Severity</th>
                  <th class="px-3 py-2 font-medium">Package</th>
                  <th class="px-3 py-2 font-medium">Advisory</th>
                  <th class="px-3 py-2 font-medium">Fix</th>
                  <th class="px-3 py-2 font-medium w-8"></th>
                </tr>
              </thead>
              <tbody>
                {#each dependabot.alerts as alert}
                  <tr class="border-t border-border/40 align-top">
                    <td class="px-3 py-1.5">
                      <span class="px-1.5 py-0.5 rounded-full text-[10px] uppercase font-semibold {severityClass(alert.severity)}">{alert.severity || "unranked"}</span>
                    </td>
                    <td class="px-3 py-1.5">
                      <div class="font-mono text-textPrimary">{alert.package}</div>
                      <div class="text-[10px] text-textMuted">
                        {alert.ecosystem}{alert.scope ? ` · ${alert.scope}` : ""}
                        {#if alert.manifest_path} · {alert.manifest_path}{/if}
                        {#if alert.vulnerable_range} · {alert.vulnerable_range}{/if}
                      </div>
                    </td>
                    <td class="px-3 py-1.5 text-textPrimary">
                      {alert.title}
                      {#if alert.advisory_id || alert.cve_id}
                        <div class="text-[10px] font-mono text-textMuted">
                          {[alert.advisory_id, alert.cve_id].filter(Boolean).join(" · ")}
                        </div>
                      {/if}
                    </td>
                    <td class="px-3 py-1.5 font-mono text-textMuted">{alert.first_patched || "no fix yet"}</td>
                    <td class="px-2 py-1.5">
                      {#if alert.url}
                        <button
                          type="button"
                          class="p-1 rounded-full hover:bg-surfaceHover text-textMuted hover:text-accent transition-colors"
                          title="Open alert on GitHub"
                          onclick={() => openExternal(alert.url)}
                        >
                          <ExternalLink size={13} />
                        </button>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    {/if}
  {/snippet}

  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between shrink-0">
    <div class="flex items-center gap-2 min-w-0">
      <ShieldAlert size={16} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary">Health</span>
      {#if report}
        <span class="text-textMuted truncate">
          Local: {formatAuditCounts(report.audit, { complete: auditComplete, ran: auditsRan })}
          {#if outdatedTotal > 0}
            · {outdatedTotal} outdated npm
          {/if}
        </span>
      {/if}
      {#if openDependabotCount > 0}
        <span class={`truncate ${dependabotBadgeClass}`}>
          · Dependabot {openDependabotCount}{dependabot?.truncated ? "+" : ""}
        </span>
      {:else if dependabot && !dependabot.available}
        <span class="truncate text-amber-300">· Dependabot unavailable</span>
      {/if}
    </div>
    <div class="flex items-center gap-2">
      {#if report}
        <span class="text-[11px] text-textMuted font-mono">
          {report.node_version ? `node ${report.node_version}` : "node —"}
          ·
          {report.npm_version ? `npm ${report.npm_version}` : "npm —"}
        </span>
        <button
          type="button"
          onclick={copyReport}
          class="gp-btn"
          title="Copy the full health report as text"
        >
          <Clipboard size={13} />
          {copied ? "Copied" : "Copy report"}
        </button>
        <button
          type="button"
          onclick={fixWithManvi}
          disabled={fixing}
          class="gp-btn-primary"
          title={aiReady
            ? "Ask the local model (via the MANVI harness) for a remediation plan"
            : "Needs a local model server — see the MANVI view. The exact error will be reported if none is running."}
        >
          {#if fixing}
            <LoaderCircle size={13} class="animate-spin" />
            Planning…
          {:else}
            <Sparkles size={13} />
            Fix with MANVI
          {/if}
        </button>
      {/if}
      <button
        type="button"
        onclick={() => scan()}
        disabled={loading}
        class="gp-btn disabled:opacity-40 disabled:cursor-not-allowed"
        title="Rescan vulnerabilities and updates"
      >
        <RefreshCw size={13} class={loading ? "animate-spin" : ""} />
        Scan
      </button>
    </div>
  </div>

  <div class="flex-1 overflow-auto p-4 space-y-5">
    <!-- Non-fatal action failures render ahead of the state chain so they stay
         visible in every state instead of shadowing (or being shadowed by) the
         load-error branch. Same shape as GitHubPanel's actionError banner. -->
    {#if actionError}
      <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-200 max-w-2xl">
        {actionError}
      </div>
    {/if}
    {#if loading && !report}
      <div class="space-y-4 max-w-4xl">
        <div class="grid grid-cols-1 md:grid-cols-3 gap-3">
          <Skeleton variant="card" count={3} />
        </div>
        <div class="space-y-2 pt-2">
          <Skeleton variant="text" count={4} height="2rem" />
        </div>
      </div>
    {:else if errorMsg}
      <div class="p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-200 max-w-2xl">
        {errorMsg}
      </div>
      {@render dependabotSection()}
    {:else if report}
      {#if planError}
        <div class="p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-200 max-w-3xl">
          Fix with MANVI failed: {planError}
        </div>
      {/if}

      {#if plan || fixing}
        <section class="space-y-3 max-w-4xl rounded-2xl border border-accent/30 bg-surface shadow-card p-4">
          <div class="flex items-center justify-between gap-2">
            <h3 class="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-textMuted">
              <Sparkles size={11} class="text-accent" />
              MANVI remediation plan
            </h3>
            <div class="flex items-center gap-2">
              {#if plan}
                {#if planSteps.some((s) => s.argv)}
                  <button
                    type="button"
                    onclick={runAllSteps}
                    disabled={runningAll || Object.values(stepResults).some((r) => r.running)}
                    class="gp-btn-primary !py-1 !text-[11px]"
                    title="Execute all executable plan steps sequentially"
                  >
                    {#if runningAll}
                      <LoaderCircle size={12} class="animate-spin" />
                      <span>Running all…</span>
                    {:else}
                      <Play size={12} />
                      <span>Run all steps</span>
                    {/if}
                  </button>
                {/if}
                <button
                  type="button"
                  onclick={() => repoStore.setActiveTab("terminal")}
                  class="gp-btn !py-1 !text-[11px]"
                  title="Open Terminal view"
                >
                  <Terminal size={12} />
                  <span>Terminal</span>
                </button>
                <button type="button" onclick={copyPlan} class="gp-btn !py-1 !text-[11px]" title="Copy the remediation plan">
                  <Clipboard size={12} />
                  {planCopied ? "Copied" : "Copy plan"}
                </button>
              {/if}
            </div>
          </div>
          {#if fixing && !plan}
            <div class="flex items-center gap-2 text-textMuted py-2">
              <LoaderCircle size={14} class="animate-spin" />
              Sending the health report to the local model…
            </div>
          {:else if plan}
            <p class="text-[11px] text-textMuted font-mono truncate">
              {plan.model} @ {plan.base_url} · {plan.elapsed_ms} ms
            </p>
            {#each plan.warnings as warning}
              <div class="text-amber-400 leading-relaxed">{warning}</div>
            {/each}

            {#if planSteps.length > 0}
              <div class="space-y-2.5 pt-1">
                {#each planSteps as step (step.id)}
                  {@const res = stepResults[step.id]}
                  <div class="p-3 rounded-xl border border-border/70 bg-background/60 space-y-2">
                    <div class="flex items-start justify-between gap-2">
                      <div class="space-y-1 min-w-0">
                        <div class="flex items-center gap-2">
                          {#if step.number !== null}
                            <span class="px-1.5 py-0.2 rounded bg-surface border border-border text-[10px] font-bold text-accent">
                              {step.number}
                            </span>
                          {/if}
                          <span class="font-medium text-textPrimary text-xs">{step.text}</span>
                        </div>
                      </div>

                      {#if step.argv}
                        <button
                          type="button"
                          onclick={() => void runStep(step, beginSteps())}
                          disabled={res?.running || runningAll}
                          class="gp-btn !py-1 !px-2.5 text-xs shrink-0 disabled:opacity-50"
                          title="Execute this command step directly"
                        >
                          {#if res?.running}
                            <LoaderCircle size={12} class="animate-spin text-accent" />
                            <span>Running…</span>
                          {:else if res?.status === "passed"}
                            <Check size={12} class="text-emerald-400" />
                            <span>Run again</span>
                          {:else if res?.status === "failed"}
                            <Play size={12} class="text-rose-400" />
                            <span>Retry</span>
                          {:else}
                            <Play size={12} class="text-accent" />
                            <span>Run</span>
                          {/if}
                        </button>
                      {/if}
                    </div>

                    {#if step.command}
                      <div class="flex items-center justify-between gap-2 px-2.5 py-1.5 rounded-lg bg-surface border border-border/60 font-mono text-[11px]">
                        <span class="text-textPrimary truncate">{step.command}</span>
                        {#if res?.status}
                          <span class="px-1.5 py-0.5 rounded text-[9px] font-bold uppercase shrink-0 {res.status === 'passed' ? 'bg-emerald-500/10 text-emerald-300 border border-emerald-500/30' : 'bg-rose-500/10 text-rose-300 border border-rose-500/30'}">
                            {res.status} {res.duration_ms ? `(${res.duration_ms}ms)` : ""}
                          </span>
                        {/if}
                      </div>
                    {/if}

                    {#if step.error}
                      <div class="text-[10px] text-amber-300">
                        {step.error}
                      </div>
                    {/if}

                    {#if res?.detail}
                      <div class="p-2 rounded bg-surface/80 border border-border/40 font-mono text-[10px] text-textMuted whitespace-pre-wrap max-h-32 overflow-y-auto">
                        {res.detail}
                      </div>
                    {/if}
                  </div>
                {/each}
              </div>
            {:else}
              <div class="whitespace-pre-wrap leading-relaxed text-textSecondary">{plan.text}</div>
            {/if}

            <div class="pt-2 border-t border-border/40 flex items-center justify-between text-textMuted text-[11px]">
              <p>Review each remediation step. Run commands individually or execute all sequentially.</p>
              <button
                type="button"
                onclick={() => void scan()}
                disabled={loading}
                class="gp-btn !py-1 text-xs shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
                title="Rescan repository health"
              >
                <RefreshCw size={11} class={loading ? "animate-spin" : ""} />
                <span>Rescan Health</span>
              </button>
            </div>
          {/if}
        </section>
      {/if}

      {#if report.truncated}
        <div class="text-amber-300 space-y-0.5">
          <div>Scan was capped; some findings may be omitted.</div>
          {#each report.limit_notices ?? [] as notice}
            <div class="font-mono text-[10px]">
              {notice.resource}: retained {notice.kept} of {notice.total}
            </div>
          {/each}
        </div>
      {/if}

      {#if report.issues.length > 0}
        <section class="space-y-1.5 max-w-3xl">
          <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
            Issues ({issuesTotal}{issuesTotal > report.issues.length
              ? `; showing ${report.issues.length}`
              : ""})
          </h3>
          {#each report.issues as issue}
            <div class="px-3 py-2 rounded-xl border {issueClass(issue.severity)}">
              <div class="flex items-center gap-2">
                <AlertTriangle size={12} class="shrink-0" />
                <span class="font-medium uppercase text-[10px]">{issue.severity}</span>
                <span class="font-mono text-[10px] opacity-70">{issue.code}</span>
                {#if issue.path}
                  <span class="font-mono truncate opacity-70">{issue.path}</span>
                {/if}
              </div>
              <p class="mt-1 leading-relaxed">{issue.message}</p>
            </div>
          {/each}
        </section>
      {/if}

      <section class="space-y-2">
        <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Packages</h3>
        {#if report.manifests.length === 0}
          <p class="text-textMuted">No package.json found. Other ecosystems are listed below when detected.</p>
        {:else}
          <div class="grid gap-2 md:grid-cols-2 max-w-4xl">
            {#each report.manifests as pkg}
              <div class="p-3.5 rounded-2xl border border-border/70 bg-surface shadow-card">
                <div class="flex items-center gap-2 text-textPrimary font-medium">
                  <Package size={13} class="text-accent shrink-0" />
                  <span class="truncate">{pkg.name || pkg.path}</span>
                  {#if pkg.version}
                    <span class="font-mono text-textMuted font-normal">{pkg.version}</span>
                  {/if}
                  {#if pkg.private}
                    <span class="text-[10px] px-1.5 py-0.5 rounded-full bg-surfaceHover text-textMuted">private</span>
                  {/if}
                </div>
                <div class="mt-1.5 text-[11px] text-textMuted font-mono space-y-0.5">
                  <div>{pkg.path} · {pkg.package_manager}{pkg.lockfile ? ` · ${pkg.lockfile}` : ""}</div>
                  <div>
                    {pkg.dep_count} deps · {pkg.dev_dep_count} dev
                    {#if pkg.has_workspaces} · workspaces{/if}
                    {#if pkg.license} · {pkg.license}{/if}
                  </div>
                  {#if pkg.engines_node}
                    <div>engines.node {pkg.engines_node}</div>
                  {/if}
                  {#if pkg.lifecycle_scripts.length > 0}
                    <div class="text-amber-300">scripts: {pkg.lifecycle_scripts.join(", ")}</div>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {/if}
        {#if report.ecosystems.length > 0}
          <div class="space-y-1 max-w-3xl pt-1">
            {#each report.ecosystems as eco}
              <div class="text-textMuted">
                <span class="text-textPrimary font-medium">{eco.family}</span>
                <span class="mx-1.5">·</span>
                {eco.note}
                <span class="font-mono text-[10px] ml-1.5 opacity-70">{eco.manifests.slice(0, 4).join(", ")}</span>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <section class="space-y-2">
        <div class="flex items-center justify-between max-w-5xl">
          <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
            Vulnerabilities ({filter === "direct"
              ? `${visibleVulns.length} direct`
              : `${vulnerabilitiesTotal}${
                  vulnerabilitiesTotal > visibleVulns.length
                    ? `; showing ${visibleVulns.length}`
                    : ""
                }`})
          </h3>
          <div class="gp-segmented">
            <button
              type="button"
              data-active={filter === "all" ? "true" : "false"}
              class="gp-seg-btn !text-[11px] !py-0.5"
              onclick={() => (filter = "all")}
            >All</button>
            <button
              type="button"
              data-active={filter === "direct" ? "true" : "false"}
              class="gp-seg-btn !text-[11px] !py-0.5"
              onclick={() => (filter = "direct")}
            >Direct</button>
          </div>
        </div>
        {#if !report.npm_cli_present && report.manifests.length > 0}
          <p class="text-textMuted max-w-2xl">Install npm on PATH to run <span class="font-mono">npm audit</span> against this lockfile. GitPulse does not apply <span class="font-mono">npm audit fix</span>.</p>
        {:else if visibleVulns.length === 0}
          <p class="text-textMuted">
            {report.audit.total === 0
              ? auditComplete
                ? "No vulnerabilities found by completed local audits."
                : auditsRan
                  ? `Local audit incomplete${skipped.length > 0 ? ` (not run: ${skipped.join(", ")})` : ""}; no all-clear is available.`
                  : `Local audit did not run${skipped.length > 0 ? ` (not run: ${skipped.join(", ")})` : ""}.`
              : "No direct dependencies are vulnerable."}
          </p>
        {:else}
          <div class="border border-border/70 rounded-2xl overflow-hidden max-w-5xl shadow-card">
            <table class="w-full text-left">
              <thead class="bg-surface text-[10px] uppercase text-textMuted">
                <tr>
                  <th class="px-3 py-2 font-medium">Severity</th>
                  <th class="px-3 py-2 font-medium">Package</th>
                  <th class="px-3 py-2 font-medium">Advisory</th>
                  <th class="px-3 py-2 font-medium">Fix</th>
                  <th class="px-3 py-2 font-medium w-8"></th>
                </tr>
              </thead>
              <tbody>
                {#each visibleVulns as vuln}
                  <tr class="border-t border-border/40 align-top">
                    <td class="px-3 py-1.5">
                      <span class="px-1.5 py-0.5 rounded-full text-[10px] uppercase font-semibold {severityClass(vuln.severity)}">{vuln.severity}</span>
                    </td>
                    <td class="px-3 py-1.5">
                      <div class="font-mono text-textPrimary">{vuln.name}</div>
                      <div class="text-[10px] text-textMuted">
                        {vuln.ecosystem}{vuln.is_direct ? " · direct" : " · transitive"}
                        {#if vuln.range} · {vuln.range}{/if}
                      </div>
                    </td>
                    <td class="px-3 py-1.5 text-textPrimary">{vuln.title}</td>
                    <td class="px-3 py-1.5 font-mono text-textMuted">{vuln.fix_available}</td>
                    <td class="px-2 py-1.5">
                      {#if vuln.url}
                        <button
                          type="button"
                          class="p-1 rounded-full hover:bg-surfaceHover text-textMuted hover:text-accent transition-colors"
                          title="Open advisory"
                          onclick={() => openExternal(vuln.url)}
                        >
                          <ExternalLink size={13} />
                        </button>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>

      {@render dependabotSection()}

      <section class="space-y-2 pb-4">
        <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
          Outdated npm packages ({outdatedTotal})
        </h3>
        {#if report.outdated.length === 0}
          <p class="text-textMuted">
            {report.npm_cli_present ? "No outdated npm packages reported." : "Outdated checks need npm on PATH."}
          </p>
        {:else}
          <div class="border border-border/70 rounded-2xl overflow-hidden max-w-5xl shadow-card">
            <table class="w-full text-left">
              <thead class="bg-surface text-[10px] uppercase text-textMuted">
                <tr>
                  <th class="px-3 py-2 font-medium">Package</th>
                  <th class="px-3 py-2 font-medium">Current</th>
                  <th class="px-3 py-2 font-medium">Wanted</th>
                  <th class="px-3 py-2 font-medium">Latest</th>
                  <th class="px-3 py-2 font-medium">Type</th>
                </tr>
              </thead>
              <tbody>
                {#each report.outdated as pkg}
                  {@const kind = updateKind(pkg.current, pkg.latest)}
                  <tr class="border-t border-border/40">
                    <td class="px-3 py-1.5 font-mono text-textPrimary">{pkg.name}</td>
                    <td class="px-3 py-1.5 font-mono text-textMuted">{pkg.current}</td>
                    <td class="px-3 py-1.5 font-mono text-textMuted">{pkg.wanted}</td>
                    <td class="px-3 py-1.5 font-mono {updateKindClass(kind)}">
                      {pkg.latest}
                      <span class="text-[10px] ml-1 uppercase">{kind}</span>
                    </td>
                    <td class="px-3 py-1.5 text-textMuted">{pkg.dep_type || "—"}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    {:else}
      <div class="text-textMuted">Open a repository to scan dependency health.</div>
    {/if}
  </div>
</div>
