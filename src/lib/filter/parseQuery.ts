export interface ParsedFilterQuery {
  author?: string;
  path?: string;
  sha?: string;
  commitType?: string;
  /** Local calendar day `YYYY-MM-DD`. Matching uses the row's author timestamp. */
  date?: string;
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
      // Parity with CommitFilter::parse (analyzer/filter.rs): any non-empty
      // value is taken verbatim. Gating on the conventional set here made the
      // client and the backend disagree about which commits survive a query,
      // and every disagreement shifts row indices under the solved lanes.
      const value = token.slice("type:".length).toLowerCase();
      if (value) parsed.commitType = value;
      else free.push(token);
    } else if (token.startsWith("date:")) {
      // Consumed even when the value is malformed, so `date:2026-09-02`
      // cannot fall through to free-text and match nothing. Matching is
      // local-calendar and lives in matchesCommit.
      const value = token.slice("date:".length);
      if (value) parsed.date = value;
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

function localDayKeyFromEpochSeconds(timestampSec: number): string | null {
  if (!Number.isFinite(timestampSec) || timestampSec <= 0) return null;
  const d = new Date(timestampSec * 1000);
  const month = `${d.getMonth() + 1}`.padStart(2, "0");
  const day = `${d.getDate()}`.padStart(2, "0");
  return `${d.getFullYear()}-${month}-${day}`;
}

export function matchesCommit(
  row: {
    id: string;
    summary: string;
    author_name: string;
    author_email: string;
    timestamp?: number;
  },
  query: ParsedFilterQuery
): boolean {
  if (query.author) {
    const hay = `${row.author_name} ${row.author_email}`.toLowerCase();
    if (!hay.includes(query.author)) return false;
  }
  if (query.date) {
    // Fail closed: a date: predicate without a timestamp is not a match.
    const key = localDayKeyFromEpochSeconds(row.timestamp ?? 0);
    if (key !== query.date) return false;
  }
  if (query.sha && !row.id.toLowerCase().startsWith(query.sha)) return false;
  if (query.commitType) {
    // All four conventional-commit shapes the backend accepts — plain,
    // scoped, and both breaking variants. Missing `!` here made the client
    // drop breaking-change commits the server had kept (and solved around).
    const header = (row.summary || "").toLowerCase();
    const kind = query.commitType;
    if (
      !header.startsWith(`${kind}:`) &&
      !header.startsWith(`${kind}(`) &&
      !header.startsWith(`${kind}!:`) &&
      !header.startsWith(`${kind}!(`)
    ) {
      return false;
    }
  }
  if (query.text) {
    // Backend parity (filter.rs matches_commit): the haystack includes the
    // author email, and multi-word text is a conjunction over words, not one
    // contiguous phrase — "oauth flow" must match "fix: flow for oauth".
    const hay = `${row.summary} ${row.author_name} ${row.author_email} ${row.id}`.toLowerCase();
    for (const word of query.text.split(/\s+/)) {
      if (!word) continue;
      if (!hay.includes(word)) return false;
    }
  }
  return true;
}
