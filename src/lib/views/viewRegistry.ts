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
   * Glanceable sentence for the section tab's tooltip. Required: a new
   * section that ships without one is a labelled button that still does not
   * say what the pane is.
   */
  readonly summary: string;
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
   * What this view is for, in the header tab's tooltip. Required for the
   * same reason section summaries are: a new view cannot ship as a name
   * that only repeats itself.
   */
  readonly summary: string;
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
    summary:
      "Everything in flight: worktrees, pull requests, CI runs and MANVI verdicts. Blocked items sort first.",
    paletteCommand: "Open Work — tasks, worktrees, PRs, runs and verdicts",
    // Everything in flight, and the surfaces that act on it. GitHub and MANVI
    // were separate views rendering halves of the same answer Work already
    // joins — both issued `cmd_github_context`, four `gh` round trips each,
    // to draw overlapping lists. Resolve was never a destination: it is what
    // a blocked row opens into, and Work already sorts blocked rows first.
    sections: [
      {
        id: "overview",
        label: "Overview",
        summary:
          "The joined list of this repository's worktrees, pull requests and runs. A blocked row is the one to open.",
      },
      {
        id: "resolve",
        label: "Resolve",
        summary:
          "Finish a parked merge or rebase. Conflicted files, chunk by chunk, without leaving Work.",
        paletteCommand: "Open Resolve — finish a parked merge or rebase",
      },
      {
        id: "remote",
        label: "Remote",
        summary:
          "Pull requests, issues, Actions runs and releases from GitHub for this repository.",
        paletteCommand: "Open GitHub — pull requests, issues, runs and releases",
      },
      {
        id: "stack",
        label: "Stack",
        summary:
          "Branch chains: which branch sits on which, and restack after the base moved.",
        paletteCommand: "Open Stack — branch chains and restacking",
      },
      {
        id: "policy",
        label: "Policy",
        summary:
          "MANVI gates, verdicts and cleanup: what is allowed to merge, and branches that are safe to delete.",
        paletteCommand: "Open MANVI — gates, verdicts and branch cleanup",
      },
      {
        id: "tasks",
        label: "Tasks",
        summary: "Track this repository's issues, bugs, features and shared tasks on a Kanban board.",
        paletteCommand: "Open repository Tasks — issues, bugs and features",
      },
    ],
  },
  code: {
    id: "code",
    label: "Code",
    summary:
      "The working tree. Open a file in Explorer, inspect Blame, or navigate the code map.",
    paletteCommand: "Open Code — the file explorer, editor, blame and map",
    // Three readings of one repository. Explorer and Blame still share
    // `selectedFilePath`; Map is the structural navigator over
    // `.devcouncil/repo_map.json` plus docs search, doc graph, and workspace links.
    sections: [
      {
        id: "explorer",
        label: "Explorer",
        summary:
          "Browse and edit files. The tree, editor tabs and the live pulse dashboard live here.",
      },
      {
        id: "blame",
        label: "Blame",
        summary:
          "Who last touched each line, and how old that code is. Keeps the file Explorer had open.",
        paletteCommand: "Open Blame — line authorship and code age",
      },
      {
        id: "map",
        label: "Map",
        summary:
          "Subsystems, entry points, doc search, the code/doc graph canvas, and cross-repo link candidates. Caps and truncation are named, not dressed as complete.",
        paletteCommand: "Open Map — subsystems, docs, graphs and link candidates",
      },
    ],
  },
  history: {
    id: "history",
    label: "History",
    summary:
      "What happened in this repository. The graph, the selected commit's diff and the reflog share one selection.",
    paletteCommand: "Open History — the commit graph, diffs and the reflog",
    // Three renderings of one subject: what happened to this repository.
    // They were three tabs, and the split cost more than it saved — the Diff
    // view had to grow its own commit picker and file rail purely so the
    // user would not have to go back to Graph for the commit they had just
    // been looking at. Sharing `selectedCommitId`, the sections keep it.
    sections: [
      {
        id: "graph",
        label: "Graph",
        summary: "The commit graph: branches, merges, and the commit you have selected.",
      },
      {
        id: "diff",
        label: "Diff",
        summary:
          "The changes in the selected commit — or the working tree, if none is selected.",
        paletteCommand: "Open Diff — changes in the selected commit",
      },
      {
        id: "reflog",
        label: "Reflog",
        summary:
          "Every movement of HEAD. Recovery points after a reset, checkout or amend.",
        paletteCommand: "Open Reflog — HEAD movements and recovery points",
      },
    ],
  },
  insights: {
    id: "insights",
    label: "Insights",
    summary:
      "On-demand measurements of this repository: activity, coverage, dependency health and disk. A scan that could not run is never shown as clean.",
    paletteCommand: "Open Insights — activity, dependencies, coverage and disk",
    // Four scans of one subject: this repository. They were four header
    // entries, each empty until someone ran it — over half the Inspect menu
    // costing attention every session and paying occasionally. As sections
    // they share one scan-card shell and one honesty contract about
    // truncation, which is the thing all four actually had in common.
    sections: [
      {
        id: "pulse",
        label: "Pulse",
        summary:
          "Rhythm and churn: heatmap, hotspots, who knows which files, and DORA-style movement.",
        paletteCommand: "Open Pulse — repository rhythm, churn and metrics",
      },
      {
        id: "coverage",
        label: "Coverage",
        summary:
          "Line and file coverage from this project's own test commands. Truncation and failures are named, not dressed as 100%.",
        paletteCommand: "Open Coverage",
      },
      {
        id: "health",
        label: "Health",
        summary:
          "Local audits across ecosystems, Dependabot and code scanning alerts, and dead code from the code graph. A scanner that did not run is listed, not implied clean.",
        paletteCommand: "Scan npm vulnerabilities and updates",
      },
      {
        id: "storage",
        label: "Storage",
        summary: "Where disk went: git objects, worktrees, and ignored build artifacts.",
        paletteCommand: "Scan repository disk usage",
      },
    ],
  },
};

/**
 * DOM id of the pane the view tabs control.
 *
 * The header tablist declared `role="tab"` with no `aria-controls`, so the
 * relationship it announced pointed at nothing. App stamps this id on the
 * `<main>` element the tabs actually swap.
 */
export const VIEW_PANE_ID = "gitpulse-view-pane";

/** Registry entries in declaration (= header) order. */
export const REGISTERED_VIEWS: readonly ViewRegistration[] = Object.values(VIEW_REGISTRY);

/** The sections a view offers, empty for a view that renders one pane. */
export function sectionsFor(id: ViewTab): readonly ViewSection[] {
  return VIEW_REGISTRY[id].sections ?? [];
}

/**
 * A destination's name, the way the header and section bar spell it.
 *
 * For UI elsewhere in the app that sends the reader to a pane and has to say
 * where they are about to land ("Work → Resolve"). Derived from the registry
 * rather than written out at the call site, so renaming a section renames
 * every promise made about it. An unknown section id degrades to the view's
 * own name instead of inventing one.
 */
export function describeDestination(id: ViewTab, section?: string | null): string {
  const view = VIEW_REGISTRY[id];
  const found = section ? sectionsFor(id).find((entry) => entry.id === section) : undefined;
  return found ? `${view.label} → ${found.label}` : view.label;
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
