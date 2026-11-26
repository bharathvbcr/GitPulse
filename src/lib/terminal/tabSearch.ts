/**
 * Search over terminal tabs, launchers, and the global session registry.
 *
 * Pure so a crowded tab strip can be filtered and keyboard-cycled without a
 * PTY or a webview. The query is bounded and stripped of control characters
 * before it ever reaches the DOM.
 */

import { matchFolded } from "../branches/groupBranches";
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

function isSeparatorCode(code: number): boolean {
  // space, tab, '-', '_', '/', '\', '.', ':'
  return (
    code === 32 ||
    code === 9 ||
    code === 45 ||
    code === 95 ||
    code === 47 ||
    code === 92 ||
    code === 46 ||
    code === 58
  );
}

function isWordBoundary(text: string, index: number): boolean {
  if (index <= 0) return true;
  const prev = text.charCodeAt(index - 1);
  if (isSeparatorCode(prev)) return true;
  const curr = text.charCodeAt(index);
  const currUpper = curr >= 65 && curr <= 90;
  const prevLowerOrDigit = (prev >= 97 && prev <= 122) || (prev >= 48 && prev <= 57);
  return currUpper && prevLowerOrDigit;
}

function wordInitials(text: string): string {
  let out = "";
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i);
    if (isSeparatorCode(code)) continue;
    if (isWordBoundary(text, i)) out += text[i].toLowerCase();
  }
  return out;
}

/**
 * Quality rank of one folded token against one field:
 * - 0: no match
 * - 1: fuzzy subsequence
 * - 2: contiguous substring
 * - 3: word-boundary prefix or camelCase/separator initials (len >= 2)
 * - 4: field prefix
 * - 5: exact match
 */
export function fieldSearchRank(tokenLower: string, field: string): number {
  if (!tokenLower) return 5;
  if (!field) return 0;
  const folded = field.toLowerCase();
  if (folded === tokenLower) return 5;
  if (folded.startsWith(tokenLower)) return 4;
  let hasSubstring = false;
  for (
    let at = folded.indexOf(tokenLower);
    at !== -1;
    at = folded.indexOf(tokenLower, at + 1)
  ) {
    hasSubstring = true;
    if (isWordBoundary(field, at)) return 3;
  }
  if (tokenLower.length >= 2 && wordInitials(field).startsWith(tokenLower)) {
    return 3;
  }
  if (hasSubstring) return 2;
  return matchFolded(tokenLower, folded) ? 1 : 0;
}

/**
 * Ranked match across one or more searchable fields. Supports both phrase
 * matches inside a single field and multi-token queries spread across fields
 * (e.g. `"devcouncil claude"` matching repo `"DevCouncil"` + agent `"Claude"`).
 */
export function terminalSearchRank(
  query: unknown,
  ...fields: Array<string | null | undefined>
): number {
  const needle = normalizeTerminalSearchQuery(query).toLowerCase();
  if (!needle) return 5;
  const validFields: string[] = [];
  for (const field of fields) {
    if (typeof field === "string" && field.length > 0) validFields.push(field);
  }
  if (validFields.length === 0) return 0;
  let wholeBest = 0;
  for (const field of validFields) {
    const rank = fieldSearchRank(needle, field);
    if (rank > wholeBest) wholeBest = rank;
    if (wholeBest === 5) return 5;
  }
  const tokens = needle.split(/\s+/).filter(Boolean);
  if (tokens.length <= 1) return wholeBest;
  let minTokenRank = 5;
  for (const token of tokens) {
    let tokenBest = 0;
    for (const field of validFields) {
      const rank = fieldSearchRank(token, field);
      if (rank > tokenBest) tokenBest = rank;
      if (tokenBest === 5) break;
    }
    if (tokenBest === 0) return wholeBest;
    if (tokenBest < minTokenRank) minTokenRank = tokenBest;
  }
  return Math.max(wholeBest, minTokenRank);
}

export function matchesTerminalSearch(
  query: unknown,
  ...fields: Array<string | null | undefined>
): boolean {
  return terminalSearchRank(query, ...fields) > 0;
}

export function filterTerminalTabs(
  tabs: readonly TerminalTab[],
  query: unknown,
  extras?: Readonly<Record<string, { status?: string; unread?: boolean; agentDetail?: string; groupLabel?: string }>>,
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
      tab.launchName,
      tab.title,
      tab.checkout,
      tab.filePath,
      tab.url,
      extra?.agentDetail,
      extra?.groupLabel,
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
      session.checkout ?? "",
      session.label,
      session.title ?? "",
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
