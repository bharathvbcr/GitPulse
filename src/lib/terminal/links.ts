/**
 * Linkification for terminal output: which spans of a rendered line are
 * openable, and what opening them is allowed to mean.
 *
 * Terminal output is the least trusted text in the application. It is
 * whatever a shell, an agent CLI, a build tool, or a `curl` of a hostile
 * server decided to print, and a linkifier turns it into one click away from
 * the OS URL dispatcher and the repository's own file viewer. So the rules
 * here are allowlists that fail closed, not denylists:
 *
 *  - A URL is only a link when its scheme is one this module names. Anything
 *    else — `javascript:`, `data:`, `file:`, a registered app handler like
 *    `vscode:` or `smb:` — is not decorated, not hoverable, and not openable.
 *    Output that contains one renders as ordinary text, which is the honest
 *    result: refusing to linkify is visible, whereas a scheme check that only
 *    ran at click time would look identical to a safe link until it fired.
 *  - A file reference only resolves inside the session's repository. An
 *    absolute path elsewhere on disk, or a relative path that climbs out with
 *    `..`, resolves to null and never becomes a link. Terminal output naming
 *    `/etc/passwd` must not become a one-click read of it.
 *
 * Scanning is hand-rolled rather than regex-driven. Every pass over the text
 * is a single left-to-right walk with no backtracking, so a pathological line
 * costs time linear in its length instead of exponential in its structure —
 * the terminal's line length is attacker-chosen, which is exactly the input
 * that makes a nested-quantifier regex a freeze.
 */

/**
 * Schemes GitPulse will hand to the OS opener. Deliberately just the two web
 * schemes: every other scheme reaches a handler chosen by whatever is
 * installed on the machine, which is not a decision terminal output gets to
 * make. Compared lowercase; schemes are case-insensitive.
 */
export const OPENABLE_URL_SCHEMES: ReadonlySet<string> = new Set(["http", "https"]);

/**
 * How much of one logical (wrap-joined) line is scanned. A wrapped line can
 * be thousands of columns by thousands of rows; the cost of scanning it lands
 * on a mousemove, so the work is capped and the cap is reported rather than
 * being allowed to read as "this line has no links".
 */
export const MAX_SCAN_LENGTH = 8192;

/** Links reported for one logical line. A line of nothing but URLs is a
 * legitimate output shape and must not be able to allocate without bound. */
export const MAX_LINKS_PER_LINE = 64;

/** Longest URL that is still a URL. Beyond this the span is left as text. */
export const MAX_URL_LENGTH = 2048;

/** Longest path that is still a path, before any `:line:column` suffix. */
export const MAX_PATH_LENGTH = 1024;

export type TerminalLinkKind = "url" | "file";

export interface DetectedLink {
  /** Inclusive start index into the scanned text. */
  start: number;
  /** Exclusive end index into the scanned text. */
  end: number;
  /** The exact substring `[start, end)`. */
  text: string;
  kind: TerminalLinkKind;
}

export interface FileTarget {
  /** The path as written, separators untouched. */
  path: string;
  /** 1-based line, when the reference carried one. */
  line: number | null;
  /** 1-based column, when the reference carried one. */
  column: number | null;
}

export interface ResolvedFile {
  /** Repository-relative POSIX path — what `repoStore.selectFilePath` takes. */
  path: string;
  line: number | null;
  column: number | null;
}

const CHAR_COLON = 58;
const CHAR_SLASH = 47;
const CHAR_BACKSLASH = 92;
const CHAR_DOT = 46;

/**
 * Characters that end a bare token. Whitespace and control characters are
 * handled by code point so the set stays small and the test stays a
 * comparison rather than a lookup.
 *
 * The brackets and quotes are here because output quotes and parenthesises
 * paths constantly — `(src/a.ts:3)`, `"src/b.ts"`, `[src/c.ts]` — and the
 * delimiter is never part of the name. `=` is here so `--out=dist/x.js`
 * yields the path rather than the flag.
 */
const TOKEN_DELIMITERS = new Set([
  '"', "'", "`", "<", ">", "|", "(", ")", "[", "]", "{", "}", ",", ";", "*", "?", "=", "&",
]);

/** True for space, tab, and every C0/C1 control code. */
function isBlankOrControl(code: number): boolean {
  return code <= 0x20 || code === 0x7f || (code >= 0x80 && code <= 0x9f);
}

function isTokenChar(ch: string): boolean {
  return !isBlankOrControl(ch.charCodeAt(0)) && !TOKEN_DELIMITERS.has(ch);
}

/**
 * Characters that end a URL. A much smaller set than `TOKEN_DELIMITERS`,
 * because almost everything that delimits a *path* is legal inside a URL:
 * `?`, `=`, `&` and `;` carry the query, `(` and `)` appear in article
 * titles, `,` in matrix parameters. Ending the scan at those truncates real
 * URLs silently — `https://x/a?b=1` becomes `https://x/a`, which still opens
 * and still looks right, so the loss is invisible at the click.
 *
 * What remains are characters that cannot appear unescaped in a URL and do
 * appear around one: the quote forms output wraps links in, the angle
 * brackets of `<https://…>`, the pipe of a shell, and the backslash of a
 * Windows path. The unbalanced tail is the trim's job, not the scan's.
 */
const URL_TERMINATORS = new Set(['"', "'", "`", "<", ">", "|", "\\"]);

function isUrlChar(ch: string): boolean {
  return !isBlankOrControl(ch.charCodeAt(0)) && !URL_TERMINATORS.has(ch);
}

/** `a`-`z`, `A`-`Z`, `0`-`9`, `+`, `-`, `.` — RFC 3986 scheme characters. */
function isSchemeChar(code: number): boolean {
  return (
    (code >= 97 && code <= 122) ||
    (code >= 65 && code <= 90) ||
    (code >= 48 && code <= 57) ||
    code === 43 ||
    code === 45 ||
    code === 46
  );
}

function isAsciiLetter(code: number): boolean {
  return (code >= 97 && code <= 122) || (code >= 65 && code <= 90);
}

function isDigit(code: number): boolean {
  return code >= 48 && code <= 57;
}

/**
 * Drops the punctuation a sentence leaves stuck to the end of a URL.
 *
 * The bracket rule is the reason this is not a simple trailing-character
 * strip: `https://en.wikipedia.org/wiki/Shell_(computing)` ends in `)` that
 * belongs to the URL, while `(see https://example.com)` ends in one that does
 * not. Counting tells them apart — a closer is dropped only when the span
 * holds no opener to match it.
 */
function trimUrlTail(url: string): string {
  // Bracket counts for the whole string up front, then decremented as the
  // tail is stripped, so `counts` always describes `url.slice(0, end)`.
  // Recounting inside the loop instead would be quadratic in the URL length,
  // and the URL length comes from terminal output — a line of 2048 `)` would
  // buy four million character comparisons on a mousemove.
  const counts = new Map<string, number>([
    ["(", 0], [")", 0], ["[", 0], ["]", 0], ["{", 0], ["}", 0],
  ]);
  for (let i = 0; i < url.length; i++) {
    const existing = counts.get(url[i]);
    if (existing !== undefined) counts.set(url[i], existing + 1);
  }
  let end = url.length;
  for (;;) {
    if (end === 0) break;
    const ch = url[end - 1];
    if (ch === "." || ch === "," || ch === ";" || ch === ":" || ch === "!" || ch === "?" || ch === "'" || ch === '"') {
      end -= 1;
      continue;
    }
    const opener = ch === ")" ? "(" : ch === "]" ? "[" : ch === "}" ? "{" : null;
    if (opener && (counts.get(ch) ?? 0) > (counts.get(opener) ?? 0)) {
      counts.set(ch, (counts.get(ch) ?? 0) - 1);
      end -= 1;
      continue;
    }
    break;
  }
  return url.slice(0, end);
}

/**
 * Whether this URL may be handed to the OS opener.
 *
 * Parsed with the platform `URL` rather than string-matched: `http:/\/\x`
 * and percent-encoded scheme tricks are the whole reason a prefix comparison
 * is not enough. A URL that will not parse is not openable.
 */
export function isOpenableUrl(url: string): boolean {
  if (!url || url.length > MAX_URL_LENGTH) return false;
  // A control character or space inside the string means it was never one
  // token to begin with; `URL` tolerates some of them by stripping.
  for (let i = 0; i < url.length; i++) {
    if (isBlankOrControl(url.charCodeAt(i))) return false;
  }
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  // `protocol` keeps the trailing colon and is already lowercased.
  return OPENABLE_URL_SCHEMES.has(parsed.protocol.slice(0, -1));
}

/**
 * Splits a `path:line:column` reference. Returns null when the token is not
 * plausibly a path at all, so callers never have to re-validate.
 *
 * A token qualifies when it contains a separator (`src/a.ts`, `.\a.rs`) or
 * when a `:line` suffix makes the intent explicit (`main.rs:42`). A bare word
 * is left alone: linkifying every word in `cargo build` would be noise, and
 * noise in a security-relevant affordance trains people to click past it.
 */
export function parseFileTarget(token: string): FileTarget | null {
  if (!token || token.length > MAX_PATH_LENGTH + 24) return null;
  // A URL is not a path, even though `https://x/y.ts` looks like one once the
  // scheme is ignored. URLs are claimed by the URL pass before this runs; this
  // is the guard for the case where they are not.
  if (token.includes("://")) return null;
  if (token.includes("\0")) return null;

  let path = token;
  let line: number | null = null;
  let column: number | null = null;

  // Two trailing `:N` groups at most, taken from the right. Bounded digit
  // runs keep an absurd `a.ts:99999999999999` from becoming a line number
  // instead of failing to be one.
  for (let pass = 0; pass < 2; pass++) {
    const colon = path.lastIndexOf(":");
    if (colon <= 0 || colon === path.length - 1) break;
    const digits = path.slice(colon + 1);
    if (digits.length > 9) break;
    let allDigits = true;
    for (let i = 0; i < digits.length; i++) {
      if (!isDigit(digits.charCodeAt(i))) { allDigits = false; break; }
    }
    if (!allDigits) break;
    const value = Number(digits);
    if (value < 1) break;
    // Groups are consumed right to left, so the first one found is the
    // rightmost. In `a.ts:12:5` that is the column: the second pass demotes
    // the value already held into `column` and keeps the newer, leftward one
    // as the line. One group alone is always a line.
    if (line === null) line = value;
    else { column = line; line = value; }
    path = path.slice(0, colon);
  }

  if (!path || path.length > MAX_PATH_LENGTH) return null;
  // A Windows drive prefix is not a line number, and `C:` alone is not a path.
  if (path.length === 2 && path[1] === ":" && isAsciiLetter(path.charCodeAt(0))) return null;

  /**
   * Anything carrying a URI scheme is a URI, not a path — and the `://` test
   * above is too narrow to say so. `data:text/html,<script>…` has no
   * authority, so it slipped past, and then read as a relative path because
   * it happens to contain a `/`: the activation path would have called it a
   * file inside the repository. Any scheme-shaped prefix is refused here, so
   * the URL allowlist cannot be walked around by dropping the slashes.
   *
   * The one exception is a Windows drive, which is a single letter — a real
   * scheme is at least two characters, so the two never collide.
   */
  const firstColon = path.indexOf(":");
  if (firstColon > 1) {
    let schemeLike = isAsciiLetter(path.charCodeAt(0));
    for (let i = 1; schemeLike && i < firstColon; i++) {
      if (!isSchemeChar(path.charCodeAt(i))) schemeLike = false;
    }
    if (schemeLike) return null;
  }

  const hasSeparator = path.includes("/") || path.includes("\\");
  const hasExtension = (() => {
    const dot = path.lastIndexOf(".");
    if (dot <= 0 || dot === path.length - 1) return false;
    const ext = path.slice(dot + 1);
    if (ext.length > 12) return false;
    for (let i = 0; i < ext.length; i++) {
      const code = ext.charCodeAt(i);
      if (!isAsciiLetter(code) && !isDigit(code)) return false;
    }
    return true;
  })();

  if (!hasSeparator && !(hasExtension && line !== null)) return null;
  // Separators with nothing between them: `/`, `//`, `../..`, `\\`.
  let hasName = false;
  for (let i = 0; i < path.length; i++) {
    const code = path.charCodeAt(i);
    if (code !== CHAR_SLASH && code !== CHAR_BACKSLASH && code !== CHAR_DOT) { hasName = true; break; }
  }
  if (!hasName) return null;
  // A protocol-relative reference is a URL missing its scheme, not a path.
  if (path.startsWith("//") || path.startsWith("\\\\")) return null;

  return { path, line, column };
}

/**
 * Normalizes a path to POSIX separators with `.` and `..` resolved, without
 * touching the filesystem. Returns null when `..` climbs above the root of
 * the path, which for a relative path is the signal that it escapes.
 */
function normalizeSegments(path: string): string[] | null {
  const out: string[] = [];
  for (const segment of path.replace(/\\/g, "/").split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") {
      if (out.length === 0) return null;
      out.pop();
      continue;
    }
    out.push(segment);
  }
  return out;
}

function isAbsolute(path: string): boolean {
  if (path.startsWith("/") || path.startsWith("\\")) return true;
  // `C:/x` and `C:\x`.
  return path.length >= 3 && path[1] === ":" && isAsciiLetter(path.charCodeAt(0)) &&
    (path[2] === "/" || path[2] === "\\");
}

function driveOf(path: string): string | null {
  return isAbsolute(path) && path[1] === ":" ? path[0].toLowerCase() : null;
}

/**
 * Resolves a file reference against the session's repository, or refuses.
 *
 * Containment is the whole point, and it is checked on the *normalized
 * segment lists* rather than on strings. A string prefix test says
 * `/repo-evil/x` is inside `/repo`, and says `/repo/../etc/passwd` is too;
 * comparing resolved segments says neither is. An absolute path on a
 * different Windows drive is refused for the same reason.
 */
export function resolveRepoFile(repoPath: string, target: FileTarget): ResolvedFile | null {
  if (!repoPath) return null;
  const rootSegments = normalizeSegments(repoPath);
  if (!rootSegments) return null;

  let segments: string[] | null;
  if (isAbsolute(target.path)) {
    const targetDrive = driveOf(target.path);
    const rootDrive = driveOf(repoPath);
    if (targetDrive !== rootDrive) return null;
    segments = normalizeSegments(target.path);
    if (!segments) return null;
    if (segments.length <= rootSegments.length) return null;
    for (let i = 0; i < rootSegments.length; i++) {
      if (segments[i] !== rootSegments[i]) return null;
    }
    segments = segments.slice(rootSegments.length);
  } else {
    segments = normalizeSegments(target.path);
    // `normalizeSegments` returns null exactly when `..` climbed out.
    if (!segments || segments.length === 0) return null;
  }

  return { path: segments.join("/"), line: target.line, column: target.column };
}

/**
 * Finds every openable span in one logical line, left to right.
 *
 * URLs are claimed first and files only fill the gaps, so the `y.ts:1` tail
 * of a URL never becomes a second, overlapping link. Both passes are single
 * walks; neither can revisit a character.
 */
export function detectTerminalLinks(text: string): DetectedLink[] {
  const links: DetectedLink[] = [];
  if (!text) return links;
  const limit = Math.min(text.length, MAX_SCAN_LENGTH);

  // Pass 1: URLs, located by their `://` and then walked back to the scheme.
  for (let i = 0; i + 2 < limit && links.length < MAX_LINKS_PER_LINE; i++) {
    if (text.charCodeAt(i) !== CHAR_COLON) continue;
    if (text.charCodeAt(i + 1) !== CHAR_SLASH || text.charCodeAt(i + 2) !== CHAR_SLASH) continue;
    let start = i;
    while (start > 0 && isSchemeChar(text.charCodeAt(start - 1))) start -= 1;
    // A scheme is at least one character and must begin with a letter.
    if (start === i || !isAsciiLetter(text.charCodeAt(start))) continue;
    // Do not re-claim a span already inside a link (`http://a/http://b`).
    const previous = links[links.length - 1];
    if (previous && start < previous.end) continue;
    let end = i + 3;
    // Stop at the length cap rather than consuming an unbounded run and
    // discarding it afterwards: the trim and the `URL` parse that follow both
    // cost time proportional to what they are handed.
    const ceiling = Math.min(limit, start + MAX_URL_LENGTH + 1);
    while (end < ceiling && isUrlChar(text[end])) end += 1;
    if (end - start > MAX_URL_LENGTH) continue;
    const raw = trimUrlTail(text.slice(start, end));
    if (!isOpenableUrl(raw)) continue;
    links.push({ start, end: start + raw.length, text: raw, kind: "url" });
    i = start + raw.length - 1;
  }

  // Pass 2: file references, in the gaps the URL pass left.
  const gaps: Array<[number, number]> = [];
  let cursor = 0;
  for (const link of links) {
    if (link.start > cursor) gaps.push([cursor, link.start]);
    cursor = link.end;
  }
  if (cursor < limit) gaps.push([cursor, limit]);

  const files: DetectedLink[] = [];
  for (const [from, to] of gaps) {
    let i = from;
    while (i < to && links.length + files.length < MAX_LINKS_PER_LINE) {
      if (!isTokenChar(text[i])) { i += 1; continue; }
      let end = i;
      while (end < to && isTokenChar(text[end])) end += 1;
      // Sentence punctuation clings to paths exactly as it does to URLs, but
      // a path's own trailing `.` is never meaningful, so a plain strip is
      // right here where the bracket-counting rule was needed above.
      let stop = end;
      while (stop > i) {
        const ch = text[stop - 1];
        if (ch === "." || ch === ":" || ch === "!") { stop -= 1; continue; }
        break;
      }
      const token = text.slice(i, stop);
      const target = parseFileTarget(token);
      if (target) files.push({ start: i, end: stop, text: token, kind: "file" });
      i = end;
    }
  }

  if (files.length) {
    links.push(...files);
    links.sort((a, b) => a.start - b.start);
  }
  return links;
}

/** True when a logical line was longer than the scanner will read. Callers
 * report this rather than presenting a capped scan as a complete one. */
export function scanWasTruncated(text: string): boolean {
  return text.length > MAX_SCAN_LENGTH;
}

/**
 * How many wrapped rows may be joined into one logical line. A single
 * `cat` of a minified bundle wraps across the entire 5,000-row scrollback;
 * walking all of it on every mousemove is the cost this bounds.
 */
export const MAX_WRAP_ROWS = 64;

/** One row of the terminal buffer, as the linkifier needs to see it. */
export interface LinkBufferRow {
  /** True when this row continues the row above rather than starting a line. */
  isWrapped: boolean;
  /** The row's text. One string index per rendered character. */
  text: string;
  /**
   * `columns[i]` is the 1-based buffer column of `text[i]`.
   *
   * This exists because the two are not the same number. A double-width
   * character — CJK, most emoji — is one index in the string and two cells in
   * the grid, so a link range computed from string offsets lands progressively
   * further left as a line accumulates wide characters, and the underline
   * ends up under the wrong text.
   */
  columns: number[];
}

/** Random access to the buffer by 1-based absolute row. */
export interface LinkBuffer {
  row(index: number): LinkBufferRow | null;
}

export interface LogicalLine {
  text: string;
  /** `cells[i]` is the buffer position of `text[i]`, both 1-based. */
  cells: Array<{ x: number; y: number }>;
  /** True when the join stopped at a cap instead of at the line's real end. */
  truncated: boolean;
}

/**
 * Joins the wrapped rows around `row` into the one logical line a link may
 * span, carrying each character's buffer position along with it.
 *
 * Both directions are bounded. Climbing to the start of a wrap run and
 * descending to its end are each capped at `MAX_WRAP_ROWS`, and the joined
 * text is capped at `MAX_SCAN_LENGTH`; hitting any of them sets `truncated`
 * so a partial line is never mistaken for a whole one.
 */
export function logicalLineAt(buffer: LinkBuffer, row: number): LogicalLine {
  const text: string[] = [];
  const cells: Array<{ x: number; y: number }> = [];
  let truncated = false;

  let start = row;
  let climbed = 0;
  for (;;) {
    const current = buffer.row(start);
    if (!current?.isWrapped) break;
    if (climbed >= MAX_WRAP_ROWS) { truncated = true; break; }
    const above = buffer.row(start - 1);
    if (!above) break;
    start -= 1;
    climbed += 1;
  }

  let length = 0;
  for (let offset = 0; offset <= MAX_WRAP_ROWS; offset++) {
    const current = buffer.row(start + offset);
    if (!current) break;
    // The first row of the run is the line's own start; later rows only
    // belong to it while they are continuations.
    if (offset > 0 && !current.isWrapped) break;
    for (let i = 0; i < current.text.length; i++) {
      if (length >= MAX_SCAN_LENGTH) { truncated = true; break; }
      text.push(current.text[i]);
      cells.push({ x: current.columns[i] ?? i + 1, y: start + offset });
      length += 1;
    }
    if (length >= MAX_SCAN_LENGTH) { truncated = true; break; }
    if (offset === MAX_WRAP_ROWS) {
      // Only a truncation if the run actually continues past the cap.
      if (buffer.row(start + offset + 1)?.isWrapped) truncated = true;
    }
  }

  return { text: text.join(""), cells, truncated };
}

export interface LinkRange {
  start: { x: number; y: number };
  end: { x: number; y: number };
}

export interface PositionedLink extends DetectedLink {
  range: LinkRange;
}

/**
 * Places each detected span on the grid.
 *
 * `end` is inclusive of the link's last cell, which is xterm's convention for
 * `IBufferRange` — an exclusive end would underline one cell too many.
 */
export function positionLinks(line: LogicalLine, links: DetectedLink[]): PositionedLink[] {
  const placed: PositionedLink[] = [];
  for (const link of links) {
    const first = line.cells[link.start];
    const last = line.cells[link.end - 1];
    if (!first || !last) continue;
    placed.push({ ...link, range: { start: { ...first }, end: { ...last } } });
  }
  return placed;
}

/** The whole read path for one row: join, scan, place. */
export function linksForRow(buffer: LinkBuffer, row: number): PositionedLink[] {
  const line = logicalLineAt(buffer, row);
  if (!line.text) return [];
  return positionLinks(line, detectTerminalLinks(line.text));
}

export type LinkAction =
  | { kind: "url"; url: string }
  | { kind: "file"; path: string; line: number | null; column: number | null }
  | { kind: "refused"; reason: string };

/**
 * The single place that decides what clicking a link span is allowed to do.
 *
 * Both routes into activation come here: the spans this module detected, and
 * the OSC 8 hyperlinks the program embedded in its own output. The second is
 * the reason the check cannot live in the detector — an OSC 8 target is never
 * scanned, it arrives as an escape sequence with an arbitrary URI attached,
 * and `https://good.example` may be the *text* while the target is something
 * else entirely. Judging the target, here, at the moment of activation, is
 * what makes the two paths obey one rule.
 *
 * A refusal carries its reason so the UI can say why nothing happened.
 * Silence would be indistinguishable from a click that missed.
 */
export function resolveLinkAction(text: string, repoPath: string): LinkAction {
  const trimmed = text.trim();
  if (!trimmed) return { kind: "refused", reason: "Empty link." };
  // A path can never carry an http(s) scheme, so asking the URL question
  // first costs nothing and keeps the two branches mutually exclusive.
  if (isOpenableUrl(trimmed)) return { kind: "url", url: trimmed };
  const target = parseFileTarget(trimmed);
  if (target) {
    const resolved = resolveRepoFile(repoPath, target);
    if (resolved) return { kind: "file", ...resolved };
    return {
      kind: "refused",
      reason: `${target.path} is outside this repository — GitPulse only opens files inside it.`,
    };
  }
  return {
    kind: "refused",
    reason: "GitPulse only opens http and https links, and files inside this repository.",
  };
}
