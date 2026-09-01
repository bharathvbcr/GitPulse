/**
 * Aligned two-column text for the script reports.
 *
 * Backlog A3: `formatReport()` padded labels with fixed-width string literals,
 * so adding a metric meant re-counting spaces and a long label silently broke
 * the alignment. The width is computed from the labels instead.
 *
 * @typedef {{ label: string, value: string, note?: string }} Row
 */

/**
 * @param {Row[]} rows
 * @param {{ indent?: string, gap?: string, separator?: string }} [options]
 * @returns {string[]}
 */
export function alignRows(rows, options = {}) {
  const { indent = "  ", gap = "  ", separator = ": " } = options;
  const width = rows.reduce((max, row) => Math.max(max, row.label.length), 0);
  return rows.map(({ label, value, note }) => {
    const head = `${indent}${label.padEnd(width)}${separator}${value}`;
    return note ? `${head}${gap}(${note})` : head;
  });
}

/**
 * Align a flag/description list, as used by the usage printer.
 *
 * @param {Array<{ flag: string, description: string }>} entries
 * @param {{ indent?: string, gap?: string }} [options]
 * @returns {string[]}
 */
export function alignFlags(entries, options = {}) {
  const { indent = "  ", gap = "  " } = options;
  const width = entries.reduce((max, { flag }) => Math.max(max, flag.length), 0);
  return entries.map(({ flag, description }) => `${indent}${flag.padEnd(width)}${gap}${description}`);
}
