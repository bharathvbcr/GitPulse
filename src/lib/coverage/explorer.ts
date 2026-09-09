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

export function coverageScanStatus(report: CoverageReport | null, scanning: boolean, failed: boolean, excluded: boolean): string {
  if (scanning) return "Scanning…";
  if (failed) return report ? "Stale · scan failed" : "Scan failed";
  if (!report) return "Not scanned";
  if (report.overall.lines_found <= 0) return report.truncated ? "Unmeasured · partial scan" : "Unmeasured";
  if (report.truncated || report.go_modules_partial || report.limit_notices?.length || report.artifacts.some(a => a.skipped)
    || report.families.some(f => !f.found) || excluded) return "Partial coverage";
  return "Measured";
}
