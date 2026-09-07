import { formatAuditCounts } from "./format";
import { cappedSuffix, observedTotal as observedScanTotal } from "../scan/limits";
import type { CodeScanningReport, DependabotReport, DepsHealthReport } from "./types";

function line(items: (string | undefined | null)[]): string {
  return items.filter((item) => item !== undefined && item !== null && item !== "").join(" · ");
}

/**
 * How many of a bounded collection the scan actually observed, as opposed to
 * how many survived the cap. Canonical owner for both the text report and the
 * Health view, which previously each re-derived it and could disagree.
 *
 * Thin adapter over the shared reader in `scan/limits`; the coverage report
 * asks the same question of its own notices.
 */
export function observedTotal(
  report: DepsHealthReport,
  resource: string,
  retained: number,
): number {
  return observedScanTotal(report, resource, retained);
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
 * Issue codes the backend writes when an audit it dispatched then failed,
 * mapped to the name a reader would recognise.
 *
 * These are the same codes `audit_is_complete` treats as disqualifying in
 * `analyzer/deps.rs`, and `scripts/health-failure-codes-contract.test.ts`
 * asserts the two lists stay identical — otherwise a scanner could start
 * failing in a way that clears `audit_complete` while this map stays silent
 * about which one it was.
 */
export const AUDIT_FAILURE_LABELS: Readonly<Record<string, string>> = Object.freeze({
  audit_cwd: "audit path validation",
  audit_failed: "npm audit",
  cargo_audit_failed: "cargo-audit",
  pip_audit_failed: "pip-audit",
  govulncheck_failed: "govulncheck",
  composer_audit_failed: "composer audit",
  bundler_audit_failed: "bundler-audit",
});

/**
 * Audits that ran and failed — as opposed to [`skippedAudits`], which covers
 * only audits that never started because their CLI was missing.
 *
 * Between them these are the two reasons `audit_complete` goes false with a
 * scan that otherwise looks finished, and until now the UI could name only
 * the first. A scanner that was installed, dispatched and then errored left
 * "Local audit incomplete" on screen with nothing saying which one or why,
 * while the reason sat in the issues list further down the page.
 *
 * Derived from the issues actually returned, so a capped scan may name fewer
 * failures than occurred. That is not a silent undercount: capping sets
 * `truncated`, which forces `audit_complete` false and puts the cap notice on
 * screen ahead of this.
 */
export function failedAudits(report: DepsHealthReport): string[] {
  const named = new Set<string>();
  for (const issue of report.issues) {
    const label = AUDIT_FAILURE_LABELS[issue.code];
    if (label) named.add(label);
  }
  return [...named].sort();
}

/**
 * Every reason local audit coverage is incomplete, in one sentence, or null
 * when there is nothing to explain.
 *
 * Canonical owner for the Health view's one-line phrasing, so its header and
 * body cannot disagree about the same scan. The text report states the two
 * halves on separate lines instead — it has the room, and a reader pasting it
 * into an issue acts differently on "never ran" than on "ran and failed" —
 * but both sides read the same two functions, so neither can omit a reason
 * the other names.
 */
export function coverageGap(report: DepsHealthReport): string | null {
  const skipped = skippedAudits(report);
  const failed = failedAudits(report);
  const clauses: string[] = [];
  if (skipped.length > 0) clauses.push(`not installed: ${skipped.join(", ")}`);
  if (failed.length > 0) clauses.push(`failed: ${failed.join(", ")}`);
  if (clauses.length === 0) return null;
  return clauses.join("; ");
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
  codeScanning?: CodeScanningReport | null,
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
    codeScanning?.available ? "github-code-scanning" : null,
  ].filter(Boolean);
  out.push(
    line([
      report.node_version ? `node ${report.node_version}` : undefined,
      report.npm_version ? `npm ${report.npm_version}` : undefined,
      scanners.length ? `scanners: ${scanners.join(", ")}` : "no audit scanner available",
      dependabot && !dependabot.available && dependabot.error
        ? `dependabot unavailable (${dependabot.error})`
        : undefined,
      codeScanning && !codeScanning.available && codeScanning.error
        ? `code scanning unavailable (${codeScanning.error})`
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
  // A scanner that was installed, dispatched and then errored is the other
  // way coverage goes short, and the report named only the first. Its own
  // line rather than a clause on the one above: "did not run" and "ran and
  // failed" are different facts and a reader acts on them differently.
  const failed = failedAudits(report);
  if (failed.length > 0) {
    out.push(
      `NOTE: checks that ran and FAILED: ${failed.join(", ")}. The counts above are not complete coverage.`,
    );
  }
  if (dependabot?.available) {
    out.push(
      `GitHub Dependabot: ${dependabot.truncated ? "at least " : ""}${dependabot.alerts.length} open alert(s).`,
    );
  }
  if (codeScanning?.available) {
    out.push(
      `GitHub Code Scanning: ${codeScanning.truncated ? "at least " : ""}${codeScanning.alerts.length} open alert(s).`,
    );
  }
  if (report.truncated || dependabot?.truncated || codeScanning?.truncated) {
    out.push("NOTE: the scan was capped; findings below are not complete coverage.");
  }
  for (const notice of report.limit_notices ?? []) {
    out.push(`- ${notice.resource}: retained ${notice.kept} of ${notice.total}`);
  }

  if (report.issues.length > 0) {
    const issueTotal = observedTotal(report, "health issues", report.issues.length);
    out.push("", `## Issues (${issueTotal}${cappedSuffix(issueTotal, report.issues.length)})`);
    for (const issue of report.issues) {
      out.push(`- [${issue.severity}] ${issue.code}${issue.path ? ` (${issue.path})` : ""}: ${issue.message}`);
    }
  }

  if (report.vulnerabilities.length > 0) {
    out.push("", `## Vulnerabilities (${report.audit.total}${cappedSuffix(report.audit.total, report.vulnerabilities.length)})`);
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

  if (codeScanning?.available && codeScanning.alerts.length > 0) {
    out.push(
      "",
      `## GitHub Code Scanning alerts (${codeScanning.truncated ? "at least " : ""}${codeScanning.alerts.length})`,
    );
    for (const alert of codeScanning.alerts) {
      const rule = alert.rule_id || alert.rule_name || "rule";
      const location =
        alert.path && alert.start_line > 0
          ? `${alert.path}:${alert.start_line}`
          : alert.path;
      const tool = alert.tool
        ? `${alert.tool}${alert.tool_version ? ` ${alert.tool_version}` : ""}`
        : undefined;
      out.push(`- [${alert.severity}] ${rule} — ${alert.title}`);
      out.push(
        line([
          tool ? `tool: ${tool}` : undefined,
          location ? `at: ${location}` : undefined,
          alert.url ? `alert: ${alert.url}` : undefined,
        ]),
      );
    }
  }

  if (report.outdated.length > 0) {
    out.push("", `## Outdated npm packages (${outdatedTotal}${cappedSuffix(outdatedTotal, report.outdated.length)})`);
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
    (!dependabot?.available || dependabot.alerts.length === 0) &&
    (!codeScanning?.available || codeScanning.alerts.length === 0);
  if (nothingReported && skipped.length === 0 && auditComplete) {
    out.push("", "No issues, vulnerabilities or outdated packages were reported.");
  } else if (nothingReported && !auditComplete) {
    out.push("", "No reportable findings were collected; local audit coverage is incomplete.");
  }

  return out.join("\n");
}
