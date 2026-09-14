/**
 * Record what `npm run mcp:install` just installed, and from which source.
 *
 * Runs after `cargo install` in the same npm script, so the record is written
 * only when the install actually succeeded. What it writes is the one thing
 * the binaries cannot tell the doctor themselves: the digest of the sources
 * they were built from. See `install-identity.mjs` for why that is not simply
 * asked of the binary.
 *
 * Loud on every failure. A missing binary after a successful `cargo install`,
 * or an unwritable state directory, must not leave a silently absent record —
 * that would read downstream as "never installed", which is a different and
 * wrong story.
 *
 * Exit codes: 0 recorded · 1 the install did not leave what it claimed.
 */

import { readFileSync } from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";
import {
  RECORD_VERSION,
  fileDigest,
  isFile,
  sourceDigest,
  writeInstallRecord,
} from "./install-identity.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/**
 * The `--bin` names the install script actually passes to cargo.
 *
 * Parsed from the script rather than listed here, exactly as
 * `plugin-contract.test.ts` parses it, so a binary added to the install is
 * recorded without anyone editing this file. An empty parse is an error, not
 * an empty record: "we could not tell what was installed" must never be
 * written down as "nothing was".
 *
 * @returns {string[]}
 */
export function installedBinNames() {
  const pkg = /** @type {{ scripts?: Record<string, string> }} */ (
    JSON.parse(readFileSync(path.join(REPO_ROOT, "package.json"), "utf8"))
  );
  const script = pkg.scripts?.["mcp:install"];
  if (!script) throw new Error("package.json has no mcp:install script");
  const names = [...script.matchAll(/--bin\s+(\S+)/g)].map((match) => match[1]);
  if (names.length === 0) {
    throw new Error("mcp:install names no --bin targets; nothing could be recorded");
  }
  return names;
}

/**
 * Where cargo puts installed binaries. `CARGO_INSTALL_ROOT` wins, then
 * `CARGO_HOME`, then the default — the same precedence cargo itself uses.
 *
 * @returns {string}
 */
export function cargoBinDir() {
  const root = process.env.CARGO_INSTALL_ROOT ?? process.env.CARGO_HOME;
  return root ? path.join(root, "bin") : path.join(os.homedir(), ".cargo", "bin");
}

function main() {
  const binDir = cargoBinDir();
  const exe = process.platform === "win32" ? ".exe" : "";
  /** @type {Record<string, string>} */
  const binaries = {};
  /** @type {string[]} */
  const missing = [];

  for (const name of installedBinNames()) {
    const full = path.join(binDir, `${name}${exe}`);
    if (!isFile(full)) {
      missing.push(full);
      continue;
    }
    const digest = fileDigest(full);
    if (digest === null) {
      missing.push(`${full} (unreadable)`);
      continue;
    }
    binaries[full] = digest;
  }

  if (missing.length > 0) {
    console.error("cargo install reported success but these are not on disk:");
    for (const entry of missing) console.error(`  ${entry}`);
    console.error("\nNothing was recorded. The doctor will report that it cannot verify the install.");
    process.exit(1);
  }

  const { digest, fileCount } = sourceDigest(REPO_ROOT);
  const written = writeInstallRecord({
    version: RECORD_VERSION,
    installedAt: new Date().toISOString(),
    sourceRoot: REPO_ROOT,
    sourceDigest: digest,
    sourceFileCount: fileCount,
    binaries,
  });

  console.log(`\nRecorded install provenance for ${Object.keys(binaries).length} binaries.`);
  console.log(`  source digest : ${digest.slice(0, 16)}… (${fileCount} files)`);
  console.log(`  record        : ${written}`);
}

// Only when run as the script. Importing this module — the contract test reads
// `installedBinNames` out of it — must not install, record, or exit.
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
