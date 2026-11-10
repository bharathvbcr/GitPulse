/**
 * The Swift WebKit runner's source with `page` appended to its harness
 * allow-list, for scripts that open a page BROWSER_HARNESSES does not list.
 *
 * The list is found by its terminator, `].contains(url.path)`, not by its last
 * entry: anchoring on one harness name broke the moment another harness was
 * registered after it (browser-session after mutation-routing, 2026-11-09).
 * Throws when the source has no such list, or more than one.
 *
 * @param {string} source webkit-regressions.swift
 * @param {string} page   a path such as "/harness/terminal-scroll.html"
 * @returns {string}
 */
export function allowWebkitPage(source, page) {
  const terminator = /("\/harness\/[^"]+\.html")\](\.contains\(url\.path\))/g;
  const lists = source.match(terminator) ?? [];
  if (lists.length !== 1) throw new Error(`webkit-regressions.swift has ${lists.length} harness allow-lists; expected one to patch`);
  return source.replace(terminator, (_match, last, contains) => `${last}, ${JSON.stringify(page)}]${contains}`);
}
