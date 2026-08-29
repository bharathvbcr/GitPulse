import { formatAuditCounts } from "./format";
import type { DependabotReport, DepsHealthReport } from "./types";

function line(items: (string | undefined | null)[]): string {
  return items.filter((item) => item !== undefined && item !== null && item !== "").join(" · ");
}

function observedTotal(report: DepsHealthReport, resource: string, retained: number): number {
  return report.limit_notices?.find((notice) => notice.resource === resource)?.total ?? retained;
}

/**
 * Audits whose artifacts exist but whose CLI was unavailable — a check that
 * could not run must never read as one that ran clean.
 *
 * Gated on each scanner's actual artifact (mirroring the Rust enrichers), not
 * just the ecosystem hint: a lone `.rs` file hints "cargo" without giving
 * cargo-audit anything to scan.
 */
export function skippedAudits(report: DepsHealthReport): string[] {
  const files = report.ecosystems.flatMap((eco) => eco.manifests);
  const base = (p: string) => p.slice(p.lastIndexOf("/") + 1);
  const has = (name: string) => files.some((f) => base(f) === name);
  const skipped: string[] = [];
  if (!report.npm_cli_present && report.manifests.length > 0) {
    skipped.push("npm audit/outdated");
  }
  if (!report.cargo_audit_present && has("Cargo.lock")) skipped.push("cargo-audit");
  if (
    !report.pip_audit_present &&
    files.some((f) => {
      const b = base(f);
      return (b.startsWith("requirements") && b.endsWith(".txt")) || b === "constraints.txt";
    })
  ) {
    skipped.push("pip-audit");
  }
  if (!report.govulncheck_present && has("go.mod")) skipped.push("govulncheck");
  if (!report.composer_present && has("composer.lock")) skipped.push("composer audit");
  if (!report.bundler_audit_present && has("Gemfile.lock")) skipped.push("bundler-audit");
  return skipped;
}

/**
 * Renders the health report as plain markdown-ish text that survives a paste
 * into an issue, an agent prompt or a notes file: every finding keeps its
 * severity, fix version and advisory link, and capped scans say so.
 */
export function formatHealthReport(
  report: DepsHealthReport,
  repoPath?: string | null,
  dependabot?: DependabotReport | null,
): string {
  const out: string[] = [];
  out.push("# Dependency health report");
  if (repoPath) out.push(`Repository: ${repoPath}`);

  const scannerLabels: Record<string, string> = {
    npm: "npm audit",
    cargo: "cargo-audit",
    "pip-audit": "pip-audit",
    govulncheck: "govulncheck",
    composer: "composer-audit",
    "bundler-audit": "bundler-audit",
  };
  const localScanners = (report.scanners_ran ?? []).map(
    (scanner) => scannerLabels[scanner] ?? scanner,
  );
  const scanners = [
    ...localScanners,
    dependabot?.available ? "github-dependabot" : null,
  ].filter(Boolean);
  out.push(
    line([
      report.node_version ? `node ${report.node_version}` : undefined,
      report.npm_version ? `npm ${report.npm_version}` : undefined,
      scanners.length ? `scanners: ${scanners.join(", ")}` : "no audit scanner available",
      dependabot && !dependabot.available && dependabot.error
        ? `dependabot unavailable (${dependabot.error})`
        : undefined,
    ]),
  );
  const skipped = skippedAudits(report);
  const auditsRan = (report.scanners_ran ?? []).length > 0;
  const auditComplete = report.audit_complete === true;
  const outdatedTotal = observedTotal(report, "outdated npm packages", report.outdated.length);
  const auditSummary = auditsRan || auditComplete
    ? formatAuditCounts(report.audit, { complete: auditComplete, ran: auditsRan })
    : `audit did not run${skipped.length > 0 ? ` (CLI missing: ${skipped.join(", ")})` : ""}`;
  out.push(`Audit summary: ${auditSummary}; ${outdatedTotal} outdated npm package(s).`);
  if (skipped.length > 0) {
    out.push(
      `NOTE: checks that did NOT run (CLI missing): ${skipped.join(", ")}. The counts above are not complete coverage.`,
    );
  }
  if (dependabot?.available) {
    out.push(
      `GitHub Dependabot: ${dependabot.truncated ? "at least " : ""}${dependabot.alerts.length} open alert(s).`,
    );
  }
  if (report.truncated || dependabot?.truncated) {
    out.push("NOTE: the scan was capped; findings below are not complete coverage.");
  }
  for (const notice of report.limit_notices ?? []) {
    out.push(`- ${notice.resource}: retained ${notice.kept} of ${notice.total}`);
  }

  if (report.issues.length > 0) {
    const issueTotal = observedTotal(report, "health issues", report.issues.length);
    out.push("", `## Issues (${issueTotal}${issueTotal > report.issues.length ? `; showing ${report.issues.length}` : ""})`);
    for (const issue of report.issues) {
      out.push(`- [${issue.severity}] ${issue.code}${issue.path ? ` (${issue.path})` : ""}: ${issue.message}`);
    }
  }

  if (report.vulnerabilities.length > 0) {
    out.push("", `## Vulnerabilities (${report.audit.total}${report.audit.total > report.vulnerabilities.length ? `; showing ${report.vulnerabilities.length}` : ""})`);
    for (const vuln of report.vulnerabilities) {
      out.push(
        `- [${vuln.severity}] ${vuln.ecosystem}/${vuln.name}${vuln.range ? ` ${vuln.range}` : ""} — ${vuln.title}`,
      );
      out.push(
        `  direct: ${vuln.is_direct ? "yes" : "no"} · fix available: ${vuln.fix_available || "none reported"}${
          vuln.via.length ? ` · via: ${vuln.via.join(", ")}` : ""
        }${vuln.url ? `\n  advisory: ${vuln.url}` : ""}`,
      );
    }
  }

  if (dependabot?.available && dependabot.alerts.length > 0) {
    out.push("", `## GitHub Dependabot alerts (${dependabot.truncated ? "at least " : ""}${dependabot.alerts.length})`);
    for (const alert of dependabot.alerts) {
      const ids = [alert.advisory_id, alert.cve_id].filter(Boolean).join(", ");
      out.push(
        `- [${alert.severity}] ${alert.ecosystem}/${alert.package}${alert.vulnerable_range ? ` ${alert.vulnerable_range}` : ""} — ${alert.title}`,
      );
      out.push(
        line([
          ids ? `ids: ${ids}` : undefined,
          alert.manifest_path ? `manifest: ${alert.manifest_path}` : undefined,
          alert.first_patched
            ? `fix available: ${alert.first_patched}`
            : "fix available: none reported",
          alert.url ? `alert: ${alert.url}` : undefined,
        ]),
      );
    }
  }

  if (report.outdated.length > 0) {
    out.push("", `## Outdated npm packages (${outdatedTotal}${outdatedTotal > report.outdated.length ? `; showing ${report.outdated.length}` : ""})`);
    for (const pkg of report.outdated) {
      out.push(
        `- ${pkg.name}: ${pkg.current} -> ${pkg.latest} (wanted ${pkg.wanted}, ${pkg.dep_type || "dep"})${
          pkg.location ? ` @ ${pkg.location}` : ""
        }`,
      );
    }
  }

  const nothingReported =
    report.issues.length === 0 &&
    report.vulnerabilities.length === 0 &&
    report.outdated.length === 0 &&
    (!dependabot?.available || dependabot.alerts.length === 0);
  if (nothingReported && skipped.length === 0 && auditComplete) {
    out.push("", "No issues, vulnerabilities or outdated packages were reported.");
  } else if (nothingReported && !auditComplete) {
    out.push("", "No reportable findings were collected; local audit coverage is incomplete.");
  }

  return out.join("\n");
}
