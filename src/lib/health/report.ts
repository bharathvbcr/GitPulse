import { formatAuditCounts } from "./format";
import type { DependabotReport, DepsHealthReport } from "./types";

function line(items: (string | undefined | null)[]): string {
  return items.filter((item) => item !== undefined && item !== null && item !== "").join(" · ");
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

  const scanners = [
    report.npm_cli_present ? "npm audit" : null,
    report.cargo_audit_present ? "cargo-audit" : null,
    report.pip_audit_present ? "pip-audit" : null,
    report.govulncheck_present ? "govulncheck" : null,
    report.composer_present ? "composer-audit" : null,
    dependabot?.available ? "github-dependabot" : null,
  ].filter(Boolean);
  out.push(
    line([
      report.node_version ? `node ${report.node_version}` : undefined,
      report.npm_version ? `npm ${report.npm_version}` : undefined,
      scanners.length ? `scanners: ${scanners.join(", ")}` : "no audit scanner on PATH",
      dependabot && !dependabot.available && dependabot.error
        ? `dependabot unavailable (${dependabot.error})`
        : undefined,
    ]),
  );
  out.push(`Audit summary: ${formatAuditCounts(report.audit)}; ${report.outdated.length} outdated.`);
  if (dependabot?.available) {
    out.push(`GitHub Dependabot: ${dependabot.alerts.length} open alert(s).`);
  }
  if (report.truncated || dependabot?.truncated) {
    out.push("NOTE: the scan was capped; findings below are not complete coverage.");
  }

  if (report.issues.length > 0) {
    out.push("", `## Issues (${report.issues.length})`);
    for (const issue of report.issues) {
      out.push(`- [${issue.severity}] ${issue.code}${issue.path ? ` (${issue.path})` : ""}: ${issue.message}`);
    }
  }

  if (report.vulnerabilities.length > 0) {
    out.push("", `## Vulnerabilities (${report.vulnerabilities.length})`);
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
    out.push("", `## GitHub Dependabot alerts (${dependabot.alerts.length})`);
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
    out.push("", `## Outdated packages (${report.outdated.length})`);
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
  if (nothingReported) {
    out.push("", "No issues, vulnerabilities or outdated packages were reported.");
  }

  return out.join("\n");
}
