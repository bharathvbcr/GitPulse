import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import HealthPanel from "./HealthPanel.svelte";

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
  });

  it("feeds scanners_ran into formatAuditCounts so an unrun audit never renders as clean", () => {
    expect(source).toContain("report?.audit_complete === true");
    expect(source).toContain("formatAuditCounts(report.audit, { complete: auditComplete, ran: auditsRan })");
  });

  it("includes Dependabot in the copied report and shows every alert severity in the header", () => {
    expect(source).toContain("formatHealthReport(current, repoPath, dependabot)");
    expect(source).toContain("{#if openDependabotCount > 0}");
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
    expect(source).toContain("healthCache.set(repoPath, { deps: deps.value, dependabot })");
    const effectBody = source.slice(source.indexOf("scanned.path = path;"), source.indexOf("async function openExternal"));
    expect(effectBody).toContain("healthCache.get(path)");
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
