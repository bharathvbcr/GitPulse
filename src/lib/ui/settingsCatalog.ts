import {
  SETTINGS_SECTIONS,
  type SettingsSectionId,
} from "./settingsSections";

/**
 * A host capability a setting cannot work without.
 *
 * Named rather than boolean so the reason a control is absent is legible at the
 * definition, and so the check is made once here instead of as a platform
 * conditional scattered through the modal's markup.
 */
export type SettingRequirement = "dockHiding" | "appleIntelligence";

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
  /**
   * Host capability this setting needs. Absent means every host has it.
   *
   * A setting whose requirement is unmet is hidden rather than disabled: a
   * greyed switch invites a reader to hunt for what would enable it, and on a
   * platform where the implementing code does not exist, nothing will.
   */
  readonly requires?: SettingRequirement;
}

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
export const SETTINGS_CATALOG: readonly SettingEntry[] = [
  { id: "status-icon", section: "layout", label: "Menu bar status icon",
    keywords: "tray background close hide window repository conflicts menu bar stash pulse fleet terminal dock launch login autostart counts" },
  // macOS-only: the activation-policy path behind it is compiled only there,
  // and Windows and Linux have no Dock to hide.
  { id: "hide-dock", section: "layout", label: "Hide Dock icon while closed",
    keywords: "dock accessory menu bar hide closed window", requires: "dockHiding" },
  { id: "status-icon-counts", section: "layout", label: "Show counts beside the icon",
    keywords: "tray title counts conflicts changed menu bar" },
  { id: "launch-at-login", section: "layout", label: "Launch at login",
    keywords: "autostart launch agent login item startup background" },
  { id: "hygiene-defaults", section: "hygiene", label: "Hygiene defaults", keywords: "repository hygiene default retention days inherit override per repository shared cache review weekly all repositories scope storage preview" },
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
    id: "terminal-screen-reader",
    section: "appearance",
    label: "Terminal screen reader support",
    keywords: "terminal screen reader voiceover accessibility a11y announce assistive shell console",
  },
  {
    id: "timestamps",
    section: "appearance",
    label: "Timestamps",
    keywords: "date time relative absolute ago commit age when clock",
  },
  {
    id: "branch-rows",
    section: "appearance",
    label: "Sidebar branch rows",
    keywords:
      "branch list sidebar two line rows truncated truncate ellipsis name width churn lines changed author age density compact",
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
    id: "walkthrough-button",
    section: "layout",
    label: "Walkthrough button",
    keywords: "tour guided replay onboarding walkthrough header title bar hide show",
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
    id: "auto-github-alerts",
    section: "analysis",
    label: "Check GitHub alerts on launch",
    keywords: "dependabot code scanning codeql github security gh alerts launch automatic",
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
    keywords: "mcp codex plugin tools read-only agents claude grok antigravity agy protocol",
  },
  // macOS-only: on every other host the framework cannot exist, so the row
  // would be a permanent "unavailable" with nothing the reader could do.
  {
    id: "apple-intelligence",
    section: "agents",
    label: "Apple Intelligence",
    keywords: "on-device foundation models local drafting private offline mac apple intelligence task enhance",
    requires: "appleIntelligence",
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

/**
 * Ids the host cannot support, given which capabilities it has.
 *
 * Passed to `matchSettings` so search agrees with the page: a reader on Windows
 * typing "dock" must not be shown a section that then renders nothing.
 */
export function unsupportedSettingIds(
  capabilities: Readonly<Record<SettingRequirement, boolean>>,
): ReadonlySet<string> {
  return new Set(
    SETTINGS_CATALOG.filter(
      (entry) => entry.requires !== undefined && !capabilities[entry.requires],
    ).map((entry) => entry.id),
  );
}

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
export function matchSettings(
  query: string,
  unsupported: ReadonlySet<string> = new Set(),
): SettingsMatch {
  const supported = SETTINGS_CATALOG.filter((entry) => !unsupported.has(entry.id));
  const words = terms(query);
  if (words.length === 0) {
    return {
      all: true,
      sections: SETTINGS_SECTIONS.map((entry) => entry.id),
      settings: supported.map((entry) => entry.id),
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

  const settings = supported.filter((entry) => {
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
