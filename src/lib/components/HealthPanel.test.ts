import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import HealthPanel, { dependabotFreshness } from "./HealthPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "HealthPanel.svelte"),
  "utf8",
);

describe("HealthPanel source contracts & interactive remediation", () => {
  it("invokes the scoped Manvi runner for remediation steps", () => {
    // The result type is the shared declaration, not an inline copy. This
    // panel used to spell the whole wire shape out anonymously at the invoke
    // call, which is why a backend field rename could reach it as `undefined`
    // with nothing to catch it.
    expect(source).toContain("invoke<TerminalRunResult>");
    expect(source).not.toContain("invoke<{");
    expect(source).toContain('"cmd_manvi_run_action"');
    expect(source).toContain('actionKind: "health"');
    expect(source).toContain("args: step.argv,");
  });

  it("keeps both output streams, and marks a clipped tail as clipped", () => {
    // Regression: this panel built its detail as `stderr || stdout`, so a
    // non-empty stderr discarded stdout entirely, and it never noted
    // truncation at all — a clipped tail was shown as the whole log.
    expect(source).toContain("formatRunDetail(res)");
    expect(source).toContain("formatRunSummary(res)");
    expect(source).not.toContain("res.stderr_tail || res.stdout_tail");
    expect(source).not.toContain('"Timed out and was killed."');
  });

  it("extracts plan steps and tokenizes commands safely", () => {
    expect(source).toContain("buildRunnablePlanSteps(plan.text)");
  });

  it("journals step execution into harnessStore", () => {
    expect(source).toContain("harnessStore.recordAction({");
    expect(source).toContain('kind: "remediation-step",');
  });

  it("journals a settled step before any stale-UI return", () => {
    const body = source.slice(
      source.indexOf("async function runStep"),
      source.indexOf("async function runAllSteps"),
    );
    const settled = body.indexOf("await invoke<TerminalRunResult>");
    const successJournal = body.indexOf("harnessStore.recordAction", settled);
    const successGuard = body.indexOf("if (!guard.isLive()) return false;", settled);
    expect(successJournal).toBeGreaterThan(settled);
    expect(successJournal).toBeLessThan(successGuard);

    const caught = body.indexOf("} catch", successGuard);
    const failureJournal = body.indexOf("harnessStore.recordAction", caught);
    const failureGuard = body.indexOf("if (!guard.isLive()) return false;", caught);
    expect(failureJournal).toBeGreaterThan(caught);
    expect(failureJournal).toBeLessThan(failureGuard);
  });

  it("provides single-step and sequential run-all execution", () => {
    expect(source).toContain("async function runStep");
    expect(source).toContain("async function runAllSteps");
  });

  it("uses one cancellation guard for a batch and ignores stale step results", () => {
    const stepBody = source.slice(
      source.indexOf("async function runStep"),
      source.indexOf("async function runAllSteps"),
    );
    const batchBody = source.slice(
      source.indexOf("async function runAllSteps"),
      source.indexOf("async function copyPlan"),
    );
    expect(source).toContain("function beginSteps(): AsyncGuard");
    expect(stepBody).toContain("guard: AsyncGuard");
    expect(stepBody).toContain("if (!guard.isLive()) return false;");
    expect(batchBody).toContain("const guard = beginSteps()");
    expect(batchBody).toContain("runStep(step, guard)");
  });

  it("provides navigation to Terminal view and rescan affordances", () => {
    expect(source).toContain('repoStore.setActiveTab("terminal")');
    expect(source).toContain("Rescan Health");
  });
});

describe("HealthPanel rendering", () => {
  it("renders the Health header and initial scan state", () => {
    const { body } = render(HealthPanel);
    expect(body).toContain("Health");
    expect(body).toContain("Scan");
    expect(body).toContain("GitHub alerts are not checked automatically");
  });

  it("feeds scanners_ran into formatAuditCounts so an unrun audit never renders as clean", () => {
    expect(source).toContain("report?.audit_complete === true");
    expect(source).toContain("formatAuditCounts(report.audit, { complete: auditComplete, ran: auditsRan })");
  });

  it("includes Dependabot in the copied report and shows every alert severity in the header", () => {
    expect(source).toContain("formatHealthReport(current, repoPath, dependabot)");
    expect(source).toContain("Dependabot checked at:");
    expect(source).toContain("{#if openDependabotCount > 0}");
  });

  it("keeps credentialed GitHub checks behind an explicit user action", () => {
    const localScan = source.slice(
      source.indexOf("async function scan("),
      source.indexOf("async function scanDependabot"),
    );
    expect(localScan).not.toContain("cmd_github_dependabot_alerts");
    expect(source).toContain("async function scanDependabot");
    expect(source).toContain("GitHub alerts are not checked automatically");
    expect(source).toMatch(/uses the GitHub CLI,\s+its credentials, and the network/);
    expect(source).toContain('aria-describedby="dependabot-permission-note"');
    expect(source).not.toContain('class="hidden xl:inline text-[10px] text-textMuted"');
    expect(source).toContain("Check GitHub alerts");
    expect(source).toContain("onclick={() => scanDependabot()}");
  });

  it("shows an IPC failure instead of misdiagnosing it as a missing GitHub CLI", () => {
    const section = source.slice(
      source.indexOf("{#snippet dependabotSection()}"),
      source.indexOf("{/snippet}"),
    );
    const errorBranch = section.indexOf("{#if dependabot.error}");
    const missingCliBranch = section.indexOf("{:else if !dependabot.cli_present}");
    expect(errorBranch).toBeGreaterThan(-1);
    expect(missingCliBranch).toBeGreaterThan(-1);
    expect(errorBranch).toBeLessThan(
      missingCliBranch,
    );
    const renderedError = section.slice(errorBranch, missingCliBranch);
    expect(renderedError).toContain("!dependabotRequestFailed");
    expect(source).toContain("dependabotRequestFailed = true;");
    expect(source).toContain("dependabotRequestFailed = false;");
  });

  it("labels outdated results as npm-only and renders exact cap notices", () => {
    expect(source).toContain("Outdated npm packages ({outdatedTotal})");
    expect(source).toContain("report.limit_notices");
    expect(source).toContain("retained {notice.kept} of {notice.total}");
  });
});

describe("HealthPanel flicker contracts", () => {
  it("hydrates the cached report before rescanning so revisits render instantly", () => {
    expect(source).toContain("createRepoPanelCache<{");
    expect(source).toContain("dependabotCheckedAt: number | null");
    expect(source).toContain(
      "dependabotRequestFailed,",
    );
    const effectBody = source.slice(source.indexOf("scanned.path = path;"), source.indexOf("async function openExternal"));
    expect(effectBody).toContain("healthCache.get(path)");
    expect(effectBody).toContain("dependabotCheckedAt = cached.dependabotCheckedAt;");
  });

  it("timestamps and caches the latest successful or failed explicit GitHub check", () => {
    const body = source.slice(
      source.indexOf("async function scanDependabot"),
      source.indexOf("function renderedReport"),
    );
    expect(body).toContain("const checkedAt = Date.now();");
    expect(body).toContain("dependabotCheckedAt = checkedAt;");
    expect(body).toContain("cacheDependabotResult(repoPath, next, checkedAt, false)");
    expect(body).toContain("cacheDependabotResult(repoPath, failed, checkedAt, true)");
  });

  it("does not resurrect an older GitHub result after the local rescan fails", () => {
    const helper = source.slice(
      source.indexOf("function cacheDependabotResult"),
      source.indexOf("async function scanDependabot"),
    );
    expect(helper).toContain(
      "const currentReport = scanned.path === repoPath ? report : null;",
    );
    expect(helper).toContain("currentReport ?? healthCache.get(repoPath)?.deps");
    expect(helper).toContain(
      "dependabotRequestFailed: requestFailed,",
    );
  });

  it("renders an explicit age for cached Dependabot results", () => {
    const checkedAt = Date.UTC(2026, 8, 3, 12, 34, 56);
    const freshness = dependabotFreshness(checkedAt);
    expect(freshness.iso).toBe("2026-09-03T12:34:56.000Z");
    expect(freshness.label.length).toBeGreaterThan(0);
    expect(source).toContain("This result may be cached");
    expect(source).toContain("dependabotFreshness(dependabotCheckedAt)");
  });

  it("never shows the previous repository's health data while an uncached repo scans", () => {
    const effectBody = source.slice(source.indexOf("scanned.path = path;"), source.indexOf("async function openExternal"));
    expect(effectBody).toContain("report = null;");
    expect(effectBody).toContain("dependabot = null;");
    expect(effectBody).toContain("dependabotCheckedAt = null;");
    expect(effectBody).toContain("deadSymbols = [];");
    expect(effectBody).toContain("codegraph = null;");
    expect(effectBody).toContain("dependabotInflight?.cancel();");
    expect(effectBody).toContain("checkingGithub = false;");
  });

  it("gates its loading placeholder on having no data yet", () => {
    expect(source).toContain("{#if loading && !report}");
  });
});

describe("HealthPanel error-state separation (regression)", () => {
  /**
   * `openExternal` failures are non-fatal: the advisory link did not open,
   * but the scan behind it is still valid. Routing them into the same state
   * that guards the "load failed" branch meant one failed link click
   * replaced the entire report with a bare banner — and made the in-report
   * banner (whose own comment promised otherwise) unreachable dead code.
   */
  it("does not let a non-fatal action error replace the whole report", () => {
    const opener = source.slice(
      source.indexOf("async function openExternal"),
      source.lastIndexOf("</script>"),
    );
    const assigned = opener.match(/(\w+)\s*=\s*reportPanelError\("health"/);
    expect(assigned, "openExternal must report its failure").not.toBeNull();
    const actionState = assigned![1];

    const chain = source.slice(source.indexOf("{#if loading && !report}"));
    const branches = [...chain.matchAll(/\{:else if ([^}]+)\}/g)].map((m) => m[1].trim());
    const reportBranch = branches.indexOf("report");
    expect(reportBranch, "the report branch must exist").toBeGreaterThanOrEqual(0);
    // Nothing that a non-fatal action sets may shadow the report branch.
    expect(branches.slice(0, reportBranch)).not.toContain(actionState);
  });

  it("keeps the action banner outside the load-error else-chain so it can render", () => {
    const chainStart = source.indexOf("{#if loading && !report}");
    expect(source.indexOf("{#if actionError}")).toBeGreaterThan(-1);
    // Rendered before the chain, like GitHubPanel's actionError banner, so it
    // is visible in every state rather than nested in one unreachable branch.
    expect(source.indexOf("{#if actionError}")).toBeLessThan(chainStart);
  });

  it("clears a stale action error when a new scan starts", () => {
    const scanBody = source.slice(
      source.indexOf("async function scan("),
      source.indexOf("function renderedReport"),
    );
    expect(scanBody).toContain("actionError = null");
  });

  it("tints the Dependabot header badge through the shared normalizer", () => {
    expect(source).toContain("dependabotBadgeClass");
    // Raw string equality on GitHub severities missed "HIGH"/"Critical",
    // which github/mod.rs passes through verbatim.
    expect(source).not.toMatch(/severity === "(critical|high|medium)"/);
  });

  it("does not treat a failed dead-code check as an empty clean graph", () => {
    expect(source).toContain("Dead-code check could not run");
    expect(source).toContain("No unreferenced symbols in the indexed graph");
    expect(source).toContain("not stored — not that it is zero");
  });

  it("discloses capped section counts instead of showing only what survived", () => {
    expect(source).toContain("observedTotal(");
    // Both bounded sections must headline the observed total, not just the
    // rows that survived the cap.
    const issuesHeading = source.slice(source.indexOf(">\n            Issues ("));
    expect(issuesHeading.slice(0, 200)).toContain("issuesTotal");
    const vulnHeading = source.slice(source.indexOf("Vulnerabilities ("));
    expect(vulnHeading.slice(0, 300)).toContain("vulnerabilitiesTotal");
  });
});
