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

export interface ScanLimitNotice {
  resource: string;
  kept: number;
  total: number;
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
  /**
   * Scanners that actually dispatched a command this scan ("npm", "cargo",
   * "pip-audit", "govulncheck", "composer", "bundler-audit").
   * Empty or absent means nothing ran — zero findings then mean nothing.
   */
  scanners_ran?: string[];
  /** True only when every discovered supported audit target completed. */
  audit_complete?: boolean;
  manifests: NpmManifest[];
  ecosystems: EcosystemHint[];
  issues: HealthIssue[];
  vulnerabilities: Vulnerability[];
  audit: AuditSummary;
  outdated: OutdatedPackage[];
  truncated: boolean;
  /** Exact retained/observed counts for each safety budget that fired. */
  limit_notices?: ScanLimitNotice[];
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

/** One open GitHub code scanning alert. Mirrors `CodeScanningAlertInfo`. */
export interface CodeScanningAlertInfo {
  number: number;
  rule_id: string;
  rule_name: string;
  /** Prefer GitHub `security_severity_level`; else CodeQL `rule.severity`. */
  severity: string;
  state: string;
  tool: string;
  tool_version: string;
  title: string;
  path: string;
  /** 0 when GitHub omitted `most_recent_instance.location.start_line`. */
  start_line: number;
  url: string;
  dismissed_reason: string;
  created_at: string;
  updated_at: string;
}

/**
 * Result of fetching code scanning alerts for the opened repository.
 * Same fail-closed contract as Dependabot: `available: false` with an `error`
 * is "could not check", never an empty success.
 */
export interface CodeScanningReport {
  available: boolean;
  cli_present: boolean;
  is_github_remote: boolean;
  slug: string;
  alerts: CodeScanningAlertInfo[];
  truncated: boolean;
  error?: string | null;
}

/**
 * Dead-code query the Health view already shows. Field names match
 * `CodeintelDeadSymbol` / `CodeintelResponse` so the panel can pass its
 * bindings through without a second mapping layer.
 *
 * `available: false` is "could not check" — distinct from an empty `items`
 * list, which only ever means the indexed graph had no unreferenced symbols.
 */
export interface DeadCodeFinding {
  symbol_name: string;
  file_path: string;
  confidence: number;
  is_exempt: boolean;
  exemption_reason?: string | null;
}

export interface DeadCodeReport {
  available: boolean;
  reason?: string | null;
  items: DeadCodeFinding[];
  /** Observed total, never below `items.length`. */
  total: number;
  truncated: boolean;
}
