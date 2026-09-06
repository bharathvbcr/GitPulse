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
 * Detects the two exponential-backtracking shapes that hang a search box.
 *
 * 1. **Nested quantifier** — a quantifier applied to a group that itself
 *    contains an unbounded quantifier: `(a+)+`, `([a-z]*\s*)+`, `(\d{2,})*`.
 * 2. **Ambiguous alternation under a quantifier** — a quantified group whose
 *    branches can match the same text, so the engine has to try every way of
 *    splitting the input between them: `(a|a)*`, `(?:aa|a)*`,
 *    `([a-z]|[a-z][a-z])*`.
 *
 * Family 2 was measured on this repository's own code and is not theoretical:
 * `(a|a)*$` took **6.3 s against a 28-character string** and quadruples every
 * two characters, and `(?:aa|a)*$` took 2.1 s at 38. It contains no nested
 * quantifier at all, so the original rule passed it straight through.
 *
 * A JavaScript regex is not interruptible: once `exec` has started, no
 * timeout, budget or worker cancellation can shorten it. Refusing the pattern
 * is the only thing that keeps the window responsive — which is also why this
 * predicate is not the only defence. It is a static approximation, the app
 * runs on WKWebView (JavaScriptCore) rather than the V8 these numbers were
 * measured on, and no static check classifies backtracking in general. The
 * callers that scan many strings therefore also carry a wall-clock budget, so
 * a pattern this misses costs one string rather than the whole scan.
 *
 * Deliberately conservative in BOTH directions, and the asymmetry is
 * intentional. Left alone: an unbounded quantifier inside an *unquantified*
 * group (`(\d+)`), and a quantified group whose branches cannot start with
 * the same character (`(foo|bar)+`). Refused even though a given engine may
 * optimize them today: branches that merely *could* overlap, such as
 * `(a|b|ab)*` (`a` also starts `ab`) and any pair involving a character
 * class. Refusing a rare working pattern costs one error message; admitting
 * one that blows up costs the window.
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
    if (!groupRepeats(pattern, i)) continue;
    // The opener (`?:`, `?=`, `?<name>`) is syntax, not something the group
    // matches. Stripped once, here, so neither check below can mistake the
    // `?` in `(?:x|y)+` for a quantifier and refuse a safe pattern.
    const body = pattern.slice(start + 1, i).replace(GROUP_PREFIX, "");
    if (bodyIsVariableLength(body)) return true;
    if (alternationIsAmbiguous(body)) return true;
  }
  return false;
}

/**
 * True when the quantifier after a group's `)` can run the group MORE THAN
 * ONCE — the precondition for every backtracking blow-up below it, because
 * one iteration has no partition to get wrong.
 *
 * Bounded repetition counts. The original rule recognized only `*`, `+` and
 * `{n,}`, so `{n}` and `{n,m}` slipped past every check underneath it — and
 * a fixed count multiplies ambiguity just as well as an open one. This
 * repository's fuzz run surfaced `(?:\w|(\w+){2,4}){3}$`, which the old rule
 * admitted twice over (neither `{3}` nor `{2,4}` registered as a quantifier)
 * and which takes **5.0 s at 26 characters**, roughly tripling every two.
 *
 * `?` and `{0,1}` are deliberately not repetition: at most one iteration.
 */
function groupRepeats(pattern: string, closeIndex: number): boolean {
  const next = pattern[closeIndex + 1];
  if (next === "*" || next === "+") return true;
  if (next !== "{") return false;
  const brace = /^\{(\d*)(?:,(\d*))?\}/.exec(pattern.slice(closeIndex + 1));
  if (!brace) return false;
  const min = brace[1] === "" ? 0 : Number(brace[1]);
  if (brace[2] === undefined) return min >= 2; // {n}
  if (brace[2] === "") return true; // {n,}
  return Number(brace[2]) >= 2; // {n,m}
}

/** Group openers that are not part of the matched body: `?:`, `?=`, `?<name>`. */
const GROUP_PREFIX = /^\?(?::|=|!|<[=!]|<[A-Za-z_$][\w$]*>)/;

/**
 * True when a quantified group's branches can begin with the same character,
 * which is what makes the repetition ambiguous and the backtracking
 * exponential. A single branch cannot be ambiguous with anything, so an
 * alternation-free body is always false here and is judged only by
 * {@link bodyHasUnboundedQuantifier}.
 */
function alternationIsAmbiguous(rawBody: string): boolean {
  const body = rawBody.replace(GROUP_PREFIX, "");
  const branches = splitTopLevelAlternatives(body);
  if (branches.length < 2) return false;
  const atoms = branches.map(firstAtom);
  for (let i = 0; i < atoms.length; i += 1) {
    for (let j = i + 1; j < atoms.length; j += 1) {
      if (atomsCanOverlap(atoms[i], atoms[j])) return true;
    }
  }
  return false;
}

/** Splits on `|` at nesting depth zero, respecting escapes and classes. */
function splitTopLevelAlternatives(body: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let inClass = false;
  let current = "";
  for (let i = 0; i < body.length; i += 1) {
    const ch = body[i];
    if (ch === "\\") {
      current += ch + (body[i + 1] ?? "");
      i += 1;
      continue;
    }
    if (inClass) {
      current += ch;
      if (ch === "]") inClass = false;
      continue;
    }
    if (ch === "[") {
      inClass = true;
      current += ch;
      continue;
    }
    if (ch === "(") depth += 1;
    if (ch === ")") depth -= 1;
    if (ch === "|" && depth === 0) {
      parts.push(current);
      current = "";
      continue;
    }
    current += ch;
  }
  parts.push(current);
  return parts;
}

/**
 * What a branch can start with. `wide` means "could be anything" — an empty
 * branch (which makes the whole group nullable), a nested group, a wildcard,
 * or an anchor. Every `wide` overlaps everything, so it fails closed.
 */
type FirstAtom =
  | { kind: "char"; value: string }
  | { kind: "class" }
  | { kind: "wide" };

function firstAtom(branch: string): FirstAtom {
  if (branch.length === 0) return { kind: "wide" };
  const ch = branch[0];
  if (ch === "\\") {
    const next = branch[1];
    if (next === undefined) return { kind: "wide" };
    // Shorthand classes cover many characters; a literal escape is one.
    return /[dDwWsSbB]/.test(next) ? { kind: "class" } : { kind: "char", value: next };
  }
  if (ch === "[") return { kind: "class" };
  if (ch === "." || ch === "(" || ch === "^" || ch === "$") return { kind: "wide" };
  return { kind: "char", value: ch };
}

/**
 * Whether two branch openings can accept the same character. Character-class
 * membership is not computed: proving `[a-z]` and `[0-9]` disjoint would mean
 * parsing ranges, negation and escapes, and getting that subtly wrong is how
 * a guard silently stops guarding. Any class is therefore assumed to overlap.
 */
function atomsCanOverlap(a: FirstAtom, b: FirstAtom): boolean {
  if (a.kind === "wide" || b.kind === "wide") return true;
  if (a.kind === "char" && b.kind === "char") return a.value === b.value;
  return true;
}

/**
 * True when `body` can match runs of DIFFERENT lengths.
 *
 * Repeating a variable-length body is what forces the engine to try every way
 * of partitioning the input among the iterations. Unboundedness is not the
 * property that matters — `(aa?)*` is bounded at two characters per iteration
 * and still took 132 ms against 24 characters in this repository's fuzz run,
 * because "a" and "aa" are both legal iterations and every split has to be
 * explored. `?` and `{n,m}` are therefore as disqualifying as `*` and `+`;
 * only a fixed `{n}` leaves the body one length.
 *
 * A quantifier character never legally follows `(`, so one that does is a
 * group opener (`(?:…)`) rather than a repetition, and is skipped.
 */
function bodyIsVariableLength(body: string): boolean {
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
    const opensGroup = i > 0 && body[i - 1] === "(";
    if ((ch === "*" || ch === "+") && !opensGroup) return true;
    if (ch === "?" && !opensGroup) return true;
    if (ch === "{") {
      const brace = /^\{(\d*),(\d*)\}/.exec(body.slice(i));
      // `{n,}` and `{n,m}` with n !== m both vary; a bare `{n}` does not.
      if (brace && brace[1] !== brace[2]) return true;
    }
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
