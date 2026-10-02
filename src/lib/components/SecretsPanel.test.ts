import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { render } from "svelte/server";
import SecretsPanel from "./SecretsPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "SecretsPanel.svelte"),
  "utf8",
);

describe("SecretsPanel", () => {
  it("invokes the secrets scan command and never asks for secret fields", () => {
    expect(source).toContain('invoke<unknown>("cmd_scan_secrets"');
    expect(source).toContain("parseSecretsReport");
    expect(source).not.toContain("snippet");
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

  it("renders without a repository", () => {
    const { body } = render(SecretsPanel);
    expect(body).toContain("Secrets");
    expect(body).toContain("No repository open");
  });
});
