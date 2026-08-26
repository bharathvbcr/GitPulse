/**
 * Pure-TS tree model for the File Explorer: flat repo-relative paths
 * (as emitted by `git ls-files`) in, expandable FileTree out, plus a
 * render-order row flattener mirroring branches/flattenRows' contract.
 */

export interface FileTreeDir {
  /** Slash-joined path from repo root, e.g. "src/lib". Empty string means root-level container is NOT used; roots live in FileTree. */
  path: string;
  name: string;
  dirs: FileTreeDir[]; // sorted subdirectories
  files: string[]; // FULL paths of leaf files directly inside this dir, sorted
}

export interface FileTree {
  /** top-level directories, sorted */
  dirs: FileTreeDir[];
  /** full paths of root-level files, sorted */
  files: string[];
}

export type FileRow =
  | { kind: "dir"; depth: number; name: string; path: string; key: string }
  | { kind: "file"; depth: number; name: string; path: string; key: string };

// One collator shared by every sort site: constructing Intl.Collator per
// comparison is the expensive part, and grouping sorts thousands of names.
const nameCollator = new Intl.Collator();

/**
 * Defensive per-entry normalization. Returns slash-split segments ready for
 * insertion, or null when the entry must be SKIPPED ENTIRELY (fail closed):
 * absolute paths, any ".." segment, or entries that normalize to nothing.
 * Backslashes become slashes; ONE trailing slash is stripped (submodule
 * edge); empty and "." segments are dropped.
 */
function normalizeSegments(raw: string): string[] | null {
  if (!raw.trim()) return null;
  if (raw.startsWith("/")) return null;
  let s = raw.replaceAll("\\", "/");
  if (s.endsWith("/")) s = s.slice(0, -1);
  const parts: string[] = [];
  for (const seg of s.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") return null;
    parts.push(seg);
  }
  return parts.length === 0 ? null : parts;
}

/**
 * Path-indexed folder walk: one Map hit per path part instead of a linear
 * sibling scan, so building is O(total parts) across 100k+ paths — same
 * shape as groupBranches' ensureFolder. Duplicate normalized entries dedupe
 * silently via the seen-set. Sorting (dirs and files separately, collator
 * order everywhere) happens in one final recursive pass.
 */
export function buildFileTree(paths: readonly string[]): FileTree {
  const index = new Map<string, FileTreeDir>();
  const roots: FileTreeDir[] = [];
  const rootFiles = new Set<string>();
  const seen = new Set<string>();

  for (const raw of paths) {
    const parts = normalizeSegments(raw);
    if (!parts) continue;
    // A trailing slash (after backslash normalization) marks a DIRECTORY
    // entry — git ls-files emits submodules that way ("vendor/libfoo/").
    // It must materialize its folder chain but never become a leaf file.
    const isDirEntry = raw.replaceAll("\\", "/").endsWith("/");
    const joined = parts.join("/");
    if (!isDirEntry) {
      if (seen.has(joined)) continue;
      seen.add(joined);
    }

    // Directories to ensure exist for this entry: every segment for a dir
    // marker, the parent chain for a file leaf.
    const ancestorCount = isDirEntry ? parts.length : parts.length - 1;

    if (!isDirEntry && parts.length === 1) {
      rootFiles.add(parts[0]);
      continue;
    }

    let list = roots;
    let dir: FileTreeDir | undefined;
    let path = "";
    for (let i = 0; i < ancestorCount; i += 1) {
      const seg = parts[i];
      path = path ? `${path}/${seg}` : seg;
      dir = index.get(path);
      if (!dir) {
        dir = { path, name: seg, dirs: [], files: [] };
        list.push(dir);
        index.set(path, dir);
      }
      list = dir.dirs;
    }

    if (!isDirEntry) {
      dir!.files.push(joined);
    }
  }

  const sortDir = (dir: FileTreeDir): void => {
    dir.dirs.sort((a, b) => nameCollator.compare(a.name, b.name));
    dir.files.sort(nameCollator.compare);
    for (const child of dir.dirs) sortDir(child);
  };

  roots.sort((a, b) => nameCollator.compare(a.name, b.name));
  roots.forEach(sortDir);

  return { dirs: roots, files: [...rootFiles].sort(nameCollator.compare) };
}

/**
 * Flattens the tree into render-order rows for the explorer's scroller:
 * each dir emits its header row then — unless collapsed — its subdirs
 * (DFS) followed by its direct files; root-level files come last at
 * depth 0. Keys are `d:${path}` / `f:${path}` on the full slash path.
 * Row order is deterministic given the same tree + collapse state.
 */
export function flattenFileTree(tree: FileTree, isCollapsed: (dirPath: string) => boolean): FileRow[] {
  const rows: FileRow[] = [];

  const pushDir = (dir: FileTreeDir, depth: number): void => {
    rows.push({ kind: "dir", depth, name: dir.name, path: dir.path, key: `d:${dir.path}` });
    if (isCollapsed(dir.path)) return;
    for (const child of dir.dirs) pushDir(child, depth + 1);
    for (const file of dir.files) {
      rows.push({
        kind: "file",
        depth: depth + 1,
        name: leafNameOf(file),
        path: file,
        key: `f:${file}`,
      });
    }
  };

  for (const dir of tree.dirs) pushDir(dir, 0);
  for (const file of tree.files) {
    rows.push({ kind: "file", depth: 0, name: leafNameOf(file), path: file, key: `f:${file}` });
  }
  return rows;
}

/** Last "/"-separated segment of a full path (whole path when no slash). */
function leafNameOf(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/**
 * Case-insensitive SUBSTRING filter over full paths. A trimmed-empty query
 * (including whitespace-only) returns ALL paths as a fresh shallow copy in
 * input order — never null, never re-sorted; buildFileTree owns sorting.
 */
export function filterPathsByQuery(paths: readonly string[], query: string): string[] {
  const q = query.trim().toLowerCase();
  if (!q) return [...paths];
  return paths.filter((p) => p.toLowerCase().includes(q));
}

/**
 * Cumulative ancestor directory paths of `path`, nearest-last:
 * "src/lib/a.ts" → ["src", "src/lib"]; root-level files yield []. Empty
 * segments (leading/double/trailing slashes) are tolerated and dropped.
 */
export function ancestorsOf(path: string): string[] {
  const parts = path.split("/").filter((p) => p.length > 0);
  const result: string[] = [];
  let current = "";
  for (let i = 0; i < parts.length - 1; i += 1) {
    current = current ? `${current}/${parts[i]}` : parts[i];
    result.push(current);
  }
  return result;
}
