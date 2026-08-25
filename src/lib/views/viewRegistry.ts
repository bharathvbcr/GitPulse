import type { ViewTab } from "../repos/persist";

/**
 * Header group a view belongs to. The group's tab-vs-menu placement lives in
 * viewNav; this id only assigns membership.
 */
export type ViewGroupId = "work" | "inspect" | "more";

export interface ViewRegistration {
  readonly id: ViewTab;
  /** Display name in the header tabs/menus and the default palette phrasing. */
  readonly label: string;
  /** Required for every registered view: which header group owns it. */
  readonly menuGroup: ViewGroupId;
  /**
   * Command-palette label for views that are reachable as commands. Omitted
   * means the view gets no palette command (matching historical behavior for
   * the always-visible work views).
   */
  readonly paletteCommand?: string;
}

/**
 * Single catalog of application views.
 *
 * Adding a view means adding its `ViewTab` member + `VIEW_TABS` entry in
 * repos/persist.ts AND one entry here — TypeScript rejects anything less
 * (`Record<ViewTab, …>`), and every consumer (header nav, native menu,
 * command palette) derives from this record, so no other file needs editing.
 *
 * Declaration order is the display order inside each menuGroup, so keep the
 * record ordered the way the header should read.
 */
export const VIEW_REGISTRY: Readonly<Record<ViewTab, ViewRegistration>> = {
  history: { id: "history", label: "Graph", menuGroup: "work" },
  diff: { id: "diff", label: "Diff", menuGroup: "work" },
  conflict: { id: "conflict", label: "Resolve", menuGroup: "work" },
  blame: { id: "blame", label: "Blame", menuGroup: "inspect" },
  coverage: { id: "coverage", label: "Coverage", menuGroup: "inspect", paletteCommand: "Open Coverage" },
  health: {
    id: "health",
    label: "Health",
    menuGroup: "inspect",
    paletteCommand: "Scan npm vulnerabilities and updates",
  },
  storage: {
    id: "storage",
    label: "Storage",
    menuGroup: "inspect",
    paletteCommand: "Scan repository disk usage",
  },
  stack: { id: "stack", label: "Stack", menuGroup: "inspect" },
  manvi: { id: "manvi", label: "MANVI", menuGroup: "more", paletteCommand: "Open MANVI View" },
  github: { id: "github", label: "GitHub", menuGroup: "more", paletteCommand: "Open GitHub Panel" },
  reflog: { id: "reflog", label: "Reflog", menuGroup: "more", paletteCommand: "Open Reflog" },
};

/** Registry entries in declaration (= header) order. */
export const REGISTERED_VIEWS: readonly ViewRegistration[] = Object.values(VIEW_REGISTRY);

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
