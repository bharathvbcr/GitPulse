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
    expect(source).toContain('invoke<{');
    expect(source).toContain('"cmd_manvi_run_action"');
    expect(source).toContain('actionKind: "health"');
    expect(source).toContain("args: step.argv,");
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
    expect(source).toContain("let auditsRan = $derived");
    expect(source).toContain("(report?.scanners_ran ?? []).length > 0");
    expect(source).toContain("formatAuditCounts(report.audit, { ran: auditsRan })");
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
