/**
 * Paths as git writes them in a patch, decoded once.
 *
 * Three modules were parsing these independently — the patch builder (to
 * target `git apply`), the single-file filter in `wordDiff`, and now the
 * outline — and they disagreed. Only the patch builder decoded git's octal
 * escapes, so a path with a non-ASCII byte read as `sp ace/Ã©.ts` in one place
 * and `sp ace/é.ts` in another; only `wordDiff` handled the fully quoted
 * `diff --git` form. Every one of those disagreements is a file the UI
 * cannot line up with the patch it is about to stage.
 *
 * Rules git actually follows, which this encodes:
 * - A path needing quoting is wrapped in `"` with C escapes, and every
 *   non-ASCII byte becomes `\NNN` octal — BYTES, so they must be reassembled
 *   before being decoded as UTF-8.
 * - `--- a/x` and `+++ b/y` are authoritative for where a patch applies;
 *   `diff --git` is a header line and is ambiguous for paths containing
 *   ` b/`, which is exactly why git repeats the paths below it.
 * - `/dev/null` on either side is unprefixed and means the file was created
 *   or deleted.
 */

/**
 * Decodes the body of git's C-quoted path (the text between the quotes).
 *
 * Escapes are bytes, so they are collected and decoded as UTF-8 in one pass;
 * decoding each escape on its own turns `\303\251` into `Ã©`.
 */
export function decodeQuotedGitPath(body: string): string {
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

/** Unwraps `"…"` when present, leaving a bare path untouched. */
export function unquoteGitPath(value: string): string {
  if (value.length >= 2 && value.startsWith('"') && value.endsWith('"')) {
    return decodeQuotedGitPath(value.slice(1, -1));
  }
  return value;
}

/**
 * The path from a raw `--- `/`+++ ` header line.
 *
 * Drops the tab-separated timestamp git appends in some modes, and the stray
 * `\r` a CRLF diff leaves on the header. Returns the path with its `a/`/`b/`
 * side prefix still attached, because whether to strip it is the caller's
 * decision: `/dev/null` must survive unprefixed.
 */
export function parseHeaderPath(headerLine: string): string {
  let rest = headerLine.slice(4);
  const tab = rest.indexOf("\t");
  if (tab >= 0) rest = rest.slice(0, tab);
  rest = rest.replace(/\r$/, "");
  return unquoteGitPath(rest);
}

/**
 * Strips exactly one leading side prefix.
 *
 * Only the leading occurrence goes, so a repository with a real top-level
 * `a/` directory keeps targeting the right files, and `/dev/null` passes
 * through verbatim because git requires it unprefixed.
 */
export function stripSidePrefix(path: string, side?: "a" | "b"): string {
  if (path === "/dev/null") return path;
  if (side) return path.startsWith(`${side}/`) ? path.slice(2) : path;
  return path.startsWith("a/") || path.startsWith("b/") ? path.slice(2) : path;
}

/** Reads one quoted side starting at `from`, or null when it is not quoted. */
function readQuoted(rest: string, from: number): { value: string; next: number } | null {
  if (rest[from] !== '"') return null;
  let i = from + 1;
  while (i < rest.length) {
    if (rest[i] === "\\") {
      i += 2;
      continue;
    }
    if (rest[i] === '"') break;
    i += 1;
  }
  if (i >= rest.length) return null;
  return { value: decodeQuotedGitPath(rest.slice(from + 1, i)), next: i + 1 };
}

/**
 * Splits `diff --git <old> <new>` into both sides, side prefixes removed.
 *
 * Git quotes a side only when it has to, so all four combinations of quoted
 * and bare occur. The bare/bare case is genuinely ambiguous for a path
 * containing ` b/`; see the comment on that branch. `---`/`+++` override this
 * wherever they exist, which is every case except a binary file.
 */
export function gitHeaderSides(header: string): [string, string] | null {
  const rest = header.slice("diff --git ".length);
  if (rest.length === 0) return null;

  const first = readQuoted(rest, 0);
  if (first) {
    const tail = rest.slice(first.next).replace(/^ /, "");
    const second = readQuoted(tail, 0);
    const right = second ? second.value : tail;
    return [stripSidePrefix(first.value), stripSidePrefix(right)];
  }

  const quotedSecond = rest.indexOf(' "b/');
  if (quotedSecond > 0) {
    const right = readQuoted(rest, quotedSecond + 1);
    if (right) {
      return [stripSidePrefix(rest.slice(0, quotedSecond)), stripSidePrefix(right.value)];
    }
  }

  // Bare/bare. Git does not quote a path merely for containing spaces, so
  // `a/foo bar.txt b/foo bar.txt` is a real line with three ` b/`-shaped
  // splits and only one right answer. Prefer the split whose two sides name
  // the same file, which settles every case except a rename — and a rename
  // is the one case git itself repeats below in `rename from`/`rename to`.
  let fallback = -1;
  for (let cut = rest.indexOf(" b/"); cut > 0; cut = rest.indexOf(" b/", cut + 1)) {
    if (fallback < 0) fallback = cut;
    const left = stripSidePrefix(rest.slice(0, cut));
    const right = stripSidePrefix(rest.slice(cut + 1));
    if (left === right) return [left, right];
  }
  if (fallback <= 0) return null;
  return [stripSidePrefix(rest.slice(0, fallback)), stripSidePrefix(rest.slice(fallback + 1))];
}
