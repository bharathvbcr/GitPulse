<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type {
    DepsHealthReport,
    DependabotReport,
    CodeScanningReport,
  } from "../health/types";

  export interface DependabotFreshness {
    iso: string;
    label: string;
  }

  /** An unambiguous machine timestamp plus a local-time label for the UI. */
  export function dependabotFreshness(checkedAt: number): DependabotFreshness {
    const checked = new Date(checkedAt);
    return {
      iso: checked.toISOString(),
      label: checked.toLocaleString(),
    };
  }

  // Survives the per-tab remount so revisiting the Health view renders the
  // last scan instantly; the fetch then refreshes it in place.
  const healthCache = createRepoPanelCache<{
    deps: DepsHealthReport;
    dependabot: DependabotReport | null;
    dependabotCheckedAt: number | null;
    dependabotRequestFailed: boolean;
    codeScanning: CodeScanningReport | null;
    codeScanningCheckedAt: number | null;
    codeScanningRequestFailed: boolean;
  }>();
</script>

<script lang="ts">
  import { untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { invoke } from "@tauri-apps/api/core";
  import { openExternal as openExternalUrl } from "../desktop/openExternal";
  import {
    ShieldAlert,
    RefreshCw,
    Package,
    AlertTriangle,
    Clipboard,
    LoaderCircle,
    Sparkles,
    Play,
    Check,
    Terminal,
  } from "@lucide/svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import {
    harnessStore,
    type AiGeneration,
  } from "../stores/harnessStore";
  import { copyText } from "../desktop/clipboard";
  import { coverageGap, formatHealthReport, formatDeadCodeStatus, observedTotal } from "../health/report";
  import {
    githubAlertsCache,
    loadGithubAlerts,
  } from "../health/githubAlerts";
  import { buildRunnablePlanSteps } from "../terminal/tokenize";
  import {
    parseDepsHealthReport,
    type Vulnerability,
  } from "../health/types";

  import {
    dependabotBadgeClass as badgeClassFor,
    issueClass,
    severityClass,
    toneChipClass,
    toneLabel,
    updateKind,
    updateKindClass,
  } from "../health/format";
  import { summarizeHealth, type HealthSummary, type HealthTone } from "../health/summary";
  import { HEALTH_CONTENT_WIDTH, HEALTH_PROSE_WIDTH } from "../health/sections";
  import type { SectionCount } from "../health/counts";
  import { keyedList } from "../ui/eachKeys";
  import HealthSection from "./health/HealthSection.svelte";
  import HealthTable from "./health/HealthTable.svelte";
  import HealthSummaryCard from "./health/HealthSummaryCard.svelte";
  import GithubAlertsSection from "./health/GithubAlertsSection.svelte";
  import AlertLinkButton from "./health/AlertLinkButton.svelte";
  import { formatError } from "../ui/formatError";
  import {
    formatRunDetail,
    formatRunSummary,
    runPassed,
    type TerminalRunResult,
  } from "../terminal/runResult";
  import { reportPanelError } from "../diagnostics/report";
  import { getCodeintelStatus, getDeadSymbols } from "../codeintel/client";
  import type { CodeintelDeadSymbol, CodeintelStatus } from "../codeintel/types";
  import Skeleton from "./Skeleton.svelte";

  let report = $state<DepsHealthReport | null>(null);
  let dependabot = $state<DependabotReport | null>(null);
  let dependabotCheckedAt = $state<number | null>(null);
  let dependabotRequestFailed = $state(false);
  let codeScanning = $state<CodeScanningReport | null>(null);
  let codeScanningCheckedAt = $state<number | null>(null);
  let codeScanningRequestFailed = $state(false);
  let deadSymbols = $state<CodeintelDeadSymbol[]>([]);
  let deadSymbolsAvailable = $state(false);
  let deadSymbolsReason = $state<string | null>(null);
  /**
   * How many unreferenced symbols the query actually observed, and whether it
   * stopped early.
   *
   * `cmd_codeintel_dead_symbols` answers under a token budget and reports
   * `total`/`truncated` alongside the rows it returns. Both were dropped on
   * arrival, so the heading counted the rows that survived the budget and
   * presented that as the number of unreferenced symbols in the repository.
   */
  let deadSymbolsTotal = $state(0);
  let deadSymbolsTruncated = $state(false);
  let deadSymbolsIncomplete = $state<string | null>(null);
  /**
   * Whether this repository has a devmap code graph at all.
   *
   * Three features query it — impact edges in the diff pane, symbol search in
   * the palette, dead symbols below — and each of them renders nothing when
   * the graph is missing. Without this, "there is no code graph here" and
   * "the graph is here and found nothing" are the same blank space, and a
   * reader has no way to tell which they are looking at.
   */
  let codegraph = $state<CodeintelStatus | null>(null);
  let loading = $state(false);
  let checkingGithub = $state(false);
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
        /** Complete captured output, for the copied diagnostics. */
        detail?: string;
        /** One line naming what happened, for the status row. */
        summary?: string;
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

  /**
   * Every reason local coverage is short, missing CLIs and failed scanners
   * alike. The panel used to derive only the missing-CLI half, which left a
   * scanner that ran and errored unexplained: the summary said "incomplete"
   * and named nothing.
   */
  let gap = $derived(report ? coverageGap(report) : null);

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
  let displayedDependabotFreshness = $derived(
    dependabotCheckedAt === null ? null : dependabotFreshness(dependabotCheckedAt),
  );
  /** Open code scanning alerts, for the header badge. */
  let openCodeScanningCount = $derived(
    codeScanning?.available ? codeScanning.alerts.length : 0,
  );
  let codeScanningBadgeClass = $derived(
    codeScanning?.available ? badgeClassFor(codeScanning.alerts) : "",
  );
  let displayedCodeScanningFreshness = $derived(
    codeScanningCheckedAt === null ? null : dependabotFreshness(codeScanningCheckedAt),
  );
  let displayedGithubFreshness = $derived(
    displayedDependabotFreshness ?? displayedCodeScanningFreshness,
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
  /**
   * True when findings were dropped by the scan cap.
   *
   * The "All" count could always say `N; showing M` because `audit.total` is
   * computed before `cap_report` truncates. "Direct" has no such total — it
   * filters the rows that survived — so an unqualified "3 direct" was a floor
   * printed as a total. The cap sorts by severity first, so the rows dropped
   * are the least severe, and how many of them were direct is unknowable.
   */
  /**
   * How many artifacts of this family the scan actually saw.
   *
   * Two separate cuts hid behind one list: the backend keeps at most
   * `MAX_ECOSYSTEM_MANIFESTS` per family (recording a limit notice), and the
   * row then printed only the first four of whatever survived. Neither said
   * so, so "4 lockfiles" was indistinguishable from "the 4 lockfiles there
   * are". The notice resource is the family name, so the observed total comes
   * from the same reader every other capped section uses.
   */
  function ecosystemArtifactTotal(eco: { family: string; manifests: string[] }): number {
    if (!report) return eco.manifests.length;
    return observedTotal(report, `${eco.family} ecosystem artifacts`, eco.manifests.length);
  }

  let vulnerabilitiesCapped = $derived(
    report ? vulnerabilitiesTotal > report.vulnerabilities.length : false,
  );

  /**
   * The verdict, derived once and read by the header chip, the summary card
   * and every section's tone chip.
   *
   * Three surfaces previously each decided for themselves what this scan
   * meant, which is how the header could truncate its own truncation notice
   * while the body printed a different one.
   */
  let summary = $derived.by<HealthSummary | null>(() => {
    const current = report;
    if (!current) return null;
    return summarizeHealth({
      report: current,
      dependabot,
      codeScanning,
      codegraph,
      deadCode:
        deadSymbolsAvailable || deadSymbolsReason != null
          ? {
              available: deadSymbolsAvailable,
              reason: deadSymbolsReason,
              total: Math.max(deadSymbolsTotal, deadSymbols.length),
              shown: deadSymbols.length,
              truncated: deadSymbolsTruncated,
              walkIncomplete: deadSymbolsIncomplete,
            }
          : null,
    });
  });

  let facetById = $derived.by(
    () => new Map((summary?.facets ?? []).map((facet) => [facet.id, facet])),
  );

  /** A section's tone, or null before there is a scan to have an opinion on. */
  function facetTone(id: string): HealthTone | null {
    return facetById.get(id)?.tone ?? null;
  }

  /**
   * Section ids present in the DOM right now.
   *
   * The summary card's facet chips double as the page's navigation, and a
   * chip that scrolls nowhere is worse than a chip that does not offer to.
   * Every catalog section renders whenever there is a report — an explicit
   * "not checked" beats the blank space that used to make "looked, found
   * nothing" and "never looked" identical — so this is the report plus the
   * two conditional sections.
   */
  let renderedSections = $derived.by(() => {
    const ids = new Set<string>();
    if (!report) return ids;
    ids.add("summary");
    if (plan || fixing) ids.add("plan");
    for (const id of [
      "vulnerabilities",
      "dependabot",
      "code-scanning",
      "issues",
      "outdated",
      "packages",
      "code-graph",
      "dead-code",
    ]) {
      ids.add(id);
    }
    return ids;
  });

  // Counts are `SectionCount` objects, never pre-rendered strings: the shared
  // formatter is the only thing that turns one into text, so no section can
  // print the rows that survived a cap without printing what the scan saw.
  let vulnerabilityCount = $derived.by<SectionCount>(() =>
    filter === "direct"
      ? {
          shown: visibleVulns.length,
          total: visibleVulns.length,
          // "Direct" filters the rows that survived the cap and has no total
          // of its own, so the number it prints is a floor.
          atLeast: vulnerabilitiesCapped,
          qualifier: "direct",
        }
      : { shown: visibleVulns.length, total: vulnerabilitiesTotal },
  );
  let issuesCount = $derived.by<SectionCount>(() => ({
    shown: report?.issues.length ?? 0,
    total: issuesTotal,
  }));
  let outdatedCount = $derived.by<SectionCount>(() => ({
    shown: report?.outdated.length ?? 0,
    total: outdatedTotal,
  }));
  let deadCodeCount = $derived.by<SectionCount>(() => ({
    shown: deadSymbols.length,
    total: deadSymbolsTotal,
    atLeast: deadSymbolsTruncated,
  }));
  let dependabotCount = $derived.by<SectionCount>(() => ({
    shown: dependabot?.alerts.length ?? 0,
    total: dependabot?.alerts.length ?? 0,
    atLeast: dependabot?.truncated === true,
  }));
  let codeScanningCount = $derived.by<SectionCount>(() => ({
    shown: codeScanning?.alerts.length ?? 0,
    total: codeScanning?.alerts.length ?? 0,
    atLeast: codeScanning?.truncated === true,
  }));

  /**
   * Why the dead-code answer is not an all-clear, above the table rather than
   * after it. Both halves were previously paragraphs that rendered *between*
   * sections, so a reader who scrolled to the table met the rows first and
   * the reason they are unreliable second.
   */
  let deadCodeCaveat = $derived.by<string | null>(() => {
    if (!deadSymbolsAvailable) return null;
    if (deadSymbolsIncomplete) {
      return `Dead-code analysis is incomplete: ${deadSymbolsIncomplete}. Missing callers can produce false positives; this is not an all-clear.`;
    }
    if (deadSymbolsTruncated && deadSymbols.length > 0) {
      return "The dead-symbol query stopped at its token budget, so this list is a floor, not the complete set.";
    }
    return null;
  });

  const DEPENDABOT_COLUMNS = Object.freeze([
    { label: "Severity" },
    { label: "Package" },
    { label: "Advisory" },
    { label: "Fix" },
    { label: "Open on GitHub", width: "w-8", hideLabel: true },
  ]);
  const CODE_SCANNING_COLUMNS = Object.freeze([
    { label: "Severity" },
    { label: "Rule" },
    { label: "Location" },
    { label: "Tool" },
    { label: "Open on GitHub", width: "w-8", hideLabel: true },
  ]);
  const VULNERABILITY_COLUMNS = Object.freeze([
    { label: "Severity" },
    { label: "Package" },
    { label: "Advisory" },
    { label: "Fix" },
    { label: "Open advisory", width: "w-8", hideLabel: true },
  ]);
  const OUTDATED_COLUMNS = Object.freeze([
    { label: "Package" },
    { label: "Current" },
    { label: "Wanted" },
    { label: "Latest" },
    { label: "Type" },
  ]);
  const DEAD_CODE_COLUMNS = Object.freeze([
    { label: "Symbol" },
    { label: "File" },
    { label: "Confidence" },
    { label: "Status" },
  ]);

  const scanned = { path: "" };
  let inflight: AsyncGuard | null = null;
  let dependabotInflight: AsyncGuard | null = null;
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
    // Automatic local scans stay local. GitHub alerts use the GitHub CLI
    // and the network; they run through scanDependabot, not this path.
    const [deps, dead, graph] = await Promise.allSettled([
      invoke("cmd_scan_deps_health", { repoPath }).then(parseDepsHealthReport),
      getDeadSymbols(repoPath),
      getCodeintelStatus(repoPath),
    ]);
    if (!guard.isLive()) return;
    try {
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
      if (dead.status === "fulfilled") {
        deadSymbolsAvailable = dead.value.available;
        deadSymbols = dead.value.available ? dead.value.items : [];
        // `total` counts what the query saw; `items` is what fitted in the
        // budget. Never below the row count, so a backend that reports only
        // `shown` cannot make the heading claim fewer than it lists.
        deadSymbolsTotal = dead.value.available
          ? Math.max(dead.value.total ?? 0, dead.value.items.length)
          : 0;
        deadSymbolsTruncated = dead.value.available && dead.value.truncated === true;
        deadSymbolsIncomplete = dead.value.available ? dead.value.walk_incomplete?.trim() || null : null;
        deadSymbolsReason = dead.value.available
          ? null
          : (dead.value.reason ?? "dead-symbol query unavailable");
      } else {
        deadSymbols = [];
        deadSymbolsAvailable = false;
        deadSymbolsTotal = 0;
        deadSymbolsTruncated = false;
        deadSymbolsIncomplete = null;
        deadSymbolsReason = formatError(dead.reason);
      }
      // An IPC-level failure is folded into the same unavailable shape the
      // command itself uses, so "could not ask" never renders as "asked, and
      // there is no graph".
      codegraph =
        graph.status === "fulfilled"
          ? graph.value
          : {
              available: false,
              db_path: "",
              reason: formatError(graph.reason),
            };
      if (deps.status === "fulfilled") {
        healthCache.set(repoPath, {
          deps: deps.value,
          dependabot,
          dependabotCheckedAt,
          dependabotRequestFailed,
          codeScanning,
          codeScanningCheckedAt,
          codeScanningRequestFailed,
        });
      }
    } catch (err: unknown) {
      errorMsg = formatError(err);
      report = null;
      scanned.path = "";
      deadSymbols = [];
      deadSymbolsAvailable = false;
      deadSymbolsTotal = 0;
      deadSymbolsTruncated = false;
      deadSymbolsIncomplete = null;
      deadSymbolsReason = formatError(err);
      codegraph = { available: false, db_path: "", reason: formatError(err) };
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  /**
   * Preserve the newest explicit GitHub outcome even when the simultaneous
   * local rescan failed. Otherwise an older successful cache entry would
   * reappear on the next tab mount and look newer than the failure.
   */
  function cacheDependabotResult(
    repoPath: string,
    result: DependabotReport,
    checkedAt: number,
    requestFailed: boolean,
    codeScanningResult: CodeScanningReport,
    codeScanningFailed: boolean,
  ) {
    const currentReport = scanned.path === repoPath ? report : null;
    const deps = currentReport ?? healthCache.get(repoPath)?.deps;
    if (!deps) return;
    healthCache.set(repoPath, {
      deps,
      dependabot: result,
      dependabotCheckedAt: checkedAt,
      dependabotRequestFailed: requestFailed,
      codeScanning: codeScanningResult,
      codeScanningCheckedAt: checkedAt,
      codeScanningRequestFailed: codeScanningFailed,
    });
  }

  async function scanDependabot(path?: string, options?: { force?: boolean }) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    dependabotInflight?.cancel();
    const guard = createAsyncGuard();
    dependabotInflight = guard;
    checkingGithub = true;
    actionError = null;
    try {
      const snapshot = await loadGithubAlerts(repoPath, {
        force: options?.force === true,
      });
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      dependabot = snapshot.dependabot;
      dependabotCheckedAt = snapshot.checkedAt;
      dependabotRequestFailed = snapshot.dependabotRequestFailed;
      codeScanning = snapshot.codeScanning;
      codeScanningCheckedAt = snapshot.checkedAt;
      codeScanningRequestFailed = snapshot.codeScanningRequestFailed;
      cacheDependabotResult(
        repoPath,
        snapshot.dependabot,
        snapshot.checkedAt,
        snapshot.dependabotRequestFailed,
        snapshot.codeScanning,
        snapshot.codeScanningRequestFailed,
      );
    } finally {
      if (guard.isLive()) checkingGithub = false;
    }
  }

  /** The rendered text behind both "Copy report" and "Fix with MANVI". */
  function renderedReport(): string | null {
    const current = report;
    const repoPath = $repoStore.currentPath;
    if (!current || !repoPath) return null;
    const deadCode =
      deadSymbolsAvailable || deadSymbolsReason != null
        ? {
            available: deadSymbolsAvailable,
            reason: deadSymbolsReason,
            items: deadSymbols,
            total: Math.max(deadSymbolsTotal, deadSymbols.length),
            truncated: deadSymbolsTruncated,
            walk_incomplete: deadSymbolsIncomplete,
          }
        : null;
    const text = formatHealthReport(current, repoPath, dependabot, codeScanning, deadCode);
    const stamps: string[] = [];
    if (dependabot && dependabotCheckedAt !== null) {
      stamps.push(
        `Dependabot checked at: ${dependabotFreshness(dependabotCheckedAt).iso} (result may be cached)`,
      );
    }
    if (codeScanning && codeScanningCheckedAt !== null) {
      stamps.push(
        `Code scanning checked at: ${dependabotFreshness(codeScanningCheckedAt).iso} (result may be cached)`,
      );
    }
    if (stamps.length === 0) return text;
    return `${text}\n${stamps.join("\n")}`;
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
    const actionLabel = step.command ?? step.text;
    let actionRecorded = false;
    stepResults[step.id] = { running: true };

    try {
      const res = await invoke<TerminalRunResult>("cmd_manvi_run_action", {
        repoPath,
        args: step.argv,
        actionKind: "health",
        // Long enough for a cold install/build; the backend clamps to [1s, 30min].
        timeoutSecs: 600,
      });
      const passed = runPassed(res);
      harnessStore.recordAction({
        repoPath,
        kind: "remediation-step",
        label: actionLabel,
        ok: passed,
        verdict: res.policy ?? null,
      });
      actionRecorded = true;
      if (!guard.isLive()) return false;

      stepResults[step.id] = {
        running: false,
        status: passed ? "passed" : "failed",
        // Both streams, kept on timeout, with a clipped tail marked as clipped
        // — this panel previously dropped stdout whenever stderr had anything,
        // and never noted truncation at all.
        detail: formatRunDetail(res),
        summary: formatRunSummary(res),
        duration_ms: res.duration_ms,
      };

      return passed;
    } catch (err) {
      if (!actionRecorded) {
        harnessStore.recordAction({
          repoPath,
          kind: "remediation-step",
          label: actionLabel,
          ok: false,
        });
      }
      if (!guard.isLive()) return false;
      const msg = formatError(err);
      stepResults[step.id] = {
        running: false,
        status: "failed",
        detail: msg,
      };
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
      dependabotInflight?.cancel();
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
      dependabotInflight?.cancel();
      fixInflight?.cancel();
      stepsInflight?.cancel();
      untrack(() => {
        scanned.path = "";
        report = null;
        dependabot = null;
        dependabotCheckedAt = null;
        dependabotRequestFailed = false;
        codeScanning = null;
        codeScanningCheckedAt = null;
        codeScanningRequestFailed = false;
        errorMsg = null;
        actionError = null;
        loading = false;
        checkingGithub = false;
        plan = null;
        planError = null;
      });
      return;
    }
    if (path === scanned.path) return;
    scanned.path = path;
    dependabotInflight?.cancel();
    // Hydrate last-known data synchronously so a revisit renders instantly
    // (the placeholder below only fires when there is no cached report).
    const cached = healthCache.get(path);
    const github = githubAlertsCache.get(path);
    untrack(() => {
      checkingGithub = false;
      if (cached) {
        report = cached.deps;
        dependabot = cached.dependabot;
        dependabotCheckedAt = cached.dependabotCheckedAt;
        dependabotRequestFailed = cached.dependabotRequestFailed;
        codeScanning = cached.codeScanning;
        codeScanningCheckedAt = cached.codeScanningCheckedAt;
        codeScanningRequestFailed = cached.codeScanningRequestFailed;
      } else {
        report = null;
        dependabot = null;
        dependabotCheckedAt = null;
        dependabotRequestFailed = false;
        codeScanning = null;
        codeScanningCheckedAt = null;
        codeScanningRequestFailed = false;
        deadSymbols = [];
        deadSymbolsAvailable = false;
        deadSymbolsReason = null;
        deadSymbolsTotal = 0;
        deadSymbolsTruncated = false;
        deadSymbolsIncomplete = null;
        codegraph = null;
      }
      if (github) {
        dependabot = github.dependabot;
        dependabotCheckedAt = github.checkedAt;
        dependabotRequestFailed = github.dependabotRequestFailed;
        codeScanning = github.codeScanning;
        codeScanningCheckedAt = github.checkedAt;
        codeScanningRequestFailed = github.codeScanningRequestFailed;
      }
    });
    void scan(path);
    // Launch also fetches these; this call joins that in-flight request or
    // hydrates from its cache so opening Health after a warning is not empty.
    if ($interfaceStore.autoScanGithubAlerts && !github) void scanDependabot(path);
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
  {#snippet dependabotRows()}
    {#each keyedList(dependabot?.alerts ?? [], (alert) => `${alert.number}:${alert.package}:${alert.manifest_path}`) as { item: alert, key } (key)}
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
            <AlertLinkButton
              url={alert.url}
              label={`Open the Dependabot alert for ${alert.package} on GitHub`}
              onopen={openExternal}
            />
          {/if}
        </td>
      </tr>
    {/each}
  {/snippet}

  {#snippet codeScanningRows()}
    {#each keyedList(codeScanning?.alerts ?? [], (alert) => `${alert.number}:${alert.rule_id}:${alert.path}:${alert.start_line}`) as { item: alert, key } (key)}
      <tr class="border-t border-border/40 align-top">
        <td class="px-3 py-1.5">
          <span class="px-1.5 py-0.5 rounded-full text-[10px] uppercase font-semibold {severityClass(alert.severity)}">{alert.severity || "unranked"}</span>
        </td>
        <td class="px-3 py-1.5">
          <div class="font-mono text-textPrimary">{alert.rule_id || alert.rule_name || "rule"}</div>
          <div class="text-[10px] text-textMuted">{alert.title}</div>
        </td>
        <td class="px-3 py-1.5 font-mono text-textMuted">
          {#if alert.path}
            {alert.path}{#if alert.start_line > 0}:{alert.start_line}{/if}
          {:else}
            —
          {/if}
        </td>
        <td class="px-3 py-1.5 text-textMuted">
          {alert.tool || "—"}{#if alert.tool_version} {alert.tool_version}{/if}
        </td>
        <td class="px-2 py-1.5">
          {#if alert.url}
            <AlertLinkButton
              url={alert.url}
              label={`Open the code scanning alert ${alert.rule_id || alert.rule_name || "rule"} on GitHub`}
              onopen={openExternal}
            />
          {/if}
        </td>
      </tr>
    {/each}
  {/snippet}

  <!-- Rendered by both the load-error branch and the report branch: a failed
       local scan says nothing about GitHub alerts that were fetched fine. -->
  {#snippet githubSections()}
    <GithubAlertsSection
      id="dependabot"
      report={dependabot}
      requestFailed={dependabotRequestFailed}
      count={dependabotCount}
      tone={facetTone("dependabot")}
      noun="Dependabot alerts"
      columns={DEPENDABOT_COLUMNS}
      row={dependabotRows}
    />
    <GithubAlertsSection
      id="code-scanning"
      report={codeScanning}
      requestFailed={codeScanningRequestFailed}
      count={codeScanningCount}
      tone={facetTone("code-scanning")}
      noun="code scanning alerts"
      columns={CODE_SCANNING_COLUMNS}
      row={codeScanningRows}
    />
  {/snippet}

  <div class="px-4 py-2 border-b border-border/60 gp-section-edge bg-surface/60 flex items-center justify-between gap-3 shrink-0">
    <div class="flex items-center gap-2 min-w-0">
      <ShieldAlert size={16} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary shrink-0">Health</span>
      <!-- One verdict, from one owner, instead of the five `truncate` spans
           this row used to carry. Those rendered as "58 outd… · Dependabot 0
           … · Code scanning unav…" — the summary was unreadable at exactly
           the moment it mattered, and three of the five were saying "nothing
           to report" at full price. The states that are not findings now live
           in the summary card below, which wraps instead of clipping. -->
      <!-- The tone word, not the whole headline. The summary card repeats the
           headline verbatim about 130px below this, so printing it twice cost
           header width to say nothing new — and it was the long string that
           needed truncating. The card scrolls away and this does not, which is
           what the header badge is for; the full sentence stays on the title. -->
      {#if summary}
        <span
          class="shrink-0 px-2 py-0.5 rounded-full border text-[11px] font-medium capitalize {toneChipClass(summary.tone)}"
          title={summary.headline}
        >{toneLabel(summary.tone)}</span>
      {/if}
      <!-- Open GitHub alerts keep a header badge of their own: it is the one
           state worth spending header room on, and its tint carries the worst
           open severity. -->
      {#if openDependabotCount > 0}
        <span class={`shrink-0 ${dependabotBadgeClass}`}>
          · Dependabot {openDependabotCount}{dependabot?.truncated ? "+" : ""}
        </span>
      {/if}
      {#if openCodeScanningCount > 0}
        <span class={`shrink-0 ${codeScanningBadgeClass}`}>
          · Code scanning {openCodeScanningCount}{codeScanning?.truncated ? "+" : ""}
        </span>
      {/if}
    </div>
    <div class="flex items-center gap-2 shrink-0">
      <button
        type="button"
        aria-describedby="dependabot-permission-note"
        onclick={() => scanDependabot(undefined, { force: true })}
        disabled={checkingGithub}
        class="gp-btn disabled:opacity-40 disabled:cursor-not-allowed"
        title="Use the GitHub CLI, its credentials, and the network to check Dependabot and code scanning alerts"
      >
        <ShieldAlert size={13} class={checkingGithub ? "animate-pulse" : ""} />
        {checkingGithub ? "Checking GitHub…" : "Check GitHub alerts"}
      </button>
      {#if report}
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
        title="Rescan local vulnerabilities, updates, and code health"
      >
        <RefreshCw size={13} class={loading ? "animate-spin" : ""} />
        Scan local
      </button>
    </div>
  </div>

  <div class="flex-1 overflow-auto p-4 space-y-5">
    <!-- Four lines of prose about GitHub CLI permissions used to be pinned at
         the top of this scroll in every state, above every finding, forever.
         It is the button's explanation, not the page's, so it is the button's
         `aria-describedby` target and a one-line disclosure: the freshness —
         the part a reader actually re-reads — stays on the visible summary
         line, and the permission text is one click away rather than four
         lines of column height on every visit. -->
    <details
      class="rounded-xl border border-border/70 bg-surface px-3 py-1.5 text-[11px] text-textMuted {HEALTH_PROSE_WIDTH}"
    >
      <!-- `role="status"` belongs on the text, not on the `<summary>`: a
           summary is an interactive disclosure control and cannot also be a
           live region. The freshness is what re-announces after a check. -->
      <summary class="cursor-pointer select-none">
        <span role="status">
          {#if displayedGithubFreshness}
            GitHub alerts · last checked
            <time datetime={displayedGithubFreshness.iso}>{displayedGithubFreshness.label}</time>
            · may be cached
          {:else}
            GitHub alerts · no result loaded for this repository
          {/if}
        </span>
      </summary>
      <p id="dependabot-permission-note" class="pt-2 leading-relaxed">
        GitHub alerts are checked when GitPulse launches and when this repository
        opens, unless turned off in Settings → Analysis. The check uses the GitHub CLI,
        its credentials, and the network to fetch Dependabot and code scanning alerts.
        Critical and high findings raise a warning.
      </p>
    </details>
    <!-- Non-fatal action failures render ahead of the state chain so they stay
         visible in every state instead of shadowing (or being shadowed by) the
         load-error branch. Same shape as GitHubPanel's actionError banner. -->
    {#if actionError}
      <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-200 {HEALTH_PROSE_WIDTH}">
        {actionError}
      </div>
    {/if}
    {#if loading && !report}
      <div class="space-y-4 {HEALTH_CONTENT_WIDTH}">
        <div class="grid grid-cols-1 md:grid-cols-3 gap-3">
          <Skeleton variant="card" count={3} />
        </div>
        <div class="space-y-2 pt-2">
          <Skeleton variant="text" count={4} height="2rem" />
        </div>
      </div>
    {:else if errorMsg}
      <div class="p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-200 {HEALTH_PROSE_WIDTH}">
        {errorMsg}
      </div>
      {@render githubSections()}
    {:else if report}
      {#if summary}
        <HealthSection id="summary">
          <HealthSummaryCard {summary} rendered={renderedSections} />
        </HealthSection>
      {/if}

      {#if planError}
        <div class="p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-200 {HEALTH_PROSE_WIDTH}">
          Fix with MANVI failed: {planError}
        </div>
      {/if}

      {#if plan || fixing}
        <HealthSection id="plan">
          {#snippet actions()}
            <div class="flex items-center gap-2">
              {#if plan}
                {#if planSteps.some((s) => s.argv)}
                  <button
                    type="button"
                    onclick={runAllSteps}
                    disabled={runningAll || Object.values(stepResults).some((r) => r.running)}
                    class="gp-btn-primary py-1! text-[11px]!"
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
                  onclick={() => repoStore.setTerminalOpen(true)}
                  class="gp-btn py-1! text-[11px]!"
                  title="Open the terminal below this panel, so the plan stays on screen while you run it"
                >
                  <Terminal size={12} />
                  <span>Terminal</span>
                </button>
                <button type="button" onclick={copyPlan} class="gp-btn py-1! text-[11px]!" title="Copy the remediation plan">
                  <Clipboard size={12} />
                  {planCopied ? "Copied" : "Copy plan"}
                </button>
              {/if}
            </div>
          {/snippet}
          <div class="space-y-3 rounded-2xl border border-accent/30 bg-surface shadow-card p-4">
            {#if fixing && !plan}
              <div class="flex items-center gap-2 text-textMuted py-2">
                <LoaderCircle size={14} class="animate-spin" />
                Sending the health report to the local model…
              </div>
            {:else if plan}
              <p class="text-[11px] text-textMuted font-mono truncate">
                {plan.model} @ {plan.base_url} · {plan.elapsed_ms} ms
              </p>
              {#each keyedList(plan.warnings, (warning) => warning) as { item: warning, key } (key)}
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
                            class="gp-btn py-1! px-2.5! text-xs shrink-0 disabled:opacity-50"
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

              <div class="pt-2 border-t border-border/40 flex items-center justify-between gap-2 text-textMuted text-[11px]">
                <p>Review each remediation step. Run commands individually or execute all sequentially.</p>
                <button
                  type="button"
                  onclick={() => void scan()}
                  disabled={loading}
                  class="gp-btn py-1! text-xs shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
                  title="Rescan repository health"
                >
                  <RefreshCw size={11} class={loading ? "animate-spin" : ""} />
                  <span>Rescan Health</span>
                </button>
              </div>
            {/if}
          </div>
        </HealthSection>
      {/if}

      <HealthSection
        id="vulnerabilities"
        count={vulnerabilityCount}
        tone={facetTone("vulnerabilities")}
      >
        {#snippet actions()}
          <div class="gp-segmented" role="group" aria-label="Vulnerability scope">
            <button
              type="button"
              aria-pressed={filter === "all"}
              data-active={filter === "all" ? "true" : "false"}
              class="gp-seg-btn text-[11px]! py-0.5!"
              onclick={() => (filter = "all")}
            >All</button>
            <button
              type="button"
              aria-pressed={filter === "direct"}
              data-active={filter === "direct" ? "true" : "false"}
              class="gp-seg-btn text-[11px]! py-0.5!"
              onclick={() => (filter = "direct")}
            >Direct</button>
          </div>
        {/snippet}
        {#if !report.npm_cli_present && report.manifests.length > 0}
          <p class="text-textMuted {HEALTH_PROSE_WIDTH}">Install npm on PATH to run <span class="font-mono">npm audit</span> against this lockfile. GitPulse does not apply <span class="font-mono">npm audit fix</span>.</p>
        {:else if visibleVulns.length === 0}
          <p class="text-textMuted">
            {report.audit.total === 0
              ? auditComplete
                ? "No vulnerabilities found by completed local audits."
                : auditsRan
                  ? `Local audit incomplete${gap ? ` (${gap})` : ""}; no all-clear is available.`
                  : `Local audit did not run${gap ? ` (${gap})` : ""}.`
              : "No direct dependencies are vulnerable."}
          </p>
        {:else}
          <HealthTable caption="Known vulnerabilities in this repository's dependencies" columns={VULNERABILITY_COLUMNS}>
            {#each keyedList(visibleVulns, (vuln) => `${vuln.ecosystem}:${vuln.name}:${vuln.range}:${vuln.title}`) as { item: vuln, key } (key)}
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
                    <AlertLinkButton
                      url={vuln.url}
                      label={`Open the advisory for ${vuln.name}`}
                      onopen={openExternal}
                    />
                  {/if}
                </td>
              </tr>
            {/each}
          </HealthTable>
        {/if}
      </HealthSection>

      {@render githubSections()}

      <HealthSection id="issues" count={issuesCount} tone={facetTone("issues")}>
        {#if report.issues.length === 0}
          <p class="text-textMuted">No repository configuration issues were reported by this scan.</p>
        {:else}
          <div class="space-y-1.5 {HEALTH_PROSE_WIDTH}">
            {#each keyedList(report.issues, (issue) => `${issue.code}:${issue.path ?? ""}:${issue.message}`) as { item: issue, key } (key)}
              <div class="px-3 py-2 rounded-xl border {issueClass(issue.severity)}">
                <div class="flex items-center gap-2 min-w-0">
                  <AlertTriangle size={12} class="shrink-0" aria-hidden="true" />
                  <span class="font-medium uppercase text-[10px] shrink-0">{issue.severity}</span>
                  <!-- A flex item has to be allowed to shrink before `truncate`
                       can clip it; without `min-w-0` it sizes to its content. -->
                  <span class="font-mono text-[10px] opacity-70 truncate min-w-0">{issue.code}</span>
                  {#if issue.path}
                    <span class="font-mono truncate opacity-70 min-w-0">{issue.path}</span>
                  {/if}
                </div>
                <p class="mt-1 leading-relaxed [overflow-wrap:anywhere]">{issue.message}</p>
              </div>
            {/each}
          </div>
        {/if}
      </HealthSection>

      <HealthSection id="outdated" count={outdatedCount} tone={facetTone("outdated")}>
        {#if report.outdated.length === 0}
          <p class="text-textMuted">
            {report.npm_cli_present ? "No outdated npm packages reported." : "Outdated checks need npm on PATH."}
          </p>
        {:else}
          <HealthTable caption="npm packages behind their latest published release" columns={OUTDATED_COLUMNS}>
            {#each keyedList(report.outdated, (pkg) => `${pkg.location}:${pkg.name}:${pkg.current}`) as { item: pkg, key } (key)}
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
          </HealthTable>
        {/if}
      </HealthSection>

      <HealthSection id="packages">
        {#snippet actions()}
          <!-- The toolchain the local scan actually used. It sat between the
               header buttons, where it wrapped to a second line and read as a
               control; it is evidence about this scan, so it belongs beside
               the manifests it was run against. -->
          <!-- Optional chaining, not the branch's narrowing: a snippet is
               compiled as its own function, so `report` is not narrowed here
               even though the branch that declares it has checked. -->
          <span class="text-[11px] text-textMuted font-mono">
            {report?.node_version ? `node ${report.node_version}` : "node —"}
            ·
            {report?.npm_version ? `npm ${report.npm_version}` : "npm —"}
          </span>
        {/snippet}
        {#if report.manifests.length === 0}
          <p class="text-textMuted">No package.json found. Other ecosystems are listed below when detected.</p>
        {:else}
          <div class="grid gap-2 md:grid-cols-2">
            {#each keyedList(report.manifests, (pkg) => pkg.path) as { item: pkg, key } (key)}
              <!-- Grid items default to `min-width: auto`, so a long manifest
                   path widens its column instead of wrapping inside it. -->
              <div class="p-3.5 rounded-2xl border border-border/70 bg-surface shadow-card min-w-0 [overflow-wrap:anywhere]">
                <div class="flex items-center gap-2 text-textPrimary font-medium">
                  <Package size={13} class="text-accent shrink-0" aria-hidden="true" />
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
          <!-- One row per family, as a definition list rather than a prose run.
               These used to render as family + note + a comma-joined path list
               in a single wrapping paragraph, so a repository with four Rust
               crates produced three lines of run-together file paths with no
               structure to scan. -->
          <dl class="pt-1 space-y-1.5">
            {#each keyedList(report.ecosystems, (eco) => eco.family) as { item: eco, key } (key)}
              {@const shown = eco.manifests.slice(0, 4)}
              {@const seen = ecosystemArtifactTotal(eco)}
              <!-- `minmax(0,1fr)`, not `1fr`: a grid track's default minimum is
                   its content's min-content width, so one unbreakable artifact
                   path pushes the column — and the page — wider than the pane. -->
              <div class="grid grid-cols-[6rem_minmax(0,1fr)] gap-x-3 gap-y-0.5 items-baseline">
                <dt class="text-textPrimary font-medium truncate">{eco.family}</dt>
                <dd class="text-textMuted min-w-0">
                  <div>{eco.note}</div>
                  <ul class="mt-0.5 flex flex-wrap gap-x-2 gap-y-0.5 font-mono text-[10px] opacity-70 [overflow-wrap:anywhere]">
                    {#each keyedList(shown, (path) => path) as { item: path, key: pathKey } (pathKey)}
                      <li class="min-w-0 max-w-full">{path}</li>
                    {/each}
                    {#if seen > shown.length}
                      <li class="text-amber-300">+{seen - shown.length} more</li>
                    {/if}
                  </ul>
                </dd>
              </div>
            {/each}
          </dl>
        {/if}
      </HealthSection>

      <!-- Stated whether or not there is anything to show. Impact edges,
           symbol search and the dead-code table all go quiet without a code
           graph, and this is the only thing that says which kind of quiet
           it is. -->
      <HealthSection id="code-graph">
        {#if codegraph?.available}
          <div class="rounded-2xl border border-border/70 bg-surface p-3 shadow-card">
            <div class="grid grid-cols-3 gap-2 text-center">
              <div>
                <div class="text-[15px] font-semibold text-textPrimary">{codegraph.total_files ?? "—"}</div>
                <div class="text-[10px] uppercase tracking-wider text-textMuted">files</div>
              </div>
              <div>
                <div class="text-[15px] font-semibold text-textPrimary">{codegraph.total_symbols ?? "—"}</div>
                <div class="text-[10px] uppercase tracking-wider text-textMuted">symbols</div>
              </div>
              <div>
                <div class="text-[15px] font-semibold text-textPrimary">{codegraph.total_edges ?? "—"}</div>
                <div class="text-[10px] uppercase tracking-wider text-textMuted">edges</div>
              </div>
            </div>
            {#if codegraph.generation_id != null}
              <p class="mt-2 text-[10px] font-mono text-textMuted">generation {codegraph.generation_id}</p>
            {/if}
            <p class="mt-2 text-[11px] text-textMuted">
              Agents query this map through `gitpulse_codeintel_search`, `impact`, `trace` and
              `dead_symbols`. A dash means that count was not stored — not that it is zero.
            </p>
          </div>
        {:else if codegraph}
          <p class="text-[11px] text-amber-300">
            No code graph for this repository — impact edges, symbol search and dead-code
            detection have nothing to read.
            {#if codegraph.reason}
              <span class="text-textMuted font-mono">({codegraph.reason})</span>
            {/if}
          </p>
        {:else}
          <p class="text-[11px] text-textMuted">Code graph status has not been read for this repository.</p>
        {/if}
      </HealthSection>

      <HealthSection
        id="dead-code"
        count={deadSymbols.length > 0 ? deadCodeCount : null}
        tone={facetTone("dead-code")}
        caveat={deadCodeCaveat}
      >
        {#if codegraph?.available && !deadSymbolsAvailable}
          <p class="text-[11px] text-amber-300">
            Dead-code check could not run{#if deadSymbolsReason}: {deadSymbolsReason}{/if}.
            That is not the same as finding no unreferenced symbols.
          </p>
        {:else if deadSymbolsAvailable && deadSymbols.length === 0 && !deadSymbolsIncomplete}
          <p class="text-[11px] {deadSymbolsTruncated ? 'text-amber-300' : 'text-textMuted'}">
            {deadSymbolsTruncated
              ? "The dead-symbol query stopped at its token budget before returning anything; this is not an all-clear."
              : "No dead-code candidates in the indexed graph."}
          </p>
        {:else if deadSymbolsAvailable && deadSymbols.length > 0}
          <HealthTable caption="Symbols the code graph found no reference to" columns={DEAD_CODE_COLUMNS}>
            {#each keyedList(deadSymbols, (sym) => `${sym.file_path}:${sym.symbol_name}`) as { item: sym, key } (key)}
              <tr class="border-t border-border/40">
                <td class="px-3 py-1.5 font-mono text-textPrimary font-medium">{sym.symbol_name}</td>
                <td class="px-3 py-1.5 font-mono text-textMuted">{sym.file_path}</td>
                <td class="px-3 py-1.5 font-mono text-textMuted">{(sym.confidence * 100).toFixed(0)}%</td>
                <td class="px-3 py-1.5 text-textMuted">
                  {#if sym.is_exempt}
                    <span class="px-1.5 py-0.5 rounded text-[10px] bg-surfaceHover text-textMuted">
                      Exempt{sym.exemption_reason ? `: ${sym.exemption_reason}` : ""}
                    </span>
                  {:else}
                    <span class="px-1.5 py-0.5 rounded text-[10px] bg-amber-500/15 text-amber-300 font-medium">
                      {formatDeadCodeStatus(sym)}
                    </span>
                  {/if}
                </td>
              </tr>
            {/each}
          </HealthTable>
        {:else}
          <p class="text-[11px] text-textMuted">
            No dead-code result for this repository yet.
          </p>
        {/if}
      </HealthSection>
    {:else}
      <div class="text-textMuted">Open a repository to scan dependency health.</div>
    {/if}
  </div>
</div>
