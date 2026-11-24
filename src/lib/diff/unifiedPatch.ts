/**
 * Pure unified-diff parser shared across PR file diffs (`gh pr diff`) and
 * agent turn-checkpoint diffs (`diff_turn_checkpoint`).
 */

export type DiffLineKind = "add" | "del" | "ctx" | "meta";

export interface UnifiedDiffLine {
  kind: DiffLineKind;
  oldLine: number | null;
  newLine: number | null;
  text: string;
}

export interface UnifiedDiffHunk {
  header: string;
  oldStart: number;
  newStart: number;
  lines: UnifiedDiffLine[];
}

export type UnifiedFileStatus = "added" | "deleted" | "renamed" | "binary" | "modified";

export interface UnifiedPatchFile {
  path: string;
  oldPath: string | null;
  status: UnifiedFileStatus;
  additions: number;
  deletions: number;
  binary: boolean;
  hunks: UnifiedDiffHunk[];
}

function unquoteGitPath(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.startsWith('"') && trimmed.endsWith('"') && trimmed.length >= 2) {
    return trimmed
      .slice(1, -1)
      .replace(/\\"/g, '"')
      .replace(/\\\\/g, "\\")
      .replace(/\\t/g, "\t");
  }
  return trimmed;
}

function stripDiffPrefix(raw: string, prefix: "a/" | "b/"): string {
  const clean = unquoteGitPath(raw);
  if (clean === "/dev/null") return clean;
  return clean.startsWith(prefix) ? clean.slice(prefix.length) : clean;
}

function parseDiffGitHeader(line: string): { oldPath: string; newPath: string } | null {
  const rest = line.slice("diff --git ".length).trim();
  if (!rest) return null;
  if (rest.startsWith('"')) {
    const firstEnd = rest.indexOf('" ', 1);
    if (firstEnd !== -1) {
      const aPart = rest.slice(0, firstEnd + 1);
      const bPart = rest.slice(firstEnd + 2).trim();
      return {
        oldPath: stripDiffPrefix(aPart, "a/"),
        newPath: stripDiffPrefix(bPart, "b/"),
      };
    }
  }
  const bMarker = rest.indexOf(" b/");
  if (bMarker > 0) {
    return {
      oldPath: stripDiffPrefix(rest.slice(0, bMarker), "a/"),
      newPath: stripDiffPrefix(rest.slice(bMarker + 1), "b/"),
    };
  }
  const parts = rest.split(/\s+/);
  if (parts.length >= 2) {
    return {
      oldPath: stripDiffPrefix(parts[0], "a/"),
      newPath: stripDiffPrefix(parts[1], "b/"),
    };
  }
  return null;
}

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

/** Splits a git unified patch string into per-file sections with hunks and line numbers. */
export function splitUnifiedPatch(patch: string): UnifiedPatchFile[] {
  if (!patch) return [];

  const files: UnifiedPatchFile[] = [];
  let current: UnifiedPatchFile | null = null;
  let currentHunk: UnifiedDiffHunk | null = null;
  let oldCursor = 0;
  let newCursor = 0;
  let sawNewMode = false;
  let sawDeletedMode = false;
  let renameFrom: string | null = null;
  let renameTo: string | null = null;

  const finalizeFile = () => {
    if (!current) return;
    if (current.binary) {
      current.status = "binary";
    } else if (renameTo !== null && renameFrom !== null && renameFrom !== renameTo) {
      current.status = "renamed";
      current.oldPath = renameFrom;
      current.path = renameTo;
    } else if (sawDeletedMode) {
      current.status = "deleted";
    } else if (sawNewMode) {
      current.status = "added";
    }
    files.push(current);
    current = null;
    currentHunk = null;
    sawNewMode = false;
    sawDeletedMode = false;
    renameFrom = null;
    renameTo = null;
  };

  const ensureFile = (fallbackPath = "patch"): UnifiedPatchFile => {
    if (!current) {
      current = {
        path: fallbackPath,
        oldPath: null,
        status: "modified",
        additions: 0,
        deletions: 0,
        binary: false,
        hunks: [],
      };
    }
    return current;
  };

  for (const rawLine of patch.split(/\r?\n/)) {
    if (rawLine.startsWith("diff --git ")) {
      finalizeFile();
      const parsed = parseDiffGitHeader(rawLine);
      const oldPath = parsed?.oldPath ?? "patch";
      const newPath = parsed?.newPath ?? oldPath;
      current = {
        path: newPath,
        oldPath: oldPath !== newPath ? oldPath : null,
        status: "modified",
        additions: 0,
        deletions: 0,
        binary: false,
        hunks: [],
      };
      continue;
    }

    if (currentHunk !== null) {
      if (rawLine.startsWith("@@ ")) {
        const match = HUNK_RE.exec(rawLine);
        if (match) {
          oldCursor = Number.parseInt(match[1], 10);
          newCursor = Number.parseInt(match[2], 10);
          currentHunk = {
            header: rawLine,
            oldStart: oldCursor,
            newStart: newCursor,
            lines: [],
          };
          ensureFile().hunks.push(currentHunk);
          continue;
        }
      }
      if (rawLine.startsWith("+")) {
        const file = ensureFile();
        file.additions += 1;
        currentHunk.lines.push({
          kind: "add",
          oldLine: null,
          newLine: newCursor,
          text: rawLine.slice(1),
        });
        newCursor += 1;
        continue;
      }
      if (rawLine.startsWith("-")) {
        const file = ensureFile();
        file.deletions += 1;
        currentHunk.lines.push({
          kind: "del",
          oldLine: oldCursor,
          newLine: null,
          text: rawLine.slice(1),
        });
        oldCursor += 1;
        continue;
      }
      if (rawLine.startsWith(" ")) {
        currentHunk.lines.push({
          kind: "ctx",
          oldLine: oldCursor,
          newLine: newCursor,
          text: rawLine.slice(1),
        });
        oldCursor += 1;
        newCursor += 1;
        continue;
      }
      if (rawLine.startsWith("\\")) {
        currentHunk.lines.push({
          kind: "meta",
          oldLine: null,
          newLine: null,
          text: rawLine,
        });
        continue;
      }
      if (rawLine.length === 0) {
        // Trailing newline at end of patch, or blank context line missing leading space.
        continue;
      }
    }

    // Inside file metadata (before first hunk or between hunks)
    if (rawLine.startsWith("new file mode ")) {
      ensureFile();
      sawNewMode = true;
      continue;
    }
    if (rawLine.startsWith("deleted file mode ")) {
      ensureFile();
      sawDeletedMode = true;
      continue;
    }
    if (rawLine.startsWith("rename from ")) {
      ensureFile();
      renameFrom = unquoteGitPath(rawLine.slice("rename from ".length));
      continue;
    }
    if (rawLine.startsWith("rename to ")) {
      ensureFile();
      renameTo = unquoteGitPath(rawLine.slice("rename to ".length));
      continue;
    }
    if (rawLine === "GIT binary patch" || (rawLine.startsWith("Binary files ") && rawLine.endsWith(" differ"))) {
      ensureFile().binary = true;
      continue;
    }
    if (rawLine.startsWith("--- ")) {
      const fromPath = stripDiffPrefix(rawLine.slice(4), "a/");
      if (!current) {
        ensureFile(fromPath === "/dev/null" ? "patch" : fromPath);
      }
      if (fromPath === "/dev/null") {
        sawNewMode = true;
      } else if (current && current.oldPath === null) {
        current.oldPath = fromPath;
      }
      continue;
    }
    if (rawLine.startsWith("+++ ")) {
      const toPath = stripDiffPrefix(rawLine.slice(4), "b/");
      const file = ensureFile(toPath === "/dev/null" ? "patch" : toPath);
      if (toPath === "/dev/null") {
        sawDeletedMode = true;
        if (file.oldPath && file.oldPath !== "/dev/null") {
          file.path = file.oldPath;
        }
      } else {
        file.path = toPath;
      }
      continue;
    }
    if (rawLine.startsWith("@@ ")) {
      const match = HUNK_RE.exec(rawLine);
      if (match) {
        oldCursor = Number.parseInt(match[1], 10);
        newCursor = Number.parseInt(match[2], 10);
        currentHunk = {
          header: rawLine,
          oldStart: oldCursor,
          newStart: newCursor,
          lines: [],
        };
        ensureFile().hunks.push(currentHunk);
      }
      continue;
    }
  }

  finalizeFile();
  return files;
}
