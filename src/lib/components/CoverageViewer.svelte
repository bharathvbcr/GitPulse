<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type { CoverageReport, FileCoverageSummary } from "../coverage/types";

  // Survives the per-tab remount so revisiting the coverage view renders the
  // last scan instantly instead of a full-pane "Scanning coverage…" flash.
  const reportCache = createRepoPanelCache<CoverageReport>();

  /** True when a file's coverage summary is unchanged between two reports. */
  function sameCoverageSummary(a: FileCoverageSummary, b: FileCoverageSummary): boolean {
    return (
      a.path === b.path &&
      a.language === b.language &&
      a.lines_found === b.lines_found &&
      a.lines_hit === b.lines_hit &&
      a.percentage === b.percentage
    );
  }
</script>

<script lang="ts">
  import { untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    Percent,
    RefreshCw,
    FileCode,
    Sparkles,
    LoaderCircle,
    Clipboard,
    Play,
    Check,
    X,
    Bug,
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatError } from "../ui/formatError";
  import { diagnostics } from "../diagnostics/diagnostics";
  import { reportPanelError } from "../diagnostics/report";
  import {
    coverageBarColor,
    coverageHitClass,
    formatCoveragePercent,
  } from "../coverage/format";
  import { buildHitMap, fetchFileCoverage, hitBadgeClass } from "../coverage/fileCoverage";
  import {
    buildCoverageIssueDraft,
    formatCoverageReport,
    formatFailedCoverageDiagnostics,
  } from "../coverage/report";
  import {
    missingCoveragePipelines,
    suggestedCoverageCommands,
    coverageFamilyRunLabel,
    type MissingCoveragePipeline,
  } from "../coverage/scripts";
  import {
    harnessStore,
    type AiGeneration,
    type PolicyVerdict,
    verdictLabel,
  } from "../stores/harnessStore";
  import { askConfirm } from "../stores/modalStore";
  import { openExternal as openExternalUrl } from "../desktop/openExternal";
  import { copyText } from "../desktop/clipboard";
  import { buildRunnablePlanSteps, tokenizeCommand } from "../terminal/tokenize";
  import VirtualList from "./VirtualList.svelte";
  import EmptyState from "./EmptyState.svelte";

  let report: CoverageReport | null = $state(null);
  let isScanning = $state(false);
  let scanError = $state<string | null>(null);
  let selectedPath = $state<string | null>(null);
  let sourceLines: string[] = $state([]);
  let hitMap: Map<number, number> = $state(new Map());
  let fileError = $state<string | null>(null);
  let contentError = $state<string | null>(null);
  let isLoadingFile = $state(false);
  let fileLoaded = $state(false);
  let linesTruncated = $state(false);
  let scanTruncated = $state(false);
  let reportVersion = $state(0);
  let scanInflight: AsyncGuard | null = null;
  let reportCopied = $state(false);
  let reportCopyTimer: number | null = null;

  function beginScan(): AsyncGuard {
    scanInflight?.cancel();
    const guard = createAsyncGuard();
    scanInflight = guard;
    return guard;
  }

  async function scan(repo: string, guard: AsyncGuard) {
    isScanning = true;
    scanError = null;
    try {
      const next = await invoke<CoverageReport>("cmd_scan_coverage", { repoPath: repo });
      if (!guard.isLive()) return;
      const prev = report;
      report = next;
      reportCache.set(repo, next);
      // Case-insensitive on purpose: the backend folds case when serving
      // detail lookups, so a path differing only in case from the artifact's
      // recorded form must not be reset to files[0].
      const wanted = selectedPath;
      const retained =
        wanted !== null && next.files.some((f) => f.path.toLowerCase() === wanted.toLowerCase());
      if (!retained) {
        const first = next.files[0]?.path ?? null;
        selectedPath = first;
        // The auto-pick is a real selection: echo it into the per-repo session
        // store so persistence and the other views agree on what is showing.
        if (first) repoStore.selectFilePath(first);
      }
      // Only invalidate gutters when the selected file's coverage actually
      // changed: plans/scripts call rescan() after every step, and bumping
      // unconditionally re-ran the gutter fetch behind a full-pane
      // "Loading …" swap even when nothing moved.
      const nextEntry = selectedPath
        ? next.files.find((f) => f.path === selectedPath)
        : undefined;
      const prevEntry =
        prev && selectedPath ? prev.files.find((f) => f.path === selectedPath) : undefined;
      if (!prevEntry || !nextEntry || !sameCoverageSummary(prevEntry, nextEntry)) {
        reportVersion += 1;
      }
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      scanError = reportPanelError("coverage", err);
    } finally {
      if (guard.isLive()) isScanning = false;
    }
  }

  function rescan() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const guard = beginScan();
    void scan(repo, guard);
  }

  /** Copies the same complete, bounded snapshot used by MANVI analysis. */
  async function copyCoverageReport() {
    const current = report;
    const repo = $repoStore.currentPath;
    if (!current || !repo) return;
    if (await copyText(formatCoverageReport(current, repo))) {
      // A copy can settle after a repo switch. The clipboard write already
      // happened, but stale success feedback must not leak into the new repo.
      if ($repoStore.currentPath !== repo || report !== current) return;
      reportCopied = true;
      if (reportCopyTimer !== null) window.clearTimeout(reportCopyTimer);
      reportCopyTimer = window.setTimeout(() => {
        reportCopyTimer = null;
        reportCopied = false;
      }, 1500);
    }
  }

  // ---------------------------------------------------------------------------
  // MANVI: local-model analysis of the coverage report
  // ---------------------------------------------------------------------------

  interface RunnableStep {
    id: string;
    number: number | null;
    text: string;
    command: string | null;
    argv: string[] | null;
    error?: string;
  }

  /** Wire shape of `crate::terminal::TerminalRunResult`. */
  interface TerminalRunResult {
    command: string;
    gated: boolean;
    policy?: PolicyVerdict | null;
    timed_out: boolean;
    exit_code: number | null;
    stdout_tail: string;
    stderr_tail: string;
    truncated: boolean;
    duration_ms: number;
  }

  interface ScriptStatus {
    label: string;
    running: boolean;
    status?: "passed" | "failed";
    detail?: string;
  }

  let aiOpen = $state(false);
  let generating = $state(false);
  let aiGeneration: AiGeneration | null = $state(null);
  let aiError = $state<string | null>(null);
  let generationCopied = $state(false);
  let generationInflight: AsyncGuard | null = null;
  let opsInflight: AsyncGuard | null = null;
  let stepResults: Record<
    string,
    { running: boolean; status?: "passed" | "failed"; detail?: string; duration_ms?: number }
  > = $state({});
  let runningAll = $state(false);
  let scriptStatuses: Record<string, ScriptStatus> = $state({});
  let runningMissing = $state(false);
  let aiCopyTimer: number | null = null;
  let copiedScriptKey = $state<string | null>(null);
  let copiedScriptTimer: number | null = null;
  let copiedAllFailed = $state(false);
  let copiedAllTimer: number | null = null;
  let scanErrorCopied = $state(false);
  let scanErrorCopyTimer: number | null = null;
  let copiedStepId = $state<string | null>(null);
  let copiedStepTimer: number | null = null;
  let issueSubmitting = $state(false);
  let issueNotice = $state<string | null>(null);
  let issueError = $state<string | null>(null);
  let issueUrl = $state<string | null>(null);

  let aiReady = $derived($harnessStore.ai?.ready ?? false);
  let missingPipelines = $derived.by(() =>
    missingCoveragePipelines((report as CoverageReport | null)?.families),
  );
  let anyScriptRunning = $derived(Object.values(scriptStatuses).some((status) => status.running));
  let failedScriptList = $derived.by(() =>
    Object.values(scriptStatuses).filter((status) => status.status === "failed"),
  );

  let aiSteps = $derived.by<RunnableStep[]>(() => {
    if (!aiGeneration?.text) return [];
    return buildRunnablePlanSteps(aiGeneration.text);
  });

  function beginOps(): AsyncGuard {
    opsInflight?.cancel();
    const guard = createAsyncGuard();
    opsInflight = guard;
    return guard;
  }

  /** Drops all MANVI state; called on repo switch so stale output never leaks across repos. */
  function resetManvi() {
    generationInflight?.cancel();
    generationInflight = null;
    opsInflight?.cancel();
    opsInflight = null;
    generating = false;
    aiGeneration = null;
    aiError = null;
    reportCopied = false;
    if (reportCopyTimer !== null) {
      window.clearTimeout(reportCopyTimer);
      reportCopyTimer = null;
    }
    generationCopied = false;
    if (aiCopyTimer !== null) {
      window.clearTimeout(aiCopyTimer);
      aiCopyTimer = null;
    }
    copiedScriptKey = null;
    if (copiedScriptTimer !== null) {
      window.clearTimeout(copiedScriptTimer);
      copiedScriptTimer = null;
    }
    copiedAllFailed = false;
    if (copiedAllTimer !== null) {
      window.clearTimeout(copiedAllTimer);
      copiedAllTimer = null;
    }
    scanErrorCopied = false;
    if (scanErrorCopyTimer !== null) {
      window.clearTimeout(scanErrorCopyTimer);
      scanErrorCopyTimer = null;
    }
    copiedStepId = null;
    if (copiedStepTimer !== null) {
      window.clearTimeout(copiedStepTimer);
      copiedStepTimer = null;
    }
    stepResults = {};
    runningAll = false;
    runningMissing = false;
    scriptStatuses = {};
    issueSubmitting = false;
    issueNotice = null;
    issueError = null;
    issueUrl = null;
  }

  /**
   * Files this exact bounded snapshot through repoStore's canonical guarded
   * GitHub issue owner. The draft intentionally excludes the local path and
   * command output; MANVI prose is optional and path-redacted by the formatter.
   */
  async function reportCoverageIssue() {
    const current = report;
    const repo = $repoStore.currentPath;
    if (
      !current ||
      !repo ||
      issueSubmitting ||
      isScanning ||
      generating ||
      runningAll ||
      runningMissing ||
      anyScriptRunning
    ) return;
    issueSubmitting = true;
    issueNotice = null;
    issueError = null;
    issueUrl = null;
    try {
      const draft = buildCoverageIssueDraft(current, repo, aiGeneration?.text);
      const confirmed = await askConfirm({
        title: "Create coverage issue",
        message:
          `${draft.title}\n\nCreate this issue on the repository's configured GitHub remote? ` +
          "The draft excludes the local checkout path and command output.",
        confirmLabel: "Create issue",
      });
      if (!confirmed || $repoStore.currentPath !== repo) return;

      // Do not require a `coverage` label: repositories are not guaranteed to
      // define one, and a missing label would make an otherwise valid issue fail.
      const outcome = await repoStore.reportIssue(draft.title, draft.body, []);
      if ($repoStore.currentPath !== repo) return;
      if (!outcome.ok) {
        issueError = outcome.error ?? "Coverage issue creation failed.";
        return;
      }
      issueUrl = outcome.output ?? null;
      const policy = outcome.policy ? ` — ${verdictLabel(outcome.policy)}` : "";
      const clipped = draft.clipped ? " Draft content was explicitly clipped." : "";
      issueNotice = `Coverage issue created${policy}.${clipped}`;
    } catch (err: unknown) {
      if ($repoStore.currentPath === repo) issueError = formatError(err);
    } finally {
      issueSubmitting = false;
    }
  }

  async function openCoverageIssue() {
    if (!issueUrl) return;
    try {
      await openExternalUrl(issueUrl);
    } catch (err: unknown) {
      issueError = reportPanelError("coverage", err);
    }
  }

  async function generateAiReport() {
    const current = report;
    const repo = $repoStore.currentPath;
    if (!current || !repo || generating) return;
    generationInflight?.cancel();
    const guard = createAsyncGuard();
    generationInflight = guard;
    generating = true;
    aiError = null;
    aiGeneration = null;
    stepResults = {};
    try {
      const next = await harnessStore.coverageReport(repo, formatCoverageReport(current, repo));
      if (!guard.isLive()) return;
      aiGeneration = next;
      harnessStore.recordAction({
        kind: "coverage-report",
        label: "AI coverage analysis",
        ok: true,
      });
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      aiError = reportPanelError("coverage", err);
      harnessStore.recordAction({
        kind: "coverage-report",
        label: "AI coverage analysis",
        ok: false,
      });
    } finally {
      if (guard.isLive()) generating = false;
    }
  }

  async function copyGeneration() {
    if (!aiGeneration?.text) return;
    if (await copyText(aiGeneration.text)) {
      generationCopied = true;
      if (aiCopyTimer !== null) window.clearTimeout(aiCopyTimer);
      aiCopyTimer = window.setTimeout(() => (generationCopied = false), 1500);
    }
  }

  async function copyFailedScript(key: string, status: ScriptStatus) {
    const text = formatFailedCoverageDiagnostics([status], {
      repoPath: $repoStore.currentPath,
    });
    if (await copyText(text)) {
      copiedScriptKey = key;
      if (copiedScriptTimer !== null) window.clearTimeout(copiedScriptTimer);
      copiedScriptTimer = window.setTimeout(() => {
        copiedScriptTimer = null;
        if (copiedScriptKey === key) copiedScriptKey = null;
      }, 1500);
    }
  }

  async function copyAllFailedScripts() {
    if (failedScriptList.length === 0) return;
    const text = formatFailedCoverageDiagnostics(failedScriptList, {
      repoPath: $repoStore.currentPath,
      scanError,
    });
    if (await copyText(text)) {
      copiedAllFailed = true;
      if (copiedAllTimer !== null) window.clearTimeout(copiedAllTimer);
      copiedAllTimer = window.setTimeout(() => {
        copiedAllTimer = null;
        copiedAllFailed = false;
      }, 1500);
    }
  }

  async function copyScanError() {
    if (!scanError) return;
    const text = formatFailedCoverageDiagnostics([], {
      repoPath: $repoStore.currentPath,
      scanError,
    });
    if (await copyText(text)) {
      scanErrorCopied = true;
      if (scanErrorCopyTimer !== null) window.clearTimeout(scanErrorCopyTimer);
      scanErrorCopyTimer = window.setTimeout(() => {
        scanErrorCopyTimer = null;
        scanErrorCopied = false;
      }, 1500);
    }
  }

  async function copyStepOutput(step: RunnableStep, res: { detail?: string }) {
    let text = `$ ${step.command ?? step.text}\n`;
    if (res.detail) text += `${res.detail}\n`;
    if (await copyText(text.trim())) {
      copiedStepId = step.id;
      if (copiedStepTimer !== null) window.clearTimeout(copiedStepTimer);
      copiedStepTimer = window.setTimeout(() => {
        copiedStepTimer = null;
        if (copiedStepId === step.id) copiedStepId = null;
      }, 1500);
    }
  }

  /**
   * Runs one plan step through the gated terminal runner. Fresh artifacts only
   * matter once the command has settled — pass or fail — so a rescan always
   * follows.
   */
  /** Appends the tail-truncation note so a clipped log never reads as whole. */
  function withClipNote(res: TerminalRunResult, base: string): string {
    return res.truncated ? `${base}\n(output clipped)` : base;
  }

  async function runStep(step: RunnableStep, guard: AsyncGuard): Promise<boolean> {
    const repoPath = $repoStore.currentPath;
    if (!step.argv || step.argv.length === 0 || !repoPath || issueSubmitting) return false;
    stepResults[step.id] = { running: true };

    let passed = false;
    try {
      const res = await invoke<TerminalRunResult>("cmd_manvi_run_action", {
        repoPath,
        args: step.argv,
        actionKind: "coverage",
        // Long enough for a cold install/build/test cycle; the backend clamps to [1s, 30min].
        timeoutSecs: 900,
      });
      if (!guard.isLive()) return false;

      passed = !res.timed_out && res.exit_code === 0;
      const detail = withClipNote(
        res,
        res.timed_out
          ? "Timed out and was killed."
          : passed
            ? res.stdout_tail || "Command completed successfully (exit 0)"
            : res.stderr_tail || res.stdout_tail || `Command failed (exit ${res.exit_code ?? "?"})`,
      );
      stepResults[step.id] = {
        running: false,
        status: passed ? "passed" : "failed",
        detail,
        duration_ms: res.duration_ms,
      };

      if (!passed) {
        diagnostics.error(
          "coverage",
          `Coverage step "${step.command ?? step.text}" failed (exit ${res.exit_code ?? "?"}):\n${detail}`,
        );
      }

      harnessStore.recordAction({
        kind: "coverage-step",
        label: step.command ?? step.text,
        ok: passed,
        verdict: res.policy ?? null,
      });
    } catch (err: unknown) {
      if (!guard.isLive()) return false;
      passed = false;
      const detail = reportPanelError("coverage", err);
      stepResults[step.id] = {
        running: false,
        status: "failed",
        detail,
      };
      harnessStore.recordAction({
        kind: "coverage-step",
        label: step.command ?? step.text,
        ok: false,
      });
    }

    if (guard.isLive()) rescan();
    return passed;
  }

  async function runAllSteps() {
    if (runningAll || runningMissing || issueSubmitting) return;
    runningAll = true;
    const guard = beginOps();
    try {
      for (const step of aiSteps) {
        if (!guard.isLive()) break;
        if (step.argv && step.argv.length > 0) {
          const ok = await runStep(step, guard);
          if (!ok) break;
        }
      }
    } finally {
      runningAll = false;
    }
  }

  /** First line of a command tail, capped so the chips strip stays readable. */
  function briefDetail(detail: string | undefined): string {
    if (!detail) return "";
    const firstLine = detail.split("\n")[0]?.trim() ?? "";
    if (firstLine.length <= 200) return firstLine;
    return `${firstLine.slice(0, 200)}…`;
  }

  function scriptKey(family: string, command: string): string {
    return `${family}:${command}`;
  }

  async function runCoverageScript(
    family: string,
    command: string,
    options: {
      rescan?: boolean;
      guard?: AsyncGuard;
      kind?: "setup" | "generate";
      durationHint?: string;
    } = {},
  ): Promise<boolean> {
    const repoPath = $repoStore.currentPath;
    if (!repoPath || issueSubmitting) return false;
    const key = scriptKey(family, command);
    const tokenized = tokenizeCommand(command);
    const label = options.kind === "setup" ? `setup: ${command}` : command;
    if (!tokenized.ok) {
      scriptStatuses[key] = {
        label,
        running: false,
        status: "failed",
        detail: tokenized.error,
      };
      reportPanelError("coverage", `Invalid coverage command "${command}": ${tokenized.error}`);
      harnessStore.recordAction({
        kind: "coverage-script",
        label: command,
        ok: false,
      });
      return false;
    }
    const guard = options.guard ?? beginOps();
    if (!guard.isLive()) return false;
    scriptStatuses[key] = {
      label,
      running: true,
      detail: options.durationHint || undefined,
    };

    let passed = false;
    try {
      const res = await invoke<TerminalRunResult>("cmd_manvi_run_action", {
        repoPath,
        args: tokenized.argv,
        actionKind: "coverage_generator",
        timeoutSecs: 900,
      });
      if (!guard.isLive()) return false;

      passed = !res.timed_out && res.exit_code === 0;
      const detail = withClipNote(
        res,
        res.timed_out
          ? "Timed out and was killed."
          : passed
            ? res.stdout_tail || "Command completed successfully (exit 0)"
            : res.stderr_tail || res.stdout_tail || `Command failed (exit ${res.exit_code ?? "?"})`,
      );
      scriptStatuses[key] = {
        label,
        running: false,
        status: passed ? "passed" : "failed",
        detail,
      };

      if (!passed) {
        diagnostics.error(
          "coverage",
          `Coverage command "${command}" failed (exit ${res.exit_code ?? "?"}):\n${detail}`,
        );
      }

      harnessStore.recordAction({
        kind: "coverage-script",
        label: command,
        ok: passed,
        verdict: res.policy ?? null,
      });
    } catch (err: unknown) {
      if (!guard.isLive()) return false;
      const detail = reportPanelError("coverage", err);
      scriptStatuses[key] = {
        label,
        running: false,
        status: "failed",
        detail,
      };
      harnessStore.recordAction({
        kind: "coverage-script",
        label: command,
        ok: false,
      });
    }

    if ((options.rescan ?? true) && guard.isLive()) rescan();
    return passed;
  }

  /**
   * Setup then generate for one language. Rust workspace and Go module
   * commands are cumulative and all must pass; other ecosystems expose
   * alternative runners, so stop at the first success instead of overwriting
   * artifacts by running every alternative sequentially.
   */
  async function runCoveragePipeline(
    pipeline: MissingCoveragePipeline,
    options: { rescan?: boolean; guard?: AsyncGuard } = {},
  ): Promise<boolean> {
    const guard = options.guard ?? beginOps();
    const setup = pipeline.steps.filter((step) => step.kind === "setup");
    const generate = pipeline.steps.filter((step) => step.kind === "generate");
    for (const step of setup) {
      if (!guard.isLive()) return false;
      const passed = await runCoverageScript(step.family, step.command, {
        rescan: false,
        guard,
        kind: step.kind,
        durationHint: pipeline.durationHint,
      });
      if (!passed) return false;
    }
    let generated = generate.length === 0;
    for (const step of generate) {
      if (!guard.isLive()) return false;
      const passed = await runCoverageScript(step.family, step.command, {
        rescan: false,
        guard,
        kind: step.kind,
        durationHint: pipeline.durationHint,
      });
      if (passed) {
        generated = true;
        if (pipeline.mode === "first_success") break;
      } else if (pipeline.mode === "all") {
        return false;
      }
    }
    if (!generated) return false;
    if ((options.rescan ?? true) && guard.isLive()) rescan();
    return true;
  }

  async function runCoverageFamily(familyName: string) {
    if (runningMissing || runningAll || anyScriptRunning || issueSubmitting) return;
    const pipeline = missingPipelines.find((item) => item.family === familyName);
    if (!pipeline) return;
    runningMissing = true;
    const guard = beginOps();
    try {
      await runCoveragePipeline(pipeline, { rescan: true, guard });
    } finally {
      runningMissing = false;
    }
  }

  async function runMissingCoverage() {
    if (
      runningMissing ||
      runningAll ||
      anyScriptRunning ||
      issueSubmitting ||
      missingPipelines.length === 0
    ) return;
    runningMissing = true;
    const batchGuard = beginOps();
    const batch = missingPipelines;
    try {
      for (const pipeline of batch) {
        if (!batchGuard.isLive()) return;
        // One language's failure must not skip the others (JS must still run
        // if Rust's llvm-cov install fails).
        await runCoveragePipeline(pipeline, { rescan: false, guard: batchGuard });
      }
    } finally {
      runningMissing = false;
      if (batchGuard.isLive()) rescan();
    }
  }

  $effect(() => {
    return () => {
      scanInflight?.cancel();
      generationInflight?.cancel();
      opsInflight?.cancel();
      if (reportCopyTimer !== null) window.clearTimeout(reportCopyTimer);
      if (aiCopyTimer !== null) window.clearTimeout(aiCopyTimer);
      if (copiedScriptTimer !== null) window.clearTimeout(copiedScriptTimer);
      if (copiedAllTimer !== null) window.clearTimeout(copiedAllTimer);
      if (scanErrorCopyTimer !== null) window.clearTimeout(scanErrorCopyTimer);
      if (copiedStepTimer !== null) window.clearTimeout(copiedStepTimer);
    };
  });

  // Memoized on the actual dependency: every status-poll / stats-drain
  // emission re-runs the effect (project() hands out fresh objects), and an
  // unguarded rerun would clobber the locally selected coverage file.
  let prevSyncedSelection: string | null = null;
  $effect(() => {
    const selected = $repoStore.selectedFilePath;
    if (selected === prevSyncedSelection) return;
    prevSyncedSelection = selected;
    if (selected) {
      selectedPath = selected;
    }
  });

  // Repo-scoped scan lifecycle, memoized on currentPath: poll ticks re-emit
  // the store object, and an unguarded rerun would blank the report and
  // restart the scan IPC on every emission.
  let prevScanRepo: string | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    if (repo === prevScanRepo) return;
    prevScanRepo = repo;
    if (!repo) {
      scanInflight?.cancel();
      report = null;
      selectedPath = null;
      scanError = null;
      isScanning = false;
      resetManvi();
      return;
    }
    // Seed from the persisted per-repo selection instead of null. This effect
    // runs after the store-sync effect on mount, so an unconditional wipe here
    // would discard the selection that effect just restored and make the sync
    // dead code; scan() still validates membership (case-insensitively) when
    // the report lands.
    selectedPath = untrack(() => $repoStore.selectedFilePath);
    fileError = null;
    contentError = null;
    sourceLines = [];
    hitMap = new Map();
    fileLoaded = false;
    linesTruncated = false;
    scanTruncated = false;
    // Hydrate last-known data synchronously so a revisit renders instantly;
    // the scan below then refreshes in place behind the visible content.
    report = reportCache.get(repo) ?? null;
    resetManvi();
    const guard = beginScan();
    void scan(repo, guard);
    return () => {
      if (scanInflight === guard) {
        guard.cancel();
      }
    };
  });

  // Gutter fetch keyed on repo + selected file + report version. Memoized on
  // exactly that key: reading the store tracks every emission, so without the
  // guard each poll tick would cancel and refetch the file content and hits.
  let prevFileLoadKey: string | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    const path = selectedPath;
    void reportVersion; // read so each successful rescan forces a gutter refetch
    const loadKey = `${repo ?? "\u0000"}\u0000${path ?? "\u0000"}\u0000${reportVersion}`;
    if (loadKey === prevFileLoadKey) return;
    prevFileLoadKey = loadKey;
    if (!repo || !path) {
      sourceLines = [];
      hitMap = new Map();
      contentError = null;
      fileLoaded = false;
      linesTruncated = false;
      scanTruncated = false;
      return;
    }
    let cancelled = false;
    isLoadingFile = true;
    fileError = null;
    contentError = null;
    fileLoaded = false;
    linesTruncated = false;
    scanTruncated = false;
    void (async () => {
      try {
        const [detail, contentOutcome] = await Promise.all([
          fetchFileCoverage(repo, path),
          invoke<string>("cmd_get_file_content", {
            repoPath: repo,
            filePath: path,
            commitId: null,
          }).then(
            (content) => ({ ok: true as const, content }),
            (err: unknown) => ({ ok: false as const, reason: reportPanelError("coverage", err) })
          ),
        ]);
        if (cancelled) return;
        hitMap = buildHitMap(detail.lines);
        linesTruncated = detail.lines_truncated;
        // The backend flags a capped scan explicitly so absence here reads as
        // "unknown", not "uncovered"; surfacing it keeps that contract.
        scanTruncated = detail.truncated;
        let lines: string[] = [];
        if (contentOutcome.ok) {
          fileLoaded = true;
          if (contentOutcome.content.length > 0) {
            lines = contentOutcome.content.split("\n").map((l) => l.replace(/\r$/, ""));
            // content ending in "\n" yields one phantom trailing "" from split
            if (contentOutcome.content.endsWith("\n")) lines.pop();
          }
        } else {
          contentError = contentOutcome.reason;
        }
        sourceLines = lines;
      } catch (err: unknown) {
        if (!cancelled) {
          fileError = reportPanelError("coverage", err);
          sourceLines = [];
          hitMap = new Map();
          contentError = null;
          fileLoaded = false;
          linesTruncated = false;
          scanTruncated = false;
        }
      } finally {
        if (!cancelled) isLoadingFile = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function selectFile(path: string) {
    selectedPath = path;
    repoStore.selectFilePath(path);
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs overflow-hidden">
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between font-sans shrink-0">
    <div class="flex items-center gap-3 min-w-0">
      <Percent size={16} class="text-accent shrink-0" />
      {#if report}
        <div class="flex items-center gap-2">
          <span
            class="font-semibold tabular-nums"
            style="color: {coverageBarColor(report.overall.percentage)}"
          >{formatCoveragePercent(report.overall.percentage)}</span>
          <span class="text-textMuted">
            {report.overall.lines_hit}/{report.overall.lines_found} lines
          </span>
        </div>
      {:else}
        <span class="text-textMuted">Test coverage</span>
      {/if}
    </div>
    <div class="flex items-center gap-3">
      <div class="flex items-center gap-3 text-[11px] text-textMuted">
        <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-emerald-500/50"></span> hit</span>
        <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-red-500/50"></span> missed</span>
        <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-gray-500/30"></span> uninstrumented</span>
      </div>
      <button
        type="button"
        class="gp-icon-btn !p-1 hover:text-accent"
        class:text-emerald-400={reportCopied}
        title={reportCopied ? "Coverage report copied" : "Copy coverage report"}
        aria-label={reportCopied ? "Coverage report copied" : "Copy coverage report"}
        onclick={() => void copyCoverageReport()}
        disabled={!report || !$repoStore.currentPath}
      >
        {#if reportCopied}
          <Check size={13} />
        {:else}
          <Clipboard size={13} />
        {/if}
      </button>
      <button
        type="button"
        class="gp-icon-btn !p-1 hover:text-accent"
        class:text-accent={aiOpen}
        title="MANVI: analyze coverage with the local model"
        onclick={() => (aiOpen = !aiOpen)}
        disabled={!report}
      >
        <Sparkles size={13} />
      </button>
      <button
        type="button"
        class="gp-icon-btn !p-1 hover:text-accent"
        title="Create a guarded GitHub issue from this coverage snapshot"
        onclick={() => void reportCoverageIssue()}
        disabled={!report || issueSubmitting || isScanning || generating || runningAll || runningMissing || anyScriptRunning}
      >
        {#if issueSubmitting}
          <LoaderCircle size={13} class="animate-spin" />
        {:else}
          <Bug size={13} />
        {/if}
      </button>
      {#if missingPipelines.length > 0}
        <button
          type="button"
          class="gp-btn-primary !py-1 !px-2.5 !text-[11px]"
          title="Generate each missing language with MANVI. Rust needs cargo-llvm-cov; a full run can take several minutes."
          onclick={() => void runMissingCoverage()}
          disabled={runningMissing || runningAll || anyScriptRunning || isScanning || issueSubmitting}
        >
          {#if runningMissing}
            <LoaderCircle size={11} class="animate-spin" />
            Running…
          {:else}
            <Play size={11} />
            Run coverage
          {/if}
        </button>
      {/if}
      <button
        type="button"
        class="gp-icon-btn !p-1 hover:text-accent"
        title="Rescan coverage artifacts"
        onclick={rescan}
        disabled={isScanning}
      >
        <RefreshCw size={13} class={isScanning ? "animate-spin" : ""} />
      </button>
    </div>
  </div>

  {#if report && scanError}
    <div class="px-4 py-1.5 border-b border-border bg-red-500/10 text-red-400 font-sans shrink-0 flex items-center justify-between gap-2">
      <span class="truncate" title={scanError}>Rescan failed: {scanError} — showing previous results.</span>
      <button
        type="button"
        onclick={() => void copyScanError()}
        class="gp-btn !py-0.5 !px-2 !text-[10px] shrink-0"
        title="Copy scan error"
      >
        {#if scanErrorCopied}
          <Check size={10} class="text-emerald-400" />
          <span>Copied</span>
        {:else}
          <Clipboard size={10} />
          <span>Copy error</span>
        {/if}
      </button>
    </div>
  {/if}

  {#if issueNotice || issueError}
    <div class="px-4 py-1.5 border-b border-border bg-surface/80 font-sans shrink-0 flex items-center gap-2">
      {#if issueError}
        <span class="text-rose-400 truncate" title={issueError}>Issue creation failed: {issueError}</span>
      {:else}
        <span class="text-emerald-400 truncate">{issueNotice}</span>
        {#if issueUrl}
          <button type="button" class="gp-btn !py-0.5 !px-2 !text-[10px] shrink-0" onclick={() => void openCoverageIssue()}>
            Open issue
          </button>
        {/if}
      {/if}
    </div>
  {/if}

  {#if report && (report.families.length > 0 || report.truncated)}
    <div class="border-b border-border/40 bg-surface/40 font-sans shrink-0">
      <div class="px-4 py-1.5 flex items-center gap-3 overflow-x-auto">
        {#each report.families as family (family.family)}
          <div class="flex items-center gap-1.5 shrink-0" title="{family.expected_formats.join(', ')} · {family.expected_paths.join(', ')}">
            <span class="w-2 h-2 rounded-full" style="background-color: {family.color_hex}"></span>
            <span class="text-textPrimary/80">{family.languages.join(", ")}</span>
            <span class="text-textMuted/70">{family.family}</span>
            {#if family.found}
              <span class="text-emerald-400/80">report found</span>
            {:else}
              <span class="text-textMuted/60">no report</span>
              {#if family.tool_ready === false && family.tool_detail}
                <span class="text-amber-400/90">{family.tool_detail}</span>
              {/if}
              {#if family.duration_hint}
                <span class="text-textMuted/50">{family.duration_hint}</span>
              {/if}
              {#if suggestedCoverageCommands(family).length > 0}
                <button
                  type="button"
                  class="shrink-0 px-1.5 py-0.5 rounded-full border border-accent/40 bg-accent/10 font-sans text-[10px] text-textPrimary hover:bg-accent/20 hover:text-accent transition-colors disabled:opacity-40"
                  title={family.duration_hint || "Generate coverage artifacts with MANVI"}
                  disabled={runningMissing || runningAll || anyScriptRunning || isScanning || issueSubmitting}
                  onclick={() => void runCoverageFamily(family.family)}
                >Run {coverageFamilyRunLabel(family.family)} coverage</button>
              {/if}
              {#if family.tool_ready !== false}
                {#each suggestedCoverageCommands(family) as cmd (`${family.family}:${cmd}`)}
                  <button
                    type="button"
                    class="shrink-0 px-1.5 py-0.5 rounded-full border border-border/70 bg-background/60 font-mono text-[10px] text-textPrimary hover:bg-surfaceHover hover:text-accent transition-colors disabled:opacity-40"
                    title="Generate coverage artifacts with MANVI"
                    disabled={runningMissing || runningAll || anyScriptRunning || isScanning || issueSubmitting}
                    onclick={() => void runCoverageScript(family.family, cmd, { durationHint: family.duration_hint })}
                  >{cmd}</button>
                {/each}
              {/if}
            {/if}
          </div>
        {/each}
        {#if report.truncated}
          <span class="text-amber-400 shrink-0 font-medium">scan capped</span>
        {/if}
      </div>
      {#if Object.keys(scriptStatuses).length > 0}
        <div class="px-4 pb-1.5 space-y-0.5 max-h-24 overflow-auto">
          {#if failedScriptList.length > 0}
            <div class="flex items-center justify-between gap-2 pb-0.5">
              <span class="text-[9px] uppercase tracking-wider text-rose-400 font-semibold">Failed coverage</span>
              <button
                type="button"
                onclick={() => void copyAllFailedScripts()}
                class="text-[10px] text-rose-400 hover:text-rose-300 underline inline-flex items-center gap-1 shrink-0"
                title="Copy all failed coverage command diagnostics"
              >
                {#if copiedAllFailed}
                  <Check size={10} class="text-emerald-400" />
                  <span class="text-emerald-400">Copied</span>
                {:else}
                  <Clipboard size={10} />
                  <span>Copy failure diagnostics{failedScriptList.length > 1 ? ` (${failedScriptList.length})` : ""}</span>
                {/if}
              </button>
            </div>
          {/if}
          {#each Object.entries(scriptStatuses) as [key, status] (key)}
            <div class="flex items-center gap-2 text-[10px] min-w-0">
              {#if status.running}
                <LoaderCircle size={10} class="animate-spin text-accent shrink-0" />
                <span class="text-textMuted shrink-0">running</span>
              {:else if status.status === "passed"}
                <Check size={10} class="text-emerald-400 shrink-0" />
                <span class="text-emerald-400/90 shrink-0">passed</span>
              {:else}
                <X size={10} class="text-rose-400 shrink-0" />
                <span class="text-rose-400/90 shrink-0">failed</span>
              {/if}
              <span class="font-mono text-textPrimary/80 truncate shrink-0">{status.label}</span>
              {#if briefDetail(status.detail)}
                <span class="text-textMuted/70 truncate">{briefDetail(status.detail)}</span>
              {/if}
              {#if status.status === "failed"}
                <button
                  type="button"
                  onclick={() => void copyFailedScript(key, status)}
                  class="ml-auto p-0.5 rounded hover:bg-surfaceHover text-textMuted hover:text-textPrimary shrink-0"
                  title="Copy failure diagnostics for {status.label}"
                  aria-label="Copy failure diagnostics for {status.label}"
                >
                  {#if copiedScriptKey === key}
                    <Check size={10} class="text-emerald-400" />
                  {:else}
                    <Clipboard size={10} />
                  {/if}
                </button>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}

  {#if report && report.languages.length > 0}
    <div class="px-4 py-2 border-b border-border/40 bg-surface/20 flex items-center gap-4 overflow-x-auto font-sans">
      <span class="text-[10px] uppercase tracking-wider text-textMuted/60 shrink-0">by language</span>
      {#each report.languages as lang (lang.language)}
        <div class="shrink-0 min-w-36">
          <div class="flex items-center justify-between gap-2 mb-0.5">
            <span class="flex items-center gap-1.5 min-w-0">
              <span class="w-2 h-2 rounded-full shrink-0" style="background-color: {lang.color_hex}"></span>
              <span class="truncate text-textPrimary/80">{lang.language}</span>
              <span class="text-textMuted/50">{lang.files} {lang.files === 1 ? "file" : "files"}</span>
            </span>
            <span class="tabular-nums shrink-0" style="color: {coverageBarColor(lang.percentage)}">{formatCoveragePercent(lang.percentage)}</span>
          </div>
          <div class="h-1 rounded-full bg-surfaceHover overflow-hidden">
            <div
              class="h-full rounded-full"
              style="width: {Math.min(100, Math.max(0, lang.percentage))}%; background-color: {coverageBarColor(lang.percentage)};"
            ></div>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  <div class="flex-1 flex min-h-0">
    <div class="w-72 shrink-0 border-r border-border/60 flex flex-col bg-surface/40 p-1.5">
      {#if isScanning && !report}
        <div class="flex-1 flex items-center justify-center text-textMuted font-sans">Scanning coverage…</div>
      {:else if report && report.files.length > 0}
        <!-- Virtualized: the scan cap allows 4,000 files, and a keyed each of
             that size mounts ~20k nodes and re-diffs them on every selection. -->
        <VirtualList items={report.files} rowHeight={26} overscan={20} class="flex-1">
          {#snippet row(file)}
            {#if file}
              <button
                type="button"
                onclick={() => selectFile(file.path)}
                style="height: 26px;"
                class="w-full px-2.5 rounded-full text-left flex items-center gap-2 transition-colors {selectedPath === file.path ? 'bg-accent/15 ring-1 ring-inset ring-accent/30' : 'hover:bg-surfaceHover'}"
              >
                <span class="w-2 h-2 rounded-full shrink-0" style="background-color: {file.color_hex}"></span>
                <span class="flex-1 truncate font-mono text-[11px] text-textPrimary">{file.path}</span>
                <span class="tabular-nums shrink-0" style="color: {coverageBarColor(file.percentage)}">{formatCoveragePercent(file.percentage)}</span>
              </button>
            {/if}
          {/snippet}
        </VirtualList>
      {:else if report}
        <div class="p-3 text-textMuted font-sans space-y-2">
          <p>No coverage reports for the detected languages.</p>
          {#each missingPipelines as pipeline (pipeline.family)}
            {#if pipeline.toolDetail}
              <p class="text-amber-400/90">{pipeline.toolDetail}</p>
            {/if}
            {#if pipeline.durationHint}
              <p>{pipeline.durationHint}</p>
            {/if}
            <button
              type="button"
              class="gp-btn-primary !py-1.5"
              title={pipeline.durationHint || "Generate missing coverage artifacts with MANVI"}
              onclick={() => void runCoverageFamily(pipeline.family)}
              disabled={runningMissing || runningAll || anyScriptRunning || isScanning || issueSubmitting}
            >
              {#if runningMissing}
                <LoaderCircle size={12} class="animate-spin" />
                Running…
              {:else}
                <Play size={12} />
                Run {pipeline.label} coverage with MANVI
              {/if}
            </button>
          {/each}
          {#if missingPipelines.length > 1}
            <button
              type="button"
              class="gp-btn !py-1.5"
              title="Generate each missing language with MANVI. Rust needs cargo-llvm-cov; a full run can take several minutes."
              onclick={() => void runMissingCoverage()}
              disabled={runningMissing || runningAll || anyScriptRunning || isScanning || issueSubmitting}
            >
              Run all missing coverage
            </button>
          {/if}
          {#each report.families as family (family.family)}
            <p>
              Looked for {family.family} ({family.expected_formats.join(", ")}):
              {family.expected_paths.join(", ")}
            </p>
            {#if !family.found && family.tool_ready !== false}
              <div class="flex flex-wrap gap-1.5">
                {#each suggestedCoverageCommands(family) as cmd (cmd)}
                  <button
                    type="button"
                    class="px-2 py-0.5 rounded-full border border-border/70 bg-background/60 font-mono text-[10px] text-textPrimary hover:bg-surfaceHover hover:text-accent transition-colors disabled:opacity-40"
                    title="Generate coverage artifacts with MANVI"
                    disabled={runningMissing || runningAll || anyScriptRunning || isScanning || issueSubmitting}
                    onclick={() => void runCoverageScript(family.family, cmd, { durationHint: family.duration_hint })}
                  >{cmd}</button>
                {/each}
              </div>
            {/if}
          {/each}
          {#if report.families.length === 0}
            <p>No programming languages found to scan.</p>
          {/if}
        </div>
      {:else if scanError}
        <div class="p-3 text-rose-400 font-sans space-y-2">
          <div>{scanError}</div>
          <button
            type="button"
            onclick={() => void copyScanError()}
            class="gp-btn !py-1 !text-[11px]"
            title="Copy scan error diagnostics"
          >
            {#if scanErrorCopied}
              <Check size={12} class="text-emerald-400" />
              <span>Copied</span>
            {:else}
              <Clipboard size={12} />
              <span>Copy error</span>
            {/if}
          </button>
        </div>
      {:else}
        <EmptyState
          icon={Percent}
          title="No coverage report"
          hint="Open a repository to scan coverage artifacts."
        />
      {/if}

      {#if report && report.artifacts.length > 0}
        <div class="border-t border-border/60 p-2 text-[10px] text-textMuted font-sans max-h-28 overflow-auto">
          {#each report.artifacts as artifact, i (`${i}:${artifact.path}`)}
            <div class="truncate" title={artifact.skip_reason || artifact.format}>
              {artifact.path}
              <span class="text-textMuted/70">({artifact.format})</span>
              {#if artifact.skipped}
                <span class="text-amber-400"> skipped</span>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <div class="flex-1 min-h-0 font-mono flex flex-col">
      {#if scanTruncated}
        <div class="px-4 py-1.5 border-b border-border bg-amber-500/10 text-amber-300 font-sans text-[11px] shrink-0">
          The scan hit a cap before every artifact was read — missing gutters here mean unknown, not uncovered.
        </div>
      {/if}
      {#if linesTruncated}
        <div class="px-4 py-1.5 border-b border-border bg-amber-500/10 text-amber-300 font-sans text-[11px] shrink-0">
          Gutter view capped for display; totals still reflect the full file.
        </div>
      {/if}
      {#if isLoadingFile && sourceLines.length === 0}
        <div class="h-full flex items-center justify-center text-textMuted font-sans">Loading {selectedPath}…</div>
      {:else if fileError}
        <div class="h-full flex items-center justify-center text-rose-400 font-sans p-4">{fileError}</div>
      {:else if sourceLines.length > 0}
        <VirtualList items={sourceLines} rowHeight={24} overscan={15} class="h-full">
          {#snippet row(line, index)}
            {#if line !== undefined}
              {@const hits = hitMap.get(index + 1)}
              <div class="flex items-center h-6 hover:bg-surfaceHover/40 {coverageHitClass(hits)}" style="height: 24px;">
                <span class="w-10 px-2 text-right text-textMuted/40 text-[10px] select-none shrink-0">{index + 1}</span>
                <span class={hitBadgeClass(hits)}>{hits === undefined ? "·" : hits}</span>
                <span class="px-3 whitespace-pre overflow-hidden text-textPrimary">{line}</span>
              </div>
            {/if}
          {/snippet}
        </VirtualList>
      {:else if fileLoaded && sourceLines.length === 0}
        <div class="h-full flex items-center justify-center text-textMuted font-sans p-4">
          Empty file (0 lines)
        </div>
      {:else if selectedPath}
        <EmptyState
          icon={FileCode}
          title={!contentError && hitMap.size === 0 ? "Not in any coverage report" : "No source available"}
          hint={contentError
            ? `The source for this path could not be loaded: ${contentError}`
            : hitMap.size === 0
              ? scanTruncated
                ? "The scan was capped, so this file's absence may be incomplete rather than a real zero."
                : "This file was not instrumented by the coverage artifacts found in the repository."
              : "The source for this path could not be loaded."}
        />
      {:else}
        <EmptyState
          icon={Percent}
          title="Pick a file"
          hint="Select a file on the left to inspect its line coverage."
        />
      {/if}
    </div>

    {#if aiOpen && report}
      <aside class="w-[22rem] shrink-0 border-l border-border/60 flex flex-col bg-surface/30 overflow-hidden font-sans">
        <div class="px-3 py-2 border-b border-border/60 space-y-2 shrink-0">
          <h3 class="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-textMuted">
            <Sparkles size={11} class="text-accent" />
            MANVI coverage analysis
          </h3>
          <button
            type="button"
            onclick={generateAiReport}
            disabled={generating}
            class="gp-btn-primary w-full justify-center"
            title={aiReady
              ? "Send the rendered coverage report to the local model (via the MANVI harness)"
              : "Needs a local model server. The exact error will be reported if none is reachable."}
          >
            {#if generating}
              <LoaderCircle size={12} class="animate-spin" />
              Generating…
            {:else}
              <Sparkles size={12} />
              Generate AI report
            {/if}
          </button>
        </div>

        {#if !aiReady}
          <div class="px-3 py-2 border-b border-border/40 text-[11px] text-textMuted shrink-0">
            No local model server found. Start Ollama, LM Studio, llama.cpp, vLLM or Jan.
          </div>
        {/if}

        <div class="flex-1 overflow-auto p-3 space-y-3 min-h-0">
          {#if aiError}
            <div class="p-2.5 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-300">
              Analysis failed: {aiError}
            </div>
          {/if}

          {#if generating && !aiGeneration}
            <div class="flex items-center gap-2 text-textMuted py-2">
              <LoaderCircle size={14} class="animate-spin" />
              Sending the coverage report to the local model…
            </div>
          {:else if aiGeneration}
            <p class="text-[11px] text-textMuted font-mono truncate" title="{aiGeneration.model} · {aiGeneration.context_source}">
              {aiGeneration.model} · {aiGeneration.elapsed_ms} ms · {aiGeneration.prompt_tokens}/{aiGeneration.completion_tokens} tokens · {aiGeneration.context_source}
            </p>
            {#if aiGeneration.warnings.length > 0}
              <ul class="space-y-1">
                {#each aiGeneration.warnings as warning}
                  <li class="text-amber-400 leading-relaxed">{warning}</li>
                {/each}
              </ul>
            {/if}
            <div class="whitespace-pre-wrap select-all leading-relaxed text-textSecondary">{aiGeneration.text}</div>
            <div class="flex justify-end">
              <button type="button" onclick={copyGeneration} class="gp-btn !py-1 !text-[11px]" title="Copy the analysis">
                <Clipboard size={12} />
                {generationCopied ? "Copied" : "Copy"}
              </button>
            </div>

            {#if aiSteps.length > 0}
              <div class="space-y-2 pt-1 border-t border-border/40">
                <div class="flex items-center justify-between gap-2 pt-1">
                  <h4 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Runnable steps</h4>
                  <button
                    type="button"
                    onclick={runAllSteps}
                    disabled={runningAll || runningMissing || issueSubmitting || Object.values(stepResults).some((r) => r.running)}
                    class="gp-btn-primary !py-1 !text-[11px]"
                    title="Execute all executable steps sequentially, stopping at the first failure"
                  >
                    {#if runningAll}
                      <LoaderCircle size={11} class="animate-spin" />
                      Running…
                    {:else}
                      <Play size={11} />
                      Run all
                    {/if}
                  </button>
                </div>
                {#each aiSteps as step (step.id)}
                  {@const res = stepResults[step.id]}
                  <div class="p-2.5 rounded-xl border border-border/70 bg-background/60 space-y-1.5">
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
                          onclick={() => void runStep(step, beginOps())}
                          disabled={res?.running || runningAll || runningMissing || issueSubmitting}
                          class="gp-btn !py-1 !px-2.5 text-xs shrink-0 disabled:opacity-50"
                          title="Execute this command step directly"
                        >
                          {#if res?.running}
                            <LoaderCircle size={11} class="animate-spin text-accent" />
                            <span>Running…</span>
                          {:else if res?.status === "passed"}
                            <Check size={11} class="text-emerald-400" />
                            <span>Run again</span>
                          {:else if res?.status === "failed"}
                            <Play size={11} class="text-rose-400" />
                            <span>Retry</span>
                          {:else}
                            <Play size={11} class="text-accent" />
                            <span>Run</span>
                          {/if}
                        </button>
                      {/if}
                    </div>

                    {#if step.command}
                      <div class="px-2.5 py-1.5 rounded-lg bg-surface border border-border/60 font-mono text-[11px] text-textPrimary truncate">
                        {step.command}
                      </div>
                    {/if}

                    {#if step.error}
                      <div class="text-[10px] text-amber-300">{step.error}</div>
                    {/if}

                    {#if res?.detail}
                      <div class="relative group">
                        <div class="p-2 pr-7 rounded bg-surface/80 border border-border/40 font-mono text-[10px] text-textMuted whitespace-pre-wrap max-h-32 overflow-y-auto">
                          {res.detail}
                        </div>
                        <button
                          type="button"
                          onclick={() => void copyStepOutput(step, res)}
                          class="absolute top-1.5 right-1.5 p-1 rounded bg-surface/90 border border-border/60 text-textMuted hover:text-textPrimary hover:bg-surfaceHover transition-colors"
                          title="Copy step diagnostics"
                          aria-label="Copy step diagnostics"
                        >
                          {#if copiedStepId === step.id}
                            <Check size={11} class="text-emerald-400" />
                          {:else}
                            <Clipboard size={11} />
                          {/if}
                        </button>
                      </div>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          {:else}
            <p class="text-textMuted text-[11px] leading-relaxed">
              Ask the local model what these results mean and which commands would raise coverage.
            </p>
          {/if}
        </div>
      </aside>
    {/if}
  </div>
</div>
