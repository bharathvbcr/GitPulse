import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CoverageViewer from "./CoverageViewer.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CoverageViewer.svelte"),
  "utf8",
);

describe("CoverageViewer", () => {
  it("renders empty state when no repo is open", () => {
    const { body } = render(CoverageViewer);
    expect(body).toContain("Test coverage");
    expect(body).toContain("Rescan coverage artifacts");
    expect(body).toContain("No coverage report");
    expect(body).toContain("Pick a file");
  });

  it("always renders the MANVI toggle affordance in the header", () => {
    const { body } = render(CoverageViewer);
    expect(body).toContain("MANVI: analyze coverage with the local model");
  });

  it("always renders a copy-report affordance in the header", () => {
    const { body } = render(CoverageViewer);
    expect(body).toContain("Copy coverage report");
    expect(body).toContain("disabled");
  });
});
describe("CoverageViewer report copy contract", () => {
  it("copies the canonical full coverage snapshot for the current repository", () => {
    const body = source.slice(
      source.indexOf("async function copyCoverageReport"),
      source.indexOf("async function generateAiReport"),
    );
    expect(body).toContain("const current = report;");
    expect(body).toContain("const repo = $repoStore.currentPath;");
    expect(body).toContain("formatCoverageReport(current, repo)");
    expect(body).toContain("await copyText(");
  });

  it("only reports success after the clipboard owner confirms the copy", () => {
    const body = source.slice(
      source.indexOf("async function copyCoverageReport"),
      source.indexOf("async function generateAiReport"),
    );
    expect(body).toContain("if (await copyText(");
    expect(body).toContain("reportCopied = true;");
    expect(source).toContain('{reportCopied ? "Coverage report copied" : "Copy coverage report"}');
  });

  it("cleans up report-copy feedback on reset and unmount", () => {
    expect(source).toContain("window.clearTimeout(reportCopyTimer);");
    expect(source.match(/window\.clearTimeout\(reportCopyTimer\)/g)?.length).toBeGreaterThanOrEqual(2);
  });
});
describe("CoverageViewer MANVI integration contracts", () => {
  it("sends the rendered report through the harness store to cmd_ai_coverage_report", () => {
    expect(source).toContain("harnessStore.coverageReport(");
    expect(source).toContain("formatCoverageReport(current, repo)");
  });

  it("runs model-authored steps through the scoped Manvi runner", () => {
    expect(source).toContain('"cmd_manvi_run_action"');
    expect(source).toContain('actionKind: "coverage"');
    expect(source).toContain("args: step.argv,");
    expect(source).toContain("timeoutSecs: 900");
  });

  it("runs curated generator commands through their own scoped Manvi origin", () => {
    expect(source).toContain('actionKind: "coverage_generator"');
  });

  it("derives runnable steps from the generation text like HealthPanel does", () => {
    expect(source).toContain("buildRunnablePlanSteps(aiGeneration.text)");
  });

  it("journals report generation, step execution and script runs into harnessStore", () => {
    expect(source).toContain('kind: "coverage-report"');
    expect(source).toContain('kind: "coverage-step"');
    expect(source).toContain('kind: "coverage-script"');
  });

  it("files a redacted snapshot through the canonical guarded issue owner", () => {
    const body = source.slice(
      source.indexOf("async function reportCoverageIssue"),
      source.indexOf("async function openCoverageIssue"),
    );
    expect(body).toContain("buildCoverageIssueDraft(current, repo, aiGeneration?.text)");
    expect(body).toContain("await askConfirm(");
    expect(body).toContain("repoStore.reportIssue(draft.title, draft.body, [])");
    expect(body).toContain("$repoStore.currentPath !== repo");
    expect(body).toContain("anyScriptRunning");
    expect(body).toContain("runningMissing");
    expect(body).toContain("isScanning");
    expect(body).not.toContain('invoke("cmd_github_create_issue"');
    expect(source).toContain("The draft excludes the local checkout path and command output.");
    expect(source).toContain("Create a guarded GitHub issue from this coverage snapshot");
  });

  it("rescans after commands settle so fresh artifacts appear", () => {
    const runStepBody = source.slice(source.indexOf("async function runStep"), source.indexOf("async function runAllSteps"));
    expect(runStepBody).toContain("rescan()");
    const scriptBody = source.slice(
      source.indexOf("async function runCoverageScript"),
      source.indexOf("async function runCoveragePipeline"),
    );
    expect(scriptBody).toContain("rescan()");
  });

  it("starts run-all with a fresh live guard instead of requiring a prior action", () => {
    const runAllBody = source.slice(
      source.indexOf("async function runAllSteps"),
      source.indexOf("function briefDetail"),
    );
    expect(runAllBody).toContain("const guard = beginOps()");
    expect(runAllBody).not.toContain("if (!opsInflight?.isLive()) break");
  });

  it("guards every async path and resets MANVI state on repo switch", () => {
    expect(source).toMatch(/generationInflight: AsyncGuard \| null/);
    expect(source).toMatch(/opsInflight: AsyncGuard \| null/);
    // The currentPath effect invokes the reset both when a repo closes and when one opens.
    expect(source.match(/resetManvi\(\);/g)?.length).toBe(2);
    // Unmount teardown cancels every guard and clears the copy timer.
    expect(source).toContain("generationInflight?.cancel();");
    expect(source).toContain("opsInflight?.cancel();");
    expect(source).toContain("window.clearTimeout(aiCopyTimer);");
  });

  it("keeps the generate button usable without a discovered model server", () => {
    expect(source).toContain("No local model server found. Start Ollama, LM Studio, llama.cpp, vLLM or Jan.");
    expect(source).not.toMatch(/onclick=\{generateAiReport\}[^>]*disabled=\{(?![^{]*generating)[^}]*aiReady/);
  });

  it("offers Run coverage with MANVI from scanner-planned commands when a family has no report", () => {
    expect(source).toContain("runMissingCoverage");
    expect(source).toContain("runCoverageFamily");
    expect(source).toContain("runCoveragePipeline");
    expect(source).toContain("Generate missing coverage artifacts with MANVI");
    expect(source).toMatch(/coverageFamilyViews\([\s\S]*report[\s\S]*\?\.families\)/);
    expect(source).toContain('actionKind: "coverage_generator"');
    expect(source).toContain("tokenized.error");
    expect(source).toContain("view.label");
    expect(source).toContain("durationHint");
    expect(source).toContain("cargo-llvm-cov");
    expect(source).toContain("several minutes");
  });

  it("runs each missing language independently and continues after a language fails", () => {
    const scriptBody = source.slice(
      source.indexOf("async function runCoverageScript"),
      source.indexOf("async function runCoveragePipeline"),
    );
    const pipelineBody = source.slice(
      source.indexOf("async function runCoveragePipeline"),
      source.indexOf("async function runCoverageFamily"),
    );
    const batchBody = source.slice(
      source.indexOf("async function runMissingCoverage"),
      source.indexOf("$effect"),
    );
    expect(scriptBody).toContain("Promise<boolean>");
    expect(pipelineBody).toContain("kind: step.kind");
    expect(pipelineBody).toContain('pipeline.mode === "first_success"');
    expect(pipelineBody).toContain('pipeline.mode === "all"');
    expect(batchBody).toContain("guard: batchGuard");
    expect(batchBody).toContain("await runCoveragePipeline(pipeline");
    expect(batchBody).not.toContain("if (!passed) break");
  });
});

describe("CoverageViewer flicker contracts", () => {
  it("hydrates from the per-repo report cache before rescanning", () => {
    expect(source).toContain("createRepoPanelCache<CoverageReport>()");
    expect(source).toContain("reportCache.set(repo, next)");
    expect(source).toContain("report = reportCache.get(repo) ?? null");
  });

  it("only invalidates gutters when the selected file's coverage changed", () => {
    const scanBody = source.slice(source.indexOf("async function scan("), source.indexOf("function rescan()"));
    expect(scanBody).toContain("sameCoverageSummary(prevEntry, nextEntry)");
  });

  it("keeps old source lines visible while a gutter reload is in flight", () => {
    expect(source).toContain("{#if isLoadingFile && sourceLines.length === 0}");
    // The full-pane scan placeholder stays gated on having no data at all.
    expect(source).toContain("{#if isScanning && !report}");
  });

  it("surfaces a capped scan as unknown coverage, not uncovered source", () => {
    // The detail payload's truncated flag must reach the UI: a capped scan's
    // missing gutters mean "unknown", and rendering them as plain misses is
    // the exact dishonesty the backend flag exists to prevent.
    expect(source).toContain("scanTruncated = detail.truncated;");
    expect(source).toContain("{#if scanTruncated}");
    expect(source).toContain("missing gutters here mean unknown, not uncovered");
    // The empty-state hint carries the same caveat.
    expect(source).toContain("this file's absence may be incomplete rather than a real zero");
  });

  it("retains selections case-insensitively and echoes auto-picks to the session", () => {
    const scanBody = source.slice(source.indexOf("async function scan("), source.indexOf("function rescan()"));
    expect(scanBody).toContain("toLowerCase()");
    expect(scanBody).toContain("repoStore.selectFilePath(first)");
  });

  it("seeds the persisted selection instead of wiping it after the sync effect", () => {
    expect(source).toContain("untrack(() => $repoStore.selectedFilePath)");
  });
});

describe("CoverageViewer failure diagnostics and copy contracts", () => {
  it("provides single and batch copy options for failed coverage scripts", () => {
    expect(source).toContain("formatFailedCoverageDiagnostics");
    expect(source).toContain("copyFailedScript");
    expect(source).toContain("copyAllFailedScripts");
    expect(source).toContain("unsuccessfulScripts");
    expect(source).toContain(
      "Copy diagnostics for every coverage command that did not produce coverage",
    );
    expect(source).toContain("Copy failure diagnostics for");
  });

  it("provides copy error affordances for rescan and scan failures", () => {
    expect(source).toContain("copyScanError");
    expect(source).toContain("Copy scan error");
  });

  it("provides copy diagnostics affordance for failed MANVI steps", () => {
    expect(source).toContain("copyStepOutput");
    expect(source).toContain("Copy step diagnostics");
  });

  it("records failed coverage runs and step failures to the diagnostics store", () => {
    expect(source).toContain('diagnostics.error(\n          "coverage"');
    expect(source).toContain('reportPanelError("coverage"');
  });

  it("cleans up all copy feedback timers on reset and unmount", () => {
    expect(source).toContain("window.clearTimeout(copiedScriptTimer);");
    expect(source).toContain("window.clearTimeout(copiedAllTimer);");
    expect(source).toContain("window.clearTimeout(scanErrorCopyTimer);");
    expect(source).toContain("window.clearTimeout(copiedStepTimer);");
  });
});

describe("CoverageViewer honesty contracts (regression)", () => {
  it("does not paint a 0.0% badge for a scan that measured nothing", () => {
    // A report with no parsable artifact has overall 0/0. Rendering that
    // through the same percentage badge a fully-uncovered repo gets turned
    // "we could not measure" into a red 0.0% finding.
    expect(source).toContain("{#if report && report.overall.lines_found > 0}");
    expect(source).toContain("No coverage data");
  });

  it("names the exact retained/observed counts on the capped-scan chip", () => {
    // "scan capped" with no numbers leaves the reader assuming the totals on
    // screen describe the repository.
    expect(source).toContain("cappedDetail");
    expect(source).toContain("limit_notices");
    expect(source).toMatch(/scan capped\{cappedDetail/);
  });

  it("only builds the chip detail from notices that actually dropped rows", () => {
    expect(source).toContain("notice.total > notice.kept");
  });

  it("shows the reason a family cannot be generated for", () => {
    // The backend guarantees a detail whenever it plans no command; the panel
    // must render it rather than leaving a bare family label. `view.toolDetail`
    // carries it whether or not anything is runnable, which is the point: a
    // family with no pipeline is exactly the one whose reason matters most.
    expect(source).toContain("{#if view.toolDetail}");
    // And the empty-state sidebar renders it too. It previously reached the
    // reason only through a pipeline, so `native` and `beam` — the families
    // that have none — appeared there as an unexplained blank.
    expect(source).toContain("{#if !view.found && !view.pipeline && view.toolDetail}");
  });

  it("never offers a Run button for a family with no planned command", () => {
    // Gated on the resolved pipeline rather than on a re-derived command
    // count. The strip used to draw the button from `suggested_commands` while
    // the click handler resolved the family through `missingCoveragePipelines`,
    // so a row the latter skipped rendered a button that did nothing.
    expect(source).toContain("{#if view.pipeline}");
    expect(source).not.toContain("suggestedCoverageCommands(family)");
  });
});

describe("CoverageViewer install-then-generate contracts (regression)", () => {
  it("offers the pipeline button whenever the scanner planned a generate command", () => {
    // Gated on the pipeline, not on tool_ready: a family whose toolchain is
    // missing is precisely the one that needs the setup-then-generate run,
    // and `coverageFamilyViews` builds a pipeline for it.
    expect(source).toContain("{#if view.pipeline}");
    expect(source).toContain("runCoverageFamily(view.family)");
  });

  it("renders the bare command chips from one snippet in both places", () => {
    // Running `vitest --coverage` or the venv pytest on its own, before setup,
    // is the failure the pipeline exists to prevent. That rule now lives in
    // `coverageFamilyViews` (see its own test), and both render sites reach
    // the chips through a single snippet rather than two copies whose
    // disabled conditions had already drifted apart.
    expect(source).toContain("{#snippet commandChips(view: CoverageFamilyView)}");
    expect(source).toContain("{#each view.commands as cmd");
    expect(source.match(/\{@render commandChips\(view\)\}/g)?.length).toBe(2);
  });

  it("runs every setup step before any generate step and stops on failure", () => {
    const pipelineFn = source.slice(
      source.indexOf("async function runCoveragePipeline"),
      source.indexOf("async function runCoverageFamily"),
    );
    expect(pipelineFn).toContain('step.kind === "setup"');
    expect(pipelineFn).toContain('step.kind === "generate"');
    expect(pipelineFn.indexOf("for (const step of setup)")).toBeLessThan(
      pipelineFn.indexOf("for (const step of generate)"),
    );
    // A failed install must not be followed by a run that cannot work.
    expect(pipelineFn).toMatch(/for \(const step of setup\)[\s\S]*?if \(!passed\) return false;/);
  });

  it("routes generation through the coverage_generator action kind", () => {
    // The read-only "coverage" kind cannot install anything; only this one is
    // permitted to mutate the project or the virtualenv.
    expect(source).toContain('actionKind: "coverage_generator"');
  });

  it("rescans after a pipeline so a newly installed toolchain is picked up", () => {
    const pipelineFn = source.slice(
      source.indexOf("async function runCoveragePipeline"),
      source.indexOf("async function runCoverageFamily"),
    );
    expect(pipelineFn).toContain("rescan()");
  });
});

/**
 * A coverage command's exit status and its *outcome* are different questions,
 * and the panel used to answer only the first. `go test ./...` over packages
 * with no test files exits 0 and writes a coverprofile containing no records;
 * a pytest run whose collection aborts can do the same. Both were reported as
 * a green "passed" against a report that had not moved.
 */
describe("CoverageViewer outcome-verification contracts (regression)", () => {
  const pipelineBody = source.slice(
    source.indexOf("async function runCoveragePipeline"),
    source.indexOf("async function runCoverageFamily"),
  );
  /**
   * The alternative-runner loop specifically. Slicing only the whole function
   * is not enough: the cumulative (`mode === "all"`) branch verifies at the
   * end, and an assertion that merely finds `familyGainedCoverage` somewhere
   * in the function still passes when the loop itself breaks on a bare exit-0.
   */
  const generateLoop = pipelineBody.slice(
    pipelineBody.indexOf("for (const step of generate)"),
    pipelineBody.indexOf("if (!ranClean) return false;"),
  );

  it("never leaves the generate loop on exit status alone", () => {
    // The old loop did `if (passed) { generated = true; break; }`, so a runner
    // that exited 0 and measured nothing shadowed the alternative that would
    // have worked, and the pipeline reported success.
    expect(generateLoop.length).toBeGreaterThan(0);
    expect(generateLoop).not.toContain("break;");
    expect(generateLoop).toContain("await familyGainedCoverage(");
    expect(generateLoop).toContain('pipeline.mode === "first_success"');
  });

  it("only returns success from the loop once coverage has appeared", () => {
    const verifyAt = generateLoop.indexOf("await familyGainedCoverage(");
    const successAt = generateLoop.indexOf("if (produced) return true;");
    expect(successAt).toBeGreaterThan(verifyAt);
  });

  it("records a clean run that produced nothing as no_data, never as passed", () => {
    expect(generateLoop).toContain("markProducedNoCoverage(");
    const marker = source.slice(
      source.indexOf("function markProducedNoCoverage"),
      source.indexOf("async function runCoveragePipeline"),
    );
    // Only ever narrows an existing pass; a real failure keeps its failure.
    expect(marker).toContain('if (!prev || prev.status !== "passed") return;');
    expect(marker).toContain('status: "no_data"');
  });

  it("verifies the cumulative set once every module has run", () => {
    // Rust workspaces and Go module sets are all-or-nothing, so the empty
    // result is attributed to the whole set rather than one arbitrary command.
    const cumulative = pipelineBody.slice(pipelineBody.indexOf("if (!ranClean) return false;"));
    expect(cumulative).toContain("await familyGainedCoverage(pipeline.family, guard)");
    expect(cumulative).toContain("markProducedNoCoverage(");
  });

  it("treats an unanswerable verification as failure, not success", () => {
    // Cancelled, no repo, or a failed rescan cannot be read as "coverage
    // appeared" — that would reinstate the bug through the error path.
    expect(generateLoop).toContain("if (produced === null) return false;");
    const helper = source.slice(
      source.indexOf("async function familyGainedCoverage"),
      source.indexOf("function markProducedNoCoverage"),
    );
    expect(helper).toContain("if (!guard.isLive() || !fresh) return null;");
    expect(helper).toContain("row.found === true");
  });

  it("surfaces a no-data run distinctly in the status list and its diagnostics", () => {
    expect(source).toContain("no coverage produced");
    expect(source).toContain('status.status === "failed" || status.status === "no_data"');
    // It is copyable: the output that explains why nothing was produced is the
    // actionable payload.
    expect(source).toContain('status.status === "no_data" ? ("no_data" as const)');
  });
});

/**
 * Every panel that runs a command must present the result through the one
 * owner, so that "what happened" is answered the same way everywhere and no
 * captured stream is silently dropped.
 */
describe("CoverageViewer run-output contracts (regression)", () => {
  it("builds detail and summary through the shared owner, not by hand", () => {
    expect(source).toContain("formatRunDetail(res)");
    expect(source).toContain("formatRunSummary(res)");
    expect(source).toContain("runPassed(res)");
  });

  it("no longer lets stderr shadow stdout", () => {
    // `res.stderr_tail || res.stdout_tail` discarded the entire diagnosis
    // whenever stderr held anything at all — at both of this panel's run
    // sites, and at HealthPanel's.
    expect(source).not.toContain("res.stderr_tail || res.stdout_tail");
    expect(source).not.toContain('"Timed out and was killed."');
    // The wire shape is imported, not redeclared.
    expect(source).not.toContain("interface TerminalRunResult {");
    expect(source).toContain('from "../terminal/runResult"');
  });

  it("shows the row summary but copies the whole detail", () => {
    expect(source).toContain("status.summary || briefDetail(status.detail)");
    expect(source).toContain("summary?: string;");
  });

  it("corrects the row text when a run is downgraded to no_data", () => {
    // The stored summary is the command's own last line, which for a clean
    // run that measured nothing is exactly the impression being corrected.
    const marker = source.slice(
      source.indexOf("function markProducedNoCoverage"),
      source.indexOf("async function runCoveragePipeline"),
    );
    expect(marker).toContain("summary: note");
  });
});
