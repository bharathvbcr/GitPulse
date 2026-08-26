import type { AnnotatedDiffLine } from "./wordDiff";

export type DiffLineType = "Context" | "Addition" | "Deletion";

export interface UnifiedDiffLine {
  line_type: DiffLineType;
  old_line_no?: number;
  new_line_no?: number;
  content: string;
  is_selected: boolean;
  /**
   * Carried from parse time (`AnnotatedDiffLine.noNewline`): the file side
   * this row belongs to lacks a trailing newline, so a patch serializer must
   * emit `\ No newline at end of file` after this line. serde ignores
   * unknown fields, so the extra key is inert until the Rust serializer
   * consumes it.
   */
  no_newline?: boolean;
}

export interface UnifiedDiffHunk {
  old_start: number;
  old_lines: number;
  new_start: number;
  new_lines: number;
  header: string;
  lines: UnifiedDiffLine[];
}

export interface FilePatch {
  old_path: string;
  new_path: string;
  hunks: UnifiedDiffHunk[];
}

const HUNK_HEADER_RE = /^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@/;

/**
 * Parses a raw `@@ -X,Y +A,B @@` header string to extract starting numbers and line counts.
 */
export function parseHunkHeaderNumbers(header: string): {
  old_start: number;
  old_lines: number;
  new_start: number;
  new_lines: number;
} {
  const match = HUNK_HEADER_RE.exec(header);
  if (!match) {
    return { old_start: 1, old_lines: 0, new_start: 1, new_lines: 0 };
  }
  return {
    old_start: parseInt(match[1], 10),
    old_lines: match[2] !== undefined ? parseInt(match[2], 10) : 1,
    new_start: parseInt(match[3], 10),
    new_lines: match[4] !== undefined ? parseInt(match[4], 10) : 1,
  };
}

// ---------------------------------------------------------------------------
// Authoritative paths from parsed ---/+++ headers
// ---------------------------------------------------------------------------

/**
 * Decodes git's C-style quoted path body (the text between the double
 * quotes). Git escapes every non-ASCII byte as \NNN octal, so the escapes
 * are BYTES that must be reassembled as UTF-8 — decoding each escape to an
 * isolated code unit would turn \303\251 into "Ã©" instead of "é".
 */
function decodeQuotedGitPath(body: string): string {
  const bytes: number[] = [];
  for (let i = 0; i < body.length; i += 1) {
    const ch = body[i];
    if (ch !== "\\") {
      bytes.push(body.charCodeAt(i));
      continue;
    }
    i += 1;
    const esc = body[i];
    if (esc === undefined) break;
    if (esc >= "0" && esc <= "7") {
      let value = 0;
      let digits = 0;
      while (digits < 3 && i < body.length && body[i] >= "0" && body[i] <= "7") {
        value = value * 8 + (body.charCodeAt(i) - 48);
        i += 1;
        digits += 1;
      }
      i -= 1;
      bytes.push(value & 0xff);
    } else if (esc === "n") bytes.push(10);
    else if (esc === "t") bytes.push(9);
    else if (esc === "r") bytes.push(13);
    else if (esc === '"') bytes.push(34);
    else if (esc === "\\") bytes.push(92);
    else bytes.push(esc.charCodeAt(0));
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes));
  } catch {
    return bytes.map((b) => String.fromCharCode(b)).join("");
  }
}

/**
 * Extracts the path from a raw `--- `/`+++ ` header line, dropping any
 * timestamp git appends after a tab and unwrapping git's quoted form.
 */
function parseHeaderPath(headerLine: string): string | null {
  let rest = headerLine.slice(4);
  const tab = rest.indexOf("\t");
  if (tab >= 0) rest = rest.slice(0, tab);
  // CRLF diff text leaves a stray \r on the header line; strip exactly that.
  rest = rest.replace(/\r$/, "");
  if (rest.startsWith('"') && rest.endsWith('"') && rest.length >= 2) {
    return decodeQuotedGitPath(rest.slice(1, -1));
  }
  return rest;
}

/**
 * Strips exactly one git side prefix. Only the leading occurrence is removed
 * ("a/a/real.rs" → "a/real.rs"), so repositories with a genuine top-level
 * `a/` directory keep targeting the right files. `/dev/null` passes through
 * verbatim — git requires it unprefixed on whichever side lacks a file.
 */
function stripSidePrefix(path: string, side: "a" | "b"): string {
  if (path === "/dev/null") return path;
  return path.startsWith(`${side}/`) ? path.slice(2) : path;
}

interface SourceHeaders {
  /** Raw parsed `--- ` path, or null when absent. */
  oldPath: string | null;
  /** Raw parsed `+++ ` path, or null when absent. */
  newPath: string | null;
}

/**
 * The last ---/+++ pair before the first hunk header is authoritative for
 * where the patch applies. Header rows are hdr-typed by the parser, so ---
 * lines that are really deletion content inside a hunk body never fool this.
 */
function extractSourceHeaders(lines: AnnotatedDiffLine[]): SourceHeaders {
  let oldPath: string | null = null;
  let newPath: string | null = null;
  for (const line of lines) {
    if (line.type === "hdr" && line.content.startsWith("@@")) break;
    if (line.type !== "hdr") continue;
    if (line.content.startsWith("--- ")) oldPath = parseHeaderPath(line.content);
    else if (line.content.startsWith("+++ ")) newPath = parseHeaderPath(line.content);
  }
  return { oldPath, newPath };
}

/**
 * Builds a `FilePatch` targeting specific line indices.
 *
 * Paths come from the diff's own ---/+++ headers when present (falling back
 * to the caller-supplied repo-relative filePath only for header-less input),
 * which keeps staging correct for repos containing a real top-level `a/`
 * directory and for quoted non-ASCII paths.
 */
export function buildFilePatchFromLines(
  lines: AnnotatedDiffLine[],
  filePath: string,
  selectedLineIndices: Set<number>
): FilePatch | null {
  const hunks: UnifiedDiffHunk[] = [];

  let currentHunk: UnifiedDiffHunk | null = null;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    if (line.type === "hdr" && line.content.startsWith("@@")) {
      if (currentHunk && currentHunk.lines.length > 0) {
        hunks.push(currentHunk);
      }
      const nums = parseHunkHeaderNumbers(line.content);
      currentHunk = {
        old_start: nums.old_start,
        old_lines: nums.old_lines,
        new_start: nums.new_start,
        new_lines: nums.new_lines,
        header: line.content,
        lines: [],
      };
      continue;
    }

    if (!currentHunk) {
      continue;
    }

    if (line.type === "ctx" || line.type === "add" || line.type === "del") {
      let lineType: DiffLineType = "Context";
      if (line.type === "add") lineType = "Addition";
      else if (line.type === "del") lineType = "Deletion";

      // Strip leading +, -, or space for the backend payload. A trailing
      // "\r" (CRLF files) is content, not a terminator, and survives here
      // verbatim.
      // NOTE: the Rust validator historically rejected "\r" outright; it is
      // being relaxed in parallel to reject only embedded "\n" and NUL —
      // CRLF staging depends on that relaxation.
      const lineContent = line.content.length > 0 ? line.content.slice(1) : "";
      const isSelected = selectedLineIndices.has(i);

      currentHunk.lines.push({
        line_type: lineType,
        old_line_no: line.oldNo,
        new_line_no: line.newNo,
        content: lineContent,
        is_selected: isSelected,
        ...(line.noNewline ? { no_newline: true } : {}),
      });
    }
  }

  if (currentHunk && currentHunk.lines.length > 0) {
    hunks.push(currentHunk);
  }

  const hasAnySelected = hunks.some((h) =>
    h.lines.some(
      (l) => l.is_selected && (l.line_type === "Addition" || l.line_type === "Deletion")
    )
  );

  if (!hasAnySelected) {
    return null;
  }

  const headers = extractSourceHeaders(lines);

  // Whole-file deletion: +++ /dev/null, or every hunk starts the new side at
  // 0,0 with nothing but deletions/context. Emitting "/dev/null" makes the
  // backend write `+++ /dev/null`, which is what actually removes the entry
  // from the index — anything else stages a 0-byte blob instead.
  const deletesFile =
    headers.newPath === "/dev/null" ||
    (hunks.length > 0 &&
      hunks.every((h) => h.new_start === 0 && h.new_lines === 0) &&
      hunks.every((h) => h.lines.every((l) => l.line_type !== "Addition")));

  // Whole-file creation mirrors it: --- /dev/null or -0,0 hunks with only
  // additions/context produce `--- /dev/null`.
  const createsFile =
    headers.oldPath === "/dev/null" ||
    (hunks.length > 0 &&
      hunks.every((h) => h.old_start === 0 && h.old_lines === 0) &&
      hunks.every((h) => h.lines.every((l) => l.line_type !== "Deletion")));

  const oldPath = createsFile
    ? "/dev/null"
    : headers.oldPath !== null
      ? stripSidePrefix(headers.oldPath, "a")
      : filePath;
  const newPath = deletesFile
    ? "/dev/null"
    : headers.newPath !== null
      ? stripSidePrefix(headers.newPath, "b")
      : filePath;

  return {
    old_path: oldPath,
    new_path: newPath,
    hunks,
  };
}

/**
 * Builds a `FilePatch` selecting all addition/deletion lines within a specific hunk.
 */
export function buildFilePatchForHunk(
  lines: AnnotatedDiffLine[],
  filePath: string,
  hunkHeaderIndex: number
): FilePatch | null {
  const selectedIndices = new Set<number>();
  let inTargetHunk = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (i === hunkHeaderIndex) {
      inTargetHunk = true;
      continue;
    }
    if (line.type === "hdr" && line.content.startsWith("@@")) {
      if (inTargetHunk) break;
    }
    if (inTargetHunk && (line.type === "add" || line.type === "del")) {
      selectedIndices.add(i);
    }
  }

  return buildFilePatchFromLines(lines, filePath, selectedIndices);
}

/**
 * Serializes a [`FilePatch`] to unified-diff text — the exact wire format
 * `git apply --cached --unidiff-zero --recount -` consumes.
 *
 * This is the byte-level twin of src-tauri/src/diff/patch_builder.rs's
 * `build_selective_patch` (the runtime authority); it exists so contract
 * tests can pin the wire format against real git without booting Tauri.
 * Keep them identical: a divergence here means one of the two is lying.
 */
export function serializeSelectivePatch(filePatch: FilePatch, isStaging: boolean): string {
  const patchBuffer: string[] = [];

  patchBuffer.push(unifiedPathHeader("---", "a", filePatch.old_path));
  patchBuffer.push(unifiedPathHeader("+++", "b", filePatch.new_path));

  for (const hunk of filePatch.hunks) {
    const hunkLinesOut: string[] = [];
    let oldCount = 0;
    let newCount = 0;
    const emit = (prefix: string, line: UnifiedDiffLine): void => {
      hunkLinesOut.push(prefix + line.content);
      // Marker lines describe the newline-ness of the side the row came
      // from and are not counted toward either side's totals. A del/add
      // replacement where both sides lack the trailing newline therefore
      // yields one marker after each of the two emitted lines.
      if (line.no_newline) hunkLinesOut.push("\\ No newline at end of file");
    };

    for (const line of hunk.lines) {
      switch (line.line_type) {
        case "Context":
          emit(" ", line);
          oldCount += 1;
          newCount += 1;
          break;
        case "Addition":
          if (line.is_selected) {
            if (isStaging) {
              emit("+", line);
              newCount += 1;
            } else {
              // Unstaging an addition: deletion in the reverse patch
              emit("-", line);
              oldCount += 1;
            }
          } else if (isStaging) {
            // Skipped addition: stays in the working tree only
          } else {
            emit(" ", line);
            oldCount += 1;
            newCount += 1;
          }
          break;
        case "Deletion":
          if (line.is_selected) {
            if (isStaging) {
              emit("-", line);
              oldCount += 1;
            } else {
              // Unstaging a deletion: restore it
              emit("+", line);
              newCount += 1;
            }
          } else if (isStaging) {
            emit(" ", line);
            oldCount += 1;
            newCount += 1;
          } else {
            // Skipped deletion: leave deleted
          }
          break;
      }
    }

    if (hunkLinesOut.length > 0) {
      patchBuffer.push(`@@ -${hunk.old_start},${oldCount} +${hunk.new_start},${newCount} @@\n`);
      for (const out of hunkLinesOut) patchBuffer.push(`${out}\n`);
    }
  }

  return patchBuffer.join("");
}

/** `--- a/path` / `+++ b/path`, except `/dev/null` which git expects unprefixed. */
function unifiedPathHeader(marker: string, prefix: string, path: string): string {
  if (path === "/dev/null") {
    return `${marker} /dev/null\n`;
  }
  return `${marker} ${prefix}/${path}\n`;
}
