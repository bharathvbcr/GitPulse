/**
 * What source the installed `gitpulse-mcp` / `gitpulse-hook` were built from.
 *
 * The binaries carry exactly one identity of their own: `CARGO_PKG_VERSION`.
 * That is a *release* identity — it does not move when a fix lands between
 * releases, which is every fix while a release is being worked on. So
 * `check-mcp-install` compared one unchanged version string against an
 * identical one and reported `ok` for a hook binary built the day before the
 * trust fix it was supposed to be carrying, while the hook on PATH went on
 * emitting the pre-fix refusal. A
 * check that cannot see the problem must not return the same verdict as one
 * that looked and found nothing.
 *
 * Rather than teach the binaries to fingerprint themselves — which means a
 * `build.rs` change and the same digest implemented twice, in two languages,
 * agreeing forever — the install records what it built from, and the doctor
 * re-derives it. One implementation, here.
 *
 * Reading this record never widens anything: it decides what to *report*, and
 * every way of not knowing is an explicit "could not verify", never an `ok`.
 */

import { createHash } from "node:crypto";
import { readFileSync, readdirSync, statSync, writeFileSync, mkdirSync } from "node:fs";
import path from "node:path";
import os from "node:os";

/** Bump when the record's shape changes; an older record reads as absent. */
export const RECORD_VERSION = 1;

/**
 * Directories never descended into when collecting the digest's inputs.
 *
 * `target` is build output. `tests` are integration tests, which are compiled
 * into their own binaries and cannot change what `gitpulse-hook` does — a
 * digest that moved when a test changed would report a stale install after
 * every test edit, and a check people learn to ignore is worse than no check.
 */
const SKIP_DIRS = new Set(["target", "tests", "node_modules", ".git", "benches", "examples"]);

/** Extensions that are compiled into the binaries. */
const SOURCE_EXTENSIONS = new Set([".rs", ".swift"]);

/** Exact filenames that select what is compiled and how. */
const SOURCE_FILENAMES = new Set(["Cargo.toml", "Cargo.lock"]);

/**
 * Every file whose contents can change what the installed binaries do.
 *
 * Derived by walking the crate, not listed here: a module added under a new
 * directory has to be covered without anyone remembering this file exists.
 *
 * Deliberately **not** covered, because they do not reach these two binaries:
 * `tauri.conf.json` and the plugin manifests (their own gates check those),
 * and anything under `target/` or a `tests/` directory. Callers that report
 * this digest should say what it spans rather than imply it spans everything.
 *
 * @param {string} root Repository root.
 * @returns {string[]} Repo-relative paths, `/`-separated, ascending.
 */
export function sourceFiles(root) {
  /** @type {string[]} */
  const found = [];
  /** @param {string} dir */
  const walk = (dir) => {
    /** @type {import("node:fs").Dirent[]} */
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch (err) {
      throw new Error(`cannot read ${dir}: ${/** @type {Error} */ (err).message}`);
    }
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!SKIP_DIRS.has(entry.name)) walk(full);
        continue;
      }
      if (!entry.isFile()) continue;
      if (SOURCE_EXTENSIONS.has(path.extname(entry.name)) || SOURCE_FILENAMES.has(entry.name)) {
        found.push(path.relative(root, full).split(path.sep).join("/"));
      }
    }
  };
  walk(path.join(root, "src-tauri"));
  // Byte-order, so the stream is the same on any platform and filesystem.
  found.sort();
  return found;
}

/**
 * A digest over those files' paths *and* contents.
 *
 * The path and the length go into the stream alongside the bytes so that
 * renaming a file, or moving a byte across a file boundary, changes the
 * result. Hashing concatenated contents alone would not.
 *
 * @param {string} root Repository root.
 * @returns {{ digest: string, fileCount: number }}
 */
export function sourceDigest(root) {
  const files = sourceFiles(root);
  if (files.length === 0) {
    // An empty walk would otherwise hash to a stable value and compare equal
    // to itself forever — a digest that agrees with everything.
    throw new Error(`no source files found under ${path.join(root, "src-tauri")}`);
  }
  const hash = createHash("sha256");
  for (const rel of files) {
    const bytes = readFileSync(path.join(root, rel));
    hash.update(rel, "utf8");
    hash.update("\0");
    hash.update(String(bytes.length), "utf8");
    hash.update("\0");
    hash.update(bytes);
  }
  return { digest: hash.digest("hex"), fileCount: files.length };
}

/**
 * SHA-256 of one file, or `null` when it cannot be read.
 *
 * `null` is "we could not look", and every caller has to treat it as such
 * rather than as a mismatch or as a match.
 *
 * @param {string} file
 * @returns {string | null}
 */
export function fileDigest(file) {
  try {
    return createHash("sha256").update(readFileSync(file)).digest("hex");
  } catch {
    return null;
  }
}

/**
 * Where the record lives: beside GitPulse's other per-user state, not beside
 * the binary. `~/.cargo/bin` belongs to cargo, and a file dropped in it would
 * outlive an uninstall.
 *
 * @returns {string}
 */
export function recordPath() {
  const home = os.homedir();
  const dir =
    process.platform === "darwin"
      ? path.join(home, "Library", "Application Support", "GitPulse")
      : process.platform === "win32"
        ? path.join(process.env.APPDATA ?? path.join(home, "AppData", "Roaming"), "GitPulse")
        : path.join(process.env.XDG_CONFIG_HOME ?? path.join(home, ".config"), "GitPulse");
  return path.join(dir, "mcp-install-v1.json");
}

/**
 * @typedef {object} InstallRecord
 * @property {number} version
 * @property {string} installedAt
 * @property {string} sourceRoot
 * @property {string} sourceDigest
 * @property {number} sourceFileCount
 * @property {Record<string, string>} binaries Installed path to its SHA-256.
 */

/**
 * The recorded install, or `null` when there is not a usable one.
 *
 * A record from a future version, a truncated file, or one missing a field all
 * read as absent. None of them is an `ok`: absent means the doctor says it
 * could not verify, which is the honest report and the one that names the fix.
 *
 * @returns {InstallRecord | null}
 */
export function readInstallRecord() {
  /** @type {unknown} */
  let parsed;
  try {
    parsed = JSON.parse(readFileSync(recordPath(), "utf8"));
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const record = /** @type {Partial<InstallRecord>} */ (parsed);
  if (record.version !== RECORD_VERSION) return null;
  if (typeof record.sourceDigest !== "string" || record.sourceDigest.length !== 64) return null;
  if (typeof record.sourceRoot !== "string" || !record.sourceRoot) return null;
  if (typeof record.installedAt !== "string" || !record.installedAt) return null;
  if (typeof record.sourceFileCount !== "number" || !Number.isInteger(record.sourceFileCount)) return null;
  if (typeof record.binaries !== "object" || record.binaries === null) return null;
  for (const digest of Object.values(record.binaries)) {
    if (typeof digest !== "string" || digest.length !== 64) return null;
  }
  return /** @type {InstallRecord} */ (record);
}

/**
 * @param {InstallRecord} record
 * @returns {string} The path written.
 */
export function writeInstallRecord(record) {
  const file = recordPath();
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600 });
  return file;
}

/**
 * Whether `file` exists and is a regular file.
 * @param {string} file
 */
export function isFile(file) {
  try {
    return statSync(file).isFile();
  } catch {
    return false;
  }
}
