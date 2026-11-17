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
import { SCAN_FRESH_MS, isScanStale } from "../runtime/autoScan";
import type { SecretFinding, SecretLocation, SecretsReport } from "./types";

/**
 * The scanner named for a header: the in-process engine's own name, with
 * Kingfisher and its version added when it ran as the second opinion.
 */
export function engineLabel(report: SecretsReport): string {
  const name = report.engine || "unknown engine";
  const second = report.second_opinion;
  if (second.state !== "ran") return name;
  return `${name} + ${second.version ? `Kingfisher ${second.version}` : "Kingfisher"}`;
}

/**
 * One sentence on the second opinion, for a headline's detail. Null when
 * there is nothing to say: the in-process result stands alone and Kingfisher
 * is not installed to add to it.
 */
export function secondOpinionNote(report: SecretsReport): string | null {
  const second = report.second_opinion;
  switch (second.state) {
    case "not_installed":
      return "Kingfisher isn't installed. It recognises more credential shapes and gives a second opinion on Rescan.";
    case "not_run":
      return report.kingfisher_present
        ? "Rescan also runs Kingfisher for a second opinion."
        : "Kingfisher isn't installed. It recognises more credential shapes and gives a second opinion on Rescan.";
    case "unavailable":
      return `Kingfisher's second opinion was unavailable: ${second.reason.replace(/[.\s]+$/, "")}. Rescan to try again.`;
    case "ran": {
      const base = `Kingfisher${second.version ? ` ${second.version}` : ""} reported ${plural(second.findings_total, "finding")}: ${second.confirmed.toLocaleString("en-US")} already listed here, ${second.added.toLocaleString("en-US")} added.`;
      if (second.completeness === "complete") return base;
      return second.completeness === "partial"
        ? `${base} It didn't read every file.`
        : `${base} It didn't report whether it read every file.`;
    }
  }
}

/** Skipped inputs named in a note before it says how many more there are. */
const SKIPPED_SHOWN = 5;

/**
 * The inputs the in-process scan did not read, by name and reason; null when
 * it read everything. A scan that skipped anything is partial, never clean.
 */
export function skippedNote(report: SecretsReport): string | null {
  if (report.skipped_total === 0) return null;
  const shown = report.skipped
    .slice(0, SKIPPED_SHOWN)
    .map((skip) => `${skip.path} (${skip.reason})`)
    .join(", ");
  const more = report.skipped_total - Math.min(report.skipped.length, SKIPPED_SHOWN);
  const tail = more > 0 ? `${shown ? ", and " : ""}${more.toLocaleString("en-US")} more` : "";
  return `${plural(report.skipped_total, "input")} not read: ${shown}${tail}.`;
}

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
    hint: "Git status couldn't be read, so this may be tracked.",
    tone: "warn",
  },
  untracked: {
    label: "Untracked",
    hint: "Not ignored: the next `git add -A` would commit it.",
    tone: "warn",
  },
  nested_repo: {
    label: "Nested repository",
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

/**
 * `ok` is green and means two engines agreed; `neutral` is a complete scan
 * that found nothing but was not corroborated, so it carries no green.
 */
export type VerdictTone = "ok" | "neutral" | "warn" | "danger";

export interface Verdict {
  tone: VerdictTone;
  title: string;
  detail: string;
}

/**
 * The headline. "No secrets reported" (green) is reachable only when the
 * in-process scan succeeded, read every input, lost no rows and found
 * nothing, and Kingfisher's second opinion also ran to completion and found
 * nothing. A complete in-process scan alone says only that no known secret
 * shape matched.
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
      const second = report.second_opinion;
      if (second.state === "ran" && second.completeness === "complete") {
        return {
          tone: "ok",
          title: "No secrets reported",
          detail: `${engineLabel(report)} read every file in scope and matched no credential shape. Kingfisher, run as a second opinion, reported nothing at medium confidence or above.`,
        };
      }
      return {
        tone: "neutral",
        title: "No known secret shapes matched",
        detail: `${report.engine || "The in-process scan"} read every file in scope. ${secondOpinionNote(report) ?? ""}`.trim(),
      };
    }
    const partial = report.completeness === "partial";
    return {
      tone: "warn",
      title: partial
        ? "No findings, but the scan was partial"
        : "No findings, completeness unverified",
      detail: partial
        ? skippedNote(report)
          ? `${skippedNote(report)} This is not a clean result.`
          : excludedNote(report.excluded_paths)
            ? `${excludedNote(report.excluded_paths)} This is not a clean result.`
            : "Some files couldn't be read (permissions, or files that changed mid-scan). This is not a clean result."
        : "The scan did not report whether it read every file. This is not a clean result.",
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
    const skipped = skippedNote(report);
    const left = excludedNote(report.excluded_paths);
    parts.push(
      skipped
        ? `${skipped} This list is a floor.`
        : left
          ? `${left} This list is a floor.`
          : "Some files couldn't be read, so this list is a floor.",
    );
  } else if (report.completeness === "unverified") {
    parts.push("The scan did not report whether it read every file.");
  }
  if (report.findings_truncated) parts.push("More findings were found than are listed.");
  const second = secondOpinionNote(report);
  if (second) parts.push(second);
  const lost = report.findings_unreadable;
  if (lost > 0) {
    parts.push(`${plural(lost, "finding")} couldn't be read and ${lost === 1 ? "is" : "are"} not listed.`);
  }
  if (!report.git_status_known) {
    parts.push(
      "Git status couldn't be read, so locations other than .git and nested repositories are unknown.",
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
 * What "every file in scope" means. The first group is a property of the
 * in-process reader; the second is Kingfisher's, each line a behaviour
 * measured with GitPulse's argv, and appears only once it has run.
 */
export function scopeNotes(report: SecretsReport): string[] {
  const notes = [
    "Working tree only: commit history is not scanned.",
    "Reads every tracked file and every untracked file git does not ignore. Ignored files (build output, local .env files) are not read by this scan.",
    "Values are matched locally and never validated against a provider.",
    "Symbolic links are not followed.",
    "Recognises far fewer credential shapes than Kingfisher (recall is measured in docs/PERFORMANCE.md), so a clean result from this scan alone is weak.",
  ];
  if (report.max_file_size_mb > 0) {
    notes.push(
      `Files over ${report.max_file_size_mb} MB, or past the scan's byte or time budget, are listed as not read.`,
    );
  }
  const skipped = skippedNote(report);
  if (skipped) notes.push(skipped);
  const left = excludedNote(report.excluded_paths);
  if (left) notes.push(left);
  const second = report.second_opinion;
  if (second.state === "ran") {
    notes.push(
      "Kingfisher, the second opinion, has no option to honour .gitignore, so it also reads ignored files (build output, local .env files) apart from wholly ignored dependency and scratch trees.",
      "Kingfisher reads nested repositories and submodules inside this tree, skips some directories itself (node_modules), and skips files over its size limit without reporting them.",
      "Inline kingfisher:ignore comments are honored. Symbolic links are not followed.",
    );
  } else if (second.state === "unavailable") {
    notes.push("Kingfisher was asked for a second opinion and did not finish; nothing here comes from it.");
  }
  return notes;
}

/** Trees named in the note before it says how many more there are. */
const EXCLUDED_SHOWN = 10;

/**
 * The nested repositories and submodules the in-process scan named and did
 * not read: they belong to another repository's index.
 */
function excludedNote(paths: readonly string[]): string | null {
  if (paths.length === 0) return null;
  const shown = paths.slice(0, EXCLUDED_SHOWN).join(", ");
  const more = paths.length - EXCLUDED_SHOWN;
  const tail = more > 0 ? `, and ${more.toLocaleString("en-US")} more` : "";
  return `Not scanned: nested repositories and submodules, which belong to another index (${shown}${tail}).`;
}

/** A cached report older than this is refreshed when the panel reopens. */
export const STALE_AFTER_MS = SCAN_FRESH_MS;

/**
 * The text behind Copy error. Kingfisher's own output is not part of it.
 * Credential redaction happens at the panel, on the way to the clipboard
 * and the diagnostics ring — this function only joins the sentences.
 */
export function scanFailureCopy(report?: SecretsReport | null): string {
  const reason = report?.error?.trim() || "The scanner did not complete.";
  const extra = report?.diagnostic?.trim();
  return extra ? `${reason}\n${extra}` : reason;
}

export function isStale(report: SecretsReport, nowMs: number): boolean {
  return isScanStale(report.scanned_at_ms, nowMs);
}

/** The backend's wording for a scan a newer one replaced (`SUPERSEDED` in secrets/run.rs). */
export const SUPERSEDED_ERROR = "superseded by a newer secrets scan";

/**
 * A scan that was replaced, not one that failed: nothing is wrong with the
 * repository or the scanner, so it earns no back-off.
 */
export function isSuperseded(report: SecretsReport): boolean {
  return !report.ok && report.error === SUPERSEDED_ERROR;
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
  return finding.line !== null ? `${finding.path}:${finding.line}` : finding.path;
}

/** Row key; `keyedList` disambiguates exact duplicates. */
export function findingKey(finding: SecretFinding): string {
  return `${finding.location}:${finding.path}:${finding.line ?? ""}:${finding.rule_id}:${finding.secret_group}`;
}
