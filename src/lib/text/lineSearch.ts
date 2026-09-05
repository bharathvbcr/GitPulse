/**
 * Find-in-lines, shared by the file viewer and the diff.
 *
 * Both grew the same loop, and it had the same hole in both: `exec` in a
 * global regex only advances `lastIndex` past a match with a length. A
 * pattern that can match nothing — `a*`, `\b`, `(?:)`, `^` — matches the
 * empty string at the same index forever, and the loop never returns. Typing
 * a `*` into a search box should not hang the window.
 *
 * So there is one implementation, it advances past zero-length matches, and
 * it stops at a cap and says it stopped: a search that quietly returns the
 * first five thousand hits of a million reports a match count that is a
 * floor presented as a total.
 */

export interface LineMatch {
  /** Index into the line list that was searched. */
  lineIndex: number;
  /** Character offset of the match within that line. */
  colStart: number;
  length: number;
}

export interface SearchOptions {
  caseSensitive?: boolean;
  regex?: boolean;
  /** Hard ceiling on collected matches; the result says when it was hit. */
  maxMatches?: number;
  /**
   * Wall-clock ceiling, checked between lines.
   *
   * A search over a 300,000-line diff is a loop the user is waiting inside,
   * and a pattern that is merely slow rather than catastrophic still adds up.
   * Stopping and saying so beats freezing.
   */
  maxMillis?: number;
}

export interface SearchResult {
  matches: LineMatch[];
  /** True when a cap — matches or time — stopped collection early. */
  truncated: boolean;
  /** True when `regex` was on and the pattern was refused. */
  invalid: boolean;
  /** Why it was refused, for a message the user can act on. */
  reason?: "syntax" | "unbounded";
}

export const DEFAULT_MAX_MATCHES = 5_000;
/** Default wall-clock ceiling for one search pass. */
export const DEFAULT_MAX_MILLIS = 400;

export const EMPTY_SEARCH: SearchResult = { matches: [], truncated: false, invalid: false };

/**
 * Detects the exponential-backtracking shape: a quantifier applied to a group
 * that itself contains an unbounded quantifier — `(a+)+`, `([a-z]*\s*)+`,
 * `(\d{2,})*`.
 *
 * This is not a general safety analysis and does not claim to be. It catches
 * the one family that actually hangs a search box, and it is worth catching:
 * `(a+)+c` against twenty-eight characters took **111 seconds** in this
 * repository's own stress run, and a JavaScript regex is not interruptible,
 * so no timeout, budget or worker cancellation can shorten it once `exec`
 * has started. Refusing the pattern with a message is the only thing that
 * keeps the window responsive.
 *
 * Deliberately conservative about what it flags: a quantified group whose
 * body has no unbounded quantifier (`(foo|bar)+`), and an unbounded
 * quantifier inside an unquantified group (`(\d+)`), are both left alone.
 */
export function hasUnboundedNesting(pattern: string): boolean {
  const opens: number[] = [];
  let inClass = false;
  for (let i = 0; i < pattern.length; i += 1) {
    const ch = pattern[i];
    if (ch === "\\") {
      i += 1;
      continue;
    }
    if (inClass) {
      if (ch === "]") inClass = false;
      continue;
    }
    if (ch === "[") {
      inClass = true;
      continue;
    }
    if (ch === "(") {
      opens.push(i);
      continue;
    }
    if (ch !== ")") continue;
    const start = opens.pop();
    if (start === undefined) continue;
    const next = pattern[i + 1];
    const quantified =
      next === "*" ||
      next === "+" ||
      (next === "{" && /^\{\d*,\s*\}/.test(pattern.slice(i + 1)));
    if (!quantified) continue;
    if (bodyHasUnboundedQuantifier(pattern.slice(start + 1, i))) return true;
  }
  return false;
}

/** True when `body` contains `*`, `+` or `{n,}` outside a character class. */
function bodyHasUnboundedQuantifier(body: string): boolean {
  let inClass = false;
  for (let i = 0; i < body.length; i += 1) {
    const ch = body[i];
    if (ch === "\\") {
      i += 1;
      continue;
    }
    if (inClass) {
      if (ch === "]") inClass = false;
      continue;
    }
    if (ch === "[") {
      inClass = true;
      continue;
    }
    if (ch === "*" || ch === "+") return true;
    if (ch === "{" && /^\{\d*,\s*\}/.test(body.slice(i))) return true;
  }
  return false;
}

/** Escapes every regex metacharacter, for literal search. */
export function escapeRegExp(input: string): string {
  return input.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Compiles a query into a global matcher, or null when it cannot be used.
 *
 * An empty (or whitespace-only) query is not an error and not a match-all: it
 * is "the user has not asked anything yet", so it compiles to null and the
 * caller renders no highlights instead of highlighting the whole file.
 */
export function buildMatcher(query: string, options: SearchOptions = {}): RegExp | null {
  if (!query.trim()) return null;
  const flags = options.caseSensitive ? "g" : "gi";
  // A literal query is escaped, so nesting is impossible by construction and
  // only a user-written pattern is ever refused.
  if (options.regex && hasUnboundedNesting(query)) return null;
  try {
    return new RegExp(options.regex ? query : escapeRegExp(query), flags);
  } catch {
    return null;
  }
}

/**
 * Collects every match of `matcher` in one line, appending to `out`.
 *
 * Returns the number appended, so a caller enforcing a global cap does not
 * have to re-measure the array. `matcher.lastIndex` is reset here rather than
 * by the caller: a shared regex object carries state between lines, and a
 * forgotten reset silently skips the head of every line after the first.
 */
export function matchesInLine(
  line: string,
  matcher: RegExp,
  lineIndex: number,
  out: LineMatch[],
  remaining: number,
): number {
  if (remaining <= 0) return 0;
  matcher.lastIndex = 0;
  let added = 0;
  let guard = line.length + 1;
  let match: RegExpExecArray | null;
  while ((match = matcher.exec(line)) !== null) {
    const length = match[0].length;
    if (length > 0) {
      out.push({ lineIndex, colStart: match.index, length });
      added += 1;
      if (added >= remaining) break;
    } else {
      // Zero-length match: `exec` would return this same position forever.
      matcher.lastIndex += 1;
      if (matcher.lastIndex > line.length) break;
    }
    // A pathological pattern can still cycle without consuming; one pass per
    // character is the most any correct search needs.
    guard -= 1;
    if (guard <= 0) break;
  }
  return added;
}

/**
 * Searches `lines`, stopping at `maxMatches`.
 *
 * The lines are read through an accessor so a caller can search a projection
 * — the diff searches line content with the `+`/`-` marker stripped, which
 * keeps a query for `+ foo` from matching the marker column of every added
 * line — without materialising a second copy of a 300,000-line array.
 */
export function findMatches(
  lines: readonly string[] | { length: number; at(index: number): string },
  query: string,
  options: SearchOptions = {},
): SearchResult {
  const matcher = buildMatcher(query, options);
  if (!matcher) {
    const asked = !!options.regex && query.trim().length > 0;
    if (!asked) return { matches: [], truncated: false, invalid: false };
    return {
      matches: [],
      truncated: false,
      invalid: true,
      reason: hasUnboundedNesting(query) ? "unbounded" : "syntax",
    };
  }
  const cap = Math.max(0, options.maxMatches ?? DEFAULT_MAX_MATCHES);
  const deadline =
    Date.now() + Math.max(1, options.maxMillis ?? DEFAULT_MAX_MILLIS);
  const total = lines.length;
  const read =
    typeof (lines as { at?: unknown }).at === "function" && !Array.isArray(lines)
      ? (index: number) => (lines as { at(index: number): string }).at(index)
      : (index: number) => (lines as readonly string[])[index] ?? "";
  const matches: LineMatch[] = [];
  for (let i = 0; i < total; i += 1) {
    if (matches.length >= cap) return { matches, truncated: true, invalid: false };
    // Checked every 256 lines rather than every line: `Date.now()` on a
    // 300,000-line loop is itself measurable, and a 256-line overshoot is
    // not.
    if ((i & 0xff) === 0 && i > 0 && Date.now() > deadline) {
      return { matches, truncated: true, invalid: false };
    }
    matchesInLine(read(i), matcher, i, matches, cap - matches.length);
  }
  return { matches, truncated: matches.length >= cap && cap > 0, invalid: false };
}

/**
 * Index of the first match at or after `lineIndex`, for "resume from where
 * the reader is looking" rather than from the top of the file.
 */
export function firstMatchFrom(matches: readonly LineMatch[], lineIndex: number): number {
  for (let i = 0; i < matches.length; i += 1) {
    if (matches[i].lineIndex >= lineIndex) return i;
  }
  return matches.length > 0 ? 0 : -1;
}

/** Wraps an index into `[0, count)`; -1 when there is nothing to step to. */
export function stepMatch(current: number, count: number, delta: number): number {
  if (count <= 0) return -1;
  return ((current + delta) % count + count) % count;
}

/** `3 of 128`, or `128+ matches` when a cap cut collection short. */
export function matchLabel(result: SearchResult, current: number): string {
  if (result.reason === "unbounded") return "pattern may not terminate";
  if (result.invalid) return "bad pattern";
  const count = result.matches.length;
  if (count === 0) return "no matches";
  const total = result.truncated ? `${count.toLocaleString()}+` : count.toLocaleString();
  return `${(current + 1).toLocaleString()} of ${total}`;
}
