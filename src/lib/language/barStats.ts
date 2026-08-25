export interface LanguageStat {
  language: string;
  color_hex: string;
  category?: string;
  code_lines?: number;
  file_count?: number;
  percentage: number;
  /** Names of languages folded into an aggregate entry (set on "Other"). */
  other_languages?: string[];
}

const MAX_SHOWN = 6;
const OTHER: LanguageStat = {
  language: "Other",
  color_hex: "#6b7280",
  category: "data",
  percentage: 0,
};

function isProgramming(stat: LanguageStat): boolean {
  return stat.category === "programming";
}

/**
 * Prefer programming languages so a small Rust crate cannot be sliced
 * off the bar by lockfiles / JSON / Markdown. Remainder folds into Other.
 */
export function pickLanguageBarStats(
  stats: LanguageStat[],
  maxShown = MAX_SHOWN,
): LanguageStat[] {
  if (stats.length === 0) return [];
  const programming = stats.filter(isProgramming);
  const rest = stats.filter((s) => !isProgramming(s));
  const shown: LanguageStat[] = [];
  shown.push(...programming.slice(0, maxShown));
  if (shown.length < maxShown) {
    shown.push(...rest.slice(0, maxShown - shown.length));
  }
  const used = new Set(shown.map((s) => s.language));
  const leftover = stats.filter((s) => !used.has(s.language));
  if (leftover.length === 0) return shown;
  const otherPct = leftover.reduce((sum, s) => sum + s.percentage, 0);
  const otherLines = leftover.reduce((sum, s) => sum + (s.code_lines ?? 0), 0);
  const otherFiles = leftover.reduce((sum, s) => sum + (s.file_count ?? 0), 0);
  shown.push({
    ...OTHER,
    percentage: Math.round(otherPct * 10) / 10,
    code_lines: otherLines,
    file_count: otherFiles,
    other_languages: leftover.map((s) => s.language),
  });
  return shown;
}
