/**
 * What a diff is *about*, read from the diff itself.
 *
 * The old header printed `selectedFilePath` regardless of what the body held.
 * A commit diff and a whole-worktree diff both cover many files and leave that
 * field holding whatever the reader last clicked, so the header would name
 * `.github/workflows/codeql-analysis.yml` — with that file's language icon and
 * its line count — above a body showing `cmd/server/server.go`. A header that
 * disagrees with the body is worse than no header: it is a wrong answer to
 * "what am I looking at", delivered confidently.
 *
 * So the identity comes from the same bytes the body renders. A single-file
 * diff names the file; a multi-file diff names the count and the totals, and
 * the sections it found become the in-body file headers, the sticky context
 * strip, the jump list, and the file marks on the minimap.
 */

import { gitHeaderSides, parseHeaderPath, stripSidePrefix } from "./gitPaths";
import type { AnnotatedDiffLine } from "./wordDiff";

export interface DiffHunk {
  /** Index of the `@@` row in the source line list. */
  index: number;
  /** The header text, minus the trailing section heading git sometimes adds. */
  header: string;
  /** The `fn main()`-style heading after the closing `@@`, when git emitted one. */
  heading: string;
  additions: number;
  deletions: number;
}

export interface DiffFileSection {
  /** Index of the first row belonging to this file. */
  index: number;
  /** Exclusive end, so a caller can slice the section without a lookahead. */
  end: number;
  /** Repo-relative path of the new side, or of the old side for a deletion. */
  path: string;
  /** Set only when the file moved, so callers need no equality check. */
  oldPath?: string;
  additions: number;
  deletions: number;
  /** True when git reported the contents as binary rather than as lines. */
  binary: boolean;
  /** True when the old side is `/dev/null`. */
  created: boolean;
  /** True when the new side is `/dev/null`. */
  deleted: boolean;
  hunks: DiffHunk[];
}

export interface DiffOutline {
  files: DiffFileSection[];
  additions: number;
  deletions: number;
  /**
   * True when the diff carried no file header at all — a bare hunk stream.
   * Callers must not claim "1 file" for it, because they do not know that.
   */
  headerless: boolean;
}

export const EMPTY_OUTLINE: DiffOutline = {
  files: [],
  additions: 0,
  deletions: 0,
  headerless: false,
};

const HUNK_SPLIT_RE = /^(@@\s+-\d+(?:,\d+)?\s+\+\d+(?:,\d+)?\s+@@)(.*)$/;

/**
 * Builds the outline for one parsed diff.
 *
 * Sections open on `diff --git`, which is what git emits for every file in
 * every mode this app fetches. A stream that never emits one — a hand-fed
 * patch, or a fragment — still gets one section so the body can render, but
 * the outline says `headerless` so the header can decline to name a file it
 * only inferred.
 */
export function buildOutline(lines: readonly AnnotatedDiffLine[]): DiffOutline {
  if (lines.length === 0) return EMPTY_OUTLINE;

  const files: DiffFileSection[] = [];
  let headerless = false;
  let current: DiffFileSection | null = null;
  let currentHunk: DiffHunk | null = null;

  const openSection = (index: number): DiffFileSection => {
    const section: DiffFileSection = {
      index,
      end: index,
      path: "",
      additions: 0,
      deletions: 0,
      binary: false,
      created: false,
      deleted: false,
      hunks: [],
    };
    files.push(section);
    return section;
  };

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const content = line.content;

    if (line.type === "meta" && content.startsWith("diff --git ")) {
      current = openSection(i);
      currentHunk = null;
      const sides = gitHeaderSides(content);
      if (sides) {
        const [oldSide, newSide] = sides;
        current.path = newSide === "/dev/null" ? oldSide : newSide;
        if (oldSide !== newSide && oldSide !== "/dev/null" && newSide !== "/dev/null") {
          current.oldPath = oldSide;
        }
      }
      current.end = i + 1;
      continue;
    }

    if (!current) {
      // Content before any file header: a bare hunk stream, or `git show`
      // prose ahead of the first patch. One implicit section carries it so
      // nothing is dropped, and `headerless` stops the caller naming it.
      current = openSection(i);
      headerless = true;
    }
    current.end = i + 1;

    if (line.type === "binary") {
      current.binary = true;
      continue;
    }

    if (line.type === "hdr") {
      if (content.startsWith("@@")) {
        const match = HUNK_SPLIT_RE.exec(content);
        currentHunk = {
          index: i,
          header: match ? match[1] : content,
          heading: match ? match[2].trim() : "",
          additions: 0,
          deletions: 0,
        };
        current.hunks.push(currentHunk);
        continue;
      }
      // `--- a/x` / `+++ b/y` refine a section whose `diff --git` line was
      // unparseable or absent; they are also the only path information a
      // headerless stream has.
      if (content.startsWith("--- ")) {
        const path = stripSidePrefix(parseHeaderPath(content));
        if (path === "/dev/null") current.created = true;
        else if (!current.path) current.path = path;
        continue;
      }
      if (content.startsWith("+++ ")) {
        const path = stripSidePrefix(parseHeaderPath(content));
        if (path === "/dev/null") current.deleted = true;
        else current.path = path;
        continue;
      }
      continue;
    }

    if (line.type === "add") {
      current.additions += 1;
      if (currentHunk) currentHunk.additions += 1;
    } else if (line.type === "del") {
      current.deletions += 1;
      if (currentHunk) currentHunk.deletions += 1;
    }
  }

  let additions = 0;
  let deletions = 0;
  for (const file of files) {
    additions += file.additions;
    deletions += file.deletions;
  }

  return { files, additions, deletions, headerless };
}

/**
 * The section containing `lineIndex`, for the sticky header.
 *
 * Binary search rather than a scan: this runs on every scroll frame of a diff
 * that can hold two hundred sections.
 */
export function sectionAt(outline: DiffOutline, lineIndex: number): DiffFileSection | null {
  const files = outline.files;
  if (files.length === 0) return null;
  let lo = 0;
  let hi = files.length - 1;
  let found: DiffFileSection | null = null;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (files[mid].index <= lineIndex) {
      found = files[mid];
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return found ?? files[0];
}

/** The hunk containing `lineIndex` within its section, or null above the first. */
export function hunkAt(section: DiffFileSection | null, lineIndex: number): DiffHunk | null {
  if (!section) return null;
  let found: DiffHunk | null = null;
  for (const hunk of section.hunks) {
    if (hunk.index > lineIndex) break;
    found = hunk;
  }
  return found;
}

/** Git's one-letter status for a section, matching what the file rail shows. */
export function sectionStatus(section: DiffFileSection): string {
  if (section.created) return "A";
  if (section.deleted) return "D";
  if (section.oldPath) return "R";
  return "M";
}

/**
 * What the header calls this diff.
 *
 * A single named file is named. Anything else is counted, because naming one
 * of many files is the lie this module exists to remove.
 */
export function outlineTitle(outline: DiffOutline, fallback: string): string {
  const named = outline.files.filter((file) => file.path.length > 0);
  if (outline.headerless || named.length === 0) return fallback;
  if (named.length === 1) return named[0].path;
  return `${named.length} files`;
}

/**
 * The path the header's language icon should key off, or null when no single
 * path describes the whole diff.
 *
 * Null for a multi-file diff: one file's language is not the diff's language,
 * and picking the first would put a YAML badge on a mostly-Go commit. The
 * caller's own fallback is used only when the diff names NO file — a
 * headerless fragment still belongs to whatever the reader opened. Handing
 * that fallback back for a multi-file diff is how a 200-file commit came to
 * wear a JSON badge, because the reader's last click happened to land on a
 * `.json`.
 */
export function outlineLanguagePath(
  outline: DiffOutline,
  fallback: string | null = null,
): string | null {
  const named = outline.files.filter((file) => file.path.length > 0);
  if (named.length === 1) return named[0].path;
  return named.length === 0 ? fallback : null;
}

/**
 * The path whose language should colour the line at `lineIndex`.
 *
 * A commit diff is a stack of files in different languages. Tokenizing all of
 * them with one language — whichever the header happened to name — coloured
 * Rust with the JSON tokenizer: `//` comments came out as plain text and
 * `fn`/`let` never read as keywords, while the commas were confidently picked
 * out as punctuation. Section-local is the only honest answer.
 */
export function languagePathForLine(
  outline: DiffOutline,
  lineIndex: number,
  fallback: string | null = null,
): string | null {
  const section = sectionAt(outline, lineIndex);
  return section && section.path.length > 0 ? section.path : fallback;
}

/** `+42 −7`, or an empty string when a diff changed no lines at all. */
export function churnSummary(additions: number, deletions: number): string {
  if (additions === 0 && deletions === 0) return "";
  return `+${additions.toLocaleString()} −${deletions.toLocaleString()}`;
}
