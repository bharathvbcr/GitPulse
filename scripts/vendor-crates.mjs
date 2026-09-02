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
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
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
export function sources(env = process.env) {
  return [
    {
      id: "manvi",
      root: env.GITPULSE_MANVI_ROOT ?? findSibling("Manvi"),
      // The workspace root inside that repository, whose inheritance applies.
      workspace: "crates",
      crates: ["dc-glob", "dc-store", "dc-verify"],
      crateDir: (/** @type {string} */ name) => path.join("crates", name),
    },
    {
      id: "devcouncil",
      root: env.GITPULSE_DEVCOUNCIL_ROOT ?? findSibling("DevCouncil"),
      workspace: "rust-port",
      crates: ["devmap-analyze", "devmap-extract", "devmap-query", "devmap-resolve", "devmap-store"],
      crateDir: (/** @type {string} */ name) => path.join("rust-port", "crates", name),
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
  return path.join(path.dirname(from), name);
}

/** Files and directories taken from each crate. */
const COPIED = ["src", "build.rs"];

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
  const out = [];
  for (const entry of readdirSync(dir).sort()) {
    const full = path.join(dir, entry);
    const rel = prefix ? `${prefix}/${entry}` : entry;
    if (statSync(full).isDirectory()) out.push(...walk(full, rel));
    else out.push(rel);
  }
  return out;
}

/** @param {string} root */
function gitCommit(root) {
  try {
    return execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  } catch {
    return "";
  }
}

/**
 * Copies every configured crate into `src-tauri/vendored/` and writes the
 * manifest. Throws if a source repository is missing — re-vendoring is an
 * explicit act and must not half-succeed.
 */
export function vendor(env = process.env) {
  const crates = [];
  for (const source of sources(env)) {
    if (!existsSync(source.root)) {
      throw new Error(`${source.id}: ${source.root} is not present; set GITPULSE_${source.id.toUpperCase()}_ROOT`);
    }
    const workspace = readToml(readFileSync(path.join(source.root, source.workspace, "Cargo.toml"), "utf8"));
    const commit = gitCommit(source.root);

    for (const name of source.crates) {
      const from = path.join(source.root, source.crateDir(name));
      const to = path.join(VENDOR_DIR, name);
      rmSync(to, { recursive: true, force: true });
      mkdirSync(to, { recursive: true });

      for (const item of COPIED) {
        const src = path.join(from, item);
        if (existsSync(src)) cpSync(src, path.join(to, item), { recursive: true });
      }

      const upstream = readFileSync(path.join(from, "Cargo.toml"), "utf8");
      const { text, rewrites } = resolveManifest(upstream, workspace);
      writeFileSync(path.join(to, "Cargo.toml"), text);

      /** @type {Record<string, string>} */
      const files = {};
      for (const rel of walk(to)) files[rel] = sha256(readFileSync(path.join(to, rel)));

      crates.push({
        name,
        origin: { repo: source.id, root_env: `GITPULSE_${source.id.toUpperCase()}_ROOT`, path: source.crateDir(name), commit },
        omitted: ["tests/", "[dev-dependencies]"],
        rewrites,
        files,
      });
    }
  }

  crates.sort((a, b) => a.name.localeCompare(b.name));
  const manifest = {
    note: "Generated by scripts/vendor-crates.mjs. Do not edit these crates here; change them upstream and re-vendor.",
    crates,
  };
  mkdirSync(VENDOR_DIR, { recursive: true });
  writeFileSync(MANIFEST, `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
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

  /** @type {CrateCheck[]} */
  const crates = [];
  for (const crate of manifest.crates) {
    const dir = path.join(VENDOR_DIR, crate.name);
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
      for (const item of COPIED) {
        const src = path.join(from, item);
        if (!existsSync(src)) continue;
        for (const rel of statSync(src).isDirectory() ? walk(src, item) : [item]) {
          const theirs = readFileSync(path.join(from, rel));
          const ours = path.join(dir, rel);
          if (!existsSync(ours) || sha256(readFileSync(ours)) !== sha256(theirs)) result.drifted.push(rel);
        }
      }
      result.upstream = result.drifted.length === 0 ? "matches" : "drifted";
      if (current && current !== crate.origin.commit) {
        result.reason = `upstream has moved to ${current.slice(0, 8)} since vendoring at ${String(crate.origin.commit).slice(0, 8)}`;
      }
    }
    crates.push(result);
  }

  return {
    ok: crates.every((c) => c.edited.length === 0 && c.upstream !== "drifted"),
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
  const unknown = argv.find((a) => a !== "--check" && a !== "--json");
  if (unknown) {
    console.error(`FAIL: unknown option ${JSON.stringify(unknown)}\n`);
    console.error(usage());
    return 2;
  }
  const asJson = argv.includes("--json");

  try {
    if (!argv.includes("--check")) {
      const manifest = vendor();
      if (asJson) console.log(JSON.stringify(manifest, null, 2));
      else {
        for (const crate of manifest.crates) {
          console.log(`  ${crate.name.padEnd(16)} ${Object.keys(crate.files).length} files  ${crate.origin.repo}@${String(crate.origin.commit).slice(0, 8)}`);
        }
        console.log(`\nOK: vendored ${manifest.crates.length} crates into src-tauri/vendored`);
      }
      return 0;
    }

    const result = check();
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
