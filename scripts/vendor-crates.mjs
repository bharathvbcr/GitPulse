#!/usr/bin/env node
/**
 * Vendor the sibling Rust crates GitPulse links, so it builds standalone.
 *
 * GitPulse depends on four crates owned by Manvi and DevCouncil, and on four
 * more they pull in. Reaching them by relative path — `../../../../../Manvi/…`
 * — meant a checkout of GitPulse alone did not build: it needed two unrelated
 * repositories present, at the right depth, on every machine and every CI
 * runner. This copies them in.
 *
 * # What is copied, and what is not
 *
 * `Cargo.toml`, `src/` and `build.rs`. Not `tests/`, and not
 * `[dev-dependencies]`: GitPulse has never run these crates' tests — as path
 * dependencies outside its workspace, cargo does not build them — so keeping
 * them would vendor code nothing here compiles, along with dev-dependencies on
 * crates outside this closure that cargo would then have to resolve. The
 * omission is recorded per crate in the manifest rather than left for a reader
 * to infer from an absence.
 *
 * # Inheritance is resolved, not carried
 *
 * Both upstreams use workspace inheritance (`version.workspace = true`,
 * `serde.workspace = true`), and the two workspaces disagree: Manvi is edition
 * 2024 / resolver 3, DevCouncil's rust-port is edition 2021 / resolver 2. One
 * workspace here could not supply both, so each vendored manifest gets the
 * concrete values its own upstream would have given it. Every rewrite is listed
 * in the manifest, and an inheritance form this script does not recognise is a
 * hard failure — never passed through to fail later as a confusing cargo error.
 *
 * # Drift
 *
 * `--check` verifies two different things and reports them separately:
 *
 *   * that no vendored file has been edited here, by comparing against the
 *     hashes recorded when it was vendored. This always runs.
 *   * that the vendored source still matches upstream. This needs the sibling
 *     repository, and when it is absent the crate is reported `unavailable` —
 *     never `matches`. A comparison that could not run must not read like one
 *     that ran and found nothing.
 *
 * Exit codes: 0 vendored / no drift · 1 drift or a local edit · 2 the run
 * could not complete.
 */
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  closeSync,
  constants,
  cpSync,
  existsSync,
  fstatSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..");

/** Where the copies live. Deliberately not `vendor/`, which by convention
 *  means `cargo vendor` source replacement — a different mechanism entirely. */
export const VENDOR_DIR = path.join(REPO, "src-tauri", "vendored");
export const MANIFEST = path.join(VENDOR_DIR, "VENDOR.json");

/**
 * The upstream workspaces, and where to find them.
 *
 * Overridable by environment variable so a machine that keeps its checkouts
 * somewhere else can still re-vendor, and so this is testable without them.
 */
export function sources(env = process.env, from = REPO) {
  return [
    {
      // dc-* crates live in DevCouncil rust-port (Phase 3). Manvi's crates/
      // directory only keeps symlinks for local builds — do not vendor from there.
      id: "devcouncil",
      root: env.GITPULSE_DEVCOUNCIL_ROOT ?? findSibling("DevCouncil", from),
      workspace: "rust-port",
      crates: [
        "dc-glob",
        "dc-store",
        "dc-verify",
        "devmap-analyze",
        "devmap-extract",
        "devmap-query",
        "devmap-resolve",
        "devmap-store",
      ],
      crateDir: (/** @type {string} */ name) => path.join("rust-port", "crates", name),
    },
    {
      // MarkDev's parse + highlight core. Package name is `markdev`; the
      // crate lives at `core/` and has no workspace inheritance of its own,
      // so `workspace` points at that same directory for resolveManifest.
      id: "markdev",
      root: env.GITPULSE_MARKDEV_ROOT ?? findSibling("MarkDev", from),
      workspace: "core",
      crates: ["markdev"],
      crateDir: (/** @type {string} */ _name) => "core",
    },
  ];
}

/**
 * Looks for a sibling checkout by walking up from this repository.
 *
 * Not a fixed number of `..` segments: this repository is often opened as a
 * git worktree under `.claude/worktrees/<name>`, which sits four levels deeper
 * than a plain checkout, and a hard-coded depth silently resolves to the wrong
 * directory in one of the two. Returns the first match, or a path that does
 * not exist so the caller reports the name of the environment variable.
 */
/**
 * @param {string} name
 * @param {string} from
 * @returns {string}
 */
function findSibling(name, from = REPO) {
  let dir = from;
  for (let i = 0; i < 8; i++) {
    const candidate = path.join(dir, name);
    if (existsSync(candidate)) return candidate;
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }

  // A linked worktree's ancestors do not include the directory that contains
  // the canonical checkout. Its common Git directory does: for
  // `/Code/devtools/GitPulse/.git`, the sibling checkout is
  // `/Code/devtools/<name>`. This also works when the worktree lives under an
  // agent-owned `.codex/worktrees/` directory at an unrelated depth.
  try {
    const commonDir = execFileSync(
      "git",
      ["-C", from, "rev-parse", "--path-format=absolute", "--git-common-dir"],
      { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
    ).trim();
    const gitDir = path.resolve(from, commonDir);
    const canonical = path.dirname(gitDir);
    const candidate = path.join(path.dirname(canonical), name);
    if (existsSync(candidate)) return candidate;
  } catch {
    // The explicit environment variables remain the escape hatch for a
    // non-Git source tree or an unusual Git directory layout.
  }
  return path.join(path.dirname(from), name);
}

/** Files and directories taken from each crate. */
const COPIED = ["src", "build.rs", "assets"];

/** Cargo manifests are small text files. Bound reads before parsing so a
 * special file cannot block the vendor command and a damaged manifest cannot
 * consume memory without limit. The source trees are trusted local inputs;
 * descriptor checks narrow path races but are not a filesystem transaction. */
const MAX_MANIFEST_BYTES = 1024 * 1024;

/**
 * Read one Cargo manifest from a regular file, through the opened descriptor.
 *
 * @param {string} manifestPath
 * @returns {string}
 */
function readManifest(manifestPath) {
  const before = lstatSync(manifestPath);
  if (before.isSymbolicLink()) throw new Error(`refusing symbolic link manifest: ${manifestPath}`);
  if (!before.isFile()) throw new Error(`refusing non-regular manifest: ${manifestPath}`);
  if (before.size > MAX_MANIFEST_BYTES) {
    throw new Error(`manifest is too large (${before.size} bytes; maximum ${MAX_MANIFEST_BYTES}): ${manifestPath}`);
  }

  const fd = openSync(manifestPath, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const opened = fstatSync(fd);
    if (!opened.isFile()) throw new Error(`refusing non-regular manifest: ${manifestPath}`);
    if (opened.size > MAX_MANIFEST_BYTES) {
      throw new Error(`manifest is too large (${opened.size} bytes; maximum ${MAX_MANIFEST_BYTES}): ${manifestPath}`);
    }

    const bytes = Buffer.alloc(opened.size + 1);
    let length = 0;
    while (length < bytes.length) {
      const count = readSync(fd, bytes, length, bytes.length - length, null);
      if (count === 0) break;
      length += count;
    }
    if (length !== opened.size) throw new Error(`manifest changed while it was read: ${manifestPath}`);
    return bytes.subarray(0, length).toString("utf8");
  } finally {
    closeSync(fd);
  }
}

// --- a very small TOML reader -------------------------------------------
//
// Only what a workspace root and a crate manifest need: section headers and
// single-line `key = value` pairs. A value that does not close on its own line
// is refused rather than truncated — this reads manifests that decide how the
// application is built, and half a value is worse than no value.

/**
 * @param {string} text
 * @returns {Map<string, Map<string, string>>} section → key → raw value text
 */
export function readToml(text) {
  /** @type {Map<string, Map<string, string>>} */
  const sections = new Map();
  let current = "";
  sections.set(current, new Map());

  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();
    if (trimmed === "" || trimmed.startsWith("#")) continue;

    const header = /^\[([^\]]+)\]$/.exec(trimmed);
    if (header) {
      current = header[1];
      if (!sections.has(current)) sections.set(current, new Map());
      continue;
    }

    const pair = /^([A-Za-z0-9_.\-"]+)\s*=\s*(.*)$/.exec(trimmed);
    if (!pair) continue;

    // A value may run over several lines — an array of workspace members, a
    // long feature list. Join until the brackets close rather than taking the
    // first line: half a value silently parses as something else entirely.
    let value = pair[2].trim();
    const startedAt = i;
    while (!isBalanced(value) && i + 1 < lines.length) {
      i += 1;
      value += ` ${lines[i].trim()}`;
    }
    if (!isBalanced(value)) {
      throw new Error(`unterminated value for ${current ? `${current}.` : ""}${pair[1]} at line ${startedAt + 1}`);
    }
    sections.get(current)?.set(pair[1], value);
  }
  return sections;
}

/**
 * Whether a value's brackets and quotes close on this line.
 *
 * @param {string} value
 */
function isBalanced(value) {
  let depth = 0;
  let inString = false;
  for (const ch of value) {
    if (ch === '"') inString = !inString;
    else if (!inString && (ch === "{" || ch === "[")) depth++;
    else if (!inString && (ch === "}" || ch === "]")) depth--;
  }
  return depth === 0 && !inString;
}

// --- inheritance resolution ---------------------------------------------

/** Keys `[package]` may inherit. */
const PACKAGE_KEYS = [
  "version", "edition", "license", "authors", "description", "repository",
  "homepage", "documentation", "readme", "keywords", "categories",
  "rust-version", "publish", "license-file", "exclude", "include",
];

/**
 * Rewrites one crate manifest so it stands on its own.
 *
 * @param {string} text the upstream manifest
 * @param {Map<string, Map<string, string>>} workspace the upstream workspace root
 * @returns {{ text: string, rewrites: string[] }}
 */
export function resolveManifest(text, workspace) {
  const wsPackage = workspace.get("workspace.package") ?? new Map();
  const wsDeps = workspace.get("workspace.dependencies") ?? new Map();
  const lintSections = [...workspace.keys()].filter((k) => k.startsWith("workspace.lints"));

  /** @type {string[]} */
  const rewrites = [];
  const out = [];
  let section = "";
  let dropping = false;

  const lines = text.split("\n");
  for (const line of lines) {
    const trimmed = line.trim();
    const header = /^\[([^\]]+)\]$/.exec(trimmed);

    if (header) {
      section = header[1];
      // dev-dependencies are dropped with `tests/`; see the module comment.
      dropping = section === "dev-dependencies";
      if (dropping) {
        rewrites.push("dropped [dev-dependencies]");
        continue;
      }
      if (section === "lints") {
        // `[lints] workspace = true` becomes the upstream lint tables inlined.
        // A crate that forbids unsafe upstream must keep forbidding it here.
        if (lintSections.length === 0) {
          throw new Error("crate declares [lints] but its workspace defines none");
        }
        for (const key of lintSections) {
          out.push(`[lints.${key.slice("workspace.lints.".length)}]`);
          for (const [k, v] of workspace.get(key) ?? []) out.push(`${k} = ${v}`);
          out.push("");
        }
        rewrites.push("inlined [lints] from the workspace");
        continue;
      }
      out.push(line);
      continue;
    }

    if (dropping) continue;
    if (section === "lints") continue; // its body was replaced above

    const pair = /^([A-Za-z0-9_.\-"]+)\s*=\s*(.*)$/.exec(trimmed);
    if (!pair) {
      out.push(line);
      continue;
    }
    const [, key, rawValue] = pair;
    const value = rawValue.trim();

    // `version.workspace = true` and friends.
    const dotted = /^([A-Za-z0-9_\-]+)\.workspace$/.exec(key);
    if (dotted && value === "true") {
      const name = dotted[1];
      if (section === "package" && PACKAGE_KEYS.includes(name)) {
        const inherited = wsPackage.get(name);
        if (inherited === undefined) {
          throw new Error(`[package] inherits ${name}, which the workspace does not define`);
        }
        out.push(`${name} = ${inherited}`);
        rewrites.push(`package.${name} = ${inherited}`);
        continue;
      }
      if (isDependencySection(section)) {
        out.push(`${name} = ${dependencySpec(name, wsDeps, [])}`);
        rewrites.push(`${section}.${name} from the workspace`);
        continue;
      }
      throw new Error(`unhandled inheritance: ${section ? `[${section}] ` : ""}${key} = ${value}`);
    }

    // `dep = { workspace = true, optional = true }`.
    if (isDependencySection(section) && /\bworkspace\s*=\s*true\b/.test(value)) {
      const extras = inlineEntries(value).filter((e) => !/^workspace\s*=/.test(e));
      out.push(`${key} = ${dependencySpec(key, wsDeps, extras)}`);
      rewrites.push(`${section}.${key} from the workspace`);
      continue;
    }

    out.push(line);
  }

  const result = out.join("\n");
  // Nothing may reference the workspace afterwards. A survivor would surface
  // later as a cargo error about a manifest this script claimed it had fixed.
  const leftover = result.split("\n").find((l) => /\bworkspace\b/.test(l) && !l.trim().startsWith("#"));
  if (leftover) throw new Error(`unresolved workspace reference: ${leftover.trim()}`);

  return { text: result, rewrites };
}

/** @param {string} section */
function isDependencySection(section) {
  return section === "dependencies" || section === "build-dependencies" || section.endsWith(".dependencies");
}

/**
 * Entries of an inline table, split at top-level commas.
 *
 * @param {string} value
 * @returns {string[]}
 */
function inlineEntries(value) {
  const inner = value.replace(/^\{/, "").replace(/\}$/, "");
  const parts = [];
  let depth = 0;
  let inString = false;
  let start = 0;
  for (let i = 0; i < inner.length; i++) {
    const ch = inner[i];
    if (ch === '"') inString = !inString;
    else if (!inString && (ch === "[" || ch === "{")) depth++;
    else if (!inString && (ch === "]" || ch === "}")) depth--;
    else if (!inString && depth === 0 && ch === ",") {
      parts.push(inner.slice(start, i).trim());
      start = i + 1;
    }
  }
  parts.push(inner.slice(start).trim());
  return parts.filter((p) => p.length > 0);
}

/**
 * The concrete dependency spec for `name`, merged with any extras.
 *
 * @param {string} name
 * @param {Map<string, string>} wsDeps
 * @param {string[]} extras
 */
function dependencySpec(name, wsDeps, extras) {
  const inherited = wsDeps.get(name);
  if (inherited === undefined) {
    throw new Error(`dependency ${name} inherits from the workspace, which does not declare it`);
  }
  const base = inherited.startsWith("{") ? inlineEntries(inherited) : [`version = ${inherited}`];
  return `{ ${[...base, ...extras].join(", ")} }`;
}

// --- vendoring ------------------------------------------------------------

/** @param {Buffer} buffer */
function sha256(buffer) {
  return createHash("sha256").update(buffer).digest("hex");
}

/**
 * Every file under `dir`, relative to it, sorted.
 *
 * @param {string} dir
 * @param {string} prefix
 * @returns {string[]}
 */
function walk(dir, prefix = "") {
  if (!existsSync(dir)) return [];
  if (lstatSync(dir).isSymbolicLink()) throw new Error(`refusing symbolic link: ${dir}`);
  const out = [];
  for (const entry of readdirSync(dir).sort()) {
    // DevCouncil / agent local state must never be part of a vendored crate.
    // It is gitignored (`logs/`, `.devcouncil/*`), so recording it in
    // VENDOR.json makes CI report the path as missing while a dirty local
    // tree looks clean.
    if (entry === ".devcouncil" || entry === ".git" || entry === "target") continue;
    const full = path.join(dir, entry);
    const rel = prefix ? `${prefix}/${entry}` : entry;
    const info = lstatSync(full);
    if (info.isSymbolicLink()) throw new Error(`refusing symbolic link: ${full}`);
    if (info.isDirectory()) out.push(...walk(full, rel));
    else if (info.isFile()) out.push(rel);
    else throw new Error(`refusing non-regular file: ${full}`);
  }
  return out;
}

/** @param {string} root */
function gitCommit(root) {
  try {
    return execFileSync("git", ["-C", root, "rev-parse", "HEAD"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return "";
  }
}

/**
 * Build the exact standalone snapshot used by both refresh and drift checks.
 * Comparing this output with the recorded manifest catches source deletions,
 * Cargo inheritance changes, and changes to standalone rewrites through one
 * canonical transform.
 *
 * @param {ReturnType<typeof sources>[number]} source
 * @param {string} name
 * @param {string} to
 * @param {Map<string, Map<string, string>>} workspace
 * @param {string} commit
 */
function prepareCrate(source, name, to, workspace, commit) {
  const from = path.join(source.root, source.crateDir(name));
  const upstream = readManifest(path.join(from, "Cargo.toml"));
  const { text, rewrites } = resolveManifest(upstream, workspace);
  mkdirSync(to, { recursive: true });

  for (const item of COPIED) {
    const src = path.join(from, item);
    const info = lstatSync(src, { throwIfNoEntry: false });
    if (!info) continue;
    if (info.isSymbolicLink()) throw new Error(`refusing symbolic link: ${src}`);
    const files = info.isDirectory() ? walk(src, item) : [item];
    if (!info.isDirectory() && !info.isFile()) throw new Error(`refusing non-regular file: ${src}`);
    for (const rel of files) {
      const input = path.join(from, rel);
      const output = path.join(to, rel);
      mkdirSync(path.dirname(output), { recursive: true });
      cpSync(input, output);
    }
  }

  writeFileSync(path.join(to, "Cargo.toml"), text);

  /** @type {Record<string, string>} */
  const files = {};
  for (const rel of walk(to)) files[rel] = sha256(readFileSync(path.join(to, rel)));
  return {
    name,
    origin: {
      repo: source.id,
      root_env: `GITPULSE_${source.id.toUpperCase()}_ROOT`,
      path: source.crateDir(name),
      commit,
    },
    omitted: ["tests/", "[dev-dependencies]"],
    rewrites,
    files,
  };
}

/**
 * Copy recorded, unrelated crates into a scoped refresh's staging tree.
 * @param {string} staging
 * @param {string} onlyCrate
 */
function copyUnselectedSnapshot(staging, onlyCrate) {
  if (!existsSync(MANIFEST)) throw new Error(`${MANIFEST} is missing; run a full vendor refresh first`);
  for (const rel of walk(VENDOR_DIR)) {
    if (rel.split("/")[0] === onlyCrate) continue;
    const output = path.join(staging, rel);
    mkdirSync(path.dirname(output), { recursive: true });
    cpSync(path.join(VENDOR_DIR, rel), output);
  }
  const manifest = JSON.parse(readFileSync(MANIFEST, "utf8"));
  if (!Array.isArray(manifest.crates)) throw new Error(`${MANIFEST} has no crates array`);
  return manifest.crates.filter((/** @type {{ name: string }} */ crate) => crate.name !== onlyCrate);
}

/**
 * Prepare a complete snapshot before replacing the live vendor tree. A lock
 * rejects concurrent writers. If installation fails after moving the old
 * tree aside, it is restored before the error escapes.
 *
 * @param {NodeJS.ProcessEnv} env
 * @param {string | null} onlyCrate
 */
export function vendor(env = process.env, onlyCrate = null) {
  const configured = sources(env);
  if (onlyCrate !== null && !configured.some((source) => source.crates.includes(onlyCrate))) {
    throw new Error(`unknown crate ${JSON.stringify(onlyCrate)}`);
  }

  const parent = path.dirname(VENDOR_DIR);
  const lock = path.join(parent, ".vendor-lock");
  const backup = path.join(lock, "previous");
  mkdirSync(parent, { recursive: true });
  mkdirSync(lock);

  let staging = "";
  try {
    staging = mkdtempSync(path.join(parent, ".vendor-stage-"));
    const crates = onlyCrate === null ? [] : copyUnselectedSnapshot(staging, onlyCrate);

    for (const source of configured) {
      if (onlyCrate !== null && !source.crates.includes(onlyCrate)) continue;
      if (!existsSync(source.root)) {
        throw new Error(`${source.id}: ${source.root} is not present; set GITPULSE_${source.id.toUpperCase()}_ROOT`);
      }
      const workspace = readToml(readManifest(path.join(source.root, source.workspace, "Cargo.toml")));
      const commit = gitCommit(source.root);
      for (const name of source.crates) {
        if (onlyCrate !== null && name !== onlyCrate) continue;
        crates.push(prepareCrate(source, name, path.join(staging, name), workspace, commit));
      }
    }

    crates.sort(
      (/** @type {{ name: string }} */ a, /** @type {{ name: string }} */ b) => a.name.localeCompare(b.name),
    );
    const manifest = {
      note: "Generated by scripts/vendor-crates.mjs. Do not edit these crates here; change them upstream and re-vendor.",
      crates,
    };
    writeFileSync(path.join(staging, "VENDOR.json"), `${JSON.stringify(manifest, null, 2)}\n`);

    if (existsSync(VENDOR_DIR)) renameSync(VENDOR_DIR, backup);
    try {
      renameSync(staging, VENDOR_DIR);
      staging = "";
    } catch (error) {
      if (existsSync(backup)) renameSync(backup, VENDOR_DIR);
      throw error;
    }
    rmSync(backup, { recursive: true, force: true });
    return manifest;
  } finally {
    if (staging) rmSync(staging, { recursive: true, force: true });
    // A retained backup means rollback itself failed. Keep both it and the
    // lock for explicit recovery rather than deleting the only old snapshot.
    if (!existsSync(backup)) rmSync(lock, { recursive: true, force: true });
  }
}

/**
 * @typedef {{ name: string, edited: string[], upstream: "matches" | "drifted" | "unavailable",
 *             drifted: string[], reason: string }} CrateCheck
 */

/**
 * Verifies the vendored tree, without writing anything.
 *
 * @returns {{ ok: boolean, comparable: boolean, crates: CrateCheck[] }}
 */
export function check(env = process.env) {
  if (!existsSync(MANIFEST)) throw new Error(`${MANIFEST} is missing; run without --check to vendor`);
  const manifest = JSON.parse(readFileSync(MANIFEST, "utf8"));
  const bySource = new Map(sources(env).map((s) => [s.id, s]));

  // Framework ports have pinned upstream provenance and reviewed local patches.
  // Their refresh policy differs from sibling snapshots, but file integrity is
  // the same contract and must remain part of the canonical vendor check.
  const frameworkDir = path.join(REPO, "src-tauri", "framework");
  const appManifest = path.join(REPO, "src-tauri", "Cargo.toml");
  const requiresFramework = existsSync(appManifest) && /\bpath\s*=\s*["']framework\//.test(readManifest(appManifest));
  const snapshots = [{ dir: VENDOR_DIR, manifest }];
  if (existsSync(frameworkDir) || requiresFramework) {
    snapshots.push({ dir: frameworkDir, manifest: JSON.parse(readManifest(path.join(frameworkDir, "PATCHES.json"))) });
  }

  /** @type {CrateCheck[]} */
  const crates = [];
  for (const snapshot of snapshots) {
    for (const crate of snapshot.manifest.crates) {
      const dir = path.join(snapshot.dir, crate.name);
      /** @type {string[]} */
      const edited = [];

      const present = new Set(walk(dir));
      for (const [rel, hash] of Object.entries(crate.files)) {
        if (!present.has(rel)) edited.push(`${rel} (missing)`);
        else if (sha256(readFileSync(path.join(dir, rel))) !== hash) edited.push(rel);
        present.delete(rel);
      }
      for (const extra of present) edited.push(`${extra} (not vendored)`);

      const source = bySource.get(crate.origin.repo);
      /** @type {CrateCheck} */
      const result = { name: crate.name, edited, upstream: "unavailable", drifted: [], reason: "" };

      const from = source ? path.join(source.root, crate.origin.path) : "";
      if (!source || !from || !existsSync(from)) {
        // The distinction this whole mode exists for: not compared is not clean.
        result.reason = `${crate.origin.repo} is not checked out here`;
      } else {
        const current = gitCommit(source.root);
        const scratch = mkdtempSync(path.join(tmpdir(), "gitpulse-vendor-check-"));
        try {
          const workspace = readToml(readManifest(path.join(source.root, source.workspace, "Cargo.toml")));
          const expected = prepareCrate(source, crate.name, scratch, workspace, current);
          // Compare upstream's transformed snapshot with the hashes recorded at
          // the last refresh. Local edits are an independent verdict above and
          // must not be misreported as upstream drift.
          const files = new Set([...Object.keys(crate.files), ...Object.keys(expected.files)]);
          for (const rel of [...files].sort()) {
            if (crate.files[rel] !== expected.files[rel]) result.drifted.push(rel);
          }
        } finally {
          rmSync(scratch, { recursive: true, force: true });
        }
        result.upstream = result.drifted.length === 0 ? "matches" : "drifted";
        if (current && current !== crate.origin.commit) {
          result.reason = `upstream has moved to ${current.slice(0, 8)} since vendoring at ${String(crate.origin.commit).slice(0, 8)}`;
        }
      }
      crates.push(result);
    }
  }

  const allowDrift = env.GITPULSE_ALLOW_DRIFT === "1" || env.GITPULSE_ALLOW_DRIFT === "true";
  return {
    ok: crates.every((c) => c.edited.length === 0 && (allowDrift || c.upstream !== "drifted")),
    comparable: crates.every((c) => c.upstream !== "unavailable"),
    crates,
  };
}

function usage() {
  return formatUsage({
    name: "vendor-crates",
    summary: "Vendor the sibling Rust crates GitPulse links, so a lone checkout builds.",
    flags: [
      { flag: "--check", description: "Verify the vendored tree instead of rewriting it" },
      { flag: "--crate=NAME", description: "Refresh one crate and preserve every unrelated crate" },
      { flag: "--allow-drift", description: "Allow upstream drift while verifying no local edits" },
      { flag: "--json", description: "Emit machine-readable output" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits: "0 vendored / no drift · 1 drift or a local edit · 2 the run could not complete",
  });
}

/** @param {string[]} argv */
export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  const unknown = argv.find(
    (a) => a !== "--check" && a !== "--json" && a !== "--allow-drift" && !a.startsWith("--crate="),
  );
  if (unknown) {
    console.error(`FAIL: unknown option ${JSON.stringify(unknown)}\n`);
    console.error(usage());
    return 2;
  }
  const asJson = argv.includes("--json");
  const allowDrift = argv.includes("--allow-drift");
  const selected = argv.filter((arg) => arg.startsWith("--crate="));
  if (selected.length > 1 || (selected.length === 1 && argv.includes("--check"))) {
    console.error("FAIL: --crate accepts one crate and cannot be combined with --check");
    return 2;
  }

  try {
    if (!argv.includes("--check")) {
      const crate = selected.length === 1 ? selected[0].slice("--crate=".length) : null;
      const manifest = vendor(process.env, crate);
      if (asJson) console.log(JSON.stringify(manifest, null, 2));
      else {
        for (const crate of manifest.crates) {
          console.log(`  ${crate.name.padEnd(16)} ${Object.keys(crate.files).length} files  ${crate.origin.repo}@${String(crate.origin.commit).slice(0, 8)}`);
        }
        console.log(`\nOK: vendored ${manifest.crates.length} crates into src-tauri/vendored`);
      }
      return 0;
    }

    const env = allowDrift ? { ...process.env, GITPULSE_ALLOW_DRIFT: "1" } : process.env;
    const result = check(env);
    if (asJson) {
      console.log(JSON.stringify(result, null, 2));
      return result.ok ? 0 : 1;
    }
    for (const crate of result.crates) {
      const upstream = crate.upstream === "unavailable" ? `not compared — ${crate.reason}` : crate.upstream;
      console.log(`  ${crate.name.padEnd(16)} local: ${crate.edited.length === 0 ? "clean" : `${crate.edited.length} edited`}   upstream: ${upstream}`);
      for (const file of crate.edited) console.log(`      edited here: ${file}`);
      for (const file of crate.drifted) console.log(`      differs from upstream: ${file}`);
      if (crate.upstream !== "unavailable" && crate.reason) console.log(`      note: ${crate.reason}`);
    }
    if (!result.comparable) {
      console.log("\nNot every crate could be compared against its upstream. This is not a clean bill of health.");
    }
    if (!result.ok) {
      console.error("\nFAIL: the vendored tree does not match what was recorded.");
      return 1;
    }
    console.log("\nOK: no vendored file has been edited here.");
    return 0;
  } catch (error) {
    console.error(`FAIL: ${error instanceof Error ? error.message : String(error)}`);
    return 2;
  }
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) process.exitCode = main();
