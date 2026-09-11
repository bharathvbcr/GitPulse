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
 * Why a Dependabot / code-scanning fetch could not run. Mirrors the Rust
 * `GithubUnavailableReason` enum — decided next to HTTP-status parsing so the
 * UI never matches English fragments of GitHub error prose.
 */
export type GithubUnavailableReason =
  | "not_github_remote"
  | "cli_missing"
  | "product_disabled"
  | "forbidden"
  | "rate_limited"
  | "transport"
  | "unknown";

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
  /** Present only when `available` is false; omitted on success (serde skip). */
  unavailable_reason?: GithubUnavailableReason;
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
  /** Present only when `available` is false; omitted on success (serde skip). */
  unavailable_reason?: GithubUnavailableReason;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function stringArray(value: unknown, label: string): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
    throw new Error(`health: ${label} was not a string list`);
  }
  return value;
}

const GITHUB_UNAVAILABLE_REASONS = new Set<GithubUnavailableReason>([
  "not_github_remote",
  "cli_missing",
  "product_disabled",
  "forbidden",
  "rate_limited",
  "transport",
  "unknown",
]);

function parseUnavailableReason(value: unknown): GithubUnavailableReason | undefined {
  if (value === undefined) return undefined;
  if (typeof value === "string" && GITHUB_UNAVAILABLE_REASONS.has(value as GithubUnavailableReason)) {
    return value as GithubUnavailableReason;
  }
  throw new Error("health: unavailable_reason was not a known reason");
}

function parseGithubSecurityEnvelope<TAlert>(
  value: unknown,
  kind: string,
): {
  available: boolean;
  cli_present: boolean;
  is_github_remote: boolean;
  slug: string;
  alerts: TAlert[];
  truncated: boolean;
  error?: string | null;
  unavailable_reason?: GithubUnavailableReason;
} {
  if (!isRecord(value)) {
    throw new Error(`${kind} returned no payload`);
  }
  if (typeof value.available !== "boolean") {
    throw new Error(`${kind} omitted available`);
  }
  if (value.available) {
    if (!Array.isArray(value.alerts)) {
      throw new Error(`${kind} claimed availability without alerts`);
    }
    if (typeof value.truncated !== "boolean") {
      throw new Error(`${kind} omitted truncated`);
    }
  } else if (value.alerts !== undefined && !Array.isArray(value.alerts)) {
    throw new Error(`${kind} returned a corrupt alerts list`);
  }
  const error = value.error;
  if (error !== undefined && error !== null && typeof error !== "string") {
    throw new Error(`${kind} returned a corrupt error`);
  }
  return {
    available: value.available,
    cli_present: value.cli_present === true,
    is_github_remote: value.is_github_remote === true,
    slug: typeof value.slug === "string" ? value.slug : "",
    alerts: Array.isArray(value.alerts) ? (value.alerts as TAlert[]) : [],
    truncated: value.truncated === true,
    error: typeof error === "string" ? error : error === null ? null : undefined,
    unavailable_reason: parseUnavailableReason(value.unavailable_reason),
  };
}

/** Fail-closed unwrap of `cmd_github_dependabot_alerts`. */
export function parseDependabotReport(value: unknown): DependabotReport {
  return parseGithubSecurityEnvelope<DependabotAlertInfo>(
    value,
    "cmd_github_dependabot_alerts",
  );
}

/** Fail-closed unwrap of `cmd_github_code_scanning_alerts`. */
export function parseCodeScanningReport(value: unknown): CodeScanningReport {
  return parseGithubSecurityEnvelope<CodeScanningAlertInfo>(
    value,
    "cmd_github_code_scanning_alerts",
  );
}

function parseNpmManifest(value: unknown): NpmManifest {
  if (!isRecord(value) || typeof value.path !== "string") {
    throw new Error("health: manifest was corrupt");
  }
  const scripts = value.lifecycle_scripts;
  if (scripts !== undefined && (!Array.isArray(scripts) || scripts.some((item) => typeof item !== "string"))) {
    throw new Error("health: lifecycle_scripts was not a string list");
  }
  const num = (key: string) => {
    const raw = value[key];
    return typeof raw === "number" && Number.isFinite(raw) ? raw : 0;
  };
  return {
    path: value.path,
    name: typeof value.name === "string" ? value.name : "",
    version: typeof value.version === "string" ? value.version : "",
    private: value.private === true,
    license: typeof value.license === "string" ? value.license : value.license === null ? null : undefined,
    engines_node:
      typeof value.engines_node === "string"
        ? value.engines_node
        : value.engines_node === null
          ? null
          : undefined,
    package_manager: typeof value.package_manager === "string" ? value.package_manager : "",
    lockfile: typeof value.lockfile === "string" ? value.lockfile : value.lockfile === null ? null : undefined,
    has_workspaces: value.has_workspaces === true,
    dep_count: num("dep_count"),
    dev_dep_count: num("dev_dep_count"),
    optional_dep_count: num("optional_dep_count"),
    peer_dep_count: num("peer_dep_count"),
    lifecycle_scripts: Array.isArray(scripts) ? scripts : [],
  };
}

function parseAudit(value: unknown): AuditSummary {
  if (!isRecord(value) || typeof value.total !== "number" || !Number.isFinite(value.total)) {
    throw new Error("health: audit.total was not a number");
  }
  const num = (key: string) => {
    const raw = value[key];
    return typeof raw === "number" && Number.isFinite(raw) ? raw : 0;
  };
  return {
    info: num("info"),
    low: num("low"),
    moderate: num("moderate"),
    high: num("high"),
    critical: num("critical"),
    unknown: typeof value.unknown === "number" && Number.isFinite(value.unknown) ? value.unknown : undefined,
    total: value.total,
  };
}

/**
 * Unwraps `cmd_scan_deps_health`. A missing `lifecycle_scripts` (or any other
 * array the Health view reads `.length` on) used to throw during render and
 * take the pane down; absent arrays become empty, present-but-wrong throws.
 */
export function parseDepsHealthReport(value: unknown): DepsHealthReport {
  if (!isRecord(value)) {
    throw new Error("cmd_scan_deps_health returned no payload");
  }
  if (typeof value.npm_cli_present !== "boolean" || typeof value.cargo_audit_present !== "boolean") {
    throw new Error("cmd_scan_deps_health omitted scanner presence");
  }
  if (!Array.isArray(value.manifests)) {
    throw new Error("cmd_scan_deps_health omitted manifests");
  }
  if (!Array.isArray(value.ecosystems) || !Array.isArray(value.issues) || !Array.isArray(value.vulnerabilities) || !Array.isArray(value.outdated)) {
    throw new Error("cmd_scan_deps_health omitted a findings list");
  }
  if (typeof value.truncated !== "boolean") {
    throw new Error("cmd_scan_deps_health omitted truncated");
  }
  const ecosystems: EcosystemHint[] = [];
  for (const item of value.ecosystems) {
    if (!isRecord(item) || typeof item.family !== "string" || typeof item.note !== "string") {
      throw new Error("health: ecosystem hint was corrupt");
    }
    const manifests = stringArray(item.manifests, "ecosystem manifests");
    ecosystems.push({ family: item.family, manifests, note: item.note });
  }
  const notices = value.limit_notices;
  if (notices !== undefined && !Array.isArray(notices)) {
    throw new Error("health: limit_notices was not a list");
  }
  return {
    node_version: typeof value.node_version === "string" ? value.node_version : value.node_version === null ? null : undefined,
    npm_version: typeof value.npm_version === "string" ? value.npm_version : value.npm_version === null ? null : undefined,
    npm_cli_present: value.npm_cli_present,
    cargo_audit_present: value.cargo_audit_present,
    pip_audit_present: typeof value.pip_audit_present === "boolean" ? value.pip_audit_present : undefined,
    govulncheck_present: typeof value.govulncheck_present === "boolean" ? value.govulncheck_present : undefined,
    composer_present: typeof value.composer_present === "boolean" ? value.composer_present : undefined,
    bundler_audit_present: typeof value.bundler_audit_present === "boolean" ? value.bundler_audit_present : undefined,
    scanners_ran: value.scanners_ran === undefined ? undefined : stringArray(value.scanners_ran, "scanners_ran"),
    audit_complete: typeof value.audit_complete === "boolean" ? value.audit_complete : undefined,
    manifests: value.manifests.map(parseNpmManifest),
    ecosystems,
    issues: value.issues as HealthIssue[],
    vulnerabilities: value.vulnerabilities as Vulnerability[],
    audit: parseAudit(value.audit),
    outdated: value.outdated as OutdatedPackage[],
    truncated: value.truncated,
    limit_notices: notices as ScanLimitNotice[] | undefined,
  };
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
