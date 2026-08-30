export interface CoverageTotals {
  lines_found: number;
  lines_hit: number;
  percentage: number;
}

export interface CoverageFamilyStatus {
  family: string;
  languages: string[];
  color_hex: string;
  expected_formats: string[];
  expected_paths: string[];
  found: boolean;
  suggested_commands: string[];
  setup_commands: string[];
  tool_ready: boolean;
  tool_detail: string;
  duration_hint: string;
}

export interface CoverageArtifact {
  path: string;
  format: string;
  family: string;
  skipped: boolean;
  skip_reason?: string | null;
  totals: CoverageTotals;
}

export interface FileCoverageSummary {
  path: string;
  language: string;
  color_hex: string;
  lines_found: number;
  lines_hit: number;
  percentage: number;
}

export interface CoveredLine {
  line_no: number;
  hits: number;
}

export interface FileCoverage {
  path: string;
  language: string;
  color_hex: string;
  lines: CoveredLine[];
  totals: CoverageTotals;
  truncated: boolean;
  lines_truncated: boolean;
}

export interface CoverageLanguageSplit {
  language: string;
  color_hex: string;
  files: number;
  lines_found: number;
  lines_hit: number;
  percentage: number;
}

/**
 * Exact retained/observed counts for one scan cap that fired. Mirrors the Rust
 * `CoverageScanLimit`. `truncated` says only that something was cut; these say
 * how much, so a bounded section can headline what was seen rather than what
 * survived.
 */
export interface CoverageScanLimit {
  resource: string;
  kept: number;
  total: number;
}

export interface CoverageReport {
  families: CoverageFamilyStatus[];
  languages: CoverageLanguageSplit[];
  artifacts: CoverageArtifact[];
  files: FileCoverageSummary[];
  overall: CoverageTotals;
  truncated: boolean;
  /**
   * Optional on the wire: `#[serde(default)]` in Rust, and reports cached by
   * an older build carry no notices at all.
   */
  limit_notices?: CoverageScanLimit[];
  /**
   * Go module directories found without the git listing, published only for
   * repositories whose plan contains a root-level `go test ./...` — the
   * command that can fail for want of a module. Empty everywhere else.
   */
  go_modules?: string[];
  /** Whether a bound cut the module search short; a partial list means partial coverage. */
  go_modules_partial?: boolean;
}
