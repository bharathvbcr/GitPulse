/**
 * Shapes of the dependency-health report, shared by the Health view and the
 * report formatter. Field names mirror the Rust `DepsHealthReport` wire type.
 */

export interface HealthIssue {
  severity: string;
  code: string;
  message: string;
  path?: string | null;
}

export interface EcosystemHint {
  family: string;
  manifests: string[];
  note: string;
}

export interface NpmManifest {
  path: string;
  name: string;
  version: string;
  private: boolean;
  license?: string | null;
  engines_node?: string | null;
  package_manager: string;
  lockfile?: string | null;
  has_workspaces: boolean;
  dep_count: number;
  dev_dep_count: number;
  optional_dep_count: number;
  peer_dep_count: number;
  lifecycle_scripts: string[];
}

export interface Vulnerability {
  name: string;
  severity: string;
  is_direct: boolean;
  title: string;
  url: string;
  range: string;
  fix_available: string;
  via: string[];
  ecosystem: string;
}

export interface AuditSummary {
  info: number;
  low: number;
  moderate: number;
  high: number;
  critical: number;
  /** Findings from scanners that publish no severity (pip-audit, govulncheck). */
  unknown?: number;
  total: number;
}

export interface OutdatedPackage {
  name: string;
  current: string;
  wanted: string;
  latest: string;
  dep_type: string;
  location: string;
}

export interface DepsHealthReport {
  node_version?: string | null;
  npm_version?: string | null;
  npm_cli_present: boolean;
  cargo_audit_present: boolean;
  pip_audit_present?: boolean;
  govulncheck_present?: boolean;
  composer_present?: boolean;
  bundler_audit_present?: boolean;
  manifests: NpmManifest[];
  ecosystems: EcosystemHint[];
  issues: HealthIssue[];
  vulnerabilities: Vulnerability[];
  audit: AuditSummary;
  outdated: OutdatedPackage[];
  truncated: boolean;
}

/** One open Dependabot alert. Mirrors the Rust `DependabotAlertInfo` wire type. */
export interface DependabotAlertInfo {
  number: number;
  package: string;
  ecosystem: string;
  manifest_path: string;
  scope: string;
  /** GitHub's own vocabulary: low | medium | high | critical (may be ""). */
  severity: string;
  title: string;
  advisory_id: string;
  cve_id: string;
  vulnerable_range: string;
  /** Empty when GitHub has published no patched version yet. */
  first_patched: string;
  url: string;
  created_at: string;
}

/**
 * Result of fetching Dependabot alerts for the opened repository.
 * `available: false` with an `error` means "could not check" — distinct from
 * an empty `alerts` list, which only ever means "no open alerts".
 */
export interface DependabotReport {
  available: boolean;
  cli_present: boolean;
  is_github_remote: boolean;
  slug: string;
  alerts: DependabotAlertInfo[];
  truncated: boolean;
  error?: string | null;
}
