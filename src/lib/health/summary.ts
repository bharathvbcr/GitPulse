/**
 * The Health view's at-a-glance verdict.
 *
 * The panel used to have none. A reader landed on a scroll that opened with a
 * paragraph about GitHub CLI permissions, then an inventory of package
 * manifests, and only then the one high-severity vulnerability — roughly a
 * thousand pixels down. The header tried to compensate by cramming five
 * `truncate` spans into one flex row, which is why it rendered as
 * "58 outd… · Dependabot 0 … · Code scanning unav…": the summary was
 * unreadable at exactly the moment it mattered.
 *
 * This is the single owner of that verdict. The header chip, the summary card
 * and the jump chips all read it, so the three cannot disagree about the same
 * scan — the failure mode that produced two different truncation stories in
 * the old header and body.
 *
 * ## The honesty invariant
 *
 * A check that could not run must never look like one that ran and passed.
 * That is mechanical here rather than a matter of wording:
 *
 *  - `unknown` is its own tone, ranked above `clear` and above `note`, so a
 *    facet nobody could establish can never be summed away into an all-clear.
 *  - `clearClaimable` is true only when *every* facet is `clear`. A caller
 *    that wants to render a green all-clear has exactly one boolean to read,
 *    and it is false the moment anything is unestablished.
 *  - Every non-clear facet carries a `caveat` naming *why*, drawn from the
 *    scanner's own reported reason. Nothing here invents a cause it was not
 *    told; an unavailable report with no reason says "reason not reported".
 */

import { formatAuditCounts, normalizeSeverity } from "./format";
import { coverageGap, inventoryOnlyTruncation, observedTotal } from "./report";
import type {
  CodeScanningReport,
  DependabotReport,
  DepsHealthReport,
} from "./types";
import type { CodeintelStatus } from "../codeintel/types";

/**
 * How a facet reads, worst last.
 *
 * `note` is deliberately between `clear` and `unknown`: 58 outdated packages
 * is real information and must not render as an all-clear, but it is less
 * urgent than a scanner whose result nobody has.
 */
export type HealthTone = "clear" | "note" | "unknown" | "warn" | "critical";

const TONE_RANK: Readonly<Record<HealthTone, number>> = Object.freeze({
  clear: 0,
  note: 1,
  unknown: 2,
  warn: 3,
  critical: 4,
});

/** Worst tone in a set; `clear` only when every input was clear. */
export function worstTone(tones: Iterable<HealthTone>): HealthTone {
  let worst: HealthTone = "clear";
  for (const tone of tones) {
    if (TONE_RANK[tone] > TONE_RANK[worst]) worst = tone;
  }
  return worst;
}

/**
 * One line of the verdict, and one jump target.
 *
 * `id` is a section id from `HEALTH_SECTIONS`, so a facet chip can scroll to
 * the section it summarises and the two lists cannot drift apart.
 */
export interface HealthFacet {
  id: string;
  label: string;
  /** Short enough to sit in a chip: "1 high", "0 open", "not checked". */
  value: string;
  tone: HealthTone;
  /** Why this is not an all-clear. Absent only when the tone is `clear`. */
  caveat?: string;
}

export interface HealthSummary {
  tone: HealthTone;
  headline: string;
  facets: HealthFacet[];
  /**
   * Every reason this scan is not a complete all-clear, in facet order and
   * then scan-wide. Rendered verbatim; never summarised into a count.
   */
  caveats: string[];
  /**
   * True only when every facet is `clear`. The one boolean a caller may use
   * to render an all-clear.
   */
  clearClaimable: boolean;
}

/** What the panel knows about the dead-code query, in the query's own terms. */
export interface DeadCodeSummaryInput {
  available: boolean;
  reason: string | null;
  total: number;
  shown: number;
  truncated: boolean;
  walkIncomplete: string | null;
}

export interface HealthSummaryInput {
  report: DepsHealthReport;
  dependabot: DependabotReport | null;
  codeScanning: CodeScanningReport | null;
  codegraph: CodeintelStatus | null;
  deadCode: DeadCodeSummaryInput | null;
}

/** Advisory severity, as a tone. Shares `normalizeSeverity` with the badges. */
export function severityTone(severity: string): HealthTone {
  switch (normalizeSeverity(severity)) {
    case "critical":
    case "high":
      return "critical";
    case "moderate":
    case "low":
      return "warn";
    default:
      return "note";
  }
}

/**
 * Health-issue severity, as a tone.
 *
 * `HealthIssue.severity` is a free string from the scanner ("error",
 * "warning", "info"), not an advisory rank, which is why it does not go
 * through `normalizeSeverity`: that one maps "error" to "high", and an
 * `error`-level lint finding is not a high-severity vulnerability.
 */
export function issueTone(severity: string): HealthTone {
  const key = severity.trim().toLowerCase();
  if (key === "error" || key === "critical") return "critical";
  if (key === "warning" || key === "warn") return "warn";
  return "note";
}

/** Tone of the audit's own counts, ignoring whether the audit was complete. */
function auditFindingTone(report: DepsHealthReport): HealthTone {
  const audit = report.audit;
  if ((audit.critical ?? 0) > 0 || (audit.high ?? 0) > 0) return "critical";
  // Anything counted but not in a clear-cut bucket is still a finding: an
  // unranked pip-audit result must not fall through to `clear`.
  if (audit.total > 0) return "warn";
  return "clear";
}

function vulnerabilityFacet(report: DepsHealthReport): HealthFacet {
  const id = "vulnerabilities";
  const label = "Vulnerabilities";
  const gap = coverageGap(report);
  const ran = (report.scanners_ran ?? []).length > 0;
  const complete = report.audit_complete === true;

  if (!ran) {
    return {
      id,
      label,
      value: "did not run",
      tone: "unknown",
      caveat: `No local audit ran${gap ? ` (${gap})` : ""}, so no finding count exists.`,
    };
  }

  const found = auditFindingTone(report);
  const counts =
    report.audit.total === 0 ? "none found" : formatAuditCounts(report.audit);

  if (!complete) {
    return {
      id,
      label,
      value: `${counts}, partial`,
      tone: worstTone([found, "unknown"]),
      caveat: `The local audit did not cover every discovered target${gap ? ` (${gap})` : ""}; these counts are a floor.`,
    };
  }

  return {
    id,
    label,
    value: counts,
    tone: found,
    caveat:
      found === "clear"
        ? undefined
        : `${counts} reported by the completed local audit.`,
  };
}

/**
 * Dependabot and code scanning, which have the same five-state contract:
 * never fetched, fetched and failed, fetched with no CLI, clear, or alerts.
 *
 * One function rather than two because the two reports carry the same fields
 * and the panel previously derived them in two hand-written copies that had
 * already drifted once.
 */
function githubFacet(
  id: string,
  label: string,
  report: DependabotReport | CodeScanningReport | null,
): HealthFacet {
  if (!report) {
    return {
      id,
      label,
      value: "not checked",
      tone: "unknown",
      caveat: `${label} alerts have not been fetched for this repository.`,
    };
  }
  if (!report.available) {
    const why =
      report.error?.trim() || report.unavailable_reason || "reason not reported";
    return {
      id,
      label,
      value: "unavailable",
      tone: "unknown",
      caveat: `${label} could not be checked: ${why}.`,
    };
  }
  if (report.alerts.length === 0) {
    return { id, label, value: "0 open", tone: "clear" };
  }
  const tone = worstTone(report.alerts.map((alert) => severityTone(alert.severity)));
  return {
    id,
    label,
    value: `${report.alerts.length}${report.truncated ? "+" : ""} open`,
    tone,
    caveat: report.truncated
      ? `Only the first ${report.alerts.length} ${label.toLowerCase()} alerts were returned; more remain.`
      : `${report.alerts.length} open ${label.toLowerCase()} alerts.`,
  };
}

function issuesFacet(report: DepsHealthReport): HealthFacet {
  const id = "issues";
  const label = "Issues";
  const total = observedTotal(report, "health issues", report.issues.length);
  if (total === 0) return { id, label, value: "none", tone: "clear" };
  const tone = worstTone(report.issues.map((issue) => issueTone(issue.severity)));
  const capped = total > report.issues.length;
  return {
    id,
    label,
    value: `${total}${capped ? `, ${report.issues.length} shown` : ""}`,
    tone,
    caveat: capped
      ? `${total} repository issues were found; only ${report.issues.length} fitted the display cap.`
      : `${total} repository ${total === 1 ? "issue" : "issues"} reported.`,
  };
}

function outdatedFacet(report: DepsHealthReport): HealthFacet {
  const id = "outdated";
  const label = "Outdated";
  if (!report.npm_cli_present) {
    return {
      id,
      label,
      value: "needs npm",
      tone: "unknown",
      caveat: "Outdated-package checks need npm on PATH; none ran.",
    };
  }
  const total = observedTotal(report, "outdated npm packages", report.outdated.length);
  if (total === 0) return { id, label, value: "none", tone: "clear" };
  return {
    id,
    label,
    value: `${total} behind`,
    tone: "note",
    caveat: `${total} npm ${total === 1 ? "package is" : "packages are"} behind their latest release.`,
  };
}

function deadCodeFacet(
  codegraph: CodeintelStatus | null,
  deadCode: DeadCodeSummaryInput | null,
): HealthFacet {
  const id = "dead-code";
  const label = "Dead code";
  if (!codegraph || !codegraph.available) {
    return {
      id,
      label,
      value: "no code graph",
      tone: "unknown",
      caveat: `Dead-code detection has no code graph to read${
        codegraph?.reason ? `: ${codegraph.reason}` : ""
      }.`,
    };
  }
  if (!deadCode || !deadCode.available) {
    return {
      id,
      label,
      value: "did not run",
      tone: "unknown",
      caveat: `The dead-code query did not run${
        deadCode?.reason ? `: ${deadCode.reason}` : ""
      }; that is not the same as finding nothing.`,
    };
  }
  if (deadCode.walkIncomplete) {
    return {
      id,
      label,
      value: `${deadCode.total} candidates, partial`,
      tone: "unknown",
      caveat: `Dead-code analysis is incomplete: ${deadCode.walkIncomplete}. Missing callers produce false positives.`,
    };
  }
  if (deadCode.truncated) {
    return {
      id,
      label,
      value: `${deadCode.total}+ candidates`,
      tone: "unknown",
      caveat:
        "The dead-symbol query stopped at its token budget, so its result is a floor and not an all-clear.",
    };
  }
  if (deadCode.total === 0) return { id, label, value: "none", tone: "clear" };
  return {
    id,
    label,
    value: `${deadCode.total} candidates`,
    tone: "note",
    caveat: `${deadCode.total} unreferenced ${
      deadCode.total === 1 ? "symbol" : "symbols"
    } in the indexed graph.`,
  };
}

const HEADLINE_PREFIX: Readonly<Record<HealthTone, string>> = Object.freeze({
  critical: "Action needed",
  warn: "Review",
  unknown: "Not established",
  note: "No findings, with notes",
  clear: "",
});

/**
 * Derive the whole verdict from one scan.
 *
 * Pure, and deliberately the only place that decides what "healthy" means:
 * the header chip, the summary card and the jump chips all render this, so a
 * reader cannot be told two different things about one scan.
 */
export function summarizeHealth(input: HealthSummaryInput): HealthSummary {
  const { report, dependabot, codeScanning, codegraph, deadCode } = input;

  const facets: HealthFacet[] = [
    vulnerabilityFacet(report),
    githubFacet("dependabot", "Dependabot", dependabot),
    githubFacet("code-scanning", "Code scanning", codeScanning),
    issuesFacet(report),
    outdatedFacet(report),
    deadCodeFacet(codegraph, deadCode),
  ];

  const tone = worstTone(facets.map((facet) => facet.tone));

  const caveats = facets
    .filter((facet) => facet.tone !== "clear" && facet.caveat)
    .map((facet) => facet.caveat!);

  // Scan-wide truncation is not any one facet's: `cap_report` can drop rows
  // from several sections at once, and the notices name the resource.
  //
  // The panel used to head this with "audit target coverage is reported
  // separately above" — and "above" was the header summary string, which was
  // being truncated mid-word at the time. A cross-reference to something the
  // reader cannot see is worse than none, so this names the facet instead.
  if (report.truncated) {
    caveats.push(
      inventoryOnlyTruncation(report)
        ? "Inventory display was capped. Audit target coverage is stated by the Vulnerabilities facet, not by these counts."
        : "The scan was capped; some findings may be omitted.",
    );
  }
  for (const notice of report.limit_notices ?? []) {
    caveats.push(`${notice.resource}: retained ${notice.kept} of ${notice.total}`);
  }

  const worstFacets = facets.filter((facet) => facet.tone === tone);
  const list = worstFacets
    .map((facet) => `${facet.label} ${facet.value}`)
    .join(" · ");

  const headline =
    tone === "clear"
      ? "Every scan completed with no findings."
      : `${HEADLINE_PREFIX[tone]}: ${list}`;

  return {
    tone,
    headline,
    facets,
    caveats,
    clearClaimable: tone === "clear",
  };
}
