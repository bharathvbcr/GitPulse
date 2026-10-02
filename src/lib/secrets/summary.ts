/**
 * What the Secrets panel says about a report, decided outside the component
 * so every sentence is unit-tested (vitest renders Svelte server-side, where
 * `$effect` never runs, so panel logic left inline is logic nobody tests).
 *
 * The rule every function here serves: a scan that could not read every
 * input never renders like one that did, and a list that was cut short
 * always says by how much.
 */
import { plural } from "../format";
import type { SecretFinding, SecretLocation, SecretsReport } from "./types";

/** Most actionable first; matches the backend's sort. */
export const LOCATION_ORDER: readonly SecretLocation[] = [
  "git_metadata",
  "tracked",
  "unknown",
  "untracked",
  "nested_repo",
  "ignored",
  "outside",
];

export type LocationTone = "danger" | "warn" | "muted";

export const LOCATION_COPY: Record<
  SecretLocation,
  { label: string; hint: string; tone: LocationTone }
> = {
  git_metadata: {
    label: "Git metadata",
    hint: "Inside .git: remote URLs and config that every Git command reads.",
    tone: "danger",
  },
  tracked: {
    label: "Tracked",
    hint: "In the index: already in history, or will be with the next commit.",
    tone: "danger",
  },
  unknown: {
    label: "Unknown",
    hint: "Git status could not be read, so this may be tracked.",
    tone: "warn",
  },
  untracked: {
    label: "Untracked",
    hint: "Not ignored: the next `git add -A` would commit it.",
    tone: "warn",
  },
  nested_repo: {
    label: "Nested repo",
    hint: "Belongs to a nested repository or submodule inside this tree.",
    tone: "warn",
  },
  ignored: {
    label: "Ignored",
    hint: "Matched by an ignore rule: build output, caches, local env files.",
    tone: "muted",
  },
  outside: {
    label: "Outside",
    hint: "Reported outside this repository's root.",
    tone: "muted",
  },
};

export type LocationFilter = SecretLocation | "all";

export function countByLocation(findings: readonly SecretFinding[]): Record<SecretLocation, number> {
  const counts = Object.fromEntries(LOCATION_ORDER.map((l) => [l, 0])) as Record<
    SecretLocation,
    number
  >;
  for (const f of findings) counts[f.location] += 1;
  return counts;
}

/** Locations present in `findings`, in display order, with their counts. */
export function locationChips(
  findings: readonly SecretFinding[],
): { location: SecretLocation; count: number }[] {
  const counts = countByLocation(findings);
  return LOCATION_ORDER.filter((l) => counts[l] > 0).map((location) => ({
    location,
    count: counts[location],
  }));
}

export function filterFindings(
  findings: readonly SecretFinding[],
  filter: LocationFilter,
): SecretFinding[] {
  return filter === "all" ? [...findings] : findings.filter((f) => f.location === filter);
}

/** How many shown rows share each value group (group 0 is "unknown"). */
export function groupSizes(findings: readonly SecretFinding[]): Map<number, number> {
  const sizes = new Map<number, number>();
  for (const f of findings) {
    if (f.secret_group > 0) sizes.set(f.secret_group, (sizes.get(f.secret_group) ?? 0) + 1);
  }
  return sizes;
}

/** Distinct values among the shown rows; rows with no group count once each. */
export function distinctSecrets(findings: readonly SecretFinding[]): number {
  const groups = new Set<number>();
  let ungrouped = 0;
  for (const f of findings) {
    if (f.secret_group > 0) groups.add(f.secret_group);
    else ungrouped += 1;
  }
  return groups.size + ungrouped;
}

export type VerdictTone = "ok" | "warn" | "danger";

export interface Verdict {
  tone: VerdictTone;
  title: string;
  detail: string;
}

/**
 * The headline. "No secrets reported" is reachable only from a scan that
 * succeeded, read every input, lost no rows, and found nothing.
 */
export function verdict(report: SecretsReport): Verdict {
  const total = report.findings_total;
  const lost = report.findings_unreadable;
  if (!report.ok) {
    return {
      tone: total > 0 ? "danger" : "warn",
      title: total > 0 ? "Secrets scan failed partway" : "Secrets scan unavailable",
      detail:
        (report.error ?? "The scanner did not complete.") +
        (total > 0 ? "" : " This is not a clean result."),
    };
  }
  if (total === 0 && lost === 0 && !report.findings_truncated) {
    if (report.completeness === "complete") {
      return {
        tone: "ok",
        title: "No secrets reported",
        detail:
          "Kingfisher read every file in scope and reported nothing at medium confidence or above.",
      };
    }
    const partial = report.completeness === "partial";
    return {
      tone: "warn",
      title: partial
        ? "No findings, but the scan was partial"
        : "No findings, completeness unverified",
      detail: partial
        ? "Kingfisher could not read some files (permissions, or files that changed mid-scan). This is not a clean result."
        : "This Kingfisher did not report whether it read every file. This is not a clean result.",
    };
  }
  const floor = report.findings_truncated || lost > 0;
  return {
    tone: "danger",
    title: `${plural(total, "finding")}${floor ? "+" : ""}`,
    detail: findingsDetail(report),
  };
}

function findingsDetail(report: SecretsReport): string {
  const parts: string[] = [];
  if (report.completeness === "partial") {
    parts.push("Some files could not be read, so this list is a floor.");
  } else if (report.completeness === "unverified") {
    parts.push("Kingfisher did not report whether it read every file.");
  }
  if (report.findings_truncated) parts.push("Kingfisher omitted findings past its own report cap.");
  const lost = report.findings_unreadable;
  if (lost > 0) {
    parts.push(`${plural(lost, "finding")} could not be read and ${lost === 1 ? "is" : "are"} not listed.`);
  }
  if (!report.git_status_known) {
    parts.push(
      "Git status could not be read, so locations other than .git and nested repositories are unknown.",
    );
  }
  return parts.join(" ");
}

/** A note when the backend capped the rows; null when every row is shown. */
export function capNote(report: SecretsReport): string | null {
  if (report.findings.length >= report.findings_total) return null;
  return `Showing the ${report.findings.length.toLocaleString("en-US")} most actionable of ${report.findings_total.toLocaleString("en-US")} findings.`;
}

/**
 * What "every file in scope" means. Each line is a behaviour measured on
 * Kingfisher 2.7.0 with GitPulse's argv, not a paraphrase of its docs.
 */
export function scopeNotes(report: SecretsReport): string[] {
  const notes = [
    "Working tree only: commit history is not scanned.",
    "Values are matched locally and never validated against a provider.",
    "Symbolic links are not followed, and Kingfisher skips some directories itself (node_modules).",
    "Inline kingfisher:ignore comments are honoured.",
  ];
  if (report.max_file_size_mb > 0) {
    notes.push(`Files over ${report.max_file_size_mb} MB are skipped without being reported.`);
  }
  if (report.nested_repos_scanned) {
    notes.push("Nested repositories and submodules inside this tree are scanned too.");
  }
  return notes;
}

/** A cached report older than this is refreshed when the panel reopens. */
export const STALE_AFTER_MS = 5 * 60_000;

export function isStale(report: SecretsReport, nowMs: number): boolean {
  return report.scanned_at_ms <= 0 || nowMs - report.scanned_at_ms >= STALE_AFTER_MS;
}

/**
 * Whether `incoming` should replace `cached` in the per-repo cache. Only a
 * scan that ran (has a finish time) is cached, and an older scan landing
 * late never overwrites a newer one.
 */
export function shouldCache(cached: SecretsReport | undefined, incoming: SecretsReport): boolean {
  if (incoming.scanned_at_ms <= 0) return false;
  return !cached || incoming.scanned_at_ms >= cached.scanned_at_ms;
}

/** `path:line`, or the bare path when Kingfisher gave no line. */
export function locationLabel(finding: SecretFinding): string {
  return finding.line > 0 ? `${finding.path}:${finding.line}` : finding.path;
}

/** Row key; `keyedList` disambiguates exact duplicates. */
export function findingKey(finding: SecretFinding): string {
  return `${finding.location}:${finding.path}:${finding.line}:${finding.rule_id}:${finding.secret_group}`;
}
