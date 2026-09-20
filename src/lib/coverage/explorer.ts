import type { CoverageReport, FileCoverageSummary } from "./types";

export type CoverageFilter = "all" | "missed" | "below80";
export type CoverageSort = "path" | "missed" | "coverage";
export interface MissedBlock { start: number; end: number }

export function filterCoverageFiles(files: readonly FileCoverageSummary[], query: string, language: string, filter: CoverageFilter, sort: CoverageSort): FileCoverageSummary[] {
  const needle = query.trim().toLowerCase();
  return files.filter(file => file.path.toLowerCase().includes(needle) && (!language || file.language === language)
    && (filter === "all" || (file.lines_found > 0 && (filter === "missed" ? file.lines_hit < file.lines_found : file.percentage < 80))))
    .sort((a, b) => {
      if (sort !== "path") {
        const unmeasured = Number(a.lines_found <= 0) - Number(b.lines_found <= 0);
        if (unmeasured) return unmeasured;
        const delta = sort === "missed" ? (b.lines_found - b.lines_hit) - (a.lines_found - a.lines_hit) : a.percentage - b.percentage;
        if (delta) return delta;
      }
      return a.path.localeCompare(b.path);
    });
}

/** Unknown/uninstrumented lines never become misses; navigation stays inside loaded source. */
export function missedLineBlocks(hits: ReadonlyMap<number, number>, sourceLength: number): MissedBlock[] {
  const lines = [...hits].filter(([line, count]) => count === 0 && Number.isInteger(line) && line > 0 && line <= sourceLength)
    .map(([line]) => line).sort((a, b) => a - b);
  const blocks: MissedBlock[] = [];
  for (const line of lines) {
    const last = blocks.at(-1);
    if (last && line === last.end + 1) last.end = line;
    else blocks.push({ start: line, end: line });
  }
  return blocks;
}

export function moveMissedBlock(blocks: readonly MissedBlock[], current: number | null, direction: 1 | -1): number | null {
  if (!blocks.length) return null;
  const index = blocks.findIndex(block => block.start === current);
  if (index < 0) return (direction === 1 ? blocks[0] : blocks[blocks.length - 1]).start;
  return blocks[(index + direction + blocks.length) % blocks.length].start;
}

/**
 * Every reason this scan covers less than the repository, in the reader's words.
 *
 * "Partial coverage" on its own names none of the six conditions that produce
 * it, so a header showing it could not answer the only question it raises. The
 * status below is now derived from this list rather than from a second copy of
 * the same predicates, which also fixes an over-report: a limit notice that
 * dropped nothing (`kept === total`) used to count as partial while the chip it
 * fed printed no detail, so the word had no recoverable cause at all.
 */
export function coverageScanReasons(report: CoverageReport | null, excluded: boolean): string[] {
  if (!report) return [];
  const reasons: string[] = [];
  if (report.truncated) reasons.push("the scan did not read every artifact");
  for (const notice of report.limit_notices ?? []) {
    if (notice && notice.total > notice.kept) reasons.push(`only ${notice.kept} of ${notice.total} ${notice.resource} were kept`);
  }
  if (report.go_modules_partial) reasons.push("the Go module search was cut short");
  const skipped = report.artifacts.filter(a => a.skipped).length;
  if (skipped > 0) reasons.push(`${skipped} coverage ${skipped === 1 ? "artifact was" : "artifacts were"} skipped`);
  const missing = report.families.filter(f => !f.found);
  if (missing.length > 0) reasons.push(`no report found for ${missing.map(f => f.family).join(", ")}`);
  if (excluded) reasons.push("a command only passed with files excluded from measurement");
  return reasons;
}

export function coverageScanStatus(report: CoverageReport | null, scanning: boolean, failed: boolean, excluded: boolean): string {
  if (scanning) return "Scanning…";
  if (failed) return report ? "Stale · scan failed" : "Scan failed";
  if (!report) return "Not scanned";
  if (report.overall.lines_found <= 0) return report.truncated ? "Unmeasured · partial scan" : "Unmeasured";
  return coverageScanReasons(report, excluded).length > 0 ? "Partial coverage" : "Measured";
}
