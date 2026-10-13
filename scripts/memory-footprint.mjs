#!/usr/bin/env node
/**
 * What one cached commit graph and one terminal's scrollback cost in memory.
 *
 * The renderer's footprint is inferred from tab and session counts until it is
 * measured, and the two figures below are the ones the graph cache limit
 * (`GRAPH_CACHE_LIMIT`) and the terminal scrollback defaults are chosen from.
 * Run it with the garbage collector exposed so the deltas are retained bytes,
 * not allocation noise:
 *
 *   node --expose-gc scripts/memory-footprint.mjs [fixture.json]
 *
 * - Graph: the rows of a committed real-repository fixture, replicated to a
 *   full first page (`DEFAULT_MAX_COMMITS`) as distinct objects, and the heap
 *   they retain.
 * - Terminal: the bundled xterm, filled past its scrollback, and the bytes its
 *   buffer lines hold (`cols * 12` per row) plus the process heap and
 *   ArrayBuffer deltas.
 *
 * It reports; it asserts nothing. The installed app's WebContent RSS is a
 * separate measurement (`scripts/native-sample.mjs`).
 */
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";

if (typeof global.gc !== "function") {
  console.error("run with: node --expose-gc scripts/memory-footprint.mjs");
  process.exit(2);
}

const require = createRequire(new URL("../package.json", import.meta.url));
/** The emulator's buffer internals are what is being measured, so its type is opened up on purpose. */
const { Terminal } = /** @type {{ Terminal: any }} */ (require("@xterm/xterm"));

const FIRST_PAGE_ROWS = 5000;
/** @param {number} bytes */
const mb = (bytes) => (bytes / 1024 / 1024).toFixed(2);
const collect = /** @type {() => void} */ (global.gc);
const settle = () => {
  collect();
  collect();
};

/** @param {string} fixtureName */
function measureGraph(fixtureName) {
  const fixture = JSON.parse(
    readFileSync(new URL(`../src/lib/canvas/__tests__/fixtures/${fixtureName}`, import.meta.url), "utf8"),
  );
  const base = fixture.rows ?? fixture;
  const text = JSON.stringify(base);
  settle();
  const before = process.memoryUsage().heapUsed;
  const rows = [];
  while (rows.length < FIRST_PAGE_ROWS) {
    for (const row of JSON.parse(text)) if (rows.length < FIRST_PAGE_ROWS) rows.push(row);
  }
  settle();
  const retained = process.memoryUsage().heapUsed - before;
  console.log(
    `graph ${fixtureName}: ${rows.length} rows (from ${base.length}) retain ${mb(retained)} MB, ${(retained / rows.length).toFixed(0)} bytes a row`,
  );
  return rows.length;
}

/**
 * @param {number} scrollback
 * @param {number} cols
 * @param {number} visibleRows
 */
async function measureTerminal(scrollback, cols, visibleRows) {
  settle();
  const before = process.memoryUsage();
  const term = new Terminal({ cols, rows: visibleRows, scrollback, allowProposedApi: true });
  const line = `${"x".repeat(cols - 4)}\r\n`;
  const total = scrollback + visibleRows + 100;
  await new Promise((resolve) => term.write(line.repeat(total), () => resolve(undefined)));
  settle();
  const after = process.memoryUsage();
  const lines = term._core._bufferService.buffer.lines;
  let cellBytes = 0;
  let held = 0;
  for (let i = 0; i < lines.length; i++) {
    const bufferLine = lines.get(i);
    if (bufferLine) {
      cellBytes += bufferLine._data.byteLength;
      held += 1;
    }
  }
  console.log(
    `terminal scrollback=${scrollback} cols=${cols}: ${held} rows hold ${mb(cellBytes)} MB of cells; ` +
      `heap +${mb(after.heapUsed - before.heapUsed)} MB, ArrayBuffers +${mb(after.arrayBuffers - before.arrayBuffers)} MB`,
  );
  term.dispose();
}

for (const fixture of process.argv[2] ? [process.argv[2]] : ["scholarlmConfluence.json", "devprismMainline.json"]) {
  measureGraph(fixture);
}
for (const scrollback of [5000, 2000, 1000, 500]) await measureTerminal(scrollback, 200, 50);
await measureTerminal(5000, 120, 40);
