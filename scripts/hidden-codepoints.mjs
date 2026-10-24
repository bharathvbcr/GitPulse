#!/usr/bin/env node
// Build-time generator for scripts/fixtures/hidden-codepoints.json, the one
// table of which code points the notification text cleaner drops or turns into
// a space. Rust (src-tauri/src/alerts/text.rs, include_str!) and the renderer
// (src/lib/terminal/sessionActivity.ts, a JSON import) both read that file, so
// the two cleaners cannot disagree and are pinned to one Unicode version.
//
//   drop  = \p{Cf} | \p{Co}                 invisible, reordering or private use
//   space = (\p{Cc} | \s) minus drop        separators: become one space
//
// `node scripts/hidden-codepoints.mjs` rewrites the file; `--check` exits 1 when
// the committed file differs from what this node's Unicode data yields. The
// vitest drift test (scripts/hidden-codepoints.test.ts) runs the same check, so
// a Node or Unicode update fails loudly and is fixed by regenerating.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const TABLE_PATH = fileURLToPath(new URL("./fixtures/hidden-codepoints.json", import.meta.url));

const DROP = /^[\p{Cf}\p{Co}]$/u;
const SEPARATOR = /^[\p{Cc}\s]$/u;

/**
 * Inclusive `[lo, hi]` ranges of the code points `matches` accepts.
 * @param {(ch: string) => boolean} matches
 * @returns {number[][]}
 */
function ranges(matches) {
  const out = [];
  for (let cp = 0; cp <= 0x10ffff; cp += 1) {
    if (cp >= 0xd800 && cp <= 0xdfff) continue; // surrogates are not scalar values
    if (!matches(String.fromCodePoint(cp))) continue;
    const last = out[out.length - 1];
    if (last && last[1] === cp - 1) last[1] = cp;
    else out.push([cp, cp]);
  }
  return out;
}

export function generate() {
  return {
    unicode: String(process.versions.unicode),
    drop: ranges((ch) => DROP.test(ch)),
    space: ranges((ch) => SEPARATOR.test(ch) && !DROP.test(ch)),
  };
}

/** @param {{ unicode: string, drop: number[][], space: number[][] }} table */
export function render(table) {
  return `${JSON.stringify(table)}\n`;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const fresh = render(generate());
  if (process.argv.includes("--check")) {
    if (readFileSync(TABLE_PATH, "utf8") !== fresh) {
      console.error("scripts/fixtures/hidden-codepoints.json is stale for this Node's Unicode data; run: node scripts/hidden-codepoints.mjs");
      process.exit(1);
    }
  } else {
    writeFileSync(TABLE_PATH, fresh);
  }
}
