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

function finiteOrZero(value: number | undefined): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
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
  const merged = new Map<string, LanguageStat>();
  for (const s of stats) {
    const prev = merged.get(s.language);
    if (!prev) {
      merged.set(s.language, { ...s, percentage: finiteOrZero(s.percentage) });
      continue;
    }
    prev.percentage = finiteOrZero(prev.percentage) + finiteOrZero(s.percentage);
    prev.code_lines = (prev.code_lines ?? 0) + finiteOrZero(s.code_lines);
    prev.file_count = (prev.file_count ?? 0) + finiteOrZero(s.file_count);
  }
  const cap = Number.isFinite(maxShown) ? Math.max(0, Math.floor(maxShown)) : 0;
  const deduped = [...merged.values()];
  const programming = deduped.filter(isProgramming);
  const rest = deduped.filter((s) => !isProgramming(s));
  const shown: LanguageStat[] = [];
  shown.push(...programming.slice(0, cap));
  if (shown.length < cap) {
    shown.push(...rest.slice(0, cap - shown.length));
  }
  const used = new Set(shown.map((s) => s.language));
  const leftover = deduped.filter((s) => !used.has(s.language));
  if (leftover.length === 0) return shown;
  const otherPct = leftover.reduce((sum, s) => sum + finiteOrZero(s.percentage), 0);
  const otherLines = leftover.reduce((sum, s) => sum + finiteOrZero(s.code_lines), 0);
  const otherFiles = leftover.reduce((sum, s) => sum + finiteOrZero(s.file_count), 0);
  shown.push({
    ...OTHER,
    percentage: Math.round(otherPct * 10) / 10,
    code_lines: otherLines,
    file_count: otherFiles,
    other_languages: leftover.map((s) => s.language),
  });
  return shown;
}
