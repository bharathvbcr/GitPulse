/**
 * The task sheet's repository picker.
 *
 * Linking a repository is the one thing a task cannot be saved without, so the
 * picker sits first in the sheet and has to stay readable at any catalog size.
 * Two rules make that safe, and both live here rather than in the markup:
 *
 * 1. A filter never hides a linked repository. Hiding a chosen row makes the
 *    control lie about what the task links to.
 * 2. Rows keep catalog order. Sorting linked rows to the top would move a row
 *    out from under the pointer that just checked it.
 */

export interface PickerRepository {
  id: string;
  name: string;
}

export interface PickerRow {
  id: string;
  name: string;
  linked: boolean;
  primary: boolean;
  /** A member of the task's home workspace. Always false with no home workspace. */
  member: boolean;
  /** Shown despite not matching the filter, because it is linked. */
  keptByLink: boolean;
}

export interface LinkSummary {
  linked: number;
  /** Display name of the primary repository, or null when none is chosen. */
  primaryName: string | null;
  /** Linked ids that are not members of the home workspace, in link order. */
  outsiders: string[];
  /** Linked ids this page of the catalog does not carry. */
  unknown: string[];
}

/** Above this many repositories the picker offers a filter. */
export const FILTER_THRESHOLD = 6;

export function shouldOfferFilter(count: number): boolean {
  return count > FILTER_THRESHOLD;
}

function matches(name: string, needle: string): boolean {
  return name.toLowerCase().includes(needle);
}

export function repositoryRows(
  known: readonly PickerRepository[],
  linked: readonly string[],
  primary: string,
  members: readonly string[] | null,
  filter: string,
): PickerRow[] {
  const chosen = new Set(linked);
  const memberIds = new Set(members ?? []);
  // Plain substring, never a regex: the filter runs on every keystroke over
  // the whole catalog, and a reader's `(` must not be a pattern.
  const needle = filter.trim().toLowerCase();
  const rows: PickerRow[] = [];
  for (const repo of known) {
    const linkedHere = chosen.has(repo.id);
    const hit = !needle || matches(repo.name, needle);
    if (!hit && !linkedHere) continue;
    rows.push({
      id: repo.id,
      name: repo.name,
      linked: linkedHere,
      primary: linkedHere && repo.id === primary,
      member: members !== null && memberIds.has(repo.id),
      keptByLink: linkedHere && !hit,
    });
  }
  return rows;
}

/** One repository as the closed trigger draws it. */
export interface TriggerChip {
  id: string;
  name: string;
  primary: boolean;
}

/** How many chips the trigger draws before it collapses the rest to a count. */
export const CHIP_LIMIT = 3;

/**
 * What the closed trigger shows.
 *
 * The trigger has to answer both questions without being opened — which
 * repositories are linked, and which one is primary — and it has a fixed width
 * to do it in, because the sheet's dock can be dragged down to 380px. So the
 * list is capped and the remainder becomes a count.
 *
 * **The primary chip is never the one that gets collapsed.** Dropping it would
 * leave the trigger answering the easy question and hiding the one that
 * actually decides where an agent runs, which is the whole reason the control
 * is not just a count. It is drawn first for the same reason.
 *
 * A linked id this page of the catalog does not carry has no name to draw, so
 * it is left to `linkSummary().unknown` and the notice the sheet puts under
 * the trigger rather than rendered as a chip reading like a UUID.
 */
export function triggerChips(
  known: readonly PickerRepository[],
  linked: readonly string[],
  primary: string,
  limit = CHIP_LIMIT,
): { chips: TriggerChip[]; overflow: number } {
  const names = new Map(known.map((repo) => [repo.id, repo.name]));
  const named = linked.filter((id) => names.has(id));
  const ordered = [...named].sort((a, b) => Number(b === primary) - Number(a === primary));
  // `Math.max(1, NaN)` is NaN, and `slice(0, NaN)` is empty — a caller passing
  // a bad limit would have emptied the trigger rather than narrowed it.
  const requested = Math.floor(limit);
  const room = Number.isFinite(requested) ? Math.max(1, requested) : 1;
  return {
    chips: ordered.slice(0, room).map((id) => ({ id, name: names.get(id) ?? id, primary: id === primary })),
    overflow: Math.max(0, ordered.length - room),
  };
}

/** A run of rows under one heading. */
export interface PickerGroup {
  id: "linked" | "workspace" | "other";
  label: string;
  rows: PickerRow[];
}

/**
 * Rows split into the three answers a reader is actually choosing between.
 *
 * A flat list makes a reader read every name to find the two they linked. The
 * split does not reorder within a group — `repositoryRows` keeps catalog order
 * on purpose, so a row never moves out from under the pointer that just
 * checked it — and an empty group is dropped rather than drawn as a heading
 * with nothing under it.
 *
 * "In this workspace" only exists when the task has a home workspace to be a
 * member of; with none, `member` is false everywhere and every unlinked row is
 * simply "other".
 */
export function groupRows(rows: readonly PickerRow[]): PickerGroup[] {
  const groups: PickerGroup[] = [
    { id: "linked", label: "Linked", rows: [] },
    { id: "workspace", label: "In this workspace", rows: [] },
    { id: "other", label: "Other repositories", rows: [] },
  ];
  for (const row of rows) {
    if (row.linked) groups[0].rows.push(row);
    else if (row.member) groups[1].rows.push(row);
    else groups[2].rows.push(row);
  }
  return groups.filter((group) => group.rows.length > 0);
}

export function linkSummary(
  known: readonly PickerRepository[],
  linked: readonly string[],
  primary: string,
  members: readonly string[] | null,
): LinkSummary {
  const names = new Map(known.map((repo) => [repo.id, repo.name]));
  const memberIds = new Set(members ?? []);
  const outsiders: string[] = [];
  const unknown: string[] = [];
  for (const id of linked) {
    if (!names.has(id)) unknown.push(id);
    if (members !== null && !memberIds.has(id)) outsiders.push(id);
  }
  return {
    linked: linked.length,
    primaryName: primary ? names.get(primary) ?? primary : null,
    outsiders,
    unknown,
  };
}

/**
 * What the picker says about the current links, in one line.
 *
 * The list itself scrolls, so this line is the only place the answer is
 * guaranteed to be on screen. It never reports a primary the task does not
 * have — an unchosen primary is a refusal to save, not a detail.
 */
export function summaryLine(summary: LinkSummary): string {
  if (summary.linked === 0) return "No repository linked yet — a task needs one.";
  const plural = summary.linked === 1 ? "repository" : "repositories";
  const primary = summary.primaryName ? `primary ${summary.primaryName}` : "no primary chosen";
  return `${summary.linked} ${plural} linked · ${primary}`;
}

/** How the sheet offers to close the gap between a task's links and its workspace. */
export function outsiderLine(outsiders: readonly string[], workspaceName: string): string {
  const count = outsiders.length;
  const plural = count === 1 ? "repository is" : "repositories are";
  return `${count} linked ${plural} not in ${workspaceName}.`;
}

/**
 * What a workspace row in the navigator says about its membership.
 *
 * An empty workspace is the one a reader has to act on, and it used to look
 * exactly like a full one until they clicked it and found a dead New task.
 */
export function workspaceMembershipLabel(count: number): string {
  if (count <= 0) return "no repositories yet";
  return `${count} ${count === 1 ? "repository" : "repositories"}`;
}
