import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { render } from "svelte/server";
import SecretsPanel from "./SecretsPanel.svelte";
import { parseSecretsReport } from "../secrets/types";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "SecretsPanel.svelte"),
  "utf8",
);

describe("SecretsPanel", () => {
  it("invokes the secrets scan command and never asks for secret fields", () => {
    expect(source).toContain('invoke<unknown>("cmd_scan_secrets"');
    expect(source).toContain("parseSecretsReport");
    expect(source).not.toContain("snippet");
    expect(source).not.toContain(".secret");
    expect(source).toContain("finding.path");
    expect(source).toContain("finding.line");
    expect(source).toContain("finding.rule_id");
  });

  it("renders unavailable and truncated honesty copy", () => {
    expect(source).toContain("Secrets scan unavailable");
    expect(source).toContain("findings_truncated");
    expect(source).toContain("nested_repos_scanned");
    const { body } = render(SecretsPanel);
    expect(body).toContain("Secrets");
  });
});

describe("parseSecretsReport", () => {
  it("keeps only allowlisted finding fields", () => {
    const report = parseSecretsReport({
      ok: true,
      kingfisher_present: true,
      kingfisher_version: "1.99.0",
      nested_repos_scanned: true,
      findings_truncated: false,
      findings: [
        {
          rule_id: "betterleaks.github-pat",
          path: "a.sh",
          line: 4,
          confidence: "medium",
          validation_outcome: "not_attempted",
          fingerprint: "1",
          snippet: "ghp_SHOULD_NOT_APPEAR",
          secret: "ghp_SHOULD_NOT_APPEAR",
        },
      ],
    });
    expect(report.findings).toHaveLength(1);
    expect(report.findings[0].rule_id).toBe("betterleaks.github-pat");
    expect(JSON.stringify(report)).not.toContain("ghp_SHOULD_NOT_APPEAR");
    expect(JSON.stringify(report)).not.toContain("snippet");
  });

  it("marks a failed payload as not ok", () => {
    const report = parseSecretsReport({
      ok: false,
      error: "kingfisher is not installed or not on PATH. Install with: brew install kingfisher",
      kingfisher_present: false,
      nested_repos_scanned: true,
      findings: [],
      findings_truncated: false,
    });
    expect(report.ok).toBe(false);
    expect(report.kingfisher_present).toBe(false);
  });
});
