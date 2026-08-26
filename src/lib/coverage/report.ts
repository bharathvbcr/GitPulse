import { formatCoveragePercent } from "./format";
import type {
  CoverageArtifact,
  CoverageFamilyStatus,
  CoverageLanguageSplit,
  CoverageReport,
  FileCoverageSummary,
} from "./types";

const MAX_FILES_LISTED = 30;
const MAX_ARTIFACTS_LISTED = 40;

/**
 * Coverage numbers cross the IPC boundary as plain JSON, so a buggy or
 * hostile producer can hand over NaN, Infinity, negatives or missing fields.
 * Everything is re-validated here: the renderer must hand the model (and the
 * clipboard) a well-formed document and must never throw on any input shape.
 */
function safePercent(value: unknown): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return "0.0%";
  return formatCoveragePercent(value);
}

function safeCount(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.max(0, Math.trunc(value));
}

function safeText(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function safeList<T>(value: T[] | undefined): T[] {
  return Array.isArray(value) ? value : [];
}

/**
 * Deterministic plain-text rendering of a coverage report, used both as the
 * payload for `cmd_ai_coverage_report` and for copy-to-clipboard. Section
 * order is stable so prompts and tests stay comparable across runs; capped
 * lists carry their counters so "shorter list" never reads as "everything".
 */
export function formatCoverageReport(report: CoverageReport, repoPath: string): string {
  const data = (report ?? {}) as Partial<CoverageReport>;
  const languageRows = safeList(data.languages);
  const fileRows = safeList(data.files);
  const artifactRows = safeList(data.artifacts);
  const familyRows = safeList(data.families);

  const out: string[] = [];
  out.push(`Coverage report — ${safeText(repoPath)}`);

  let overall =
    `${safePercent(data.overall?.percentage)} ` +
    `(${safeCount(data.overall?.lines_hit)}/${safeCount(data.overall?.lines_found)} lines)`;
  // A capped scan must say so in the same breath as its totals.
  if (data.truncated === true) overall += " [SCAN TRUNCATED — results partial]";
  out.push("", "OVERALL", overall);

  if (languageRows.length > 0) {
    out.push("", "PER-LANGUAGE");
    for (const lang of languageRows as CoverageLanguageSplit[]) {
      if (!lang || typeof lang !== "object") continue;
      out.push(
        `${safeText(lang.language)}: ${safePercent(lang.percentage)} ` +
          `(${safeCount(lang.lines_hit)}/${safeCount(lang.lines_found)} lines, ${safeCount(lang.files)} files)`,
      );
    }
  }

  // report.files already arrives sorted worst-first; slicing preserves that.
  if (fileRows.length > 0) {
    out.push(
      "",
      `LOWEST-COVERED FILES (worst first, showing ${Math.min(fileRows.length, MAX_FILES_LISTED)} of ${fileRows.length})`,
    );
    for (const file of fileRows.slice(0, MAX_FILES_LISTED) as FileCoverageSummary[]) {
      if (!file || typeof file !== "object") continue;
      out.push(
        `- ${safeText(file.path)}: ${safePercent(file.percentage)} ` +
          `(${safeCount(file.lines_hit)}/${safeCount(file.lines_found)} lines)`,
      );
    }
  }

  if (artifactRows.length > 0) {
    out.push(
      "",
      `ARTIFACTS (showing ${Math.min(artifactRows.length, MAX_ARTIFACTS_LISTED)} of ${artifactRows.length})`,
    );
    for (const artifact of artifactRows.slice(0, MAX_ARTIFACTS_LISTED) as CoverageArtifact[]) {
      if (!artifact || typeof artifact !== "object") continue;
      let line = `- ${safeText(artifact.path)} (${safeText(artifact.format)})`;
      if (artifact.skipped === true) {
        const reason = safeText(artifact.skip_reason);
        line += reason ? ` — skipped: ${reason}` : " — skipped";
      }
      out.push(line);
    }
    if (artifactRows.length > MAX_ARTIFACTS_LISTED) {
      out.push(`…and ${artifactRows.length - MAX_ARTIFACTS_LISTED} more`);
    }
  }

  // Naming what was looked for but not found steers the model toward fixing
  // the missing data instead of inventing numbers for it.
  const missingFamilies = (
    familyRows as CoverageFamilyStatus[]
  ).filter((family) => !!family && typeof family === "object" && family.found !== true);
  if (missingFamilies.length > 0) {
    out.push("", "FAMILIES WITHOUT REPORT");
    for (const family of missingFamilies) {
      const formats = Array.isArray(family.expected_formats)
        ? family.expected_formats.map(safeText).filter(Boolean).join(", ")
        : "";
      out.push(`- ${safeText(family.family)}${formats ? ` (expected: ${formats})` : ""}`);
    }
  }

  return out.join("\n");
}
