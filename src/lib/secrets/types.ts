/**
 * Mirror of `src-tauri/src/secrets/parse.rs` — `SecretsReport`,
 * `SecretFinding` and `SecretSkip`, checked field-for-field by `check:types`
 * (CONTRACTS row "secrets").
 */

import { redactDiagnosticText } from "../diagnostics/diagnostics";

/** Where a finding sits relative to the repository's Git state. */
export type SecretLocation =
  | "git_metadata"
  | "tracked"
  | "untracked"
  | "nested_repo"
  | "ignored"
  | "outside"
  | "unknown";

/**
 * Whether the scan read every input: Kingfisher's own audit for its deep
 * scan, the in-process scanner's account of what it skipped otherwise.
 */
export type ScanCompleteness = "complete" | "partial" | "unverified";

export interface SecretFinding {
  rule_id: string;
  rule_name: string;
  /** Repo-relative, `/`-separated (absolute only for `outside`). */
  path: string;
  /** 1-based; null when Kingfisher reported none. */
  line: number | null;
  /** `low` | `medium` | `high`, or empty. */
  confidence: string;
  location: SecretLocation;
  /** Shared by rows holding the same value in this scan; 0 = unknown. */
  secret_group: number;
}

/** One input the in-process scan did not read; a hole, never a clean file. */
export interface SecretSkip {
  path: string;
  /** Why, as a sentence. */
  reason: string;
  bytes: number | null;
}

export interface SecretsReport {
  ok: boolean;
  /**
   * The scanner that produced this report: the in-process engine's name and
   * version, or `kingfisher` for the opt-in deep scan.
   */
  engine: string;
  error?: string | null;
  /**
   * Copyable facts about a failed run. Never Kingfisher stdout or stderr.
   * Absent on a scan that completed.
   */
  diagnostic?: string | null;
  kingfisher_present: boolean;
  kingfisher_version?: string | null;
  /** The scan output named a version that was refused; see `kingfisher_version`. */
  kingfisher_version_rejected: boolean;
  nested_repos_scanned: boolean;
  /**
   * Repo-relative directories, each ending in `/`, left out of the scan:
   * wholly ignored build output, dependency and scratch trees, and linked
   * worktrees of this repository nested inside it.
   */
  excluded_paths: string[];
  completeness: ScanCompleteness;
  findings: SecretFinding[];
  findings_total: number;
  findings_unreadable: number;
  findings_truncated: boolean;
  git_status_known: boolean;
  max_file_size_mb: number;
  scanned_at_ms: number;
  duration_ms: number;
  /** Inputs not read, all counted; `skipped` lists the first of them. */
  skipped_total: number;
  skipped: SecretSkip[];
}

/** `report::MAX_EXCLUSIONS` in the backend. */
const MAX_EXCLUDED_PATHS = 256;
/** `leak_scan::MAX_REPORTED_SKIPS` in the backend. */
const MAX_SKIPPED = 200;

const LOCATIONS: ReadonlySet<string> = new Set<SecretLocation>([
  "git_metadata",
  "tracked",
  "untracked",
  "nested_repo",
  "ignored",
  "outside",
  "unknown",
]);
const COMPLETENESS: ReadonlySet<string> = new Set<ScanCompleteness>([
  "complete",
  "partial",
  "unverified",
]);

/**
 * A non-string or blank value is absent. Credentials are stripped before the
 * length cap, so a cut cannot leave the head of a token the redactor would
 * have matched on the whole string.
 */
function boundedText(value: unknown, max: number): string | null {
  if (typeof value !== "string") return null;
  const trimmed = redactDiagnosticText(value).trim();
  if (!trimmed) return null;
  return trimmed.length > max ? trimmed.slice(0, max) : trimmed;
}

function count(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? Math.floor(value)
    : 0;
}

/**
 * Narrow an IPC payload to a {@link SecretsReport}.
 *
 * Fails closed: a payload whose `ok` is not literally `true`, whose findings
 * are not an array, or whose completeness is unrecognised can never come
 * out as a clean, complete scan. A row that cannot be read is counted into
 * `findings_unreadable` rather than dropped, so the list never silently
 * shrinks.
 */
export function parseSecretsReport(value: unknown): SecretsReport {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Secrets scan returned an unexpected payload");
  }
  const raw = value as Record<string, unknown>;
  const findingsOk = Array.isArray(raw.findings);
  const findingsRaw: unknown[] = findingsOk ? (raw.findings as unknown[]) : [];
  const findings: SecretFinding[] = [];
  let unreadable = count(raw.findings_unreadable);
  for (const item of findingsRaw) {
    const f = item && typeof item === "object" ? (item as Record<string, unknown>) : null;
    const rule_id = f && typeof f.rule_id === "string" ? f.rule_id : "";
    const path = f && typeof f.path === "string" ? f.path : "";
    if (!f || !rule_id || !path) {
      unreadable += 1;
      continue;
    }
    const location =
      typeof f.location === "string" && LOCATIONS.has(f.location)
        ? (f.location as SecretLocation)
        : "unknown";
    findings.push({
      rule_id,
      rule_name: typeof f.rule_name === "string" ? f.rule_name : "",
      path,
      line: typeof f.line === "number" && Number.isInteger(f.line) && f.line > 0 ? f.line : null,
      confidence: typeof f.confidence === "string" ? f.confidence : "",
      location,
      secret_group: count(f.secret_group),
    });
  }
  const skipped: SecretSkip[] = [];
  if (Array.isArray(raw.skipped)) {
    for (const item of raw.skipped.slice(0, MAX_SKIPPED)) {
      const k = item && typeof item === "object" ? (item as Record<string, unknown>) : null;
      if (!k || typeof k.path !== "string" || !k.path) continue;
      skipped.push({
        path: k.path.slice(0, 4_096),
        reason: boundedText(k.reason, 200) ?? "was not read",
        bytes: typeof k.bytes === "number" && Number.isFinite(k.bytes) && k.bytes >= 0 ? k.bytes : null,
      });
    }
  }
  const declared =
    typeof raw.completeness === "string" && COMPLETENESS.has(raw.completeness)
      ? (raw.completeness as ScanCompleteness)
      : "unverified";
  // Fail closed: a report that lists unread inputs is not a complete scan,
  // whatever it declared.
  const completeness =
    declared === "complete" && (skipped.length > 0 || count(raw.skipped_total) > 0)
      ? "partial"
      : declared;
  return {
    // A report that claims success with no findings array is not a scan
    // that found nothing; it is a payload this build cannot read.
    ok: raw.ok === true && findingsOk,
    engine: boundedText(raw.engine, 128) ?? "",
    error:
      typeof raw.error === "string"
        ? boundedText(raw.error, 4_000)
        : raw.ok === true && !findingsOk
          ? "The secrets report arrived without a findings list. This is not a clean result."
          : null,
    diagnostic: boundedText(raw.diagnostic, 4_000),
    kingfisher_present: raw.kingfisher_present === true,
    kingfisher_version:
      typeof raw.kingfisher_version === "string" ? raw.kingfisher_version : null,
    kingfisher_version_rejected: raw.kingfisher_version_rejected === true,
    nested_repos_scanned: raw.nested_repos_scanned !== false,
    excluded_paths: Array.isArray(raw.excluded_paths)
      ? raw.excluded_paths
          .filter((p): p is string => typeof p === "string" && p.length > 0)
          .slice(0, MAX_EXCLUDED_PATHS)
          .map((p) => p.slice(0, 4_096))
      : [],
    completeness,
    findings,
    findings_total: Math.max(count(raw.findings_total), findings.length),
    findings_unreadable: unreadable,
    findings_truncated: raw.findings_truncated === true,
    git_status_known: raw.git_status_known === true,
    max_file_size_mb: count(raw.max_file_size_mb),
    scanned_at_ms: count(raw.scanned_at_ms),
    duration_ms: count(raw.duration_ms),
    skipped_total: Math.max(count(raw.skipped_total), skipped.length),
    skipped,
  };
}
