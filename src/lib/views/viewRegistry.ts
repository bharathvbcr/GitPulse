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
      "Everything in flight: worktrees, pull requests, CI runs and Manvi verdicts. Branch chains, blocked items and a parked merge sort first.",
    paletteCommand: "Open Work — tasks, worktrees, PRs, runs and verdicts",
    // Everything in flight, and the surfaces that act on it. GitHub and MANVI
    // were separate views rendering halves of the same answer Work already
    // joins — both issued `cmd_github_context`, four `gh` round trips each,
    // to draw overlapping lists. Stack joined Overview as a collapsible card:
    // branch chains are another reading of "this repository's branches".
    //
    // Resolve and Policy then dissolved into the sections whose subject they
    // act on. Resolve was never a destination: agents finish most parked
    // merges, so the tab was an empty state nearly every visit, and what it
    // shows is the state of this repository — the conflict editor now leads
    // Overview while a merge or rebase is parked and is absent otherwise.
    // Policy was a page named after the engine underneath rather than a
    // subject: branch and tag cleanup and the policy journal are collapsed
    // disclosures on Overview, issues, releases and outgoing commits joined
    // Remote's lists of the same objects, and the Manvi connection and model
    // servers are configuration, so they live in Settings → Agents.
    sections: [
      {
        id: "overview",
        label: "Overview",
        summary:
          "The joined list of this repository's worktrees, pull requests and runs. A parked merge or rebase leads the page until it is finished.",
      },
      {
        id: "remote",
        label: "Remote",
        summary:
          "Pull requests, issues, Actions runs and releases from GitHub, and the commits about to be pushed.",
        paletteCommand: "Open GitHub — pull requests, issues, runs and releases",
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
      "The working tree. Open, search and blame files in Explorer, or navigate the code map.",
    paletteCommand: "Open Code — the file explorer, editor, search, blame and map",
    // Two readings of one repository. Explorer is the file: its rail toggles
    // between the tree and content search, and the open file between the
    // editor and its blame (both were sections here once, and each sent the
    // user back to Explorer for the file it had just named — see
    // `files/explorerLens.ts`). Map is the structural navigator over
    // `.devcouncil/repo_map.json` plus docs search, doc graph, and workspace links.
    sections: [
      {
        id: "explorer",
        label: "Explorer",
        summary:
          "Browse, search and edit files, and toggle the open file to its blame. The tree, content search, editor tabs and the live pulse dashboard live here.",
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
      "What happened in this repository. The graph, HEAD's moves (the reflog), regression suspects and the selected commit's diff share one selection.",
    paletteCommand: "Open History — the commit graph, the reflog, suspects and diffs",
    // Renderings of one subject: what happened to this repository. They were
    // separate tabs, and the split cost more than it saved — the Diff view
    // had to grow its own commit picker and file rail purely so the user
    // would not have to go back to Graph for the commit they had just been
    // looking at. Sharing `selectedCommitId`, the sections keep it.
    //
    // Reflog was a section too, until it folded into Graph as the "HEAD
    // moves" list (views/historyList.ts): both are lists of commits feeding
    // the same details pane, and as a separate pane a click on an entry
    // selected a commit on a Graph nobody was looking at. Its palette door
    // is `history:head-moves` in palette/catalog.ts.
    //
    // Suspects followed as Graph's third list: its ranking is a list of
    // commits too, and as a section a click left it for Diff, so the ranking
    // and the commit it named were never on screen together. Its question
    // sits in the context bar, its verdict in the details pane, and the other
    // lists chip the commits it ranked. Its palette door keeps the id
    // `history:suspects`.
    sections: [
      {
        id: "graph",
        label: "Graph",
        summary:
          "The commit graph, every movement of HEAD (the reflog) with its recovery points, or the commits ranked as suspects for a symptom. The commit you select is described below whichever list is showing.",
      },
      {
        id: "diff",
        label: "Diff",
        summary:
          "The changes in the selected commit — or the working tree, if none is selected.",
        paletteCommand: "Open Diff — changes in the selected commit",
      },
    ],
  },
  insights: {
    id: "insights",
    label: "Insights",
    summary:
      "On-demand measurements of this repository: activity, coverage, health (dependencies and secrets) and disk. A scan that could not run is never shown as clean.",
    paletteCommand: "Open Insights — activity, dependencies, coverage, secrets and disk",
    // Scans of one subject: this repository. They were separate header
    // entries, each empty until someone ran it. As sections they share one
    // scan-card shell and one honesty contract about truncation.
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
      // Secrets was a section of its own. It is a finding about this
      // repository like the advisories beside it, and alone it had no way to
      // hand its findings to a coding agent; inside Health it shares the
      // verdict and the agent bar. A restored "secrets" lands here
      // (`RETIRED_SECTIONS` below).
      {
        id: "health",
        label: "Health",
        summary:
          "Local audits across ecosystems, Dependabot and code scanning alerts, working-tree secrets via Kingfisher, and dead code from the code graph. A scanner that did not run is listed, not implied clean. No secret values are shown.",
        paletteCommand: "Scan dependencies and secrets",
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
 * where they are about to land ("Work → Remote"). Derived from the registry
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
  if (typeof candidate === "string") {
    if (sections.some((s) => s.id === candidate)) return candidate;
    const merged = retiredSectionFor(id, candidate);
    if (merged) return merged;
  }
  return sections[0].id;
}

/**
 * Sections folded into another section of the same view, by retired id.
 *
 * The default fallback is right for a section that simply went away, and
 * wrong for one whose content moved: a saved selection, or a native menu
 * build, that still names it must land where the content went, not on the
 * view's first section. Resolve is the case that matters: a conflicts chip
 * must land on the conflict editor, which now leads Overview while a merge
 * or rebase is parked.
 *
 * This is the one retirement map: [`resolveSection`] reads it, so restored
 * sessions (`sanitizeViewSections` in repos/persist.ts) and live callers land
 * in the same place. Native menu destinations naming a retired section still
 * do not navigate (`nativeActions.ts` matches live sections only), so a stale
 * native menu id cannot open a surprise pane.
 */
export const RETIRED_SECTIONS: Readonly<Partial<Record<ViewTab, Readonly<Record<string, string>>>>> = {
  // Resolve leads Overview while a merge or rebase is parked. Policy's cards
  // moved to the surfaces that own their subject; the nearest whole page is
  // Overview, where cleanup and the policy journal live. Both ids point at the
  // destination directly, because this lookup does not chain.
  work: { resolve: "overview", policy: "overview" },
  // Secrets became a section of Health: one verdict, one agent bar.
  insights: { secrets: "health" },
  // Search and Blame became toggles inside Explorer (`files/explorerLens.ts`):
  // the file a saved session was searching or blaming is still the selection.
  code: { search: "explorer", blame: "explorer" },
  // Reflog and Suspects became Graph's "HEAD moves" and "Suspects" lists
  // (`views/historyList.ts`); each list is restored from the same retired id
  // by `restoredHistoryList`.
  history: { reflog: "graph", suspects: "graph" },
};

/** Where a retired section's content now lives, or null when it is not retired. */
export function retiredSectionFor(id: ViewTab, candidate: string): string | null {
  const retired = RETIRED_SECTIONS[id];
  if (!retired || !Object.hasOwn(retired, candidate)) return null;
  const target = retired[candidate];
  return sectionsFor(id).some((s) => s.id === target) ? target : null;
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
