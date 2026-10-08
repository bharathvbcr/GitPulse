import { plural } from "../format";

/** True when `candidate` is `path` itself or lies inside the directory `path`. */
export function isAtOrUnder(candidate: string, path: string): boolean {
  return candidate === path || candidate.startsWith(`${path}/`);
}

/**
 * Where a selection lands after `from` moved to `to`: the same file at its
 * new place, a file inside a moved directory at its new place, or unchanged.
 */
export function remapPath(selected: string, from: string, to: string): string {
  if (!isAtOrUnder(selected, from)) return selected;
  return `${to}${selected.slice(from.length)}`;
}

/**
 * The confirmation a file-tree delete shows.
 *
 * `files` is the explorer's listing (tracked plus untracked, never ignored),
 * and `untracked` the set of those git does not track. Untracked files are
 * named as unrecoverable because git holds no copy of them; tracked ones are
 * restorable until the deletion is committed. Ignored files are never in the
 * listing and the backend never deletes them, which the message says.
 */
export function describeDelete(
  path: string,
  kind: "file" | "dir",
  files: readonly string[],
  untracked: ReadonlySet<string>,
): string {
  const inside = files.filter((file) => isAtOrUnder(file, path));
  const lost = inside.filter((file) => untracked.has(file)).length;
  const kept = inside.length - lost;
  const lines = [
    kind === "dir"
      ? `Delete the folder ${path} and the ${plural(inside.length, "file")} it shows here?`
      : `Delete ${path}?`,
  ];
  if (kept > 0) {
    lines.push(
      `Tracked (${plural(kept, "file")}): removed with git rm and staged — restorable from HEAD until you commit. Uncommitted changes to any of them make git refuse the whole delete.`,
    );
  }
  if (lost > 0) {
    lines.push(
      `Untracked (${plural(lost, "file")}): removed with git clean. Git has no copy, so this cannot be undone.`,
    );
  }
  if (kind === "dir") lines.push("Ignored files inside it are left in place.");
  return lines.join("\n\n");
}
