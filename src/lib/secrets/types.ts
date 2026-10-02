/**
 * Mirror of `src-tauri/src/secrets/parse.rs` — `SecretsReport` and
 * `SecretFinding`, checked field-for-field by `check:types` (CONTRACTS row
 * "secrets").
 */

/** Where a finding sits relative to the repository's Git state. */
export type SecretLocation =
  | "git_metadata"
  | "tracked"
  | "untracked"
  | "nested_repo"
  | "ignored"
  | "outside"
  | "unknown";

/** Whether Kingfisher said it read every input. */
export type ScanCompleteness = "complete" | "partial" | "unverified";

export interface SecretFinding {
  rule_id: string;
  rule_name: string;
  /** Repo-relative, `/`-separated (absolute only for `outside`). */
  path: string;
  /** 1-based; 0 when Kingfisher reported none. */
  line: number;
  /** `low` | `medium` | `high`, or empty. */
  confidence: string;
  location: SecretLocation;
  /** Shared by rows holding the same value in this scan; 0 = unknown. */
  secret_group: number;
}

export interface SecretsReport {
  ok: boolean;
  error?: string | null;
  kingfisher_present: boolean;
  kingfisher_version?: string | null;
  nested_repos_scanned: boolean;
  completeness: ScanCompleteness;
  findings: SecretFinding[];
  findings_total: number;
  findings_unreadable: number;
  findings_truncated: boolean;
  git_status_known: boolean;
  max_file_size_mb: number;
  scanned_at_ms: number;
  duration_ms: number;
}

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
      line: count(f.line),
      confidence: typeof f.confidence === "string" ? f.confidence : "",
      location,
      secret_group: count(f.secret_group),
    });
  }
  const completeness =
    typeof raw.completeness === "string" && COMPLETENESS.has(raw.completeness)
      ? (raw.completeness as ScanCompleteness)
      : "unverified";
  return {
    // A report that claims success with no findings array is not a scan
    // that found nothing; it is a payload this build cannot read.
    ok: raw.ok === true && findingsOk,
    error:
      typeof raw.error === "string"
        ? raw.error
        : raw.ok === true && !findingsOk
          ? "The secrets report arrived without a findings list. This is not a clean result."
          : null,
    kingfisher_present: raw.kingfisher_present === true,
    kingfisher_version:
      typeof raw.kingfisher_version === "string" ? raw.kingfisher_version : null,
    nested_repos_scanned: raw.nested_repos_scanned !== false,
    completeness,
    findings,
    findings_total: Math.max(count(raw.findings_total), findings.length),
    findings_unreadable: unreadable,
    findings_truncated: raw.findings_truncated === true,
    git_status_known: raw.git_status_known === true,
    max_file_size_mb: count(raw.max_file_size_mb),
    scanned_at_ms: count(raw.scanned_at_ms),
    duration_ms: count(raw.duration_ms),
  };
}
