export interface ParsedFilterQuery {
  author?: string;
  path?: string;
  sha?: string;
  commitType?: string;
  text: string;
}

const CONVENTIONAL_TYPES = new Set([
  "feat",
  "fix",
  "chore",
  "docs",
  "refactor",
  "perf",
  "test",
  "build",
  "ci",
]);

export function parseFilterQuery(query: string): ParsedFilterQuery {
  const tokens = query.trim().split(/\s+/).filter(Boolean);
  const parsed: ParsedFilterQuery = { text: "" };
  const free: string[] = [];

  for (const token of tokens) {
    if (token.startsWith("author:")) {
      const value = token.slice("author:".length).toLowerCase();
      if (value) parsed.author = value;
    } else if (token.startsWith("path:")) {
      const value = token.slice("path:".length);
      if (value) parsed.path = value;
    } else if (token.startsWith("sha:")) {
      const value = token.slice("sha:".length).toLowerCase();
      if (value) parsed.sha = value;
    } else if (token.startsWith("type:")) {
      const value = token.slice("type:".length).toLowerCase();
      if (value) parsed.commitType = value;
    } else if (token.endsWith(":")) {
      const kind = token.slice(0, -1).toLowerCase();
      if (CONVENTIONAL_TYPES.has(kind)) {
        parsed.commitType = kind;
      } else {
        free.push(token);
      }
    } else {
      free.push(token);
    }
  }

  parsed.text = free.join(" ").toLowerCase();
  return parsed;
}

/**
 * True when the query can only be answered by git itself (`path:` walks file
 * history). Everything else filters the rows already loaded client-side, so
 * typing does not re-run a full log walk per keystroke.
 */
export function queryNeedsServerFetch(query: string): boolean {
  return query
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .some((token) => token.startsWith("path:"));
}

export function matchesCommit(
  row: { id: string; summary: string; author_name: string; author_email: string },
  query: ParsedFilterQuery
): boolean {
  if (query.author) {
    const hay = `${row.author_name} ${row.author_email}`.toLowerCase();
    if (!hay.includes(query.author)) return false;
  }
  if (query.sha && !row.id.toLowerCase().startsWith(query.sha)) return false;
  if (query.commitType) {
    const header = (row.summary || "").toLowerCase();
    if (!header.startsWith(`${query.commitType}:`) && !header.startsWith(`${query.commitType}(`)) {
      return false;
    }
  }
  if (query.text) {
    const hay = `${row.summary} ${row.author_name} ${row.id}`.toLowerCase();
    if (!hay.includes(query.text)) return false;
  }
  return true;
}
