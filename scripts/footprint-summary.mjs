/**
 * The category summary of macOS `footprint -v`.
 *
 * Region lines are ignored. A category that is not in the summary is absent
 * (`null`), never zero: zero is what the tool prints when it measured nothing.
 *
 *   node scripts/memory-footprint.mjs --footprint <file>
 */

const UNIT = { B: 1, KB: 1024, MB: 1024 * 1024, GB: 1024 * 1024 * 1024 };

const CATEGORY =
  /^\s*(\d+(?:\.\d+)?)\s+(B|KB|MB|GB)\s+(\d+(?:\.\d+)?)\s+(B|KB|MB|GB)\s+(\d+(?:\.\d+)?)\s+(B|KB|MB|GB)\s+(\d+)\s+(\S(?:.*\S)?)\s*$/;

/** @param {string} amount @param {string} unit */
export function footprintBytes(amount, unit) {
  if (unit !== "B" && unit !== "KB" && unit !== "MB" && unit !== "GB") return null;
  const factor = UNIT[unit];
  const value = Number(amount);
  if (!Number.isFinite(value)) return null;
  return Math.round(value * factor);
}

/**
 * @param {string} text
 * @returns {{
 *   physFootprintBytes: number | null,
 *   physFootprintPeakBytes: number | null,
 *   webkitMalloc: { dirtyBytes: number, cleanBytes: number, reclaimableBytes: number, regions: number } | null,
 *   categories: Record<string, { dirtyBytes: number, cleanBytes: number, reclaimableBytes: number, regions: number }>,
 * }}
 */
export function parseFootprint(text) {
  /** @type {Record<string, { dirtyBytes: number, cleanBytes: number, reclaimableBytes: number, regions: number }>} */
  const categories = {};
  for (const line of text.split("\n")) {
    const match = CATEGORY.exec(line);
    if (!match) continue;
    const dirty = footprintBytes(match[1], match[2]);
    const clean = footprintBytes(match[3], match[4]);
    const reclaimable = footprintBytes(match[5], match[6]);
    if (dirty === null || clean === null || reclaimable === null) continue;
    categories[match[8]] = {
      dirtyBytes: dirty,
      cleanBytes: clean,
      reclaimableBytes: reclaimable,
      regions: Number(match[7]),
    };
  }
  const phys = /^ *phys_footprint:\s*(\d+(?:\.\d+)?)\s*(B|KB|MB|GB)\s*$/m.exec(text);
  const peak = /^ *phys_footprint_peak:\s*(\d+(?:\.\d+)?)\s*(B|KB|MB|GB)\s*$/m.exec(text);
  return {
    physFootprintBytes: phys ? footprintBytes(phys[1], phys[2]) : null,
    physFootprintPeakBytes: peak ? footprintBytes(peak[1], peak[2]) : null,
    webkitMalloc: categories["WebKit Malloc"] ?? null,
    categories,
  };
}

/** @param {number | null} bytes */
function mb(bytes) {
  if (bytes === null) return "absent";
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

/** @param {ReturnType<typeof parseFootprint>} summary */
export function formatFootprintSummary(summary) {
  const lines = [
    `phys_footprint: ${mb(summary.physFootprintBytes)}`,
    `phys_footprint_peak: ${mb(summary.physFootprintPeakBytes)}`,
  ];
  const webkit = summary.webkitMalloc;
  if (!webkit) lines.push("WebKit Malloc: absent");
  else {
    lines.push(`WebKit Malloc dirty: ${mb(webkit.dirtyBytes)}`);
    lines.push(`WebKit Malloc reclaimable: ${mb(webkit.reclaimableBytes)}`);
    lines.push(`WebKit Malloc regions: ${webkit.regions}`);
  }
  return lines.join("\n");
}
