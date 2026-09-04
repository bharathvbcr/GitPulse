/**
 * Working-tree file explorer data: build once per repo snapshot, flatten per
 * render. Pure TypeScript so the 100k-path scale contract is testable in
 * Node without a webview.
 *
 * Shape notes:
 * - `FileTree.files` holds full repo-relative paths; at the root a path has
 *   no directory component, so full path and bare name coincide there.
 * - Every accepted path is normalized (`\` → `/`) and segmented strictly:
 *   absolute paths, empty/dot/dot-dot segments, and whitespace-only inputs
 *   are rejected rather than coerced — a coerced traversal escape renders as
 *   a real row and becomes clickable.
 * - Ordering uses one shared `Intl.Collator` (numeric, base-insensitive) so
 *   unicode and spaced names sort identically in the tree, the flattened
 *   rows, and every re-run (the flatten pipeline is deterministic).
 */

export interface FileTreeDir {
  /** Final path segment; never empty. */
  name: string;
  /** Repo-relative path, no trailing slash. */
  path: string;
  dirs: FileTreeDir[];
  /** Full repo-relative paths of the files directly inside this dir. */
  files: string[];
}

export interface FileTree {
  dirs: FileTreeDir[];
  files: string[];
}

export interface FileRow {
  kind: "dir" | "file";
  /** Stable identity for keyed each-blocks; unique across the whole listing. */
  key: string;
  /** Repo-relative path (dirs have no trailing slash). */
  path: string;
  /** Display name: final path segment. */
  name: string;
  /** 0 at the root listing level. */
  depth: number;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** True when `raw` is a safe repo-relative path (no absolute, drive, or `..`). */
export function isValidRelativePath(raw: string): boolean {
  if (typeof raw !== "string") return false;
  const normalized = raw.replaceAll("\\", "/");
  if (normalized.trim().length === 0) return false;
  if (normalized.startsWith("/")) return false;
  // Windows drive letters ("C:/...") are absolute in spirit even after
  // slash normalization; git never emits them repo-relative, and accepting
  // one would fabricate a clickable "C:" dir that exists nowhere.
  if (/^[a-zA-Z]:/.test(normalized)) return false;
  const segments = normalized.split("/");
  if (segments.some((segment) => segment.length === 0)) return false;
  return segments.every((segment) => segment !== "." && segment !== "..");
}

/**
 * Builds the tree in O(total segments). Nested maps keyed by segment; null
 * marks "this is a file". Duplicate-heavy storms reuse existing nodes, so
 * 100k duplicates cost one 100-path build.
 */
export function buildFileTree(paths: readonly string[]): FileTree {
  // Recursive alias: a dir node maps segment -> nested node, and a file
  // claims its slot as null. The recursion is what lets ensureDir's
  // instanceof narrow return a properly-typed Node instead of an unknown.
  type Node = Map<string, Node | null>;
  const root: Node = new Map();

  function ensureDir(parent: Node, segment: string): Node {
    let child = parent.get(segment);
    if (!(child instanceof Map)) {
      child = new Map();
      parent.set(segment, child);
    }
    return child;
  }

  for (const raw of paths) {
    if (!isValidRelativePath(raw)) continue;
    const segments = raw.replaceAll("\\", "/").split("/");
    const fileName = segments[segments.length - 1];
    let cursor = root;
    for (let i = 0; i < segments.length - 1; i += 1) {
      cursor = ensureDir(cursor, segments[i]);
    }
    // A dir slot wins over a later file claim at the same name: git cannot
    // produce both, but hostile input can try, and dir wins keeps the walk
    // total (files under it would be lost otherwise).
    if (!cursor.has(fileName)) cursor.set(fileName, null);
  }

  const rootDirs: FileTreeDir[] = [];
  const rootFiles: string[] = [];
  const pending: Array<{ map: Node; dir: FileTreeDir }> = [];

  for (const [name, child] of root) {
    if (child instanceof Map) {
      const dir: FileTreeDir = { name, path: name, dirs: [], files: [] };
      rootDirs.push(dir);
      pending.push({ map: child, dir });
    } else {
      rootFiles.push(name);
    }
  }

  while (pending.length > 0) {
    const { map, dir } = pending.pop()!;
    const childDirs: FileTreeDir[] = [];
    const childFiles: string[] = [];
    for (const [name, child] of map) {
      if (child instanceof Map) {
        const nested: FileTreeDir = { name, path: `${dir.path}/${name}`, dirs: [], files: [] };
        childDirs.push(nested);
        pending.push({ map: child, dir: nested });
      } else {
        childFiles.push(`${dir.path}/${name}`);
      }
    }
    childDirs.sort((a, b) => collator.compare(a.name, b.name));
    childFiles.sort(collator.compare);
    dir.dirs = childDirs;
    dir.files = childFiles;
  }
  rootDirs.sort((a, b) => collator.compare(a.name, b.name));
  rootFiles.sort(collator.compare);

  return { dirs: rootDirs, files: rootFiles };
}

/**
 * Depth-first rows honoring collapsed dirs. Children interleave alphabetically
 * (one shared ordering per level, matching the tree sorts); collapsing a dir
 * hides its entire subtree.
 */
export function flattenFileTree(
  tree: FileTree,
  isCollapsed: (dirPath: string) => boolean,
): FileRow[] {
  const rows: FileRow[] = [];

  const emitChildren = (
    dirs: readonly FileTreeDir[],
    files: readonly string[],
    depth: number,
  ): void => {
    type Child =
      | { order: 0; name: string; dir: FileTreeDir }
      | { order: 1; name: string; file: string };
    const merged: Child[] = [
      ...dirs.map((dir): Child => ({ order: 0, name: dir.name, dir })),
      ...files.map((file): Child => ({
        order: 1,
        name: file.slice(file.lastIndexOf("/") + 1),
        file,
      })),
    ];
    merged.sort((a, b) => {
      const byName = collator.compare(a.name, b.name);
      return byName !== 0 ? byName : a.order - b.order;
    });

    for (const child of merged) {
      if (child.order === 0) {
        const dir = child.dir;
        rows.push({ kind: "dir", key: `d:${dir.path}`, path: dir.path, name: dir.name, depth });
        if (!isCollapsed(dir.path)) {
          emitChildren(dir.dirs, dir.files, depth + 1);
        }
      } else {
        rows.push({
          kind: "file",
          key: `f:${child.file}`,
          path: child.file,
          name: child.name,
          depth,
        });
      }
    }
  };

  emitChildren(tree.dirs, tree.files, 0);
  return rows;
}

/** Case-insensitive match on basename or full path; empty query keeps all. */
export function filterPathsByQuery(paths: readonly string[], query: string): string[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [...paths];
  return paths.filter((path) => {
    const lower = path.toLowerCase();
    if (lower.includes(needle)) return true;
    const base = lower.slice(lower.lastIndexOf("/") + 1);
    return base.includes(needle);
  });
}

/**
 * Joins a repository root and a repo-relative path for desktop openers.
 * Returns null instead of coercing a traversal escape into a clickable path.
 */
export function joinWorktreePath(repoPath: string, relative: string): string | null {
  if (typeof repoPath !== "string" || repoPath.trim().length === 0) return null;
  if (!isValidRelativePath(relative)) return null;
  const root = repoPath.replaceAll("\\", "/").replace(/\/+$/, "");
  if (!root || root === "/" || root === "//") return null;
  return `${root}/${relative.replaceAll("\\", "/")}`;
}

/** Ancestor dir paths of a repo-relative file path, nearest last. */
export function ancestorsOf(path: string): string[] {
  const ancestors: string[] = [];
  // Absolute or drive-qualified input is out of contract for a repo-relative
  // tree walk; slicing it anyway would fabricate filesystem ancestors
  // ("/etc", "C:") that no tree row can ever match — reject, don't coerce.
  if (path.startsWith("/") || /^[a-zA-Z]:/.test(path)) return ancestors;
  let cursor = path.lastIndexOf("/");
  while (cursor > 0) {
    ancestors.unshift(path.slice(0, cursor));
    cursor = path.slice(0, cursor).lastIndexOf("/");
  }
  return ancestors;
}

/**
 * Finds the exact visible parent directory for ARIA tree ArrowLeft behavior.
 * Flat status/churn sorts intentionally omit directory rows; in that mode
 * there is no selectable parent, so return -1 instead of trusting a stale
 * depth value and jumping to an unrelated file.
 */
export function parentDirectoryRowIndex(rows: readonly FileRow[], index: number): number {
  const row = rows[index];
  if (!row) return -1;
  const separator = row.path.lastIndexOf("/");
  if (separator <= 0) return -1;
  const parentPath = row.path.slice(0, separator);
  return rows.findIndex((candidate) => candidate.kind === "dir" && candidate.path === parentPath);
}
