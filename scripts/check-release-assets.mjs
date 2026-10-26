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
import { spawnSync } from "node:child_process";
import { closeSync, existsSync, lstatSync, mkdtempSync, openSync, readFileSync, readSync, readdirSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";
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

/**
 * The executables the app crate builds, read from its Cargo manifest so no
 * second list can drift from it: a sixth `[[bin]]` becomes required here the
 * moment it is declared. Walks lines, not one regex, so the key may sit
 * anywhere in its block and a commented-out block counts for nothing.
 *
 * @param {string} cargoToml
 * @returns {string[]}
 */
export function declaredBinaryNames(cargoToml) {
  /** @type {string[]} */
  const names = [];
  let inBin = false;
  for (const raw of cargoToml.split(/\r?\n/)) {
    const line = raw.trim();
    if (line === "" || line.startsWith("#")) continue;
    if (line.startsWith("[")) {
      inBin = /^\[\[bin\]\]\s*(?:#.*)?$/.test(line);
      continue;
    }
    if (!inBin) continue;
    const match = /^name\s*=\s*"([^"]+)"\s*(?:#.*)?$/.exec(line);
    if (match) names.push(match[1]);
  }
  return names;
}

/** Bounds on what one bundle may cost to open. */
const MAX_BUNDLE_BYTES = 512 * 1024 * 1024;
/** A universal app with five fat binaries unpacks to roughly 150 MB; this is a gzip-bomb ceiling, not a target. */
const MAX_UNPACKED_BYTES = 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES = 100_000;
const EXTRACT_TIMEOUT_MS = 120_000;

const MACHO_FAT_MAGIC = 0xcafebabe;
const MACHO_CPU_ARM64 = 0x0100000c;
const MACHO_CPU_X86_64 = 0x01000007;
const ELF_MACHINE = { x64: 0x3e, arm64: 0xb7 };
const PE_MACHINE = { x64: 0x8664, arm64: 0xaa64 };
/** The app's main binary; every other `[[bin]]` is an extra the installer lays beside it. */
const MAIN_BINARY = "gitpulse";

/** The check could not read a bundle at all, which is not the same as reading it and finding it wrong. */
class UnreadableBundle extends Error {}

/**
 * @param {Buffer} block
 * @param {number} start
 * @param {number} length
 * @returns {number}
 */
function tarNumber(block, start, length) {
  const field = block.subarray(start, start + length);
  if (field[0] & 0x80) {
    let value = field[0] & 0x7f;
    for (let index = 1; index < field.length; index += 1) value = value * 256 + field[index];
    if (!Number.isSafeInteger(value)) throw new UnreadableBundle("tar number out of range");
    return value;
  }
  const text = field.toString("latin1").replace(/[\0 ]+$/, "").replace(/^ +/, "");
  if (text === "") return 0;
  if (!/^[0-7]+$/.test(text)) throw new UnreadableBundle("tar number is not octal");
  return parseInt(text, 8);
}

/** @param {Buffer} buffer @param {number} start @param {number} length */
function cString(buffer, start, length) {
  const field = buffer.subarray(start, start + length);
  const end = field.indexOf(0);
  return (end === -1 ? field : field.subarray(0, end)).toString("utf8");
}

/** @param {Buffer} data @returns {Map<string, string>} */
function paxRecords(data) {
  /** @type {Map<string, string>} */
  const records = new Map();
  let offset = 0;
  while (offset < data.length) {
    const space = data.indexOf(0x20, offset);
    const length = space === -1 ? NaN : Number(data.subarray(offset, space).toString("latin1"));
    if (!Number.isSafeInteger(length) || length <= space - offset + 1 || offset + length > data.length || data[offset + length - 1] !== 0x0a) {
      throw new UnreadableBundle("malformed pax record");
    }
    const body = data.subarray(space + 1, offset + length - 1).toString("utf8");
    const equals = body.indexOf("=");
    if (equals === -1) throw new UnreadableBundle("malformed pax record");
    records.set(body.slice(0, equals), body.slice(equals + 1));
    offset += length;
  }
  return records;
}

/**
 * @typedef {{ path: string, type: "file" | "dir" | "symlink" | "hardlink" | "other", mode: number, data: Buffer }} TarEntry
 */

/**
 * Reads a ustar/GNU/pax archive. The path an extractor would write is the one
 * returned: a pax `path` or a GNU `L` long name replaces the header's own name.
 * A size that runs past the archive, a checksum that does not match, or a
 * size on an entry that carries no data refuses the whole archive.
 *
 * @param {Buffer} tar
 * @returns {TarEntry[]}
 */
export function readTar(tar) {
  /** @type {TarEntry[]} */
  const entries = [];
  let offset = 0;
  /** @type {string | null} */
  let longName = null;
  /** @type {Map<string, string> | null} */
  let pax = null;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) return entries;
    if (entries.length >= MAX_ARCHIVE_ENTRIES) throw new UnreadableBundle("tar has too many entries");
    const recorded = tarNumber(header, 148, 8);
    let sum = 0;
    for (let index = 0; index < 512; index += 1) sum += index >= 148 && index < 156 ? 0x20 : header[index];
    if (sum !== recorded) throw new UnreadableBundle("tar header checksum mismatch");
    const flag = String.fromCharCode(header[156] || 0x30);
    let size = tarNumber(header, 124, 12);
    // A pax `size` governs the entry that follows its extended header, as it
    // does for an extractor; reading the header's own size there would desync.
    if (pax?.has("size") && !"LKxg".includes(flag)) {
      size = Number(pax.get("size"));
      if (!Number.isSafeInteger(size) || size < 0) throw new UnreadableBundle("pax size is not a number");
    }
    const carriesData = "0 7 L K x g".split(" ").includes(flag);
    if (!carriesData && size !== 0) throw new UnreadableBundle(`tar ${flag} entry has a size`);
    const start = offset + 512;
    if (carriesData && start + size > tar.length) throw new UnreadableBundle("tar entry runs past the archive");
    const data = carriesData ? tar.subarray(start, start + size) : tar.subarray(start, start);
    offset = start + Math.ceil(size / 512) * 512;
    if (flag === "L") { longName = cString(data, 0, data.length); continue; }
    if (flag === "K" || flag === "g") continue;
    if (flag === "x") { pax = paxRecords(data); continue; }
    const magic = header.subarray(257, 263).toString("latin1");
    let name = cString(header, 0, 100);
    if (magic === "ustar\0" && header[345] !== 0) name = `${cString(header, 345, 155)}/${name}`;
    if (longName !== null) name = longName;
    if (pax?.has("path")) name = pax.get("path") ?? name;
    longName = null;
    pax = null;
    /** @type {TarEntry["type"]} */
    const type = flag === "0" || flag === "7" ? "file" : flag === "5" ? "dir" : flag === "2" ? "symlink" : flag === "1" ? "hardlink" : "other";
    entries.push({ path: name, type, mode: tarNumber(header, 100, 8), data });
  }
  throw new UnreadableBundle("tar has no end-of-archive marker");
}

/**
 * Returns the one `data.tar.gz` member of a Debian package. Another
 * compression is a layout this check cannot open, so it refuses rather than
 * passing a package it did not read.
 *
 * @param {Buffer} deb
 * @returns {Buffer}
 */
export function readDebData(deb) {
  if (deb.subarray(0, 8).toString("latin1") !== "!<arch>\n") throw new UnreadableBundle("not an ar archive");
  /** @type {Buffer | null} */
  let data = null;
  let offset = 8;
  while (offset + 60 <= deb.length) {
    const header = deb.subarray(offset, offset + 60);
    if (header.subarray(58, 60).toString("latin1") !== "`\n") throw new UnreadableBundle("ar member header is damaged");
    const name = header.subarray(0, 16).toString("latin1").trim().replace(/\/$/, "");
    const sizeText = header.subarray(48, 58).toString("latin1").trim();
    if (!/^\d+$/.test(sizeText)) throw new UnreadableBundle("ar member size is not decimal");
    const size = Number(sizeText);
    const start = offset + 60;
    if (start + size > deb.length) throw new UnreadableBundle("ar member runs past the archive");
    if (name.startsWith("#1/")) throw new UnreadableBundle("ar BSD long member names are not supported");
    if (name.startsWith("data.tar")) {
      if (name !== "data.tar.gz") throw new UnreadableBundle(`deb data member is ${name}; only data.tar.gz can be opened`);
      if (data) throw new UnreadableBundle("deb has two data members");
      data = deb.subarray(start, start + size);
    }
    offset = start + size + (size % 2);
  }
  if (!data) throw new UnreadableBundle("deb has no data.tar.gz member");
  return data;
}

/** @param {string} entryPath @returns {string} */
function normalizeTarPath(entryPath) {
  return entryPath.replace(/^(?:\.\/)+/, "").replace(/\/+$/, "");
}

/**
 * Violations for one binary found by path in a tar's entries.
 *
 * @param {TarEntry[]} entries
 * @param {string} wanted
 * @param {string} binary
 * @param {(entry: TarEntry) => string | null} judge
 * @returns {string[]}
 */
function judgeTarBinary(entries, wanted, binary, judge) {
  const found = entries.filter((entry) => normalizeTarPath(entry.path) === wanted);
  if (found.length === 0) return [`missing: ${binary}`];
  if (found.length > 1) return [`duplicate: ${binary}`];
  const [entry] = found;
  if (entry.type === "symlink") return [`symlink: ${binary}`];
  if (entry.type === "hardlink") return [`hardlink: ${binary}`];
  if (entry.type !== "file") return [`not-regular: ${binary}`];
  if (entry.data.length === 0) return [`empty: ${binary}`];
  if ((entry.mode & 0o111) === 0) return [`not-executable: ${binary}`];
  const problem = judge(entry);
  return problem ? [`${problem}: ${binary}`] : [];
}

/** @param {TarEntry[]} entries @returns {string[]} */
function unsafeTarPaths(entries) {
  return entries
    .filter((entry) => normalizeTarPath(entry.path).split("/").includes("..") || entry.path.startsWith("/"))
    .map((entry) => `unsafe-path: ${entry.path}`);
}

/** A universal Mach-O must carry exactly an arm64 and an x86_64 slice. @param {TarEntry} entry */
function judgeUniversalMachO(entry) {
  const { data } = entry;
  if (data.length < 8 || data.readUInt32BE(0) !== MACHO_FAT_MAGIC) return "wrong-arch";
  const count = data.readUInt32BE(4);
  if (count === 0 || count > 8 || 8 + count * 20 > data.length) return "wrong-arch";
  const cpus = [];
  for (let index = 0; index < count; index += 1) cpus.push(data.readUInt32BE(8 + index * 20));
  const unique = new Set(cpus);
  return unique.size === 2 && unique.has(MACHO_CPU_ARM64) && unique.has(MACHO_CPU_X86_64) && cpus.length === 2 ? null : "wrong-arch";
}

/** @param {"x64" | "arm64"} arch @returns {(entry: TarEntry) => string | null} */
function judgeElf(arch) {
  return ({ data }) => {
    const ok = data.length >= 20 && data.readUInt32BE(0) === 0x7f454c46 && data[4] === 2 && data[5] === 1 &&
      data.readUInt16LE(18) === ELF_MACHINE[arch];
    return ok ? null : "wrong-arch";
  };
}

/**
 * Reads the PE machine of a file on disk without loading it.
 *
 * @param {string} file
 * @returns {number | null}
 */
function peMachine(file) {
  const fd = openSync(file, "r");
  try {
    const head = Buffer.alloc(64);
    if (readSync(fd, head, 0, 64, 0) < 64 || head.readUInt16BE(0) !== 0x4d5a) return null;
    const lfanew = head.readUInt32LE(0x3c);
    const nt = Buffer.alloc(6);
    if (lfanew > 0x10_0000 || readSync(fd, nt, 0, 6, lfanew) < 6 || nt.readUInt32BE(0) !== 0x50450000) return null;
    return nt.readUInt16LE(4);
  } finally {
    closeSync(fd);
  }
}

/**
 * Extracts a Windows installer with 7-Zip, which opens both MSI and NSIS and
 * ships on the runner image the release `verify` job uses. A missing `7z`
 * throws, so it can never read as an installer that held everything.
 *
 * @param {string} archive
 * @param {string} outDir
 */
export function extractWithSevenZip(archive, outDir) {
  // -aou keeps a second entry of the same name beside the first (as `name_1`)
  // instead of overwriting it, so a duplicate is visible in the output.
  const result = spawnSync("7z", ["x", "-aou", "-bd", "-bso0", "-bsp0", `-o${outDir}`, archive], {
    stdio: ["ignore", "ignore", "pipe"], encoding: "utf8", timeout: EXTRACT_TIMEOUT_MS, maxBuffer: 1024 * 1024,
  });
  if (result.error) throw new UnreadableBundle(`7z could not run: ${result.error.message}`);
  if (result.status !== 0) throw new UnreadableBundle(`7z exited ${result.status}: ${(result.stderr ?? "").trim().slice(0, 300)}`);
}

/**
 * Every file under `root`, by `lstat`: a symlink is a finding and is never
 * followed, so extraction output cannot point the walk elsewhere.
 *
 * @param {string} root
 * @returns {{ name: string, full: string, symlink: boolean, hardlink: boolean, size: number, regular: boolean }[]}
 */
function walkExtracted(root) {
  const found = [];
  const pending = [root];
  while (pending.length > 0) {
    const dir = /** @type {string} */ (pending.pop());
    for (const item of readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, item.name);
      const stat = lstatSync(full);
      if (stat.isDirectory()) { pending.push(full); continue; }
      found.push({ name: item.name, full, symlink: stat.isSymbolicLink(), hardlink: stat.isFile() && stat.nlink > 1, size: stat.size, regular: stat.isFile() });
      if (found.length > MAX_ARCHIVE_ENTRIES) throw new UnreadableBundle("installer unpacks to too many files");
    }
  }
  return found;
}

/**
 * The File Id the WiX template gives each binary. The main binary is `Path`;
 * every other `[[bin]]` is `Bin_<name>` with `-` as `_` (tauri-bundler's
 * main.wxs; observed on the v1.3.1 MSI as Bin_gitpulse_hook and friends).
 * 7-Zip names an MSI's files by Id, since the real name lives in a table it
 * does not apply.
 *
 * @param {string} binary
 */
function msiFileId(binary) {
  return binary === MAIN_BINARY ? "Path" : `Bin_${binary.replaceAll("-", "_")}`;
}

/**
 * @param {string} archive
 * @param {"msi" | "nsis"} format
 * @param {"x64" | "arm64"} arch
 * @param {string[]} binaries
 * @param {string} workDir
 * @param {(archive: string, outDir: string) => void} extract
 * @returns {string[]}
 */
function inspectWindowsInstaller(archive, format, arch, binaries, workDir, extract) {
  const outDir = mkdtempSync(path.join(workDir, "unpack-"));
  try {
    extract(archive, outDir);
    const files = walkExtracted(outDir);
    /** @type {string[]} */
    const violations = [];
    for (const binary of binaries) {
      const wanted = format === "msi" ? msiFileId(binary) : `${binary}.exe`;
      const matches = files.filter((file) => file.name === wanted);
      const stem = wanted.replace(/\.exe$/, "");
      const renamed = files.filter((file) => new RegExp(`^${stem.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}_\\d+(?:\\.exe)?$`).test(file.name));
      if (matches.length === 0) { violations.push(`missing: ${binary}`); continue; }
      if (matches.length > 1 || renamed.length > 0) { violations.push(`duplicate: ${binary}`); continue; }
      const [file] = matches;
      if (file.symlink) violations.push(`symlink: ${binary}`);
      else if (!file.regular) violations.push(`not-regular: ${binary}`);
      else if (file.hardlink) violations.push(`hardlink: ${binary}`);
      else if (file.size === 0) violations.push(`empty: ${binary}`);
      else if (peMachine(file.full) !== PE_MACHINE[arch]) violations.push(`wrong-arch: ${binary}`);
    }
    return violations;
  } finally {
    rmSync(outDir, { recursive: true, force: true });
  }
}

/**
 * @typedef {{ asset: string, format: "app-tar" | "deb" | "msi" | "nsis", arch: "universal" | "x64" | "arm64" }} BundleSpec
 */

/**
 * The installers whose contents are opened, derived from the release matrix
 * rather than listed again. The dmg, rpm and AppImage are not opened.
 *
 * @param {string} version
 * @returns {BundleSpec[]}
 */
export function bundleSpecs(version) {
  /** @type {BundleSpec[]} */
  const specs = [];
  for (const [platform, names] of Object.entries(PLATFORM_INSTALLERS)) {
    const arch = platform.endsWith("-arm") ? "arm64" : platform.startsWith("macos-") ? "universal" : "x64";
    for (const asset of names(version)) {
      if (asset.endsWith(".app.tar.gz")) specs.push({ asset, format: "app-tar", arch });
      else if (asset.endsWith(".deb")) specs.push({ asset, format: "deb", arch });
      else if (asset.endsWith(".msi")) specs.push({ asset, format: "msi", arch });
      else if (asset.endsWith("-setup.exe")) specs.push({ asset, format: "nsis", arch });
    }
  }
  return specs;
}

/**
 * @param {string} version
 * @param {number} opened
 */
function coverageSummary(version, opened) {
  const all = expectedInstallerNames(version);
  const openedNames = new Set(bundleSpecs(version).map((spec) => spec.asset));
  const unique = [...new Set(all.filter((name) => !openedNames.has(name)).map((name) => name.slice(name.lastIndexOf(".") + 1)))].sort();
  const list = unique.length > 1 ? `${unique.slice(0, -1).join(", ")} and ${unique[unique.length - 1]}` : unique.join("");
  return `opened ${opened} of ${all.length} installers; ${list} are not opened`;
}

/**
 * @typedef {{ ok: boolean, invalid: boolean, violations: string[], opened: string[], coverage: string }} ContentCheck
 */

/**
 * Opens each bundled installer in `files` (asset name to a local path) and
 * requires every declared binary inside, of the right architecture. Iterates
 * the expected assets, so an installer that was not provided is a violation,
 * not a skipped row. A bundle that cannot be read at all sets `invalid`.
 *
 * @param {{ version: string, binaries: string[], files: Record<string, string | undefined>, workDir?: string, extract?: (archive: string, outDir: string) => void, maxUnpackedBytes?: number }} input
 * @returns {ContentCheck}
 */
export function inspectBundleContents({ version, binaries, files, workDir, extract = extractWithSevenZip, maxUnpackedBytes = MAX_UNPACKED_BYTES }) {
  if (!Array.isArray(binaries) || binaries.length === 0 || !binaries.every((name) => typeof name === "string" && /^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(name))) {
    throw new Error("binaries must be a non-empty list of binary names");
  }
  if (new Set(binaries.map(msiFileId)).size !== binaries.length) throw new Error("two binaries share one MSI file id");
  if (!binaries.includes(MAIN_BINARY)) throw new Error(`binaries must include the main binary ${MAIN_BINARY}`);
  const root = workDir ?? mkdtempSync(path.join(tmpdir(), "bundle-check-"));
  /** @type {string[]} */
  const violations = [];
  /** @type {string[]} */
  const opened = [];
  let invalid = false;
  try {
    for (const spec of bundleSpecs(version)) {
      const file = files[spec.asset];
      if (!file) { violations.push(`missing: ${spec.asset} was not provided for inspection`); continue; }
      try {
        const size = statSync(file).size;
        if (size === 0 || size > MAX_BUNDLE_BYTES) throw new UnreadableBundle(`bundle size ${size} is outside 1..${MAX_BUNDLE_BYTES}`);
        /** @type {string[]} */
        let found;
        if (spec.format === "app-tar" || spec.format === "deb") {
          const archive = readFileSync(file);
          const packed = spec.format === "deb" ? readDebData(archive) : archive;
          const entries = readTar(gunzipSync(packed, { maxOutputLength: maxUnpackedBytes }));
          found = unsafeTarPaths(entries);
          for (const binary of binaries) {
            found.push(...(spec.format === "deb"
              ? judgeTarBinary(entries, `usr/bin/${binary}`, binary, judgeElf(/** @type {"x64" | "arm64"} */ (spec.arch)))
              : judgeTarBinary(entries, `GitPulse.app/Contents/MacOS/${binary}`, binary, judgeUniversalMachO)));
          }
        } else {
          found = inspectWindowsInstaller(file, spec.format, /** @type {"x64" | "arm64"} */ (spec.arch), binaries, root, extract);
        }
        opened.push(spec.asset);
        violations.push(...found.map((violation) => `${spec.asset}: ${violation}`));
      } catch (error) {
        invalid = true;
        violations.push(`${spec.asset}: cannot be opened: ${errorMessage(error)}`);
      }
    }
  } finally {
    if (!workDir) rmSync(root, { recursive: true, force: true });
  }
  return { ok: !invalid && violations.length === 0, invalid, violations, opened, coverage: coverageSummary(version, opened.length) };
}

/** @param {string[]} argv */
function parseArgs(argv) {
  /** @type {{ tag: string | null, jsonPath: string | null, bundles: string | null, platform: string | null, contents: string | null }} */
  const options = { tag: null, jsonPath: null, bundles: null, platform: null, contents: null };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) throw new Error(`${flag} requires a value`);
    if (flag === "--tag") options.tag = value;
    else if (flag === "--json") options.jsonPath = path.resolve(value);
    else if (flag === "--bundles") options.bundles = value;
    else if (flag === "--platform") options.platform = value;
    else if (flag === "--contents") options.contents = path.resolve(value);
    else throw new Error(`unknown option ${flag}`);
    index += 1;
  }
  if (options.contents !== null) {
    if (!options.tag) throw new Error("--contents needs --tag");
    if (options.jsonPath || options.bundles || options.platform) throw new Error("--contents cannot be combined with --json, --bundles or --platform");
    return options;
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
      { flag: "--contents <dir> --tag <tag>", description: "instead: open the macOS archive, debs, MSIs and NSIS installers in <dir> and require every src-tauri/Cargo.toml [[bin]] inside (needs 7z; the dmg, rpm and AppImage are not opened)" },
      { flag: "--help, -h".replace(/^"|"$/g, ""), description: "print this message and exit 0" }
    ],
    exits: "0 every expected asset is present · 1 one is missing or lacks a binary · 2 the check could not run",
  });
}

export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  try {
    const options = parseArgs(argv);
    if (options.contents) {
      const parsedTag = parseTag(options.tag ?? "");
      if (!parsedTag.ok) throw new Error(parsedTag.reason);
      const cargo = readFileSync(path.join(REPO_ROOT, "src-tauri", "Cargo.toml"), "utf8");
      /** @type {Record<string, string>} */
      const files = {};
      for (const spec of bundleSpecs(parsedTag.version)) files[spec.asset] = path.join(options.contents, spec.asset);
      const present = Object.fromEntries(Object.entries(files).filter(([, file]) => existsSync(file)));
      const result = inspectBundleContents({ version: parsedTag.version, binaries: declaredBinaryNames(cargo), files: present });
      for (const violation of result.violations) console.error(`FAIL: bundle contents ${violation}`);
      console.log(result.coverage);
      if (result.invalid) return 2;
      if (!result.ok) return 1;
      console.log("OK: every opened bundle holds every declared binary");
      return 0;
    }
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
