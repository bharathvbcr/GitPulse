export type DiffChunkKind = "Equal" | "Added" | "Removed";

export interface DiffSegment {
  kind: DiffChunkKind;
  text: string;
}

export interface IntraLineDiff {
  original_segments: DiffSegment[];
  modified_segments: DiffSegment[];
}

const MAX_TOKENS = 500;
const MAX_LINE_CHARS = 50_000;

function tokenizeLine(line: string, maxTokens: number): string[] {
  const tokens: string[] = [];
  let i = 0;
  while (i < line.length) {
    if (tokens.length >= maxTokens) {
      tokens.push(line.slice(i));
      break;
    }
    const start = i;
    const ch = line[i];
    const isAlnum = isWordChar(ch);
    i += 1;
    while (i < line.length && isWordChar(line[i]) === isAlnum) {
      i += 1;
    }
    tokens.push(line.slice(start, i));
  }
  return tokens;
}

function isWordChar(ch: string): boolean {
  return /[A-Za-z0-9_]/.test(ch);
}

function computeLcsTable(a: string[], b: string[]): number[][] {
  const m = a.length;
  const n = b.length;
  const table: number[][] = Array.from({ length: m + 1 }, () => Array(n + 1).fill(0));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      table[i][j] = a[i - 1] === b[j - 1] ? table[i - 1][j - 1] + 1 : Math.max(table[i - 1][j], table[i][j - 1]);
    }
  }
  return table;
}

function mergeConsecutive(segments: DiffSegment[]): DiffSegment[] {
  const merged: DiffSegment[] = [];
  for (const seg of segments) {
    const last = merged[merged.length - 1];
    if (last && last.kind === seg.kind) {
      last.text += seg.text;
    } else {
      merged.push({ ...seg });
    }
  }
  return merged;
}

export function computeWordDiff(oldLine: string, newLine: string): IntraLineDiff {
  if (oldLine === newLine) {
    return {
      original_segments: [{ kind: "Equal", text: oldLine }],
      modified_segments: [{ kind: "Equal", text: newLine }],
    };
  }
  if (oldLine.length > MAX_LINE_CHARS || newLine.length > MAX_LINE_CHARS) {
    return {
      original_segments: [{ kind: "Removed", text: oldLine }],
      modified_segments: [{ kind: "Added", text: newLine }],
    };
  }

  const oldTokens = tokenizeLine(oldLine, MAX_TOKENS);
  const newTokens = tokenizeLine(newLine, MAX_TOKENS);
  const lcs = computeLcsTable(oldTokens, newTokens);

  let i = oldTokens.length;
  let j = newTokens.length;
  const origRev: DiffSegment[] = [];
  const modRev: DiffSegment[] = [];

  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && oldTokens[i - 1] === newTokens[j - 1]) {
      origRev.push({ kind: "Equal", text: oldTokens[i - 1] });
      modRev.push({ kind: "Equal", text: newTokens[j - 1] });
      i -= 1;
      j -= 1;
    } else if (j > 0 && (i === 0 || lcs[i][j - 1] >= lcs[i - 1][j])) {
      modRev.push({ kind: "Added", text: newTokens[j - 1] });
      j -= 1;
    } else if (i > 0) {
      origRev.push({ kind: "Removed", text: oldTokens[i - 1] });
      i -= 1;
    }
  }

  origRev.reverse();
  modRev.reverse();
  return {
    original_segments: mergeConsecutive(origRev),
    modified_segments: mergeConsecutive(modRev),
  };
}

export interface AnnotatedDiffLine {
  type: "add" | "del" | "ctx" | "hdr" | "meta" | "binary";
  content: string;
  /** Line number in the old file, when the hunk header knows it. */
  oldNo?: number;
  /** Line number in the new file, when the hunk header knows it. */
  newNo?: number;
  segments?: DiffSegment[];
}

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

/**
 * Commit-level metadata that `git show` interleaves with per-file patches.
 * None of it belongs to a file's line stream; rendering it as fake context
 * rows corrupted commit diffs and skewed every count derived from them.
 */
const META_PREFIXES = [
  "diff --git ",
  "index ",
  "new file mode ",
  "deleted file mode ",
  "old mode ",
  "new mode ",
  "rename from ",
  "rename to ",
  "copy from ",
  "copy to ",
  "similarity index ",
  "dissimilarity index ",
];

/**
 * Classifies one raw diff line as commit metadata (`"meta"`), a binary-file
 * notice (`"binary"`), or real patch content (`null`). Binary notices are a
 * distinct kind so the UI can route image pairs to the image differ instead
 * of showing an opaque "differ" line. Beyond the known metadata prefixes,
 * anything unprefixed is classified as meta: a well-formed patch body line
 * always starts with one of `+ - space \ @`, so unprefixed text interleaved
 * with patches (`git show` commit prose, unknown future metadata) is never
 * file content.
 */
const PATCH_BODY_PREFIX_RE = /^[-+ \\@]/;

export function classifyMetaLine(line: string): "meta" | "binary" | null {
  if (!line) return null;
  if (line.startsWith("Binary files ") || line.startsWith("GIT binary patch")) return "binary";
  if (META_PREFIXES.some((prefix) => line.startsWith(prefix))) return "meta";
  return PATCH_BODY_PREFIX_RE.test(line) ? null : "meta";
}

/**
 * Parses a unified diff into light rows without any intra-line work.
 *
 * Word-diffing every changed pair up front is quadratic work on an agent's
 * massive commits; the renderer calls [`annotateRange`] for just the visible
 * window instead. Hunk headers are tracked here, so line numbers come from
 * git rather than being faked with row indices.
 */
export function parseUnifiedDiff(raw: string): AnnotatedDiffLine[] {
  const out: AnnotatedDiffLine[] = [];
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;
  for (const line of (raw || "").split("\n")) {
    if (line.startsWith("@@")) {
      const match = HUNK_RE.exec(line);
      if (match) {
        oldNo = parseInt(match[1], 10);
        newNo = parseInt(match[2], 10);
      }
      out.push({ type: "hdr", content: line });
      inHunk = true;
      continue;
    }
    const metaKind = classifyMetaLine(line);
    if (metaKind) {
      if (line.startsWith("diff --git ")) inHunk = false;
      out.push({ type: metaKind, content: line });
      continue;
    }
    // File headers only ever appear between `diff --git` and the first `@@`.
    // Inside a hunk body these prefixes are deletions/additions whose content
    // itself begins with dashes/plus signs (markdown rules, YAML fences).
    if (!inHunk && (line.startsWith("+++") || line.startsWith("---"))) {
      out.push({ type: "hdr", content: line });
      continue;
    }
    if (line.startsWith("+")) {
      out.push({ type: "add", content: line, newNo: newNo || undefined });
      if (newNo) newNo += 1;
    } else if (line.startsWith("-")) {
      out.push({ type: "del", content: line, oldNo: oldNo || undefined });
      if (oldNo) oldNo += 1;
    } else if (line.startsWith("\\")) {
      // "\ No newline at end of file"
      out.push({ type: "hdr", content: line });
    } else if (inHunk) {
      out.push({
        type: "ctx",
        content: line,
        oldNo: oldNo || undefined,
        newNo: newNo || undefined,
      });
      if (oldNo) oldNo += 1;
      if (newNo) newNo += 1;
    } else {
      // Outside a hunk body nothing can be file content: this is `git show`
      // prose or unknown metadata, so it must not masquerade as context.
      out.push({ type: "meta", content: line });
    }
  }
  return out;
}

/**
 * Annotates one window of parsed lines with word-diff segments.
 *
 * Only pairs fully inside `[start, end)` are computed, so the cost scales
 * with what is on screen rather than with the size of the diff. Returns the
 * same array object when the range holds no adjacent del/add pairs.
 */
export function annotateRange(
  lines: AnnotatedDiffLine[],
  start: number,
  end: number
): AnnotatedDiffLine[] {
  const lo = Math.max(0, start);
  const hi = Math.min(lines.length, end);
  const slice = lines.slice(lo, hi);
  let i = 0;
  while (i < slice.length) {
    if (slice[i].type === "del") {
      const delStart = i;
      while (i < slice.length && slice[i].type === "del") {
        i += 1;
      }
      const addStart = i;
      while (i < slice.length && slice[i].type === "add") {
        i += 1;
      }
      const delCount = addStart - delStart;
      const addCount = i - addStart;
      const pairCount = Math.min(delCount, addCount);
      for (let k = 0; k < pairCount; k++) {
        const delLine = slice[delStart + k];
        const addLine = slice[addStart + k];
        if (!delLine.segments && !addLine.segments) {
          const diff = computeWordDiff(delLine.content.slice(1), addLine.content.slice(1));
          delLine.segments = diff.original_segments;
          addLine.segments = diff.modified_segments;
        }
      }
    } else {
      i += 1;
    }
  }
  return slice;
}

export function isImagePath(path: string | null | undefined): boolean {
  if (!path) return false;
  return /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(path);
}

/**
 * Annotates an entire diff in one pass. Only for small diffs: large ones must
 * go through `parseUnifiedDiff` plus per-window `annotateRange` calls.
 */
export function annotateUnifiedDiff(raw: string): AnnotatedDiffLine[] {
  const parsed = parseUnifiedDiff(raw);
  return annotateRange(parsed, 0, parsed.length);
}

/** True when the line is a per-file section opener: `diff --git a/X b/Y`. */
function isFileHeader(line: string): boolean {
  return line.startsWith("diff --git ");
}

function stripGitSide(side: string): string {
  return side.startsWith("a/") || side.startsWith("b/") ? side.slice(2) : side;
}

/** Splits `a/<old> b/<new>` out of a `diff --git` line, quoted or bare. */
function gitHeaderSides(header: string): [string, string] | null {
  const rest = header.slice("diff --git ".length);
  const quoted = /^"a\/(.*)" "b\/(.*)"$/.exec(rest);
  if (quoted) return [quoted[1], quoted[2]];
  const bare = /^(.*?) b\/(.*)$/.exec(rest);
  if (bare) return [bare[1], bare[2]];
  return null;
}

function sectionMentionsPath(section: string[], path: string): boolean {
  const header = section.find(isFileHeader);
  if (!header) return false;
  const sides = gitHeaderSides(header);
  const headerMatch =
    !!sides &&
    (sides[0] === path ||
      sides[1] === path ||
      stripGitSide(sides[0]) === path ||
      stripGitSide(sides[1]) === path);
  if (headerMatch) return true;
  // Timestamps ride after a tab: "+++ b/src/main.rs<TAB>2024-01-01 ...".
  const marked = (marker: string): boolean =>
    section.some((line) => {
      if (!line.startsWith(marker)) return false;
      const rest = line.slice(marker.length);
      return rest === path || rest.startsWith(`${path}\t`);
    });
  // Rename/copy targets run to end of line: no timestamp suffix.
  const exact = (marker: string): boolean =>
    section.some((line) => line === `${marker}${path}`);
  return (
    marked("--- a/") || marked("+++ b/") || exact("rename to ") || exact("copy to ")
  );
}

/**
 * Reduces a multi-file diff (as produced by `cmd_get_commit_diff`) to the
 * patch of a single file, keeping each file's metadata block attached to its
 * hunks. The backend command takes no path filter, so commit-file selection
 * filters here; sections are matched by their `diff --git` header sides plus
 * +++/--- fallbacks so renames resolve too. Returns "" when the path is not
 * part of the diff.
 */
export function filterFilePatch(raw: string, path: string): string {
  if (!raw || !path) return "";
  const lines = raw.split("\n");
  const sections: string[][] = [];
  let preamble: string[] | null = null;
  for (const line of lines) {
    if (isFileHeader(line)) {
      sections.push([line]);
    } else if (sections.length > 0) {
      sections[sections.length - 1].push(line);
    } else {
      (preamble ??= []).push(line);
    }
  }
  return sections
    .filter((section) => sectionMentionsPath(section, path))
    .map((section) => section.join("\n"))
    .join("\n");
}

/** Empty-state copy for the diff pane, distinguishing clean merges. */
export function emptyDiffCopy(isMerge: boolean): { title: string; hint: string } {
  if (isMerge) {
    return {
      title: "Merge commit — no textual diff",
      hint:
        "Git omits the combined diff for merges that resolved cleanly. Open one of its parents or pick a changed file from the commit details.",
    };
  }
  return {
    title: "No diff selected",
    hint: "Select a changed file from the sidebar or a commit from the graph to view diffs.",
  };
}
