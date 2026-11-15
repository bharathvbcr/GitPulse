#!/usr/bin/env node
/**
 * Proves that the macOS Rust shards between them ran every test, once.
 *
 * `cargo nextest run --partition hash:M/N` runs one bucket of the suite per CI
 * job and reports the rest as skipped, so a job that is green says only that
 * its own bucket passed. A bucket that never ran (its job skipped, its report
 * missing, its build broken) leaves the others green and the suite short, and
 * nothing in the run reads "tests missing". This takes the unpartitioned list
 * (`cargo nextest list --message-format json`) and each shard's JUnit report
 * and fails unless the tests that ran are exactly the tests that were listed.
 *
 * A test counts as run when it has a <testcase> in a shard's report, passed or
 * failed: whether it passed is that shard's own verdict. The question here is
 * only whether it was attempted, and attempted once.
 *
 * Exit codes: 0 every listed test ran exactly once · 1 tests are missing,
 * duplicated or unlisted · 2 the check could not run (a file missing or not in
 * the shape nextest documents), which must never read like a pass.
 *
 * Shapes, from https://nexte.st/docs/machine-readable/list/ and
 * https://nexte.st/docs/machine-readable/junit/: the list is
 * `rust-suites.<binary-id>.testcases.<name> = { ignored, filter-match.status }`;
 * a report has one <testsuite> per binary and one <testcase> per test, whose
 * `classname` is the binary id and `name` the bare test name. Skipped and
 * ignored tests are written only when `report-skipped` is set, which the `ci`
 * profile leaves off, so a report holds exactly what that shard ran.
 */

import { existsSync, readFileSync } from "node:fs";
import { formatUsage, wantsHelp } from "./usage.mjs";

/** The id one test goes by in both documents. A tab cannot occur in either part. */
const idOf = (/** @type {string} */ binary, /** @type {string} */ name) => `${binary}\t${name}`;

export class CouldNotRun extends Error {}

/**
 * Tests a full run would execute: listed, not `#[ignore]`d, and matching the
 * (empty) filter.
 *
 * @param {unknown} document the parsed `cargo nextest list --message-format json`
 * @returns {Set<string>}
 */
export function expectedTests(document) {
  const suites = /** @type {any} */ (document)?.["rust-suites"];
  if (!suites || typeof suites !== "object" || Array.isArray(suites)) {
    throw new CouldNotRun("the test list has no `rust-suites` object");
  }
  /** @type {Set<string>} */
  const expected = new Set();
  let listed = 0;
  for (const [binary, suite] of Object.entries(suites)) {
    const cases = /** @type {any} */ (suite)?.testcases;
    if (!cases || typeof cases !== "object") {
      throw new CouldNotRun(`suite ${binary} has no \`testcases\` object`);
    }
    for (const [name, info] of Object.entries(cases)) {
      listed += 1;
      const ignored = /** @type {any} */ (info)?.ignored;
      const status = /** @type {any} */ (info)?.["filter-match"]?.status;
      if (typeof ignored !== "boolean" || typeof status !== "string") {
        throw new CouldNotRun(`test ${binary} ${name} lacks \`ignored\` or \`filter-match.status\``);
      }
      if (!ignored && status === "matches") expected.add(idOf(binary, name));
    }
  }
  if (listed === 0 || expected.size === 0) throw new CouldNotRun("the test list holds no runnable tests");
  return expected;
}

/** @param {string} text */
function unescapeXml(text) {
  return text
    .replace(/&#x([0-9a-fA-F]+);/g, (_, hex) => String.fromCodePoint(parseInt(hex, 16)))
    .replace(/&#(\d+);/g, (_, dec) => String.fromCodePoint(Number(dec)))
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
}

/**
 * @param {string} tag the attribute text of one element
 * @param {string} attribute
 */
function attributeOf(tag, attribute) {
  return new RegExp(`(?:^|\\s)${attribute}="([^"]*)"`).exec(tag)?.[1];
}

/**
 * The tests one shard's report says it ran, in order, repeats included.
 *
 * @param {string} xml
 * @returns {string[]}
 */
export function ranTests(xml) {
  if (!/<testsuites?\b/.test(xml)) throw new CouldNotRun("not a JUnit report: no <testsuite>");
  /** @type {string[]} */
  const ran = [];
  for (const match of xml.matchAll(/<testcase\b([^>]*?)\/?>/g)) {
    const binary = attributeOf(match[1], "classname");
    const name = attributeOf(match[1], "name");
    if (binary === undefined || name === undefined) {
      throw new CouldNotRun("a <testcase> has no `classname` or `name`");
    }
    ran.push(idOf(unescapeXml(binary), unescapeXml(name)));
  }
  return ran;
}

/**
 * @param {Set<string>} expected
 * @param {string[][]} shards what each shard ran
 * @returns {{ missing: string[], unlisted: string[], duplicated: string[] }}
 */
export function compare(expected, shards) {
  /** @type {Map<string, number>} */
  const seen = new Map();
  for (const shard of shards) for (const id of shard) seen.set(id, (seen.get(id) ?? 0) + 1);
  const readable = (/** @type {string} */ id) => id.replace("\t", " ");
  return {
    missing: [...expected].filter((id) => !seen.has(id)).map(readable).sort(),
    unlisted: [...seen.keys()].filter((id) => !expected.has(id)).map(readable).sort(),
    duplicated: [...seen].filter(([, count]) => count > 1).map(([id]) => readable(id)).sort(),
  };
}

function usage() {
  return formatUsage({
    name: "check-shard-coverage",
    summary: "Fail unless the Rust test shards between them ran every listed test exactly once.",
    flags: [
      { flag: "--list <file>", description: "`cargo nextest list --message-format json` of the whole suite" },
      { flag: "--shards <n>", description: "How many reports to expect" },
      { flag: "<junit.xml>...", description: "One JUnit report per shard" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits: "0 covered · 1 tests missing, duplicated or unlisted · 2 could not run",
  });
}

/** @param {string[]} argv */
export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  let list = "";
  let count = 0;
  /** @type {string[]} */
  const reports = [];
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--list") list = argv[++i] ?? "";
    else if (argv[i] === "--shards") count = Number(argv[++i]);
    else reports.push(argv[i]);
  }
  try {
    if (!list || !Number.isInteger(count) || count < 1) throw new CouldNotRun("--list and a positive --shards are required");
    if (reports.length !== count) {
      throw new CouldNotRun(`expected ${count} shard reports, got ${reports.length}`);
    }
    for (const file of [list, ...reports]) {
      if (!existsSync(file)) throw new CouldNotRun(`${file} does not exist`);
    }
    const expected = expectedTests(JSON.parse(readFileSync(list, "utf8")));
    const shards = reports.map((file) => ranTests(readFileSync(file, "utf8")));
    const { missing, unlisted, duplicated } = compare(expected, shards);
    const sizes = shards.map((shard) => shard.length).join(" + ");
    if (missing.length || unlisted.length || duplicated.length) {
      /** @type {[string, string[]][]} */
      const groups = [["not run by any shard", missing], ["run but not listed", unlisted], ["run by more than one shard", duplicated]];
      for (const [label, ids] of groups) {
        if (ids.length) console.error(`FAIL: ${ids.length} test(s) ${label}:\n  ${ids.slice(0, 20).join("\n  ")}${ids.length > 20 ? `\n  … and ${ids.length - 20} more` : ""}`);
      }
      return 1;
    }
    console.log(`OK: ${expected.size} listed tests each ran once across ${count} shards (${sizes})`);
    return 0;
  } catch (error) {
    if (error instanceof CouldNotRun || error instanceof SyntaxError) {
      console.error(`could not check shard coverage: ${error.message}`);
      return 2;
    }
    throw error;
  }
}

if (process.argv[1]?.endsWith("check-shard-coverage.mjs")) {
  process.exitCode = main();
}
