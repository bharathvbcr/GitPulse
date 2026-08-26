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
  it("invokes cmd_terminal_run with step argv and timeoutSecs for remediation steps", () => {
    expect(source).toContain('invoke<{');
    expect(source).toContain('"cmd_terminal_run"');
    expect(source).toContain("args: step.argv,");
  });

  it("extracts plan steps and tokenizes commands safely", () => {
    expect(source).toContain("extractPlanSteps(plan.text)");
    expect(source).toContain("tokenizeCommand(firstCmd)");
  });

  it("journals step execution into harnessStore", () => {
    expect(source).toContain("harnessStore.recordAction({");
    expect(source).toContain('kind: "remediation-step",');
  });

  it("provides single-step and sequential run-all execution", () => {
    expect(source).toContain("async function runStep");
    expect(source).toContain("async function runAllSteps");
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
});
