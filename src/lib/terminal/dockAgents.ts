import { displayName, identityKey, isCaseInsensitiveFs, sameRepo, type PathIdentityOptions } from "../repos/paths";
import { launcherLabel, type TabKind } from "./tabs";

export interface DockAgentStatus {
  label: string;
  cls: string;
}

export interface DockAgentRow {
  /** Tab id for `here` rows; session registry key for `elsewhere` rows. */
  id: string;
  launcher: TabKind;
  title: string;
  /** Secondary line: checkout/branch for here, "repo/checkout" for elsewhere. */
  detail?: string | null;
  /** OSC status badge, e.g. { label: "Working", cls: "text-accent" }. */
  status?: DockAgentStatus | null;
  /** Needs the reader: e.g. "Waiting", "Failed". Drawn as a warn/danger chip. */
  attention?: string | null;
  active?: boolean;
  unread?: boolean;
  /** Elsewhere rows that cannot be revealed. */
  disabled?: boolean;
  repoPath?: string | null;
  repoName?: string | null;
  checkout?: string | null;
  running?: boolean;
  done?: boolean;
  attentionDetail?: string | null;
  lastActivityAt?: number | null;
}

export type DockAgentFilter = "all" | "running" | "attention" | "done";

export interface DockRepoGroupItem {
  row: DockAgentRow;
  isHere: boolean;
}

export interface DockRepoGroup {
  key: string;
  repoPath: string;
  repoName: string;
  isCurrent: boolean;
  items: DockRepoGroupItem[];
  rows: DockAgentRow[];
  runningCount: number;
  attentionCount: number;
  doneCount: number;
  collapsed: boolean;
}

export type CurrentRepoRef = string | null | undefined | { path?: string | null; name?: string | null };

const RUNNING_STATUS_RE = /^(working|running|starting)$/i;
const DANGER_ATTENTION_RE = /fail|error/i;

function defaultPathOptions(): PathIdentityOptions {
  return { caseInsensitive: isCaseInsensitiveFs() };
}

/** True when a dock agent row represents an actively running (non-blocked, non-done) agent. */
export function isDockAgentRunning(row: DockAgentRow): boolean {
  if (row.attention || row.done) return false;
  if (typeof row.running === "boolean") return row.running;
  const label = row.status?.label?.trim() ?? "";
  return RUNNING_STATUS_RE.test(label);
}

/** Multi-word case-insensitive filter across dock agent row metadata. */
export function filterDockAgents(rows: readonly DockAgentRow[], query: string): DockAgentRow[] {
  const tokens = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return [...rows];
  return rows.filter((row) => {
    const kindLabel = row.launcher !== "file" && row.launcher !== "browser" ? launcherLabel(row.launcher) : row.launcher;
    const haystack = [
      row.title,
      row.detail,
      row.repoName,
      row.checkout,
      row.launcher,
      kindLabel,
      row.attention,
      row.attentionDetail,
      row.status?.label,
    ]
      .filter((part): part is string => typeof part === "string" && part.length > 0)
      .join("\n")
      .toLowerCase();
    return tokens.every((token) => haystack.includes(token));
  });
}

/** Filter rows by quick status pill (`all`, `running`, `attention`, `done`). */
export function filterDockAgentsByStatus(rows: readonly DockAgentRow[], filter: DockAgentFilter): DockAgentRow[] {
  if (filter === "running") return rows.filter((row) => isDockAgentRunning(row));
  if (filter === "attention") return rows.filter((row) => Boolean(row.attention));
  if (filter === "done") return rows.filter((row) => Boolean(row.done) && !row.attention);
  return [...rows];
}

/**
 * Secondary subtitle for a dock agent row.
 * When `preferCheckout` is true (grouped by repository), prefers `row.checkout`
 * and falls back to stripping any leading `${repoName} · ` or `${repoName}/`
 * prefix from `row.detail`, returning `null` if nothing distinct remains.
 */
export function dockRowSecondary(row: DockAgentRow, preferCheckout: boolean): string | null {
  const repoName = row.repoName?.trim() ?? "";
  const repoLower = repoName.toLowerCase();
  if (preferCheckout) {
    const checkout = row.checkout?.trim() ?? "";
    if (checkout && (!repoLower || checkout.toLowerCase() !== repoLower)) {
      return checkout;
    }
  }
  const detail = row.detail?.trim() ?? "";
  if (!detail) return null;
  if (!preferCheckout) return detail;
  if (repoLower) {
    const detailLower = detail.toLowerCase();
    if (detailLower === repoLower) return null;
    for (const sep of [" · ", "/"]) {
      const prefix = `${repoLower}${sep}`;
      if (detailLower.startsWith(prefix)) {
        const rest = detail.slice(prefix.length).trim();
        return rest && rest.toLowerCase() !== repoLower ? rest : null;
      }
    }
  }
  return detail;
}

/** True when any row across `here` or `elsewhere` needs attention or is done. */
export function hasDockAttentionOrDone(
  here: readonly DockAgentRow[],
  elsewhere: readonly DockAgentRow[],
): boolean {
  return (
    here.some((row) => Boolean(row.attention) || Boolean(row.done)) ||
    elsewhere.some((row) => Boolean(row.attention) || Boolean(row.done))
  );
}

function triageTier(row: DockAgentRow): number {
  if (row.attention) return DANGER_ATTENTION_RE.test(row.attention) ? 0 : 1;
  if (row.done) return 2;
  if (isDockAgentRunning(row)) return 3;
  return 4;
}

/** Urgency-first ordering for triage mode: failed -> attention -> done -> running -> idle/ended. */
export function sortDockTriageRows(rows: readonly DockAgentRow[]): DockAgentRow[] {
  return [...rows].sort((a, b) => triageTier(a) - triageTier(b));
}

/**
 * Collect every row across `here` and `elsewhere` that needs attention or is done,
 * sorted urgency-first (`failed`/`error` -> other `attention` -> `done`, then
 * most recent `lastActivityAt` descending).
 */
export function collectDockAttentionAndDone(
  here: readonly DockAgentRow[],
  elsewhere: readonly DockAgentRow[],
): DockRepoGroupItem[] {
  const items: DockRepoGroupItem[] = [];
  for (const row of here) {
    if (row.attention || row.done) items.push({ row, isHere: true });
  }
  for (const row of elsewhere) {
    if (row.attention || row.done) items.push({ row, isHere: false });
  }
  return items.sort((a, b) => {
    const tierDiff = triageTier(a.row) - triageTier(b.row);
    if (tierDiff !== 0) return tierDiff;
    const aAt = typeof a.row.lastActivityAt === "number" ? a.row.lastActivityAt : 0;
    const bAt = typeof b.row.lastActivityAt === "number" ? b.row.lastActivityAt : 0;
    return bAt - aAt;
  });
}

function resolveCurrentRepo(currentRepo: CurrentRepoRef): { path: string; name: string } {
  if (typeof currentRepo === "string") {
    const trimmed = currentRepo.trim();
    return { path: trimmed, name: trimmed ? displayName(trimmed) : "" };
  }
  if (currentRepo && typeof currentRepo === "object") {
    const path = currentRepo.path?.trim() ?? "";
    const name = currentRepo.name?.trim() || (path ? displayName(path) : "");
    return { path, name };
  }
  return { path: "", name: "" };
}

function repoNameFromRow(row: DockAgentRow, isHere: boolean, currentName: string): string {
  const explicit = row.repoName?.trim();
  if (explicit) return explicit;
  const path = row.repoPath?.trim();
  if (path) return displayName(path);
  if (isHere) return currentName || "This repository";
  const detail = row.detail?.trim() ?? "";
  if (detail) {
    const slash = detail.indexOf("/");
    const dot = detail.indexOf(" · ");
    if (dot > 0) return detail.slice(0, dot).trim();
    if (slash > 0) return detail.slice(0, slash).trim();
  }
  return "Other repository";
}

/**
 * Group dock agent rows by normalized repository path / name, placing the
 * current repository first followed by other repositories sorted alphabetically.
 */
export function groupDockAgentsByRepo(
  here: readonly DockAgentRow[],
  elsewhere: readonly DockAgentRow[],
  currentRepo: CurrentRepoRef = null,
  options: { pathOptions?: PathIdentityOptions; collapsedKeys?: ReadonlySet<string> } = {},
): DockRepoGroup[] {
  const pathOptions = options.pathOptions ?? defaultPathOptions();
  const collapsedKeys = options.collapsedKeys;
  const current = resolveCurrentRepo(currentRepo);
  const currentKey = current.path ? identityKey(current.path, pathOptions) : "";

  const groups = new Map<string, DockRepoGroup>();
  const orderedKeys: string[] = [];

  const addItem = (row: DockAgentRow, isHere: boolean) => {
    const effectivePath = row.repoPath?.trim() || (isHere ? current.path : "");
    const effectiveName = repoNameFromRow(row, isHere, current.name);
    const normalizedPathKey = effectivePath ? identityKey(effectivePath, pathOptions) : "";
    const isCurrent = normalizedPathKey
      ? Boolean(currentKey && normalizedPathKey === currentKey)
      : isHere || Boolean(current.name && effectiveName.toLowerCase() === current.name.toLowerCase());

    const key = normalizedPathKey
      ? `path:${normalizedPathKey}`
      : isCurrent
        ? "current"
        : `name:${effectiveName.toLowerCase()}`;

    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        repoPath: effectivePath,
        repoName: effectiveName,
        isCurrent,
        items: [],
        rows: [],
        runningCount: 0,
        attentionCount: 0,
        doneCount: 0,
        collapsed: collapsedKeys ? collapsedKeys.has(key) : false,
      };
      groups.set(key, group);
      orderedKeys.push(key);
    } else {
      if (!group.repoPath && effectivePath) group.repoPath = effectivePath;
      if (isCurrent && !group.isCurrent) group.isCurrent = true;
    }

    group.items.push({ row, isHere });
    group.rows.push(row);
    if (isDockAgentRunning(row)) group.runningCount += 1;
    if (row.attention) group.attentionCount += 1;
    if (row.done) group.doneCount += 1;
  };

  for (const row of here) addItem(row, true);
  for (const row of elsewhere) addItem(row, false);

  return orderedKeys
    .map((key) => groups.get(key)!)
    .sort((a, b) => {
      if (a.isCurrent !== b.isCurrent) return a.isCurrent ? -1 : 1;
      const byName = a.repoName.localeCompare(b.repoName, undefined, { sensitivity: "base" });
      if (byName !== 0) return byName;
      return a.key.localeCompare(b.key);
    });
}

/** Check whether two repository paths refer to the same repository under platform rules. */
export function isSameDockRepo(
  a: string | null | undefined,
  b: string | null | undefined,
  options: PathIdentityOptions = defaultPathOptions(),
): boolean {
  if (!a || !b) return false;
  return sameRepo(a, b, options);
}
