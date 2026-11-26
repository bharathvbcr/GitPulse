/**
 * Pure view-model helpers for the Global Agents plane: multi-word search,
 * smart repository grouping, running/done state predicates, and filter counts.
 */
import { countAgentRows, applyAgentTree, type AgentFilter, type AgentRow, type AttentionReason } from "./plane";
import { displayName, identityKey, type PathIdentityOptions } from "../repos/paths";
import { agentKindLabel } from "../work/agentWorktree";
import type { MaviAgentRow } from "../mavi/agentsRows";

export type AgentGroupMode = "auto" | "repo" | "flat";

export function isAgentGroupMode(value: unknown): value is AgentGroupMode {
  return value === "auto" || value === "repo" || value === "flat";
}

export interface AgentRepoGroup {
  readonly key: string;
  readonly repoPath: string;
  readonly repoLabel: string;
  readonly roots: AgentRow[];
  readonly total: number;
  readonly running: number;
  readonly attention: number;
  readonly done: number;
}

const BLOCKING_REASONS: ReadonlySet<AttentionReason> = new Set([
  "needs-you",
  "error",
  "pending",
  "blocked",
  "collision",
  "disconnected",
  "unstarted",
  "signalled",
]);

const DONE_OR_NOTICE_REASONS: ReadonlySet<AttentionReason> = new Set([
  "finished",
  "idle-waiting",
  "notice",
]);

function rowSearchText(row: AgentRow): string {
  const kindLabel = row.kind ? agentKindLabel(row.kind) : "";
  return [
    row.session,
    row.repoLabel,
    row.checkout,
    row.kind,
    kindLabel,
    row.launcher ?? "",
    row.attentionLabel,
    row.attentionDetail ?? "",
    row.worktreePath,
  ]
    .join("\n")
    .toLowerCase();
}

function tokenizeQuery(query: string): string[] {
  return query
    .trim()
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean);
}

function matchesTreeTokens(row: AgentRow, tokens: readonly string[], inheritedText: string): boolean {
  const combined = inheritedText ? `${inheritedText}\n${rowSearchText(row)}` : rowSearchText(row);
  if (tokens.every((token) => combined.includes(token))) return true;
  return row.children.some((child) => matchesTreeTokens(child, tokens, combined));
}

/**
 * Filters `roots` by a multi-word, case-insensitive query across row metadata
 * and descendants. Keeps a parent root whenever it or any descendant matches.
 */
export function searchAgentTree(roots: readonly AgentRow[], query: string): AgentRow[] {
  const tokens = tokenizeQuery(query);
  if (tokens.length === 0) return [...roots];
  return roots.filter((row) => matchesTreeTokens(row, tokens, ""));
}

/**
 * Filters `rows` (Mavi threads) by the same multi-word, case-insensitive query.
 */
export function searchMaviRows(rows: readonly MaviAgentRow[], query: string): MaviAgentRow[] {
  const tokens = tokenizeQuery(query);
  if (tokens.length === 0) return [...rows];
  return rows.filter((row) => {
    const haystack = [
      row.title,
      row.where,
      row.root ?? "",
      row.presence,
      row.attention,
      row.status,
      "Mavi thread",
    ]
      .join("\n")
      .toLowerCase();
    return tokens.every((token) => haystack.includes(token));
  });
}

function rowHasBlockingOrDone(row: AgentRow): boolean {
  if (row.attentionUrgency === "blocking" || row.attentionUrgency === "error") return true;
  if (row.attention.some((reason) => BLOCKING_REASONS.has(reason) || DONE_OR_NOTICE_REASONS.has(reason))) {
    return true;
  }
  return row.children.some(rowHasBlockingOrDone);
}

/**
 * True when any visible row (or descendant) has blocking attention or is
 * done/finished/notifying, or any visible Mavi row requires action or review.
 */
export function hasAttentionOrDone(
  roots: readonly AgentRow[],
  maviRows: readonly MaviAgentRow[] = [],
): boolean {
  if (roots.some(rowHasBlockingOrDone)) return true;
  return maviRows.some((row) => row.needsAction || row.status === "review");
}

function rowAttentionTier(row: AgentRow): 0 | 1 | 2 | null {
  if (row.attentionUrgency === "error" || row.attention.includes("error")) return 0;
  if (row.attentionUrgency === "blocking" || row.attention.some((reason) => BLOCKING_REASONS.has(reason))) return 1;
  if (row.attention.some((reason) => DONE_OR_NOTICE_REASONS.has(reason))) return 2;
  return null;
}

/**
 * Walks `roots` and all descendants, collecting every row whose own attention
 * has a blocking/error reason or a done/notification reason (`finished`,
 * `idle-waiting`, `notice`), sorted urgency-first (error -> blocking ->
 * finished/idle-waiting/notice), then most recent `attentionAt` descending,
 * then `repoLabel` and `session`.
 */
export function collectAttentionAndDoneRows(roots: readonly AgentRow[]): AgentRow[] {
  const collected: AgentRow[] = [];
  const walk = (row: AgentRow): void => {
    if (rowAttentionTier(row) !== null) collected.push(row);
    row.children.forEach(walk);
  };
  roots.forEach(walk);
  collected.sort((a, b) => {
    const tierDiff = (rowAttentionTier(a) ?? 9) - (rowAttentionTier(b) ?? 9);
    if (tierDiff !== 0) return tierDiff;
    const atDiff = (b.attentionAt ?? 0) - (a.attentionAt ?? 0);
    if (atDiff !== 0) return atDiff;
    return a.repoLabel.localeCompare(b.repoLabel) || a.session.localeCompare(b.session) || a.id.localeCompare(b.id);
  });
  return collected;
}

/**
 * Dynamic heading for the ungrouped Attention & Done tray.
 */
export function attentionTrayHeading(rows: readonly AgentRow[]): string {
  let hasBlocking = false;
  let hasDone = false;
  for (const row of rows) {
    const tier = rowAttentionTier(row);
    if (tier === 0 || tier === 1) hasBlocking = true;
    else if (tier === 2) hasDone = true;
  }
  if (hasBlocking && hasDone) return "Needs attention & finished";
  if (hasBlocking) return "Needs attention";
  return "Finished";
}

/**
 * True when the row represents a live agent that is not fully idle and has no
 * blocking, error, finished, or idle-waiting standing attention.
 */
export function isRowActivelyRunning(row: AgentRow): boolean {
  if (row.presence !== "live" || row.fullyIdle) return false;
  if (row.attentionUrgency === "blocking" || row.attentionUrgency === "error") return false;
  return !row.attention.some(
    (reason) => BLOCKING_REASONS.has(reason) || reason === "finished" || reason === "idle-waiting",
  );
}

/** True when the row's standing attention indicates it finished or is waiting idle. */
export function isRowDone(row: AgentRow): boolean {
  return row.attention.includes("finished") || row.attention.includes("idle-waiting");
}

function rowHasBlockingAttention(row: AgentRow): boolean {
  if (row.attentionUrgency === "blocking" || row.attentionUrgency === "error") return true;
  return row.attention.some((reason) => BLOCKING_REASONS.has(reason));
}

function summarizeGroupCounts(roots: readonly AgentRow[]): { running: number; attention: number; done: number } {
  let running = 0;
  let attention = 0;
  let done = 0;
  const walk = (row: AgentRow): void => {
    if (row.origin !== "session") {
      if (isRowActivelyRunning(row)) running += 1;
      if (rowHasBlockingAttention(row)) attention += 1;
      else if (isRowDone(row)) done += 1;
    }
    row.children.forEach(walk);
  };
  roots.forEach(walk);
  return { running, attention, done };
}

/**
 * Groups visible root rows by normalized repository identity, sorted
 * deterministically alphabetically by `repoLabel` (and then `key`).
 */
export function groupAgentTreeByRepo(
  roots: readonly AgentRow[],
  paths: PathIdentityOptions,
): AgentRepoGroup[] {
  const buckets = new Map<string, { repoPath: string; repoLabel: string; roots: AgentRow[] }>();
  for (const row of roots) {
    const rawPath = row.repoPath || row.checkoutPath || "";
    const normalized = rawPath ? identityKey(rawPath, paths) || rawPath : "__unresolved__";
    const existing = buckets.get(normalized);
    if (existing) {
      existing.roots.push(row);
    } else {
      const repoLabel = row.repoLabel || (row.repoPath ? displayName(row.repoPath) : "Repository not resolved");
      buckets.set(normalized, {
        repoPath: row.repoPath || row.checkoutPath || "",
        repoLabel,
        roots: [row],
      });
    }
  }
  const groups: AgentRepoGroup[] = [];
  for (const [key, bucket] of buckets) {
    const counts = summarizeGroupCounts(bucket.roots);
    groups.push({
      key,
      repoPath: bucket.repoPath,
      repoLabel: bucket.repoLabel,
      roots: bucket.roots,
      total: countAgentRows(bucket.roots),
      running: counts.running,
      attention: counts.attention,
      done: counts.done,
    });
  }
  groups.sort((a, b) => a.repoLabel.localeCompare(b.repoLabel) || a.key.localeCompare(b.key));
  return groups;
}

const ALL_FILTERS: readonly AgentFilter[] = [
  "all",
  "attention",
  "notifications",
  "gaps",
  "live",
  "parallel",
];

/**
 * Computes visible row counts per filter tab from the currently scoped roots.
 */
export function countAgentFilters(scoped: readonly AgentRow[]): Record<AgentFilter, number> {
  const counts = {} as Record<AgentFilter, number>;
  for (const filter of ALL_FILTERS) {
    counts[filter] = countAgentRows(applyAgentTree(scoped, filter));
  }
  return counts;
}
