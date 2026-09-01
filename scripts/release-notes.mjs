#!/usr/bin/env node
/**
 * Extract one release's notes from CHANGELOG.md.
 *
 * `release.yml` used to carry the release body as a literal YAML block, so the
 * notes described v0.0.3 no matter which tag was being built. The changelog is
 * the single place a human writes them; this reads the section for the tag.
 *
 * A tag with no section is a hard failure, not an empty release body: shipping
 * a release whose notes silently vanished is worse than failing the build.
 *
 * Exit codes: 0 notes were found · 1 no section for the tag · 2 could not run
 */

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/**
 * `v0.0.3` and `0.0.3` both address the `## [0.0.3]` heading.
 *
 * @param {unknown} tag
 * @returns {string}
 */
export function normalizeTag(tag) {
  const trimmed = String(tag ?? "").trim();
  if (!trimmed) throw new Error("tag is empty");
  return trimmed.startsWith("v") ? trimmed.slice(1) : trimmed;
}

/**
 * @param {string} markdown
 * @param {string} tag
 * @returns {{ found: true, version: string, date: string | null, body: string }
 *          | { found: false, versions: string[] }}
 */
export function extractNotes(markdown, tag) {
  const version = normalizeTag(tag);
  const lines = markdown.split("\n");
  /** @type {Array<{ version: string, date: string | null, start: number }>} */
  const sections = [];
  lines.forEach((line, index) => {
    const heading = /^##\s+\[([^\]]+)\]\s*(?:-\s*(\S+))?\s*$/.exec(line);
    if (heading) sections.push({ version: heading[1], date: heading[2] ?? null, start: index });
  });

  const at = sections.findIndex((s) => s.version === version);
  if (at === -1) return { found: false, versions: sections.map((s) => s.version) };

  const end = at + 1 < sections.length ? sections[at + 1].start : lines.length;
  const body = lines
    .slice(sections[at].start + 1, end)
    // drop the link-reference footer if it trails the last section
    .filter((line) => !/^\[[^\]]+\]:\s+https?:\/\//.test(line))
    .join("\n")
    .trim();

  if (!body) return { found: false, versions: sections.map((s) => s.version) };
  return { found: true, version, date: sections[at].date, body };
}

/**
 * @param {string[]} argv
 * @returns {{ tag: string | null, changelog: string | null, help: boolean }}
 */
function parseArgs(argv) {
  /** @type {{ tag: string | null, changelog: string | null, help: boolean }} */
  const options = { tag: null, changelog: null, help: false };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === "--help" || flag === "-h") return { ...options, help: true };
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) throw new Error(`${flag} requires a value`);
    if (flag === "--tag") options.tag = value;
    else if (flag === "--changelog") options.changelog = path.resolve(value);
    else throw new Error(`unknown option ${flag}`);
    index += 1;
  }
  return options;
}

/** @param {string[]} [argv] */
export function main(argv = process.argv.slice(2)) {
  let options;
  try {
    options = parseArgs(argv);
  } catch (error) {
    console.error(`FAIL: release-notes input: ${error instanceof Error ? error.message : error}`);
    return 2;
  }
  if (options.help) {
    console.log("Usage: node scripts/release-notes.mjs --tag <vX.Y.Z> [--changelog <path>]");
    console.log("  --tag <tag>          release tag, with or without the leading v");
    console.log("  --changelog <path>   changelog path (default: CHANGELOG.md at the repo root)");
    return 0;
  }
  if (!options.tag) {
    console.error("FAIL: --tag is required");
    return 2;
  }
  const changelog =
    options.changelog ??
    path.join(path.dirname(path.dirname(fileURLToPath(import.meta.url))), "CHANGELOG.md");
  let markdown;
  try {
    markdown = readFileSync(changelog, "utf8");
  } catch (error) {
    console.error(`FAIL: cannot read ${changelog}: ${error instanceof Error ? error.message : error}`);
    return 2;
  }
  let result;
  try {
    result = extractNotes(markdown, options.tag);
  } catch (error) {
    console.error(`FAIL: ${error instanceof Error ? error.message : error}`);
    return 2;
  }
  if (!result.found) {
    console.error(
      `FAIL: ${changelog} has no non-empty section for ${options.tag}. ` +
        `Add a "## [${normalizeTag(options.tag)}] - YYYY-MM-DD" heading. ` +
        `Sections present: ${result.versions.join(", ") || "none"}`,
    );
    return 1;
  }
  process.stdout.write(`${result.body}\n`);
  return 0;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
