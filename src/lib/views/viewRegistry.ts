import type { ViewTab } from "../repos/persist";

/**
 * One lens within a view.
 *
 * Sections exist because most of the old top-level tabs were not destinations
 * but different renderings of one subject — a commit, a file, the repository.
 * Splitting them across the header meant the app had to teleport the user
 * mid-thought (Graph to Diff, Files to Blame) and each landing lost the
 * context the last one had. A section switches the lens and keeps the
 * subject.
 */
export interface ViewSection {
  /** Stable id; persisted, and the tail of the native menu id. */
  readonly id: string;
  /** Display name in the view's own segmented control. */
  readonly label: string;
  /**
   * Command-palette label. Every section a retired view became needs one, or
   * the retirement takes away the only door that view had.
   */
  readonly paletteCommand?: string;
}

export interface ViewRegistration {
  readonly id: ViewTab;
  /** Display name in the header tabs/menus and the default palette phrasing. */
  readonly label: string;
  /**
   * Command-palette label for views that are reachable as commands. Omitted
   * means the view gets no palette command (matching historical behavior for
   * the always-visible work views).
   */
  readonly paletteCommand?: string;
  /**
   * The lenses this view offers, in display order. The first is the default.
   * A view without sections renders one pane and shows no segmented control.
   */
  readonly sections?: readonly ViewSection[];
}

/**
 * Single catalog of application views.
 *
 * Adding a view means adding its `ViewTab` member + `VIEW_TABS` entry in
 * repos/persist.ts AND one entry here — TypeScript rejects anything less
 * (`Record<ViewTab, …>`), and every consumer (header nav, native menu,
 * command palette) derives from this record, so no other file needs editing.
 *
 * Declaration order is the header order, so keep the record ordered the way
 * the header should read.
 */
export const VIEW_REGISTRY: Readonly<Record<ViewTab, ViewRegistration>> = {
  work: {
    id: "work",
    label: "Work",
    paletteCommand: "Open Work — tasks, worktrees, PRs, runs and verdicts",
    // Everything in flight, and the surfaces that act on it. GitHub and MANVI
    // were separate views rendering halves of the same answer Work already
    // joins — both issued `cmd_github_context`, four `gh` round trips each,
    // to draw overlapping lists. Resolve was never a destination: it is what
    // a blocked row opens into, and Work already sorts blocked rows first.
    sections: [
      { id: "overview", label: "Overview" },
      { id: "resolve", label: "Resolve", paletteCommand: "Open Resolve — finish a parked merge or rebase" },
      { id: "remote", label: "Remote", paletteCommand: "Open GitHub — pull requests, issues, runs and releases" },
      { id: "stack", label: "Stack", paletteCommand: "Open Stack — branch chains and restacking" },
      { id: "policy", label: "Policy", paletteCommand: "Open MANVI — gates, verdicts and branch cleanup" },
    ],
  },
  code: {
    id: "code",
    label: "Code",
    paletteCommand: "Open Code — the file explorer, editor and blame",
    // Two readings of one file. Both sections key off `selectedFilePath`, so
    // switching lens keeps the file: the Blame button in the editor header
    // stopped being a jump to another destination and became what it always
    // meant. Blame had grown a duplicate explorer rail and a path box purely
    // because it was reachable with nothing selected; Explorer is one click
    // away now, so the second picker is gone.
    sections: [
      { id: "explorer", label: "Explorer" },
      { id: "blame", label: "Blame", paletteCommand: "Open Blame — line authorship and code age" },
    ],
  },
  history: {
    id: "history",
    label: "History",
    paletteCommand: "Open History — the commit graph, diffs and the reflog",
    // Three renderings of one subject: what happened to this repository.
    // They were three tabs, and the split cost more than it saved — the Diff
    // view had to grow its own commit picker and file rail purely so the
    // user would not have to go back to Graph for the commit they had just
    // been looking at. Sharing `selectedCommitId`, the sections keep it.
    sections: [
      { id: "graph", label: "Graph" },
      { id: "diff", label: "Diff", paletteCommand: "Open Diff — changes in the selected commit" },
      { id: "reflog", label: "Reflog", paletteCommand: "Open Reflog — HEAD movements and recovery points" },
    ],
  },
  insights: {
    id: "insights",
    label: "Insights",
    paletteCommand: "Open Insights — activity, dependencies, coverage and disk",
    // Four scans of one subject: this repository. They were four header
    // entries, each empty until someone ran it — over half the Inspect menu
    // costing attention every session and paying occasionally. As sections
    // they share one scan-card shell and one honesty contract about
    // truncation, which is the thing all four actually had in common.
    sections: [
      { id: "pulse", label: "Pulse", paletteCommand: "Open Pulse — repository rhythm, churn and metrics" },
      { id: "coverage", label: "Coverage", paletteCommand: "Open Coverage" },
      { id: "health", label: "Health", paletteCommand: "Scan npm vulnerabilities and updates" },
      { id: "storage", label: "Storage", paletteCommand: "Scan repository disk usage" },
    ],
  },
};

/** Registry entries in declaration (= header) order. */
export const REGISTERED_VIEWS: readonly ViewRegistration[] = Object.values(VIEW_REGISTRY);

/** The sections a view offers, empty for a view that renders one pane. */
export function sectionsFor(id: ViewTab): readonly ViewSection[] {
  return VIEW_REGISTRY[id].sections ?? [];
}

/**
 * The section a view opens on when nothing else is remembered: its first.
 * Null for a view with no sections, which is not the same as "the first of
 * none" — callers branch on it to decide whether to draw a control at all.
 */
export function defaultSectionFor(id: ViewTab): string | null {
  return VIEW_REGISTRY[id].sections?.[0]?.id ?? null;
}

/**
 * The section a view is currently showing, given a session's section map.
 * Falls through `resolveSection`, so an unset or unknown entry reads as the
 * view's default rather than as "no section".
 */
export function activeSectionFor(
  id: ViewTab,
  sections: Readonly<Record<string, string>>,
): string | null {
  return resolveSection(id, sections[id]);
}

/** True when `section` of `tab` is the pane actually on screen. */
export function isSectionOnScreen(
  activeTab: ViewTab,
  sections: Readonly<Record<string, string>>,
  tab: ViewTab,
  section: string,
): boolean {
  return activeTab === tab && activeSectionFor(tab, sections) === section;
}

/**
 * Narrows a remembered section to one this build still offers.
 *
 * Persisted section ids are user data and outlive the build that wrote them:
 * a section renamed or removed since must fall back to the default rather
 * than leaving a view with no pane selected.
 */
export function resolveSection(id: ViewTab, candidate: unknown): string | null {
  const sections = sectionsFor(id);
  if (sections.length === 0) return null;
  if (typeof candidate === "string" && sections.some((s) => s.id === candidate)) {
    return candidate;
  }
  return sections[0].id;
}

/** Native menu ids are `tab-<id>`; derived so a new view cannot be missed. */
export const NATIVE_TAB_MENU_PREFIX = "tab-";

export function nativeTabMenuId(id: ViewTab): string {
  return `${NATIVE_TAB_MENU_PREFIX}${id}`;
}

export function viewTabForMenuId(menuId: string): ViewTab | undefined {
  if (!menuId.startsWith(NATIVE_TAB_MENU_PREFIX)) return undefined;
  const candidate = menuId.slice(NATIVE_TAB_MENU_PREFIX.length);
  return Object.hasOwn(VIEW_REGISTRY, candidate) ? (candidate as ViewTab) : undefined;
}
