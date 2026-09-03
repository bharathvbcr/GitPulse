import type { ViewLoader } from "../components/LazyView.svelte";
import type { ViewTab } from "../repos/persist";

/**
 * Views fetched on first open rather than shipped in the entry chunk.
 *
 * The split follows the registry's own taxonomy: the `work` group (work,
 * files, history, diff, conflict) is the daily driver and stays eager so the
 * first paint after boot needs no extra round trip. Everything in `inspect`
 * and `more` is reached by an explicit tab/palette action, so its code can
 * arrive with that action instead of at startup.
 *
 * Each entry must be a stable module-level thunk containing a literal
 * `import()`: Rollup needs the literal to emit a chunk, and LazyView keys its
 * resolved-component cache on the thunk's identity.
 */
export const LAZY_VIEW_LOADERS = {
  blame: () => import("../components/BlameViewer.svelte"),
  coverage: () => import("../components/CoverageViewer.svelte"),
  health: () => import("../components/HealthPanel.svelte"),
  storage: () => import("../components/StoragePanel.svelte"),
  stack: () => import("../components/CodeStackViewer.svelte"),
  pulse: () => import("../components/pulse/PulseView.svelte"),
  terminal: () => import("../components/TerminalPanel.svelte"),
  manvi: () => import("../components/ManviOpsPanel.svelte"),
  github: () => import("../components/GitHubPanel.svelte"),
  reflog: () => import("../components/ReflogViewer.svelte"),
} satisfies Partial<Record<ViewTab, ViewLoader>>;

/** Tabs whose component is code-split; the rest are bundled with the entry. */
export type LazyViewTab = keyof typeof LAZY_VIEW_LOADERS;

export function isLazyViewTab(tab: ViewTab): tab is LazyViewTab {
  return Object.hasOwn(LAZY_VIEW_LOADERS, tab);
}
