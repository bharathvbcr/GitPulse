import {
  SETTINGS_SECTIONS,
  type SettingsSectionId,
} from "./settingsSections";

/**
 * Every individual setting on the page, so the filter box can find one.
 *
 * The rail catalog answers "which categories exist"; this answers "which
 * controls exist", which is the question a reader hunting for one switch is
 * actually asking. Grouping alone stopped being enough at roughly thirty
 * controls: knowing that word wrap lives under "Diff & code" requires already
 * knowing GitPulse's taxonomy.
 *
 * Each `id` is also stamped on the control's row as `data-setting`, and
 * SettingsModal.test.ts compares the two lists in both directions — a control
 * added without an entry is unfindable by search, and an entry with no control
 * is a filter result that hides everything. Neither can ship.
 */
export interface SettingEntry {
  readonly id: string;
  readonly section: SettingsSectionId;
  /** The control's own label, as the reader sees it. */
  readonly label: string;
  /**
   * Words a reader might search that the label does not contain — synonyms,
   * the vocabulary of other Git clients, and the thing the setting affects.
   */
  readonly keywords: string;
}

export const SETTINGS_CATALOG: readonly SettingEntry[] = [
  { id: "status-icon", section: "layout", label: "Menu bar status icon",
    keywords: "tray background close hide window repository conflicts menu bar" },
  { id: "global-cleaner", section: "hygiene", label: "Global build cleaner", keywords: "repository hygiene storage disk cache clean cleanup schedule scheduled retention exclusions roots cargo go build artifacts" },
  {
    id: "theme",
    section: "appearance",
    label: "Theme",
    keywords: "dark light system appearance colour color scheme night mode",
  },
  {
    id: "accent",
    section: "appearance",
    label: "Accent colour",
    keywords: "accent color highlight tint blue violet teal green amber rose brand",
  },
  {
    id: "ui-scale",
    section: "appearance",
    label: "UI font scale",
    keywords: "zoom size text bigger smaller scale font magnify",
  },
  {
    id: "motion",
    section: "appearance",
    label: "Reduce motion",
    keywords: "animation animations transition accessibility vestibular reduce motion",
  },
  {
    id: "timestamps",
    section: "appearance",
    label: "Timestamps",
    keywords: "date time relative absolute ago commit age when clock",
  },
  {
    id: "coach-marks",
    section: "appearance",
    label: "First-run coach marks",
    keywords: "tips hints onboarding tour tutorial reset",
  },
  {
    id: "status-bar",
    section: "layout",
    label: "Status bar",
    keywords: "bottom bar footer branch cadence shortcuts hide compact",
  },
  {
    id: "diagnostics-button",
    section: "layout",
    label: "Diagnostics button",
    keywords: "errors warnings header toolbar log troubleshoot",
  },
  {
    id: "header-labels",
    section: "layout",
    label: "Header button labels",
    keywords: "open clone title bar words icons text",
  },
  {
    id: "repo-tabs",
    section: "layout",
    label: "Hide repository tabs when alone",
    keywords: "tab strip repositories single hide chrome",
  },
  {
    id: "language-bar",
    section: "layout",
    label: "Language mix",
    keywords: "languages breakdown status bar percentage code",
  },
  {
    id: "harness-badges",
    section: "layout",
    label: "MANVI status badges",
    keywords: "manvi harness local model chips toolbar agents",
  },
  {
    id: "view-visibility",
    section: "views",
    label: "Views in the header",
    keywords: "tabs nav navigation hide show work history files health insights fleet",
  },
  {
    id: "branch-spacing",
    section: "graph",
    label: "Branch spacing",
    keywords: "density compact spacious lanes rows height",
  },
  {
    id: "graph-width",
    section: "graph",
    label: "Graph width",
    keywords: "balanced wide full viewport lanes columns size",
  },
  {
    id: "ref-scope",
    section: "graph",
    label: "Refs drawn",
    keywords: "branches remotes tags namespaces all named checkpoints prefetch",
  },
  {
    id: "graph-avatars",
    section: "graph",
    label: "Author avatars",
    keywords: "initials badges gutter author who committed",
  },
  {
    id: "diff-layout",
    section: "diff",
    label: "Default diff layout",
    keywords: "unified split side by side inline two column compare",
  },
  {
    id: "diff-wrap",
    section: "diff",
    label: "Wrap long lines by default",
    keywords: "word wrap soft wrap reflow long lines horizontal scroll",
  },
  {
    id: "diff-syntax",
    section: "diff",
    label: "Syntax highlighting by default",
    keywords: "colour color tokens language highlight code",
  },
  {
    id: "diff-whitespace",
    section: "diff",
    label: "Ignore whitespace by default",
    keywords: "whitespace indentation reindent blank spaces tabs noise",
  },
  {
    id: "tab-width",
    section: "diff",
    label: "Tab width",
    keywords: "tab size indent indentation columns spaces 2 4 8",
  },
  {
    id: "auto-coverage",
    section: "analysis",
    label: "Generate coverage automatically",
    keywords: "tests test suite coverage lcov cpu artifacts run",
  },
  {
    id: "mcp-plugin",
    section: "agents",
    label: "Agent plugin surface",
    keywords: "mcp codex plugin tools read-only agents claude protocol",
  },
  {
    id: "external-tools",
    section: "agents",
    label: "devmap and manvi",
    keywords: "devmap manvi install update cargo go harness code map cli binary toolchain",
  },
  {
    id: "update-check",
    section: "updates",
    label: "Check for new releases",
    keywords: "update updates version release github network automatic",
  },
  {
    id: "update-now",
    section: "updates",
    label: "Check now",
    keywords: "update manual check release version now",
  },
];

/** Lowercase words, empty for a blank query. */
function terms(query: string): string[] {
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter((term) => term.length > 0);
}

export interface SettingsMatch {
  /** True when no query is active, so callers can skip filtering entirely. */
  readonly all: boolean;
  /** Sections keeping at least one control (or matched by their own name). */
  readonly sections: readonly SettingsSectionId[];
  /** Control ids to keep; empty and `all: false` means "nothing matched". */
  readonly settings: readonly string[];
}

/**
 * Filters the catalog by every term in `query`.
 *
 * Terms are ANDed so a second word narrows rather than widens, which is what
 * typing more of a phrase is meant to do. A section whose own name matches
 * keeps all of its controls: searching "graph" should show the Graph panel
 * intact, not just the two rows with "graph" in their label.
 */
export function matchSettings(query: string): SettingsMatch {
  const words = terms(query);
  if (words.length === 0) {
    return {
      all: true,
      sections: SETTINGS_SECTIONS.map((entry) => entry.id),
      settings: SETTINGS_CATALOG.map((entry) => entry.id),
    };
  }

  const sectionText = new Map<SettingsSectionId, string>(
    SETTINGS_SECTIONS.map((entry) => [
      entry.id,
      `${entry.label} ${entry.summary}`.toLowerCase(),
    ]),
  );
  const wholeSections = new Set<SettingsSectionId>(
    SETTINGS_SECTIONS.filter((entry) =>
      words.every((word) => (sectionText.get(entry.id) ?? "").includes(word)),
    ).map((entry) => entry.id),
  );

  const settings = SETTINGS_CATALOG.filter((entry) => {
    if (wholeSections.has(entry.section)) return true;
    const haystack =
      `${entry.label} ${entry.keywords} ${sectionText.get(entry.section) ?? ""}`.toLowerCase();
    return words.every((word) => haystack.includes(word));
  });

  const sections = new Set<SettingsSectionId>(settings.map((entry) => entry.section));
  return {
    all: false,
    // Ordered by the rail, not by match order, so filtering never reshuffles
    // the categories under the reader's cursor.
    sections: SETTINGS_SECTIONS.filter((entry) => sections.has(entry.id)).map(
      (entry) => entry.id,
    ),
    settings: settings.map((entry) => entry.id),
  };
}
