import { describe, expect, it } from "vitest";
import { formatHealthReport } from "./report";
import type { DependabotAlertInfo, DependabotReport, DepsHealthReport } from "./types";

function emptyReport(): DepsHealthReport {
  return {
    node_version: "22.1.0",
    npm_version: "10.8.0",
    npm_cli_present: true,
    cargo_audit_present: false,
    manifests: [],
    ecosystems: [],
    issues: [],
    vulnerabilities: [],
    audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 },
    outdated: [],
    truncated: false,
    scanners_ran: ["npm"],
    audit_complete: true,
    limit_notices: [],
  };
}

function dependabotAlert(overrides: Partial<DependabotAlertInfo> = {}): DependabotAlertInfo {
  return {
    number: 1,
    package: "lodash",
    ecosystem: "npm",
    manifest_path: "package.json",
    scope: "runtime",
    severity: "high",
    title: "Prototype Pollution in lodash",
    advisory_id: "GHSA-xxxx-yyyy-zzzz",
    cve_id: "CVE-2020-8203",
    vulnerable_range: "< 4.17.19",
    first_patched: "4.17.19",
    url: "https://github.com/acme/repo/security/dependabot/1",
    created_at: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}

function dependabotReport(
  overrides: Partial<DependabotReport> = {},
  alerts: DependabotAlertInfo[] = [dependabotAlert()],
): DependabotReport {
  return {
    available: true,
    cli_present: true,
    is_github_remote: true,
    slug: "acme/repo",
    alerts,
    truncated: false,
    error: null,
    ...overrides,
  };
}

describe("formatHealthReport", () => {
  it("renders a clean report with environment and an explicit all-clear", () => {
    const text = formatHealthReport(emptyReport(), "/repo");
    expect(text).toContain("# Dependency health report");
    expect(text).toContain("Repository: /repo");
    expect(text).toContain("node 22.1.0");
    expect(text).toContain("npm 10.8.0");
    expect(text).toContain("scanners: npm audit");
    expect(text).toContain("No known vulnerabilities; 0 outdated npm package(s).");
    expect(text).toContain("No issues, vulnerabilities or outdated packages were reported.");
    expect(text).not.toContain("capped");
  });

  it("names the scanners that are present and flags when none ran", () => {
    const report = emptyReport();
    report.npm_cli_present = false;
    report.scanners_ran = [];
    report.cargo_audit_present = true;
    report.pip_audit_present = true;
    report.govulncheck_present = true;
    report.composer_present = true;
    report.bundler_audit_present = true;
    report.scanners_ran = ["cargo", "pip-audit", "govulncheck", "composer", "bundler-audit"];
    const text = formatHealthReport(report);
    expect(text).toContain(
      "scanners: cargo-audit, pip-audit, govulncheck, composer-audit, bundler-audit",
    );
    report.cargo_audit_present = false;
    report.pip_audit_present = false;
    report.govulncheck_present = false;
    report.composer_present = false;
    report.bundler_audit_present = false;
    report.scanners_ran = [];
    // UPDATED: "no audit scanner on PATH" over-claimed a specific diagnostic
    // (PATH) that presence flags do not establish.
    expect(formatHealthReport(report)).toContain("no audit scanner available");
  });

  it("names local audits that could not run instead of letting counts read as clean", () => {
    const report = emptyReport();
    report.npm_cli_present = false;
    report.scanners_ran = [];
    report.audit_complete = false;
    report.manifests = [
      {
        path: "package.json",
        name: "demo",
        version: "1.0.0",
        private: true,
        package_manager: "npm",
        has_workspaces: false,
        dep_count: 1,
        dev_dep_count: 0,
        optional_dep_count: 0,
        peer_dep_count: 0,
        lifecycle_scripts: [],
      },
    ];
    const text = formatHealthReport(report);
    expect(text).toContain(
      "NOTE: checks that did NOT run (CLI missing): npm audit/outdated.",
    );
    // A skipped check must never coexist with the explicit all-clear.
    expect(text).not.toContain(
      "No issues, vulnerabilities or outdated packages were reported.",
    );
  });

  it("does not claim a clean audit when manifests exist but no scanner ran (field bug)", () => {
    const report = emptyReport();
    report.npm_cli_present = false;
    report.scanners_ran = [];
    report.audit_complete = false;
    report.manifests = [
      {
        path: "package.json",
        name: "demo",
        version: "1.0.0",
        private: true,
        package_manager: "npm",
        has_workspaces: false,
        dep_count: 12,
        dev_dep_count: 4,
        optional_dep_count: 0,
        peer_dep_count: 0,
        lifecycle_scripts: [],
      },
    ];
    const text = formatHealthReport(report);
    expect(text).not.toContain("No known vulnerabilities");
    expect(text).toContain("Audit summary: audit did not run");
    expect(text).toContain("CLI missing: npm audit/outdated");
  });

  it("keeps the clean-audit wording only when a scanner actually ran and found nothing", () => {
    const report = emptyReport();
    report.scanners_ran = ["npm"];
    const text = formatHealthReport(report);
    expect(text).toContain("Audit summary: No known vulnerabilities; 0 outdated npm package(s).");
    expect(text).not.toContain("audit did not run");
  });

  it("does not claim a dispatched-but-failed scanner produced a clean audit", () => {
    const report = emptyReport();
    report.audit_complete = false;
    report.issues = [
      {
        severity: "error",
        code: "audit_failed",
        message: "npm audit returned invalid JSON",
        path: "package.json",
      },
    ];
    const text = formatHealthReport(report);
    expect(text).toContain("Audit summary: Audit incomplete");
    expect(text).not.toContain("No known vulnerabilities");
  });

  it("reports exact retained and total counts for every bounded collection", () => {
    const report = emptyReport();
    report.audit_complete = false;
    report.truncated = true;
    report.limit_notices = [
      { resource: "vulnerabilities", kept: 200, total: 247 },
      { resource: "repository files", kept: 10_000, total: 12_345 },
    ];
    const text = formatHealthReport(report);
    expect(text).toContain("vulnerabilities: retained 200 of 247");
    expect(text).toContain("repository files: retained 10000 of 12345");
    expect(text).toContain("Audit summary: Audit incomplete; 0 outdated npm package(s).");
  });

  it("lists ecosystem audits whose CLI was absent while their artifacts exist", () => {
    const report = emptyReport();
    report.cargo_audit_present = false;
    report.pip_audit_present = false;
    report.ecosystems = [
      { family: "cargo", manifests: ["Cargo.lock"], note: "" },
      { family: "python", manifests: ["requirements-dev.txt"], note: "" },
    ];
    const text = formatHealthReport(report);
    expect(text).toContain("cargo-audit");
    expect(text).toContain("pip-audit");
    // Nothing was skipped for families whose scanner IS present.
    expect(text).not.toContain("govulncheck,");
  });

  it("does not name a scanner as skipped when its family has no auditable artifact", () => {
    const report = emptyReport();
    report.cargo_audit_present = false;
    // A lone source file hints the cargo family but gives cargo-audit
    // nothing to scan.
    report.ecosystems = [{ family: "cargo", manifests: ["src/main.rs"], note: "" }];
    const text = formatHealthReport(report);
    expect(text).not.toContain("did NOT run");
    expect(text).toContain(
      "No issues, vulnerabilities or outdated packages were reported.",
    );
  });

  it("recognizes audit artifacts in subdirectories", () => {
    const report = emptyReport();
    report.cargo_audit_present = false;
    report.govulncheck_present = false;
    report.ecosystems = [
      { family: "cargo", manifests: ["backend/Cargo.lock"], note: "" },
      { family: "go", manifests: ["services/api/go.mod"], note: "" },
    ];
    const text = formatHealthReport(report);
    expect(text).toContain("cargo-audit");
    expect(text).toContain("govulncheck");
  });

  it("does not invent an all-clear when there were no auditable artifacts", () => {
    const report = emptyReport();
    report.npm_cli_present = false;
    report.scanners_ran = [];
    report.audit_complete = false;
    const text = formatHealthReport(report);
    expect(text).not.toContain("did NOT run");
    expect(text).toContain("audit did not run");
    expect(text).not.toContain("No known vulnerabilities");
    expect(text).toContain("local audit coverage is incomplete");
  });

  it("carries severity, fix version, transitive chain and advisory link for vulnerabilities", () => {
    const report = emptyReport();
    report.audit = { info: 0, low: 1, moderate: 0, high: 1, critical: 1, total: 3 };
    report.issues = [
      { severity: "warning", code: "NO_LOCKFILE", message: "No lockfile found.", path: "app" },
      { severity: "error", code: "BAD_ENGINES", message: "engines.node is unsatisfiable." },
    ];
    report.vulnerabilities = [
      {
        name: "minimist",
        severity: "high",
        is_direct: false,
        title: "Prototype Pollution",
        url: "https://example.com/advisory",
        range: "<1.2.6",
        fix_available: "1.2.8",
        via: ["lodash", "request"],
        ecosystem: "npm",
      },
    ];
    report.outdated = [
      {
        name: "typescript",
        current: "5.0.4",
        wanted: "5.5.4",
        latest: "5.9.2",
        dep_type: "devDependencies",
        location: ".",
      },
    ];
    const text = formatHealthReport(report);
    expect(text).toContain("## Issues (2)");
    expect(text).toContain("- [warning] NO_LOCKFILE (app): No lockfile found.");
    expect(text).toContain("- [error] BAD_ENGINES: engines.node is unsatisfiable.");
    expect(text).toContain("- [high] npm/minimist <1.2.6 — Prototype Pollution");
    expect(text).toContain("direct: no · fix available: 1.2.8 · via: lodash, request");
    expect(text).toContain("advisory: https://example.com/advisory");
    expect(text).toContain("- typescript: 5.0.4 -> 5.9.2 (wanted 5.5.4, devDependencies) @ .");
    expect(text).not.toContain("were reported.");
  });

  it("says when a capped scan means the findings are not complete coverage", () => {
    const report = emptyReport();
    report.truncated = true;
    expect(formatHealthReport(report)).toContain(
      "NOTE: the scan was capped; findings below are not complete coverage.",
    );
  });

  it("omits Dependabot entirely when no GitHub data exists", () => {
    const text = formatHealthReport(emptyReport(), "/repo", null);
    expect(text).not.toContain("Dependabot");
    expect(text).not.toContain("github-dependabot");
  });

  it("carries Dependabot alerts with ids, fix and alert link, and counts them up top", () => {
    const dependabot = dependabotReport({}, [
      dependabotAlert(),
      dependabotAlert({
        number: 2,
        severity: "critical",
        package: "django",
        ecosystem: "pip",
        advisory_id: "GHSA-aaaa",
        cve_id: "",
        first_patched: "",
      }),
    ]);
    const text = formatHealthReport(emptyReport(), "/repo", dependabot);
    expect(text).toContain("scanners: npm audit, github-dependabot");
    expect(text).toContain("GitHub Dependabot: 2 open alert(s).");
    expect(text).toContain("## GitHub Dependabot alerts (2)");
    expect(text).toContain(
      "- [high] npm/lodash < 4.17.19 — Prototype Pollution in lodash",
    );
    expect(text).toContain("ids: GHSA-xxxx-yyyy-zzzz, CVE-2020-8203");
    expect(text).toContain("fix available: 4.17.19");
    expect(text).toContain("alert: https://github.com/acme/repo/security/dependabot/1");
    // No patched version published must read as "none reported".
    expect(text).toContain("- [critical] pip/django");
    // Only once: lodash does report a patched version.
    expect(text.match(/fix available: none reported/g)).toHaveLength(1);
  });

  it("reports a failed Dependabot fetch instead of laundering it into all-clear silence", () => {
    const dependabot = dependabotReport({
      available: false,
      alerts: [],
      error: "Dependabot alerts are disabled (HTTP 403)",
    });
    const text = formatHealthReport(emptyReport(), "/repo", dependabot);
    expect(text).toContain("dependabot unavailable (Dependabot alerts are disabled (HTTP 403))");
    expect(text).not.toContain("GitHub Dependabot alerts (");
  });

  it("flags a capped Dependabot list as incomplete coverage too", () => {
    const dependabot = dependabotReport({ truncated: true });
    const text = formatHealthReport(emptyReport(), "/repo", dependabot);
    expect(text).toContain(
      "NOTE: the scan was capped; findings below are not complete coverage.",
    );
    expect(text).toContain("GitHub Dependabot: at least 1 open alert(s).");
    expect(text).toContain("## GitHub Dependabot alerts (at least 1)");
  });

  it("keeps the explicit all-clear when Dependabot ran clean alongside an empty local scan", () => {
    const dependabot = dependabotReport({}, []);
    const text = formatHealthReport(emptyReport(), "/repo", dependabot);
    expect(text).toContain("GitHub Dependabot: 0 open alert(s).");
    expect(text).toContain("No issues, vulnerabilities or outdated packages were reported.");
  });
});
