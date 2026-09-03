#!/usr/bin/env node
/**
 * Verify the exact asset set produced by the configured Tauri release matrix.
 *
 * A suffix-only check can pass an old or unrelated installer. The matrix is
 * intentionally explicit here: changing a platform, target, or Tauri output
 * name must update this manifest and its tests at the same time.
 *
 * Exit codes: 0 exact non-empty manifest · 1 missing/unexpected asset ·
 * 2 malformed input or invalid asset metadata.
 */
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseTag } from "./check-release-version.mjs";
import { formatUsage, wantsHelp } from "./usage.mjs";

/**
 * These are the seven artifacts emitted by the current three-runner matrix:
 * macOS DMG and app archive, Linux RPM/AppImage/deb, and Windows MSI/NSIS.
 *
 * The macOS updater archive carries the version like every other asset.
 * `tauri-action@v0` emitted it unversioned; v1 does not, and this manifest
 * caught the rename on the first release built with v1 — which is what it is
 * for: the three build jobs were green and had uploaded a complete set, so
 * nothing else would have noticed the name change.
 *
 * @param {string} version
 * @returns {string[]}
 */
export function expectedAssetNames(version) {
  return [
    `GitPulse-${version}-1.x86_64.rpm`,
    `GitPulse_${version}_amd64.AppImage`,
    `GitPulse_${version}_amd64.deb`,
    `GitPulse_${version}_universal.dmg`,
    `GitPulse_${version}_x64-setup.exe`,
    `GitPulse_${version}_x64_en-US.msi`,
    `GitPulse_${version}_universal.app.tar.gz`,
  ].sort();
}

/** @param {unknown} error */
function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

/**
 * @typedef {{ name: string, size: number, state: string | null }} ReleaseAsset
 * @typedef {{ ok: boolean, invalid: boolean, version: string | null, expected: string[], actual: string[], violations: string[] }} AssetCheck
 */

/**
 * @param {unknown} value
 * @returns {ReleaseAsset[]}
 */
function parseAssets(value) {
  if (!value || typeof value !== "object") {
    throw new Error("JSON must contain an assets array");
  }
  const container = /** @type {{ assets: unknown[] }} */ (value);
  if (!Array.isArray(container.assets)) throw new Error("JSON must contain an assets array");
  /** @type {ReleaseAsset[]} */
  const assets = [];
  for (const [index, raw] of container.assets.entries()) {
    if (!raw || typeof raw !== "object") throw new Error(`assets[${index}] must be an object`);
    const asset = /** @type {{ name?: unknown, size?: unknown, state?: unknown }} */ (raw);
    if (typeof asset.name !== "string" || asset.name.trim() === "") {
      throw new Error(`assets[${index}].name must be a non-empty string`);
    }
    if (typeof asset.size !== "number" || !Number.isSafeInteger(asset.size) || asset.size <= 0) {
      throw new Error(`assets[${index}] ${JSON.stringify(asset.name)} must have a positive safe integer size`);
    }
    if (asset.state !== undefined && asset.state !== "uploaded") {
      throw new Error(`assets[${index}] ${JSON.stringify(asset.name)} is not uploaded (state=${JSON.stringify(asset.state)})`);
    }
    assets.push({
      name: asset.name,
      size: asset.size,
      state: typeof asset.state === "string" ? asset.state : null,
    });
  }
  if (assets.length === 0) throw new Error("assets array is empty");
  return assets;
}

/**
 * @param {{ tag: string, json: string | unknown }} input
 * @returns {AssetCheck}
 */
export function inspectReleaseAssets({ tag, json }) {
  const parsedTag = parseTag(tag);
  if (!parsedTag.ok) {
    return {
      ok: false,
      invalid: true,
      version: null,
      expected: [],
      actual: [],
      violations: [parsedTag.reason],
    };
  }

  let document;
  try {
    document = typeof json === "string" ? JSON.parse(json) : json;
  } catch (error) {
    return {
      ok: false,
      invalid: true,
      version: parsedTag.version,
      expected: expectedAssetNames(parsedTag.version),
      actual: [],
      violations: [`malformed JSON: ${errorMessage(error)}`],
    };
  }

  let assets;
  try {
    assets = parseAssets(document);
  } catch (error) {
    return {
      ok: false,
      invalid: true,
      version: parsedTag.version,
      expected: expectedAssetNames(parsedTag.version),
      actual: [],
      violations: [errorMessage(error)],
    };
  }

  const expected = expectedAssetNames(parsedTag.version);
  const actual = assets.map(({ name }) => name).sort();
  const violations = [];
  const seen = new Set();
  for (const asset of assets) {
    if (seen.has(asset.name)) violations.push(`duplicate: ${asset.name}`);
    seen.add(asset.name);
  }
  if (violations.length > 0) {
    return {
      ok: false,
      invalid: true,
      version: parsedTag.version,
      expected,
      actual,
      violations,
    };
  }
  const actualSet = new Set(actual);
  for (const name of expected) {
    if (!actualSet.has(name)) violations.push(`missing: ${name}`);
  }
  const expectedSet = new Set(expected);
  for (const name of actual) {
    if (!expectedSet.has(name)) violations.push(`unexpected: ${name}`);
  }
  return {
    ok: violations.length === 0,
    invalid: false,
    version: parsedTag.version,
    expected,
    actual,
    violations,
  };
}

/** @param {string[]} argv */
function parseArgs(argv) {
  /** @type {{ tag: string | null, jsonPath: string | null }} */
  const options = { tag: null, jsonPath: null };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) throw new Error(`${flag} requires a value`);
    if (flag === "--tag") options.tag = value;
    else if (flag === "--json") options.jsonPath = path.resolve(value);
    else throw new Error(`unknown option ${flag}`);
    index += 1;
  }
  if (!options.tag) throw new Error("--tag is required");
  if (!options.jsonPath) throw new Error("--json is required");
  return options;
}

/** @param {string[]} [argv] */

/** Backlog A2: asking for help is not an error, so this exits 0. */
export function usage() {
  return formatUsage({
    name: "check-release-assets",
    summary: "Verify a draft release's assets against the exact per-platform installer manifest.",
    flags: [
      { flag: "--tag <tag>".replace(/^"|"$/g, ""), description: "release tag being verified" },
      { flag: "--json <path>".replace(/^"|"$/g, ""), description: "path to the release JSON from the GitHub API" },
      { flag: "--help, -h".replace(/^"|"$/g, ""), description: "print this message and exit 0" }
    ],
    exits: "0 every expected asset is present · 1 one is missing · 2 the check could not run",
  });
}

export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  try {
    const options = parseArgs(argv);
    const tag = options.tag;
    const jsonPath = options.jsonPath;
    if (!tag || !jsonPath) throw new Error("--tag and --json are required");
    const json = readFileSync(jsonPath, "utf8");
    const result = inspectReleaseAssets({ tag, json });
    if (result.invalid) {
      for (const violation of result.violations) console.error(`FAIL: invalid release asset input: ${violation}`);
      return 2;
    }
    if (!result.ok) {
      for (const violation of result.violations) console.error(`FAIL: release asset manifest ${violation}`);
      return 1;
    }
    console.log(`Release ${tag} assets:`);
    for (const name of result.actual) console.log(`  ${name}`);
    console.log("OK: release asset manifest holds");
    return 0;
  } catch (error) {
    console.error(`FAIL: invalid release asset input: ${errorMessage(error)}`);
    return 2;
  }
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) process.exitCode = main();
