import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const here = dirname(fileURLToPath(import.meta.url));
const suite = readFileSync(join(here, "DevcouncilSuitePanel.svelte"), "utf8");
const integration = readFileSync(join(here, "AgentIntegrationPanel.svelte"), "utf8");
const settings = readFileSync(join(here, "SettingsModal.svelte"), "utf8");

describe("DevcouncilSuitePanel", () => {
  it("compiles", () => {
    const result = compile(suite, {
      filename: "DevcouncilSuitePanel.svelte",
      generate: "server",
    });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("delegates every worded judgement to the tested owner", () => {
    // The point of the extraction: no `not reported` / `missing` / `nothing to
    // report` string may be assembled in markup, where it cannot be tested and
    // would drift from `devcouncilSuite.ts`.
    for (const helper of ["versionText", "missingSummary", "healthSummary", "presetSummary", "needLabel"]) {
      expect(suite).toContain(helper);
    }
    expect(suite).not.toMatch(/version not reported/);
    expect(suite).not.toMatch(/components are missing/);
  });

  it("probes when its section is shown, not only when Settings mounts", () => {
    // Settings mounts once on whichever section was last open, so an
    // onMount-only probe left this panel saying "Probing installed
    // components…" forever for anyone who did not happen to land here first.
    expect(suite).toContain("$effect");
    // The word survives in the comment explaining the defect; what must not
    // survive is the import and the call.
    expect(suite).not.toMatch(/import\s*\{[^}]*\bonMount\b/);
    expect(suite).not.toMatch(/\bonMount\s*\(/);
  });

  it("branches on the health summary's kind rather than an empty warning list", () => {
    expect(suite).toContain('health?.kind === "unchecked"');
    expect(suite).toContain('health?.kind === "clean"');
    // An `warnings.length === 0` branch here would be the bug the summary
    // exists to prevent: unchecked rendering as clean.
    expect(suite).not.toContain("report.warnings.length === 0");
  });
});

describe("AgentIntegrationPanel", () => {
  it("compiles", () => {
    const result = compile(integration, {
      filename: "AgentIntegrationPanel.svelte",
      generate: "server",
    });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("previews before it applies", () => {
    expect(integration).toContain("surveyDevmapIntegration");
    expect(integration).toContain("previewDevmapIntegration");
    expect(integration).toContain("applyDevmapIntegration");
    // Applying is gated on a confirmation built by the tested owner.
    expect(integration).toContain("applyConfirmText");
    expect(integration).toContain("window.confirm");
  });

  it("never assembles its own change-count sentence", () => {
    expect(integration).toContain("planSummary");
    expect(integration).toContain("canApply");
    expect(integration).not.toMatch(/in your home directory\{/);
  });

  it("keys rows on the requested host, never on the payload's own", () => {
    // Measured in the real UI: a backend that answered every host with the
    // same payload put two plans under one key and Svelte threw
    // `each_key_duplicate`, tearing the panel down mid-render.
    expect(integration).toContain("indexPlansByHost");
    expect(integration).toContain("acceptPreview");
    expect(integration).toContain("{#each rows as row (row.host)}");
    expect(integration).not.toContain("(plan.host)");
    expect(integration).not.toMatch(/plans\.map\(/);
  });

  it("re-previews when a host row is expanded", () => {
    // A stale preview is what an apply would otherwise be consented against.
    const toggle = integration.slice(
      integration.indexOf("async function toggle"),
      integration.indexOf("async function apply"),
    );
    expect(toggle).toContain("previewDevmapIntegration");
  });
});

describe("Settings → Agents", () => {
  it("mounts both panels beside the managed-tool installer", () => {
    expect(settings).toContain("<ExternalToolsPanel />");
    expect(settings).toContain("<DevcouncilSuitePanel");
    expect(settings).toContain("<AgentIntegrationPanel");
  });

  it("scopes both panels to the agents section so they do not probe on every open", () => {
    // Probing spawns child processes; doing it because Settings opened on the
    // Appearance tab would cost a user who never looks at this section.
    const mount = settings.slice(settings.indexOf("<DevcouncilSuitePanel"));
    expect(mount).toContain('activeSection === "agents"');
  });
});
