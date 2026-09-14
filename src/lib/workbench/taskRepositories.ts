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
