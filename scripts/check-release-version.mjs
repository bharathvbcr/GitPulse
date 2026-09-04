#!/usr/bin/env node
/**
 * Release version gate — the seven manifests that carry a version must agree,
 * and (when a tag is supplied) the tag must name that same version.
 *
 * Nothing else in the repo owns this invariant, and every part of the release
 * reads a *different* source:
 *
 *   - the Git tag             -> what the GitHub Release is called
 *   - src-tauri/tauri.conf.json -> `__VERSION__` in the release name, and the
 *                                bundle version baked into the .dmg/.msi/.deb
 *   - src-tauri/Cargo.toml    -> the compiled crate version
 *   - src-tauri/Cargo.lock    -> must track Cargo.toml or `--locked` builds fail
 *   - package.json / -lock.json -> `npm ci` fails outright when these disagree
 *   - every plugin manifest   -> the version an agent client reads out of the
 *                                package, and records at install time
 *
 * The plugin manifests are the reason this list grew: the MCP binary answers
 * `initialize` with the *crate* version, so a stale plugin manifest makes a
 * client's installed-version record disagree with the server that client is
 * talking to, with nothing in the handshake to reveal it.
 *
 * Those manifests are DISCOVERED, not listed. One package is published to
 * several agent clients that each want their own manifest file
 * (`plugin.json`, `.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`),
 * and the set grows whenever a client is added. A hand-maintained list would
 * silently stop covering the newest one — which is the manifest most likely to
 * be wrong.
 *
 * So a tag pushed against a stale manifest produces a release *tagged* v0.0.2
 * whose assets are all 0.0.1 — a silent, published-artifact-level wrong answer
 * that no test, lint, or type check catches. This makes it a hard failure
 * before any build minutes are spent.
 *
 * Exit codes: 0 versions agree · 1 mismatch · 2 internal error.
 *
 * Flags (all optional; path flags exist so the tests can simulate drift on
 * scratch copies without touching the tree):
 *   --tag <vX.Y.Z>       also require the tag to match (leading `v` required)
 *   --root <dir>         resolve every default manifest under <dir>
 *   --package <path>     alternate package.json
 *   --package-lock <path> alternate package-lock.json
 *   --tauri-conf <path>  alternate tauri.conf.json
 *   --cargo-toml <path>  alternate Cargo.toml
 *   --cargo-lock <path>  alternate Cargo.lock
 *
 * Plugin manifests are found under <root>/plugins/<name>, so --root moves them
 * along with everything else.
 */
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The crate whose `[[package]]` entry in Cargo.lock carries the app version. */
export const CRATE_NAME = "gitpulse";

/**
 * Default manifest locations, relative to a repo root.
 * @param {string} root
 */
export function defaultSources(root) {
  return {
    packagePath: path.join(root, "package.json"),
    packageLockPath: path.join(root, "package-lock.json"),
    tauriConfPath: path.join(root, "src-tauri", "tauri.conf.json"),
    cargoTomlPath: path.join(root, "src-tauri", "Cargo.toml"),
    cargoLockPath: path.join(root, "src-tauri", "Cargo.lock"),
    // Discovery needs the root itself; the plugin manifests are not a fixed
    // set of paths.
    root,
  };
}

/**
 * A tag this repo's release workflow can trigger on (`tags: ['v*']`), carrying
 * a plain three-part version. Pre-release/build suffixes are rejected rather
 * than guessed at: Cargo and npm disagree on how to spell them, so a suffix
 * that round-trips here would still break one of the manifests.
 *
 * @param {string} tag
 * @returns {{ ok: true, version: string } | { ok: false, reason: string }}
 */
export function parseTag(tag) {
  if (!tag.startsWith("v")) {
    return { ok: false, reason: `tag ${JSON.stringify(tag)} must start with "v" (workflow triggers on tags: ['v*'])` };
  }
  const version = tag.slice(1);
  if (!/^\d+\.\d+\.\d+$/.test(version)) {
    return {
      ok: false,
      reason: `tag ${JSON.stringify(tag)} must be v<major>.<minor>.<patch> with no suffix, got version part ${JSON.stringify(version)}`,
    };
  }
  return { ok: true, version };
}

/**
 * Read the `[package]` section's `version` from a Cargo manifest.
 *
 * Scoped to that section on purpose: a naive `version = "..."` scan would
 * happily return a dependency's version and report agreement that does not
 * exist. Returns null when the section or key is absent.
 *
 * @param {string} source raw Cargo.toml contents
 */
export function parseCargoTomlVersion(source) {
  const lines = source.split(/\r?\n/);
  let inPackage = false;
  for (const rawLine of lines) {
    const line = rawLine.replace(/#.*$/, "").trim();
    if (/^\[[^\]]+\]$/.test(line)) {
      inPackage = line === "[package]";
      continue;
    }
    if (!inPackage) continue;
    const match = /^version\s*=\s*"([^"]*)"/.exec(line);
    if (match) return match[1];
  }
  return null;
}

/**
 * Read the version of `crate` from a Cargo.lock's `[[package]]` blocks.
 *
 * @param {string} source raw Cargo.lock contents
 * @param {string} crate
 */
export function parseCargoLockVersion(source, crate) {
  const blocks = source.split(/^\[\[package\]\]\s*$/m).slice(1);
  for (const block of blocks) {
    const upToNextSection = block.split(/^\[/m)[0];
    const name = /^name\s*=\s*"([^"]*)"/m.exec(upToNextSection);
    if (!name || name[1] !== crate) continue;
    const version = /^version\s*=\s*"([^"]*)"/m.exec(upToNextSection);
    return version ? version[1] : null;
  }
  return null;
}

/** @param {string} dir */
function isDirectory(dir) {
  try {
    return statSync(dir).isDirectory();
  } catch {
    return false;
  }
}

/**
 * Every directory under `plugins/` that is an agent-plugin package.
 *
 * @param {string} root
 * @returns {string[]} absolute directories, stable order
 */
export function discoverPluginPackages(root) {
  /** @type {string[]} */
  const packages = [];
  const nested = path.join(root, "plugins");
  if (isDirectory(nested)) {
    for (const name of readdirSync(nested).sort()) {
      const dir = path.join(nested, name);
      if (isDirectory(dir)) packages.push(dir);
    }
  }
  return packages;
}

/**
 * The per-client manifest names inside one package directory.
 *
 * `plugin.json` is required — it is the manifest every client falls back to,
 * and a package without one is a packaging error rather than a version one.
 * The dot-directory manifests are optional, because which clients a package
 * targets is a choice; whichever are present must still agree.
 */
export const REQUIRED_PLUGIN_MANIFEST = "plugin.json";
export const OPTIONAL_PLUGIN_MANIFESTS = Object.freeze([
  path.join(".claude-plugin", "plugin.json"),
  path.join(".codex-plugin", "plugin.json"),
  path.join(".agents", "plugin.json"),
]);

/**
 * @param {string} filePath
 * @returns {unknown}
 */
function readJson(filePath) {
  return JSON.parse(readFileSync(filePath, "utf8"));
}

/**
 * Collect every version source. A source that cannot be read or has no
 * version is recorded as `null` and reported as a violation by the caller —
 * never quietly dropped, which would let the remaining sources "agree".
 *
 * @param {ReturnType<typeof defaultSources>} sources
 * @returns {Array<{ label: string, file: string, version: string | null, error?: string }>}
 */
export function collectVersions(sources) {
  /** @type {Array<{ label: string, file: string, version: string | null, error?: string }>} */
  const found = [];

  /**
   * @param {string} label
   * @param {string} file
   * @param {() => string | null | undefined} read
   */
  const add = (label, file, read) => {
    try {
      const version = read();
      found.push({ label, file, version: typeof version === "string" ? version : null });
    } catch (err) {
      found.push({ label, file, version: null, error: /** @type {Error} */ (err).message });
    }
  };

  add("package.json", sources.packagePath, () => {
    const pkg = /** @type {{ version?: unknown }} */ (readJson(sources.packagePath));
    return typeof pkg.version === "string" ? pkg.version : null;
  });
  // package-lock.json carries the version twice; npm ci trusts both, so both
  // are checked rather than assuming they were written together.
  add("package-lock.json (root)", sources.packageLockPath, () => {
    const lock = /** @type {{ version?: unknown }} */ (readJson(sources.packageLockPath));
    return typeof lock.version === "string" ? lock.version : null;
  });
  add('package-lock.json (packages[""])', sources.packageLockPath, () => {
    const lock = /** @type {{ packages?: Record<string, { version?: unknown }> }} */ (
      readJson(sources.packageLockPath)
    );
    const root = lock.packages?.[""];
    return root && typeof root.version === "string" ? root.version : null;
  });
  add("src-tauri/tauri.conf.json", sources.tauriConfPath, () => {
    const conf = /** @type {{ version?: unknown }} */ (readJson(sources.tauriConfPath));
    return typeof conf.version === "string" ? conf.version : null;
  });
  add("src-tauri/Cargo.toml", sources.cargoTomlPath, () =>
    parseCargoTomlVersion(readFileSync(sources.cargoTomlPath, "utf8")),
  );
  add(`src-tauri/Cargo.lock (${CRATE_NAME})`, sources.cargoLockPath, () =>
    parseCargoLockVersion(readFileSync(sources.cargoLockPath, "utf8"), CRATE_NAME),
  );
  // Plugin manifests are separate files for separate clients, so each is read
  // on its own; a shared reader would hide one lagging the others.
  const packages = discoverPluginPackages(sources.root);
  if (packages.length === 0) {
    found.push({
      label: "plugin package",
      file: sources.root,
      version: null,
      error: "no plugins/<name>/ package directory found",
    });
  }
  for (const dir of packages) {
    const rel = (/** @type {string} */ file) => path.relative(sources.root, file).split(path.sep).join("/");
    const required = path.join(dir, REQUIRED_PLUGIN_MANIFEST);
    // Required, so absence is a violation rather than something discovery
    // quietly skips — a deleted manifest must not read as "nothing to check".
    add(rel(required), required, () => {
      const manifest = /** @type {{ version?: unknown }} */ (readJson(required));
      return typeof manifest.version === "string" ? manifest.version : null;
    });
    for (const name of OPTIONAL_PLUGIN_MANIFESTS) {
      const file = path.join(dir, name);
      if (!existsSync(file)) continue;
      add(rel(file), file, () => {
        const manifest = /** @type {{ version?: unknown }} */ (readJson(file));
        return typeof manifest.version === "string" ? manifest.version : null;
      });
    }
  }

  return found;
}

/**
 * @param {{ sources: ReturnType<typeof defaultSources>, tag?: string }} input
 */
export function runVersionCheck({ sources, tag }) {
  const found = collectVersions(sources);
  /** @type {string[]} */
  const violations = [];

  for (const entry of found) {
    if (entry.version === null) {
      violations.push(
        `unreadable: ${entry.label} yielded no version${entry.error ? ` (${entry.error})` : ""}`,
      );
    }
  }

  const readable = found.filter((entry) => entry.version !== null);
  const distinct = [...new Set(readable.map((entry) => entry.version))];
  if (distinct.length > 1) {
    // Name every source and its value; "they disagree" is not actionable.
    violations.push(`manifest versions disagree: ${distinct.map((v) => JSON.stringify(v)).join(" vs ")}`);
    for (const entry of readable) {
      violations.push(`  ${entry.label} = ${JSON.stringify(entry.version)}`);
    }
  }

  /** @type {string | null} */
  let tagVersion = null;
  if (tag !== undefined) {
    const parsed = parseTag(tag);
    if (!parsed.ok) {
      violations.push(parsed.reason);
    } else {
      tagVersion = parsed.version;
      for (const entry of readable) {
        if (entry.version !== tagVersion) {
          violations.push(
            `tag ${tag} declares version ${JSON.stringify(tagVersion)} but ${entry.label} = ${JSON.stringify(entry.version)}`,
          );
        }
      }
    }
  }

  return {
    found,
    tag: tag ?? null,
    tagVersion,
    version: distinct.length === 1 ? distinct[0] : null,
    violations,
    ok: violations.length === 0,
  };
}

/**
 * @param {ReturnType<typeof runVersionCheck>} result
 * @param {string} rootLabel
 */
export function formatReport(result, rootLabel) {
  const lines = [`Release version gate (${rootLabel})`, ""];
  for (const entry of result.found) {
    lines.push(`  ${entry.label.padEnd(34)} : ${entry.version ?? "<unreadable>"}`);
  }
  if (result.tag !== null) lines.push(`  ${"git tag".padEnd(34)} : ${result.tag}`);
  if (result.violations.length > 0) {
    lines.push("", "  violations:");
    for (const violation of result.violations) lines.push(`    - ${violation}`);
  }
  lines.push(
    "",
    result.ok
      ? `OK: all version sources agree on ${result.version}${result.tagVersion ? ` and match tag ${result.tag}` : ""}.`
      : "FAIL: release version gate violated — do not tag or publish until resolved.",
  );
  return lines.join("\n");
}

/**
 * @param {string[]} argv
 */
export function parseArgs(argv) {
  let root = REPO_ROOT;
  /** @type {Partial<ReturnType<typeof defaultSources>>} */
  const overrides = {};
  /** @type {string | undefined} */
  let tag;
  let json = false;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    /** @param {string} flag */
    const next = (flag) => {
      const value = argv[++i];
      if (value === undefined) throw new Error(`${flag} requires a value`);
      return value;
    };
    if (arg === "--json") json = true;
    else if (arg === "--tag") tag = next(arg);
    else if (arg === "--root") root = path.resolve(next(arg));
    else if (arg === "--package") overrides.packagePath = path.resolve(next(arg));
    else if (arg === "--package-lock") overrides.packageLockPath = path.resolve(next(arg));
    else if (arg === "--tauri-conf") overrides.tauriConfPath = path.resolve(next(arg));
    else if (arg === "--cargo-toml") overrides.cargoTomlPath = path.resolve(next(arg));
    else if (arg === "--cargo-lock") overrides.cargoLockPath = path.resolve(next(arg));
    else throw new Error(`unknown argument: ${arg}`);
  }

  // An empty --tag is how CI passes "no tag known" (e.g. `${{ inputs.tag }}`
  // on a tag-push run). Treat it as absent rather than as an invalid tag.
  if (tag !== undefined && tag.trim() === "") tag = undefined;

  return { root, sources: { ...defaultSources(root), ...overrides }, tag, json };
}

/**
 * @param {string[]} [argv]
 */

/** Backlog A2: asking for help is not an error, so this exits 0. */
export function usage() {
  return formatUsage({
    name: "check-release-version",
    summary: "Assert every version manifest names one version, and that it matches the release tag when given.",
    flags: [
      { flag: "--tag <tag>".replace(/^"|"$/g, ""), description: "release tag the manifests must match (empty means no tag known)" },
      { flag: "--root <dir>".replace(/^"|"$/g, ""), description: "repository root to resolve the default manifest paths from" },
      { flag: "--package <path>".replace(/^"|"$/g, ""), description: "override package.json" },
      { flag: "--package-lock <path>".replace(/^"|"$/g, ""), description: "override package-lock.json" },
      { flag: "--tauri-conf <path>".replace(/^"|"$/g, ""), description: "override tauri.conf.json" },
      { flag: "--cargo-toml <path>".replace(/^"|"$/g, ""), description: "override Cargo.toml" },
      { flag: "--cargo-lock <path>".replace(/^"|"$/g, ""), description: "override Cargo.lock" },
      { flag: "--help, -h".replace(/^"|"$/g, ""), description: "print this message and exit 0" }
    ],
    exits: "0 the manifests agree · 1 they disagree · 2 the check could not run",
  });
}

export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  /** @type {ReturnType<typeof parseArgs>} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (err) {
    console.error(`check-release-version: ${/** @type {Error} */ (err).message}`);
    return 2;
  }
  /** @type {ReturnType<typeof runVersionCheck>} */
  let result;
  try {
    result = runVersionCheck(opts);
  } catch (err) {
    console.error(`check-release-version: internal error: ${/** @type {Error} */ (err).message}`);
    return 2;
  }
  if (opts.json) console.log(JSON.stringify(result, null, 2));
  else console.log(formatReport(result, path.relative(REPO_ROOT, opts.root) || "."));
  return result.ok ? 0 : 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
