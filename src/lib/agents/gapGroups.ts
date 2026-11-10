/**
 * The Agents plane's read notes, folded for display.
 *
 * When every repository fails the same way, one note per repository is a wall
 * of identical sentences. A group keeps every repository's name and the exact
 * reason they share. It only merges notes that are the same fact about
 * different repositories (same kind, same reason). Notes that belong to no
 * repository, like the task store or the registered list, stay one per line.
 */
import { plural } from "../format";
import type { GapKind } from "./plane";

export interface GapNote {
  repoPath: string;
  label: string;
  kind: GapKind;
  reason: string;
}

export interface GapGroup {
  key: string;
  kind: GapKind;
  reason: string;
  /** Every note's label, in the order the notes came. Never trimmed; the view wraps them. */
  labels: string[];
  /** True when the group holds repository notes, so its title can count them. */
  repositories: boolean;
}

export function groupGaps(notes: readonly GapNote[]): GapGroup[] {
  const groups: GapGroup[] = [];
  const byKey = new Map<string, GapGroup>();
  notes.forEach((note, index) => {
    const repository = note.repoPath !== "";
    const key = repository ? `repo\u0000${note.kind}\u0000${note.reason}` : `note\u0000${index}`;
    const existing = byKey.get(key);
    if (existing) {
      existing.labels.push(note.label);
      return;
    }
    const group: GapGroup = { key, kind: note.kind, reason: note.reason, labels: [note.label], repositories: repository };
    byKey.set(key, group);
    groups.push(group);
  });
  return groups;
}

/** "alpha" for one note, "13 repositories" for a merged group. */
export function gapGroupTitle(group: GapGroup): string {
  if (group.labels.length === 1) return group.labels[0];
  return plural(group.labels.length, "repository", "repositories");
}

/** True when any group is a read that failed or never ran, not just a partial one. */
export function gapsBlockReading(groups: readonly GapGroup[]): boolean {
  return groups.some((group) => group.kind === "failed" || group.kind === "skipped");
}
