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

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/**
 * These are the twelve installers emitted by the current five-runner matrix:
 * macOS universal DMG and app archive; Linux RPM/AppImage/deb on x86_64 and
 * aarch64; and Windows MSI/NSIS on x64 and arm64. The ARM names are the Tauri
 * bundler's own arch spellings (deb/NSIS/MSI `arm64`, RPM/AppImage
 * `aarch64`), not a convention chosen here.
 *
 * The macOS updater archive carries the version like every other asset.
 * `tauri-action@v0` emitted it unversioned; v1 does not, and this manifest
 * caught the rename on the first release built with v1 — which is what it is
 * for: the three build jobs were green and had uploaded a complete set, so
 * nothing else would have noticed the name change.
 *
 * Keyed by the release.yml runner label that uploads each group.
 *
 * @type {Readonly<Record<string, (version: string) => string[]>>}
 */
export const PLATFORM_INSTALLERS = Object.freeze({
  "macos-latest": (version) => [`GitPulse_${version}_universal.dmg`, `GitPulse_${version}_universal.app.tar.gz`],
  "ubuntu-22.04": (version) => [
    `GitPulse-${version}-1.x86_64.rpm`, `GitPulse_${version}_amd64.AppImage`, `GitPulse_${version}_amd64.deb`,
  ],
  "ubuntu-22.04-arm": (version) => [
    `GitPulse-${version}-1.aarch64.rpm`, `GitPulse_${version}_aarch64.AppImage`, `GitPulse_${version}_arm64.deb`,
  ],
  "windows-latest": (version) => [`GitPulse_${version}_x64-setup.exe`, `GitPulse_${version}_x64_en-US.msi`],
  "windows-11-arm": (version) => [`GitPulse_${version}_arm64-setup.exe`, `GitPulse_${version}_arm64_en-US.msi`],
});

/**
 * @param {string} version
 * @returns {string[]}
 */
export function expectedInstallerNames(version) {
  return Object.values(PLATFORM_INSTALLERS).flatMap((names) => names(version)).sort();
}

/**
 * Checks the installers one runner bundled locally (tauri-action's
 * `artifactPaths`) against the names that runner must upload. tauri-action
 * renames the macOS updater archive at upload time, so a local macOS bundle
 * cannot be judged by its file name and is refused rather than passed.
 *
 * @param {{ platform: string, version: string, paths: unknown }} input
 * @returns {{ ok: boolean, expected: string[], actual: string[], violations: string[] }}
 */
export function inspectBundles({ platform, version, paths }) {
  const names = PLATFORM_INSTALLERS[platform];
  if (!names) throw new Error(`unknown release platform ${JSON.stringify(platform)}`);
  if (platform.startsWith("macos-")) throw new Error("macOS bundle names are rewritten at upload; only the release draft can check them");
  if (!Array.isArray(paths) || paths.length === 0 || !paths.every((entry) => typeof entry === "string" && entry !== "")) {
    throw new Error("artifact paths must be a non-empty array of strings");
  }
  const installer = /\.(?:rpm|deb|AppImage|msi|exe)$/;
  const actual = paths.map((entry) => path.basename(entry.replace(/\\/g, "/"))).filter((name) => installer.test(name)).sort();
  const expected = names(version).sort();
  const violations = [
    ...expected.filter((name) => !actual.includes(name)).map((name) => `missing: ${name}`),
    ...actual.filter((name) => !expected.includes(name)).map((name) => `unexpected: ${name}`),
  ];
  if (new Set(actual).size !== actual.length) violations.push("duplicate installer name");
  return { ok: violations.length === 0, expected, actual, violations };
}

/**
 * Files the attest stage of release.yml adds once every installer is on the
 * draft: the SHA-256 manifest of every other asset, an SPDX SBOM of the whole
 * build tree (Cargo, npm, Go, Actions) and a CycloneDX SBOM of the Rust graph.
 *
 * @param {string} version
 */
export function supplementNames(version) {
  return {
    checksums: `GitPulse_${version}_SHA256SUMS.txt`,
    spdx: `GitPulse_${version}_sbom.spdx.json`,
    cargo: `GitPulse_${version}_sbom.cargo.cdx.json`,
  };
}

/**
 * The complete published set: every installer plus every supplement.
 *
 * @param {string} version
 * @returns {string[]}
 */
export function expectedAssetNames(version) {
  return [...expectedInstallerNames(version), ...Object.values(supplementNames(version))].sort();
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
    if (asset.state !== "uploaded") {
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
  /** @type {{ tag: string | null, jsonPath: string | null, bundles: string | null, platform: string | null }} */
  const options = { tag: null, jsonPath: null, bundles: null, platform: null };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) throw new Error(`${flag} requires a value`);
    if (flag === "--tag") options.tag = value;
    else if (flag === "--json") options.jsonPath = path.resolve(value);
    else if (flag === "--bundles") options.bundles = value;
    else if (flag === "--platform") options.platform = value;
    else throw new Error(`unknown option ${flag}`);
    index += 1;
  }
  if (options.bundles !== null || options.platform !== null) {
    if (!options.bundles || !options.platform) throw new Error("--bundles and --platform are required together");
    if (options.tag || options.jsonPath) throw new Error("--bundles cannot be combined with --tag or --json");
    return options;
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
      { flag: "--bundles <json> --platform <label>", description: "instead: check one runner's tauri-action artifactPaths against the installers it must upload, at the src-tauri/tauri.conf.json version" },
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
    if (options.bundles && options.platform) {
      const config = JSON.parse(readFileSync(path.join(REPO_ROOT, "src-tauri", "tauri.conf.json"), "utf8"));
      if (typeof config.version !== "string" || !config.version) throw new Error("tauri.conf.json has no version");
      const result = inspectBundles({ platform: options.platform, version: config.version, paths: JSON.parse(options.bundles) });
      for (const name of result.actual) console.log(`  ${name}`);
      if (!result.ok) {
        for (const violation of result.violations) console.error(`FAIL: ${options.platform} bundle ${violation}`);
        return 1;
      }
      console.log(`OK: ${options.platform} bundled exactly the installers release.yml uploads`);
      return 0;
    }
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
