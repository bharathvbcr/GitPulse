import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { render } from "svelte/server";
import SecretsSection from "./SecretsSection.svelte";
import { HEALTH_SECTION_IDS } from "../../health/sections";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "SecretsSection.svelte"), "utf8");
const panel = readFileSync(join(here, "..", "HealthPanel.svelte"), "utf8");

describe("SecretsSection", () => {
  it("invokes the secrets scan command and never asks for secret fields", () => {
    expect(source).toContain('invoke<unknown>("cmd_scan_secrets"');
    expect(source).toContain("parseSecretsReport");
    // Kingfisher's matched context is its `snippet` field. The section uses
    // Svelte's `{#snippet}` blocks (HealthSection takes its actions as one),
    // so the guard is on reading that field or key, not on the word.
    expect(source).not.toMatch(/\.snippet\b|\[["']snippet["']\]|\bsnippet:/);
    expect(source).not.toContain("fingerprint");
    // `.secret` as a field read; `secret_group` is the ordinal, not a value.
    expect(source).not.toMatch(/\.secret\b(?!_)/);
    expect(source).toContain("finding.path");
    expect(source).toContain("finding.line");
    expect(source).toContain("finding.rule_id");
  });

  it("routes every headline through the tested verdict owner", () => {
    // The clean/partial/failed wording lives in secrets/summary.ts, where it
    // is unit-tested; a sentence re-typed here would bypass those tests.
    expect(source).toContain("verdict(report)");
    expect(source).toContain("scanFailureCopy");
    expect(source).toContain('diagnostics.warn("secrets"');
    expect(source).toContain("redactDiagnosticText");
    expect(source).toContain("Copy secrets scan error");
    expect(source).not.toContain("No secrets reported");
    expect(source).not.toContain("This is not a clean result");
  });

  it("reveals through the contained repo-relative seam, never an absolute path", () => {
    expect(source).toContain("revealInFileManager(repo, path)");
    expect(source).not.toContain("plugin-opener");
  });

  it("joins a scan already running for the repository instead of starting another", () => {
    // Health renders this section from three branches, so a dependency scan
    // that finishes remounts it mid-scan. The in-flight map is module-level
    // for exactly that reason; a per-instance guard started a second
    // Kingfisher run.
    const module = source.slice(0, source.indexOf("</script>"));
    expect(module).toContain("<script module");
    expect(module).toContain("const inflightScans = new Map<string, Promise<SecretsReport>>()");
    expect(source).toContain("inflightScans.has(path)");
  });

  it("previews the findings table and keeps the cap disclosure", () => {
    expect(source).toContain("previewSlice(shownRows, expanded, HEALTH_PREVIEW_ROWS)");
    expect(source).toContain("keyedList(previewRows, findingKey)");
    expect(source).toContain("<HealthPreviewToggle total={shownRows.length}");
    // The heading count is Kingfisher's, not the preview's.
    expect(source).toContain("total: report.findings_total");
  });

  it("is a Health section, between the GitHub alerts and the issues", () => {
    expect(source).toContain('<HealthSection id="secrets"');
    const at = HEALTH_SECTION_IDS.indexOf("secrets");
    expect(HEALTH_SECTION_IDS[at - 1]).toBe("code-scanning");
    expect(HEALTH_SECTION_IDS[at + 1]).toBe("issues");
    // Rendered in the loading, failed and loaded states, after the GitHub
    // alerts in the two that have them.
    expect(panel.match(/\{@render secretsSection\(\)\}/g)).toHaveLength(3);
    expect(panel).toContain("{@render githubSections()}\n      {@render secretsSection()}");
    expect(panel).toContain("bind:report={secretsReport}");
    expect(panel).toContain("onagent={sendSecretsToAgent}");
  });

  it("renders without a repository, and offers no agent with no agent bar", () => {
    const { body } = render(SecretsSection);
    expect(body).toContain("Working-tree secrets");
    expect(body).toContain("No repository open");
    expect(body).not.toContain("Fix with agent");
  });
});
