/**
 * Explorer filter language. Inspired by VS Code tree-find and GitHub-style
 * path pickers, but parsed in pure TypeScript so the 100k-path contract stays
 * testable without a webview.
 *
 * Text box:
 *   substring (default) | `*.ts` glob | `/regex/` | `~fuzzy`
 * Tokens mixed with the remainder:
 *   `is:staged` `is:unstaged` `is:untracked` `is:conflict` `is:modified`
 *   `ext:ts` (with or without a leading dot)
 *
 * Invalid regex fails closed: nothing matches, and `error` is set so the UI
 * can say why rather than pretending the tree is empty.
 */

import { fuzzyMatch } from "../branches/groupBranches";
import { hasUnboundedNesting } from "../text/lineSearch";

export type FileQueryKind = "all" | "substring" | "glob" | "regex" | "fuzzy";

export type FileStatusScope =
  | "all"
  | "staged"
  | "unstaged"
  | "untracked"
  | "conflict"
  | "modified";

export interface FileQuery {
  kind: FileQueryKind;
  /** Folded needle for substring/fuzzy; original source for glob/regex. */
  needle: string;
  regex: RegExp | null;
  status: FileStatusScope;
  /** Lowercase extension including the dot, e.g. `.ts`. Null means any. */
  ext: string | null;
  error: string | null;
}

const MAX_PATTERN_CHARS = 200;

const STATUS_TOKENS: Record<string, FileStatusScope> = {
  "is:staged": "staged",
  "is:unstaged": "unstaged",
  "is:untracked": "untracked",
  "is:conflict": "conflict",
  "is:conflicted": "conflict",
  "is:modified": "modified",
  "is:changed": "modified",
};

export function emptyFileQuery(): FileQuery {
  return {
    kind: "all",
    needle: "",
    regex: null,
    status: "all",
    ext: null,
    error: null,
  };
}

export function parseFileQuery(raw: string): FileQuery {
  const trimmed = raw.trim();
  if (!trimmed) return emptyFileQuery();

  let status: FileStatusScope = "all";
  let ext: string | null = null;
  const rest: string[] = [];

  for (const token of trimmed.split(/\s+/)) {
    const folded = token.toLowerCase();
    const mapped = STATUS_TOKENS[folded];
    if (mapped) {
      status = mapped;
      continue;
    }
    if (folded.startsWith("ext:")) {
      const value = folded.slice("ext:".length).replace(/^\./, "");
      if (value) ext = `.${value}`;
      continue;
    }
    rest.push(token);
  }

  const pattern = rest.join(" ").trim();
  if (!pattern) {
    return { kind: "all", needle: "", regex: null, status, ext, error: null };
  }

  if (pattern.length > MAX_PATTERN_CHARS) {
    return {
      kind: "regex",
      needle: pattern,
      regex: null,
      status,
      ext,
      error: `Filter is longer than ${MAX_PATTERN_CHARS} characters`,
    };
  }

  if (pattern.startsWith("/") && pattern.length >= 2) {
    const last = pattern.lastIndexOf("/");
    if (last > 0) {
      const body = pattern.slice(1, last);
      const flags = pattern.slice(last + 1);
      const compiled = compileRegex(body, flags);
      return {
        kind: "regex",
        needle: pattern,
        regex: compiled.regex,
        status,
        ext,
        error: compiled.error,
      };
    }
  }

  if (pattern.startsWith("~")) {
    return {
      kind: "fuzzy",
      needle: pattern.slice(1).trim().toLowerCase(),
      regex: null,
      status,
      ext,
      error: null,
    };
  }

  if (/[*?]/.test(pattern)) {
    const compiled = globToRegExp(pattern);
    return {
      kind: "glob",
      needle: pattern,
      regex: compiled.regex,
      status,
      ext,
      error: compiled.error,
    };
  }

  return {
    kind: "substring",
    needle: pattern.toLowerCase(),
    regex: null,
    status,
    ext,
    error: null,
  };
}

export function matchesFileQuery(path: string, query: FileQuery): boolean {
  if (query.error) return false;
  if (query.ext) {
    const lower = path.toLowerCase();
    if (!lower.endsWith(query.ext)) return false;
  }
  if (query.kind === "all") return true;

  const lower = path.toLowerCase();
  const base = lower.slice(lower.lastIndexOf("/") + 1);

  switch (query.kind) {
    case "substring":
      return lower.includes(query.needle) || base.includes(query.needle);
    case "fuzzy":
      return query.needle.length === 0 || fuzzyMatch(query.needle, path);
    case "glob":
      if (!query.regex) return false;
      return query.regex.test(path) || query.regex.test(base);
    case "regex":
      return query.regex ? query.regex.test(path) : false;
    default:
      return true;
  }
}

/** Wall-clock ceiling for one filter pass over the path list. */
export const DEFAULT_FILTER_MAX_MILLIS = 200;

export interface FilteredPaths {
  paths: string[];
  /**
   * True when the wall-clock budget stopped the scan before the end, so
   * `paths` is a prefix of the answer rather than the answer. The explorer
   * says so rather than presenting a partial tree as a complete one.
   */
  truncated: boolean;
}

/**
 * Filters the path list, under a wall-clock budget.
 *
 * The budget is the backstop behind {@link compileRegex}'s static refusal,
 * and it exists because that refusal cannot be complete. Classifying
 * catastrophic backtracking in general is undecidable in practice; the
 * predicate is an approximation tuned against measurements taken on V8, while
 * the shipped app runs on WKWebView's JavaScriptCore, whose regex engine
 * optimizes different shapes. A pattern that slips through therefore costs
 * one path's worth of backtracking instead of a hundred thousand — the
 * difference between a slow filter and a dead window.
 *
 * It cannot make a single `test` call return sooner: a JavaScript regex is
 * not interruptible. Both defences are needed, and neither is claimed to be
 * sufficient alone.
 */
export function filterPathsByFileQuery(
  paths: readonly string[],
  query: FileQuery,
  options: { maxMillis?: number } = {},
): FilteredPaths {
  if (query.error) return { paths: [], truncated: false };
  if (query.kind === "all" && !query.ext) return { paths: [...paths], truncated: false };
  const deadline = Date.now() + Math.max(1, options.maxMillis ?? DEFAULT_FILTER_MAX_MILLIS);
  const kept: string[] = [];
  for (let i = 0; i < paths.length; i += 1) {
    // Checked every 256 paths, matching `lineSearch`: `Date.now()` on a
    // 100,000-path loop is itself measurable, and the overshoot is not.
    if ((i & 0xff) === 0 && i > 0 && Date.now() > deadline) {
      return { paths: kept, truncated: true };
    }
    if (matchesFileQuery(paths[i], query)) kept.push(paths[i]);
  }
  return { paths: kept, truncated: false };
}

/**
 * Compiles a user-typed `/pattern/flags`, refusing the shapes that hang.
 *
 * A quantified group wrapping an unbounded quantifier — `(a+)+`, `(\w+)+`,
 * `([a-z]+)*` — backtracks exponentially, and every compiled pattern here is
 * then run against **every path in the repository** by
 * {@link filterPathsByFileQuery}, whose stated contract is 100,000 paths. A
 * JavaScript regex is not interruptible: once `test` has started, no timeout,
 * budget or worker cancellation can shorten it. Refusing to compile is the
 * only thing that keeps the window responsive.
 *
 * Measured on this module before the guard: ONE 45-character path took 1.4 s
 * for `(a+)+$` and 2.7 s for `(\w+)+$`; fifty of them took 9 s.
 *
 * The predicate is imported rather than reimplemented — `lineSearch` already
 * makes this exact call for the diff and file-viewer search boxes, and it is
 * the same question with the same answer. This box was simply never wired to
 * it, so the one search surface that scans the most strings was the one left
 * unguarded.
 */
function compileRegex(
  body: string,
  flags: string,
): { regex: RegExp | null; error: string | null } {
  if (!body) return { regex: null, error: "Empty regular expression" };
  if (hasUnboundedNesting(body)) {
    return {
      regex: null,
      error: "Pattern can backtrack exponentially — remove the nested repeat",
    };
  }
  const safeFlags = flags.replace(/[^ims]/g, "");
  try {
    return { regex: new RegExp(body, safeFlags), error: null };
  } catch {
    return { regex: null, error: "Invalid regular expression" };
  }
}

/**
 * Converts a glob to a case-insensitive regex. `*` does not cross `/`;
 * `**` does. Character classes are treated as literals so a hostile `[a-z]*`
 * cannot become an unbounded regex.
 */
export function globToRegExp(glob: string): { regex: RegExp | null; error: string | null } {
  if (!glob || glob.length > MAX_PATTERN_CHARS) {
    return { regex: null, error: "Invalid glob" };
  }
  let source = "^";
  for (let i = 0; i < glob.length; i += 1) {
    const ch = glob[i];
    if (ch === "*" && glob[i + 1] === "*") {
      source += ".*";
      i += 1;
      if (glob[i + 1] === "/") i += 1;
      continue;
    }
    if (ch === "*") {
      source += "[^/]*";
      continue;
    }
    if (ch === "?") {
      source += "[^/]";
      continue;
    }
    if (/[.+^${}()|[\]\\]/.test(ch)) {
      source += `\\${ch}`;
      continue;
    }
    source += ch;
  }
  source += "$";
  try {
    return { regex: new RegExp(source, "i"), error: null };
  } catch {
    return { regex: null, error: "Invalid glob" };
  }
}
