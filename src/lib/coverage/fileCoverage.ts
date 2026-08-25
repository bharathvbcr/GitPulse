import { invoke } from "@tauri-apps/api/core";
import type { CoveredLine, FileCoverage } from "./types";

export function buildHitMap(lines: CoveredLine[]): Map<number, number> {
  const hits = new Map<number, number>();
  for (const line of lines) {
    hits.set(line.line_no, line.hits);
  }
  return hits;
}

export function fetchFileCoverage(repoPath: string, filePath: string): Promise<FileCoverage> {
  return invoke<FileCoverage>("cmd_get_file_coverage", { repoPath, filePath });
}

export function hitBadgeClass(hits: number | undefined): string {
  const tone =
    hits === undefined ? "text-transparent" : hits > 0 ? "text-emerald-400/80" : "text-red-400/80";
  return `w-8 px-1 text-right text-[10px] tabular-nums shrink-0 ${tone}`;
}
