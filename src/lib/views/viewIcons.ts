import {
  Activity,
  Briefcase,
  ChartNoAxesCombined,
  Cloud,
  CodeXml,
  FileDiff,
  FolderTree,
  GitCommitHorizontal,
  GitGraph,
  GitMerge,
  HardDrive,
  HeartPulse,
  LayoutDashboard,
  ListChecks,
  Network,
  Search,
  ShieldCheck,
  TestTube,
} from "@lucide/svelte";
import type { ViewTab } from "../repos/persist";

/**
 * The header switcher draws each view as an icon pill and each section as an
 * icon button, so every destination in the view catalog needs a glyph.
 *
 * Kept beside the catalog rather than in it: `viewRegistry` is read by the
 * native menu, the palette and plain-Node tests that have no use for Svelte
 * components. Keyed by the catalog's own ids, so `viewIcons.test.ts` can
 * require one icon per registered view and section, and a section added to the
 * catalog without one fails there instead of rendering a blank button.
 */
export type ViewIcon = typeof Search;

export const VIEW_ICONS: Readonly<Record<ViewTab, ViewIcon>> = {
  work: Briefcase,
  code: CodeXml,
  history: GitCommitHorizontal,
  insights: ChartNoAxesCombined,
};

export const SECTION_ICONS: Readonly<Record<ViewTab, Readonly<Record<string, ViewIcon>>>> = {
  work: {
    overview: LayoutDashboard,
    resolve: GitMerge,
    remote: Cloud,
    policy: ShieldCheck,
    tasks: ListChecks,
  },
  code: {
    explorer: FolderTree,
    map: Network,
  },
  history: {
    graph: GitGraph,
    diff: FileDiff,
  },
  insights: {
    pulse: Activity,
    coverage: TestTube,
    health: HeartPulse,
    storage: HardDrive,
  },
};

/** A section's glyph; the view's own glyph for an id the map does not know. */
export function sectionIcon(view: ViewTab, section: string): ViewIcon {
  return SECTION_ICONS[view][section] ?? VIEW_ICONS[view];
}
