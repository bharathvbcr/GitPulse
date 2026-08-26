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
});

describe("CoverageViewer MANVI integration contracts", () => {
  it("sends the rendered report through the harness store to cmd_ai_coverage_report", () => {
    expect(source).toContain("harnessStore.coverageReport(");
    expect(source).toContain("formatCoverageReport(current, repo)");
  });

  it("runs plan steps and script suggestions through the gated terminal runner", () => {
    expect(source).toContain('"cmd_terminal_run"');
    expect(source).toContain("args: step.argv,");
    expect(source).toContain("timeoutSecs: 900");
  });

  it("derives runnable steps from the generation text like HealthPanel does", () => {
    expect(source).toContain("extractPlanSteps(aiGeneration.text)");
    expect(source).toContain("tokenizeCommand(firstCmd)");
  });

  it("journals report generation, step execution and script runs into harnessStore", () => {
    expect(source).toContain('kind: "coverage-report"');
    expect(source).toContain('kind: "coverage-step"');
    expect(source).toContain('kind: "coverage-script"');
  });

  it("rescans after commands settle so fresh artifacts appear", () => {
    const runStepBody = source.slice(source.indexOf("async function runStep"), source.indexOf("async function runAllSteps"));
    expect(runStepBody).toContain("rescan()");
    const scriptBody = source.slice(
      source.indexOf("async function runCoverageScript"),
      source.indexOf("$effect"),
    );
    expect(scriptBody).toContain("rescan()");
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
