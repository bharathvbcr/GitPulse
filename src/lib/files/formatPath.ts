/**
 * Splits a file path into its parent directory prefix (if any) and its basename.
 * Used across file lists (Sidebar, CommitDetails, etc.) to visually emphasize
 * the filename while keeping directory context subtle.
 */
export interface PathParts {
  dir: string;
  name: string;
}

export function formatPathParts(filePath: string): PathParts {
  if (!filePath) {
    return { dir: "", name: "" };
  }

  const normalized = filePath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");

  if (lastSlash === -1) {
    return { dir: "", name: normalized };
  }

  return {
    dir: normalized.slice(0, lastSlash + 1),
    name: normalized.slice(lastSlash + 1),
  };
}
