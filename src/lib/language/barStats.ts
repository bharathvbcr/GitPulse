/**
 * Exactly what the backend sends for one language — every field present.
 *
 * Distinct from `LanguageStat` on purpose: that one is the bar's view model,
 * and typing the wire payload as the view model told TypeScript that counts
 * Rust always sends might be missing, and that the wire might carry an
 * `other_languages` list it never does.
 */
export interface RepoLanguageStat {
  language: string;
  color_hex: string;
  category: string;
  code_lines: number;
  file_count: number;
  percentage: number;
}

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

/**
 * The backend's language scan result. Declared in LanguageBar until
 * `check:types` could reach it; `truncated` is the field that must never be
 * dropped, since it is what stops a partial scan reading as a whole one.
 */
export interface LanguageStatsReport {
  stats: RepoLanguageStat[];
  /** True when the backend scan stopped early (deadline or cap). */
  truncated: boolean;
  scanned_files: number;
  candidate_files: number;
}

export type CodePercentageMode = "code-only" | "all";

const NOTE_MARKDOWN_LANGUAGES = new Set([
  "markdown",
  "text",
  "plain text",
  "restructuredtext",
  "asciidoc",
  "notes",
  "note",
  "documentation",
]);

export function isMarkdownOrNote(stat: RepoLanguageStat | LanguageStat): boolean {
  if (stat.category === "prose") return true;
  const lower = stat.language.trim().toLowerCase();
  if (NOTE_MARKDOWN_LANGUAGES.has(lower)) return true;
  return lower.endsWith(".md") || lower.endsWith(".txt") || lower.startsWith("note");
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

/** Highest percentage first; equal percentages break ties by language name. */
function compareByPercentageThenName(a: LanguageStat, b: LanguageStat): number {
  const pa = finiteOrZero(a.percentage);
  const pb = finiteOrZero(b.percentage);
  if (pa !== pb) return pb - pa;
  if (a.language < b.language) return -1;
  if (a.language > b.language) return 1;
  return 0;
}

/**
 * Prefer programming languages so a small Rust crate cannot be sliced
 * off the bar by lockfiles / JSON / Markdown. Remainder folds into Other.
 * The bar itself is then ordered by percentage, with Other last.
 *
 * In "code-only" mode, markdowns and notes are excluded, and percentages
 * are recalculated so the code files sum to 100%.
 */
export function pickLanguageBarStats(
  stats: RepoLanguageStat[],
  maxShown = MAX_SHOWN,
  mode: CodePercentageMode = "all",
): LanguageStat[] {
  if (stats.length === 0) return [];
  const candidates =
    mode === "code-only" ? stats.filter((s) => !isMarkdownOrNote(s)) : stats;
  if (candidates.length === 0) return [];

  const merged = new Map<string, LanguageStat>();
  for (const s of candidates) {
    const prev = merged.get(s.language);
    if (!prev) {
      merged.set(s.language, { ...s, percentage: finiteOrZero(s.percentage) });
      continue;
    }
    prev.percentage = finiteOrZero(prev.percentage) + finiteOrZero(s.percentage);
    prev.code_lines = (prev.code_lines ?? 0) + finiteOrZero(s.code_lines);
    prev.file_count = (prev.file_count ?? 0) + finiteOrZero(s.file_count);
  }

  const deduped = [...merged.values()];

  if (mode === "code-only") {
    const totalLines = deduped.reduce(
      (sum, s) => sum + finiteOrZero(s.code_lines),
      0,
    );
    if (totalLines > 0) {
      for (const s of deduped) {
        const lines = finiteOrZero(s.code_lines);
        s.percentage = (lines / totalLines) * 100;
      }
    } else {
      const rawSum = deduped.reduce((sum, s) => sum + finiteOrZero(s.percentage), 0);
      for (const s of deduped) {
        s.percentage =
          rawSum > 0
            ? (finiteOrZero(s.percentage) / rawSum) * 100
            : 0;
      }
    }
  }

  const cap = Number.isFinite(maxShown) ? Math.max(0, Math.floor(maxShown)) : 0;
  const programming = deduped.filter(isProgramming).sort(compareByPercentageThenName);
  const rest = deduped.filter((s) => !isProgramming(s)).sort(compareByPercentageThenName);
  const shown: LanguageStat[] = [];
  shown.push(...programming.slice(0, cap));
  if (shown.length < cap) {
    shown.push(...rest.slice(0, cap - shown.length));
  }
  const used = new Set(shown.map((s) => s.language));
  const leftover = deduped.filter((s) => !used.has(s.language));
  shown.sort(compareByPercentageThenName);

  if (mode === "code-only") {
    for (const s of shown) {
      s.percentage = Math.round(s.percentage * 10) / 10;
    }
  }

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

/** What a language reading is worth, once the caveats are applied. */
export interface LanguageMix {
  /** Languages to draw, already deduped and capped. Empty when unmeasured. */
  stats: LanguageStat[];
  /** Highest-percentage language among `stats`, ignoring the "Other" aggregate. */
  dominant: LanguageStat | null;
  /**
   * True when the percentages are a floor rather than a total — the scan was
   * capped, or the repository moved since it ran. Both halves matter: a
   * capped scan rendered as a complete one is the exact failure the backend's
   * `truncated` flag exists to prevent, and a stale one is the same lie with
   * a later timestamp.
   */
  partial: boolean;
  /** Why the reading is partial, in the user's words; null when it is not. */
  partialNotice: string | null;
  /** True when the measurement failed and nothing survives to show. */
  failed: boolean;
  /** Whether the percentage calculation only considers code files or includes markdowns/notes. */
  mode: CodePercentageMode;
  /** Informative note when notes/markdown are included in the calculation. */
  note: string | null;
  /** Whether markdowns or notes exist in the repository scan. */
  hasNotesOrMarkdown: boolean;
}

/** The shape `describeLanguageMix` needs from a metric snapshot. */
export interface LanguageMixInput {
  value: LanguageStatsReport | null;
  state: "idle" | "loading" | "ready" | "failed";
  /** Non-null when the value no longer describes the repository. */
  stale: string | null;
  mode?: CodePercentageMode;
}

const EMPTY_MIX: LanguageMix = {
  stats: [],
  dominant: null,
  partial: false,
  partialNotice: null,
  failed: false,
  mode: "code-only",
  note: null,
  hasNotesOrMarkdown: false,
};

/**
 * Reduces a LOC metric snapshot to what the language segment draws.
 *
 * Kept out of the component so the honesty rule — that a capped or stale scan
 * never renders as a complete reading — is a testable function rather than a
 * condition buried in markup.
 */
export function describeLanguageMix(
  snapshot: LanguageMixInput,
  mode: CodePercentageMode = "code-only",
): LanguageMix {
  const effectiveMode = snapshot.mode ?? mode;
  const failed = snapshot.state === "failed" && snapshot.value === null;
  if (!snapshot.value || !Array.isArray(snapshot.value.stats)) {
    return { ...EMPTY_MIX, failed, mode: effectiveMode };
  }
  const hasNotesOrMarkdown = snapshot.value.stats.some(isMarkdownOrNote);
  const stats = pickLanguageBarStats(snapshot.value.stats, MAX_SHOWN, effectiveMode);
  const truncated = snapshot.value.truncated === true;
  const stale = snapshot.stale !== null;
  const note =
    effectiveMode === "all" && hasNotesOrMarkdown
      ? "Note: Includes Markdown and notes in percentage calculation."
      : null;

  return {
    stats,
    dominant: stats.find((s) => s.language !== "Other") ?? stats[0] ?? null,
    partial: truncated || stale,
    partialNotice: truncated
      ? `Partial scan: ${snapshot.value.scanned_files} of ${snapshot.value.candidate_files} files counted`
      : stale
        ? "The repository changed since this scan; percentages are a floor."
        : null,
    failed,
    mode: effectiveMode,
    note,
    hasNotesOrMarkdown,
  };
}
