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

export interface CoverageReport {
  families: CoverageFamilyStatus[];
  languages: CoverageLanguageSplit[];
  artifacts: CoverageArtifact[];
  files: FileCoverageSummary[];
  overall: CoverageTotals;
  truncated: boolean;
}
