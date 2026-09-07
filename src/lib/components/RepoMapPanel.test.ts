import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "RepoMapPanel.svelte"), "utf8");
const codeView = readFileSync(join(here, "CodeView.svelte"), "utf8");

describe("RepoMapPanel", () => {
  it("compiles", () => {
    const result = compile(source, { filename: "RepoMapPanel.svelte", generate: "server" });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("loads the typed map and status freshness strip", () => {
    expect(source).toContain("getDevmapRepoMap");
    expect(source).toContain("getDevmapCliStatus");
    expect(source).toContain("is_fresh");
    expect(source).toContain("schema_outdated");
    expect(source).toContain("coverage_gaps");
    expect(source).toContain("generation_id");
  });

  it("navigates subsystems with entry points, critical files and role tests", () => {
    expect(source).toContain("entry_points");
    expect(source).toContain("critical_files");
    expect(source).toContain('roleSample(selected, "tests")');
    expect(source).toContain("role_file_counts");
    expect(source).toContain("liveness_meta");
  });

  it("prefers unwired and dead candidates over unreliable unreachable", () => {
    expect(source).toContain("preferredDeadLists");
    expect(source).toContain("unreachableSuppressed");
    expect(source).toContain("liveness_unreachable_unreliable");
  });

  it("does not present capped samples as complete", () => {
    expect(source).toContain("formatCap");
    expect(source).toContain("truncated");
  });

  it("offers graph modes backed by viz, map_preview and docs graph payloads", () => {
    expect(source).toContain("getCodeGraphViz");
    expect(source).toContain("getMapPreviewViz");
    expect(source).toContain("docsGraph");
    expect(source).toContain("docGraphToLoad");
    expect(source).toContain("CodeGraphCanvas");
    expect(source).toContain('"files"');
    expect(source).toContain('"symbols"');
    expect(source).toContain('"subsystems"');
    expect(source).toContain('"docgraph"');
    expect(source).not.toContain("VisualCommitRow");
    expect(source).not.toMatch(/from ["'].*\/GraphRenderer["']/);
  });

  it("exposes docs search, broken links and link candidates with honesty", () => {
    expect(source).toContain('"docs"');
    expect(source).toContain('"links"');
    expect(source).toContain("docsSearch");
    expect(source).toContain("docsBrokenLinks");
    expect(source).toContain("docsStatusHonesty");
    expect(source).toContain("docsSearchHonesty");
    expect(source).toContain("brokenLinksHonesty");
    expect(source).toContain("getWorkspaceLinkCandidates");
    expect(source).toContain("linkCandidatesHonesty");
    expect(source).toContain('data-testid="docs-panel"');
    expect(source).toContain('data-testid="link-candidates-panel"');
  });

  it("disables Build and Refresh when the CLI is known absent", () => {
    expect(source).toContain("cliKnownAbsent");
    expect(source).toContain("devmap CLI is not installed");
    expect(source).toContain("openSetupWizard");
    expect(source).toContain("classifyMapFailure");
    expect(source).toContain("mapFailureMessage");
    expect(source).toContain('mapFailureMessage("no_cli")');
    expect(source).toContain("mapFailureMode");
  });
});
describe("CodeView map section", () => {
  it("lazy-loads the Map panel beside Explorer and Blame", () => {
    expect(codeView).toContain('section === "map"');
    expect(codeView).toContain("loadMap");
  });
});
