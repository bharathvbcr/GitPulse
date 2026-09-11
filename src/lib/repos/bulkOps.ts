import type { FileStatus, IndexAction } from "../stores/repoStore";

/** A displayed rename names two index entries. Copies retain their source. */
export function indexSelectionPaths(files: readonly FileStatus[], kind: IndexAction): string[] {
  return [...new Set(files.flatMap((file) =>
    kind === "unstage" && file.status_code.startsWith("R") && file.old_path
      ? [file.path, file.old_path]
      : [file.path]))];
}
