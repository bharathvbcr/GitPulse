/**
 * How a qualified graph node id is shown to a person.
 *
 * The kernel names a node `path/to/File.ext::Type.member`. Rendered whole in
 * a one-line slot that is what the reader gets: forty characters of directory
 * they already know, and the symbol — the part that answers "what breaks?" —
 * pushed past the clip. So the display order is inverted: symbol first, file
 * second and dimmed, path only in the tooltip.
 *
 * The `::` prefix is not always a path. `graphNodeOpenPath` already had to
 * decide that question to know whether a node could be opened in the editor,
 * and a second copy of the test would be free to drift from the one that
 * governs opening files; both now ask here.
 */

/**
 * Does this `::` prefix name a file?
 *
 * A directory separator or a trailing extension is the evidence. A bare
 * `Foo::bar` (C++, Rust paths without a file prefix) has neither and is a
 * symbol path in full.
 */
export function looksLikePath(prefix: string): boolean {
  return /[\\/]|\.[a-z0-9]+$/i.test(prefix);
}

/** The file part of a node id, or null when the id names no file. */
export function nodeFilePath(id: string): string | null {
  const separator = id.indexOf("::");
  if (separator <= 0) return null;
  const prefix = id.slice(0, separator);
  return looksLikePath(prefix) ? prefix : null;
}

export interface NodeLabel {
  /** What the reader scans for: `Type.member`, or the whole id if unqualified. */
  symbol: string;
  /** Final path segment, or null when the id names no file. */
  file: string | null;
  /** Full file path, for a tooltip. Null when the id names no file. */
  path: string | null;
}

/**
 * Split a node id for display.
 *
 * Splits on the *first* `::` only: a Rust or C++ symbol may contain more, and
 * they belong to the symbol side.
 */
export function nodeLabel(id: string): NodeLabel {
  const raw = (id ?? "").trim();
  if (!raw) return { symbol: "", file: null, path: null };

  const separator = raw.indexOf("::");
  if (separator <= 0) return { symbol: raw, file: null, path: null };

  const prefix = raw.slice(0, separator);
  const suffix = raw.slice(separator + 2).trim();
  if (!looksLikePath(prefix)) return { symbol: raw, file: null, path: null };

  // A path with nothing after `::` still names a file, and showing the file
  // is better than showing an empty row.
  const path = prefix.replace(/\\/g, "/");
  const segments = path.split("/").filter(Boolean);
  const file = segments.length > 0 ? segments[segments.length - 1]! : path;
  return { symbol: suffix || file, file, path };
}

/** `symbol · file` on one line, for a title attribute or a flat list. */
export function nodeLabelText(id: string): string {
  const label = nodeLabel(id);
  if (!label.symbol) return "";
  return label.file && label.file !== label.symbol
    ? `${label.symbol} · ${label.file}`
    : label.symbol;
}
