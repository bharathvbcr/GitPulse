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

export function filterPathsByFileQuery(paths: readonly string[], query: FileQuery): string[] {
  if (query.error) return [];
  if (query.kind === "all" && !query.ext) return [...paths];
  return paths.filter((path) => matchesFileQuery(path, query));
}

function compileRegex(
  body: string,
  flags: string,
): { regex: RegExp | null; error: string | null } {
  if (!body) return { regex: null, error: "Empty regular expression" };
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
