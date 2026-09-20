/**
 * Search over terminal tabs, launchers, and the global session registry.
 *
 * Pure so a crowded tab strip can be filtered and keyboard-cycled without a
 * PTY or a webview. The query is bounded and stripped of control characters
 * before it ever reaches the DOM.
 */

import { fuzzyMatch } from "../branches/groupBranches";
import {
  launcherLabel,
  tabLabel,
  type TerminalTab,
} from "./tabs";
import type { TerminalSessionRecord } from "./sessionRegistry";

export const TERMINAL_SEARCH_QUERY_LIMIT = 128;

/**
 * A strip with more than four tabs no longer fits a typical dock without
 * scrolling or compressing labels. Four was the old visual ceiling, not the
 * process ceiling.
 */
export function crowdedTabStrip(count: number): boolean {
  return Number.isFinite(count) && count > 4;
}

/** Fail-closed: a non-string, empty, or hostile query matches everything. */
export function normalizeTerminalSearchQuery(
  query: unknown,
  limit = TERMINAL_SEARCH_QUERY_LIMIT,
): string {
  if (typeof query !== "string") return "";
  if (!Number.isFinite(limit) || limit <= 0) return "";
  return Array.from(query.replace(/[\x00-\x1f\x7f-\x9f]/g, ""))
    .slice(0, Math.floor(limit))
    .join("")
    .trim();
}

export function matchesTerminalSearch(
  query: unknown,
  ...fields: Array<string | null | undefined>
): boolean {
  const needle = normalizeTerminalSearchQuery(query);
  if (!needle) return true;
  return fields.some(
    (field) => typeof field === "string" && field.length > 0 && fuzzyMatch(needle, field),
  );
}

export function filterTerminalTabs(
  tabs: readonly TerminalTab[],
  query: unknown,
  extras?: Readonly<Record<string, { status?: string; unread?: boolean }>>,
): TerminalTab[] {
  if (!Array.isArray(tabs)) return [];
  return tabs.filter((tab) => {
    const extra = extras?.[tab.id];
    return matchesTerminalSearch(
      query,
      tabLabel(tab),
      launcherLabel(tab.launcher),
      tab.launcher,
      tab.name,
      tab.title,
      extra?.status,
      extra?.unread ? "unread" : "",
    );
  });
}

export function filterLaunchers<T extends { kind: string; label: string }>(
  launchers: readonly T[],
  query: unknown,
): T[] {
  if (!Array.isArray(launchers)) return [];
  return launchers.filter((launcher) =>
    matchesTerminalSearch(query, launcher.kind, launcher.label),
  );
}

export function filterSessionRecords(
  sessions: readonly TerminalSessionRecord[],
  query: unknown,
): TerminalSessionRecord[] {
  if (!Array.isArray(sessions)) return [];
  return sessions.filter((session) =>
    matchesTerminalSearch(
      query,
      session.repoPath,
      session.label,
      session.status,
      session.key,
    ),
  );
}

/**
 * Wrap at both ends so ArrowUp on the first row is not a silent no-op.
 * An empty list has no highlight.
 */
export function stepSearchIndex(count: number, index: number, step: 1 | -1): number {
  if (!Number.isFinite(count) || count <= 0) return -1;
  const size = Math.floor(count);
  if (!Number.isFinite(index) || index < 0) return step === 1 ? 0 : size - 1;
  return (Math.floor(index) + step + size) % size;
}
