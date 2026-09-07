/**
 * Scan-based markup helpers for tests and source contracts.
 *
 * These exist because a regex that "strips tags" or "strips comments" is both
 * a CodeQL finding and an incomplete filter: `/<!--.*?-->/` misses comments
 * that contain newlines, `/<script>/` misses `SCRIPT`, and one pass of
 * `/<[^>]*>/` leaves an unclosed `<script`. Locating tags and comments with
 * `indexOf` is the same job without those filters.
 */

export interface ScriptBlock {
  /** Inner source of the script element. */
  inner: string;
  /** Index in `source` where `inner` begins. */
  innerStart: number;
  /** Source after the matching close tag. */
  after: string;
}

interface TagSpan {
  start: number;
  end: number;
}

/**
 * Removes HTML/XML comments, including those that span newlines.
 *
 * An unclosed comment drops the remainder rather than leaving `<!--` in the
 * output — the same fail-closed choice as dropping an unclosed tag.
 */
export function stripMarkupComments(html: string): string {
  let out = "";
  let i = 0;
  while (i < html.length) {
    const start = html.indexOf("<!--", i);
    if (start === -1) {
      out += html.slice(i);
      break;
    }
    out += html.slice(i, start);
    const end = html.indexOf("-->", start + 4);
    if (end === -1) break;
    i = end + 3;
  }
  return out;
}

/**
 * Removes markup tags by repeatedly cutting `<...>` spans.
 *
 * Unclosed tags (no `>`) drop from the `<` to the end, so a truncated
 * `<script` cannot survive the way a single `/<[^>]*>/` replace would.
 */
export function stripMarkupTags(html: string): string {
  let out = html;
  for (;;) {
    const start = out.indexOf("<");
    if (start === -1) return out;
    const end = out.indexOf(">", start + 1);
    if (end === -1) return out.slice(0, start);
    out = out.slice(0, start) + out.slice(end + 1);
  }
}

/** Every `<script>` / `<SCRIPT>` block, in document order. */
export function scriptBlocks(source: string): ScriptBlock[] {
  const out: ScriptBlock[] = [];
  let from = 0;
  for (;;) {
    const open = findOpenTag(source, "script", from);
    if (open === null) break;
    const close = findCloseTag(source, "script", open.end);
    if (close === null) break;
    out.push({
      inner: source.slice(open.end, close.start),
      innerStart: open.end,
      after: source.slice(close.end),
    });
    from = close.end;
  }
  return out;
}

export function firstScriptBlock(source: string): ScriptBlock | null {
  return scriptBlocks(source)[0] ?? null;
}

function findOpenTag(source: string, name: string, from: number): TagSpan | null {
  const needle = `<${name}`;
  const lower = source.toLowerCase();
  let i = from;
  while (i < source.length) {
    const at = lower.indexOf(needle, i);
    if (at === -1) return null;
    const afterName = at + needle.length;
    const next = source[afterName];
    if (next !== undefined && /[A-Za-z0-9-]/.test(next)) {
      i = afterName;
      continue;
    }
    const gt = source.indexOf(">", afterName);
    if (gt === -1) return null;
    return { start: at, end: gt + 1 };
  }
  return null;
}

function findCloseTag(source: string, name: string, from: number): TagSpan | null {
  const needle = `</${name}`;
  const lower = source.toLowerCase();
  const at = lower.indexOf(needle, from);
  if (at === -1) return null;
  const afterName = at + needle.length;
  const next = source[afterName];
  if (next !== undefined && /[A-Za-z0-9-]/.test(next)) {
    return findCloseTag(source, name, afterName);
  }
  const gt = source.indexOf(">", afterName);
  if (gt === -1) return null;
  return { start: at, end: gt + 1 };
}
