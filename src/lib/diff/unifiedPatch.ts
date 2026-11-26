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

const utf8Encoder = new TextEncoder();
const utf8Decoder = new TextDecoder();

function unquoteGitPath(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed.startsWith('"') || !trimmed.endsWith('"') || trimmed.length < 2) {
    return trimmed;
  }
  const inner = trimmed.slice(1, -1);
  const bytes: number[] = [];
  for (let i = 0; i < inner.length; i += 1) {
    const ch = inner[i];
    if (ch === "\\" && i + 1 < inner.length) {
      const next = inner[i + 1];
      const oct = inner.slice(i + 1, i + 4);
      if (/^[0-7]{3}$/.test(oct)) {
        bytes.push(Number.parseInt(oct, 8));
        i += 3;
        continue;
      }
      const esc: Record<string, number> = {
        '"': 34,
        "\\": 92,
        a: 7,
        b: 8,
        f: 12,
        n: 10,
        r: 13,
        t: 9,
        v: 11,
      };
      if (Object.hasOwn(esc, next)) {
        bytes.push(esc[next]);
        i += 1;
        continue;
      }
    }
    for (const byte of utf8Encoder.encode(ch)) {
      bytes.push(byte);
    }
  }
  return utf8Decoder.decode(new Uint8Array(bytes));
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

const HUNK_RE = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/** Splits a git unified patch string into per-file sections with hunks and line numbers. */
export function splitUnifiedPatch(patch: string): UnifiedPatchFile[] {
  if (!patch) return [];

  const files: UnifiedPatchFile[] = [];
  let current: UnifiedPatchFile | null = null;
  let currentHunk: UnifiedDiffHunk | null = null;
  let oldCursor = 0;
  let newCursor = 0;
  let oldRemaining = 0;
  let newRemaining = 0;
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
    oldRemaining = 0;
    newRemaining = 0;
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

  const applyHunkHeader = (rawLine: string): UnifiedDiffHunk | null => {
    const match = HUNK_RE.exec(rawLine);
    if (!match) return null;
    oldCursor = Number.parseInt(match[1], 10);
    oldRemaining = match[2] === undefined ? 1 : Number.parseInt(match[2], 10);
    newCursor = Number.parseInt(match[3], 10);
    newRemaining = match[4] === undefined ? 1 : Number.parseInt(match[4], 10);
    const hunk: UnifiedDiffHunk = {
      header: rawLine,
      oldStart: oldCursor,
      newStart: newCursor,
      lines: [],
    };
    ensureFile().hunks.push(hunk);
    return hunk;
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
      if (rawLine.startsWith("\\")) {
        currentHunk.lines.push({
          kind: "meta",
          oldLine: null,
          newLine: null,
          text: rawLine,
        });
        continue;
      }
      if (oldRemaining === 0 && newRemaining === 0) {
        currentHunk = null;
      } else {
        if (rawLine.startsWith("@@ ")) {
          const nextHunk = applyHunkHeader(rawLine);
          if (nextHunk) {
            currentHunk = nextHunk;
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
          if (newRemaining > 0) newRemaining -= 1;
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
          if (oldRemaining > 0) oldRemaining -= 1;
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
          if (oldRemaining > 0) oldRemaining -= 1;
          if (newRemaining > 0) newRemaining -= 1;
          continue;
        }
        if (rawLine.length === 0) {
          // Empty context line inside an active hunk (whitespace-stripped patch).
          currentHunk.lines.push({
            kind: "ctx",
            oldLine: oldCursor,
            newLine: newCursor,
            text: "",
          });
          oldCursor += 1;
          newCursor += 1;
          if (oldRemaining > 0) oldRemaining -= 1;
          if (newRemaining > 0) newRemaining -= 1;
          continue;
        }
        currentHunk = null;
      }
    }

    // Inside file metadata (before first hunk or between files)
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
      if (current && current.hunks.length > 0) {
        finalizeFile();
      }
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
      currentHunk = applyHunkHeader(rawLine);
      continue;
    }
  }

  finalizeFile();
  return files;
}
