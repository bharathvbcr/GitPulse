#!/usr/bin/env node
/**
 * Explorer search and listing: DevCouncil's `dcgrep` against the `git` path
 * GitPulse falls back to.
 *
 *   node scripts/bench-dcgrep.mjs [--repo <path>] [--rounds 15] [--dcgrep <bin>] [--json <file>]
 *
 * Each arm runs what GitPulse runs, minus its spawn gate:
 *   - search, dcgrep: `dcgrep search` with the request `engine::dcgrep` sends
 *     (`search_working_tree_inner`), 500 results as `DEFAULT_MAX_MATCHES`;
 *   - search, git: `content_search::search`'s working-tree path — `git grep
 *     --untracked` with the dcgrep-store exclusions, then `git ls-files -c -i
 *     --exclude-standard` and a second `git grep` over any tracked-but-ignored
 *     paths. GitPulse kills the grep at the match limit; here its stdout is
 *     closed after 500 records, which ends it the same way;
 *   - listing, dcgrep: `dcgrep files` only. GitPulse also reads `.git/index`
 *     in-process and unions the two, so this arm is a lower bound;
 *   - listing, git: `git ls-files -z --cached --others --exclude-standard`.
 *
 * Before timing, every case is checked for parity: both arms, unlimited, must
 * report the same match count, and the two listings must be the same set.
 * A case that disagrees is reported and not timed, and the exit code is 1.
 *
 * Rounds are interleaved with the order alternated (ABBA), after one discarded
 * warm-up. A third arm, dcgrep against itself ("null"), measures run-to-run
 * spread on the same host, so a gap can be read against it. Read-only: nothing
 * in the repository is written.
 */
import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { homedir, loadavg } from "node:os";
import { join, resolve } from "node:path";
import { performance } from "node:perf_hooks";

/**
 * @typedef {{ name: string, pattern: string, fixed: boolean, icase: boolean }} Case
 * @typedef {{ n: number, min: number, p50: number, max: number }} Summary
 * @typedef {{ dcgrep: Summary, git: Summary, null: Summary }} Arms
 * @typedef {{ dcgrep_matches: number, git_matches: number, equal: boolean }} SearchParity
 * @typedef {Case & { parity: SearchParity, timed: boolean, ms?: Arms }} CaseResult
 * @typedef {{
 *   parity: { dcgrep_plus_index: number, git: number, equal: boolean },
 *   timed: boolean,
 *   ms?: Arms,
 * }} ListingResult
 * @typedef {{
 *   repo: string,
 *   dcgrep: { path: string, version: string, schema_version: number },
 *   git: string,
 *   rounds: number,
 *   load_start: number,
 *   load_end: number,
 *   cases: CaseResult[],
 *   listing: ListingResult | null,
 * }} Report
 */

const args = process.argv.slice(2);
/** @param {string} name @param {string} fallback @returns {string} */
function flag(name, fallback) {
  const at = args.indexOf(name);
  return at >= 0 && at + 1 < args.length ? args[at + 1] : fallback;
}
const repo = resolve(flag("--repo", process.cwd()));
const rounds = Number.parseInt(flag("--rounds", "15"), 10);
const dcgrep = flag("--dcgrep", process.env.GITPULSE_DCGREP_BIN || join(homedir(), ".local", "bin", "dcgrep"));
const jsonOut = flag("--json", "");
if (!Number.isInteger(rounds) || rounds < 1) throw new Error(`--rounds must be a positive integer, got ${flag("--rounds", "")}`);

const LIMIT = 500; // content_search::DEFAULT_MAX_MATCHES
const MAX_BUFFER = 256 * 1024 * 1024;
const EXCLUDED = [
  ":(exclude,icase).devcouncil/dcgrep",
  ":(exclude,glob,icase)**/.devcouncil/dcgrep",
  ":(exclude,glob,icase)**/.devcouncil/dcgrep/**",
];

/** @type {Case[]} */
const CASES = [
  { name: "rare literal", pattern: "search_working_tree", fixed: true, icase: false },
  { name: "common literal", pattern: "let", fixed: true, icase: false },
  { name: "regex", pattern: "fn [a-z_]+_tests?[ (<]", fixed: false, icase: false },
  { name: "case-insensitive literal", pattern: "DCGREP", fixed: true, icase: true },
];

/** @param {string} cmd @param {string[]} argv @param {string} [input] */
function run(cmd, argv, input) {
  const out = spawnSync(cmd, argv, { input, maxBuffer: MAX_BUFFER, cwd: repo });
  if (out.error) throw new Error(`${cmd} ${argv.join(" ")}: ${out.error.message}`);
  return out;
}

/** @param {Case} c @param {number} max @returns {{ count: number }} */
function dcSearch(c, max) {
  const body = JSON.stringify({
    pattern: c.pattern,
    root: repo,
    max_results: max,
    include_ignored: false,
    include_hidden: true,
    fixed_strings: c.fixed,
    case_insensitive: c.icase,
    preserve_whitespace: true,
    precompose_unicode: process.platform === "darwin",
    include_devcouncil: true,
    ignore_dot_ignore: true,
    include_git_tracked: true,
    max_file_bytes: 64 * 1024 * 1024,
    max_line_bytes: 4 * 1024,
  });
  const out = run(dcgrep, ["search"], body);
  if (out.status !== 0) throw new Error(`dcgrep search exited ${out.status}: ${out.stderr}`);
  const reply = JSON.parse(out.stdout.toString("utf8"));
  if (!reply.ok) throw new Error(`dcgrep search: ${reply.error ?? "not ok"}`);
  return { count: Number(reply.count) };
}

/** @param {Case} c @returns {string[]} */
function grepArgv(c) {
  return ["-c", "core.quotepath=off", "grep", "-n", "--column", "-z", "-I", "--no-color", "--full-name",
    c.fixed ? "-F" : "-E", ...(c.icase ? ["-i"] : [])];
}

/**
 * The git fallback, bounded at `limit` records the way GitPulse's observer bounds it.
 * @param {Case} c @param {number} limit
 */
function gitSearch(c, limit) {
  /** @param {string[]} argv */
  const head = (argv) => {
    // Close stdout after `limit` lines; git then dies of SIGPIPE, as it does when GitPulse kills it.
    const script = `git "$@" | head -n ${limit} | wc -l`;
    const out = run("/bin/sh", ["-c", script, "sh", ...argv]);
    return Number.parseInt(out.stdout.toString().trim(), 10) || 0;
  };
  let lines = head([...grepArgv(c), "--untracked", "-e", c.pattern, "--", ...EXCLUDED]);
  const ignored = run("git", ["ls-files", "-c", "-i", "--exclude-standard", "-z"]).stdout
    .toString("utf8").split("\0").filter(Boolean);
  if (ignored.length > 0) lines += head([...grepArgv(c), "-e", c.pattern, "--", ...ignored]);
  return lines;
}

/** Unlimited git match count, for parity. @param {Case} c */
function gitCount(c) {
  const dropped = new Set(["-n", "--column", "-z", "--full-name"]);
  const out = run("git", [...grepArgv(c).filter((a) => !dropped.has(a)), "--untracked", "-c", "-e", c.pattern, "--", ...EXCLUDED]);
  if (out.status === null || out.status > 1) throw new Error(`git grep exited ${out.status}: ${out.stderr}`);
  return out.stdout.toString("utf8").split("\n").filter(Boolean)
    .reduce((sum, line) => sum + Number.parseInt(line.slice(line.lastIndexOf(":") + 1), 10), 0);
}

/** @returns {string[]} */
function dcFiles() {
  const body = JSON.stringify({
    root: repo,
    max_results: 200000,
    include_ignored: false,
    include_hidden: true,
    include_oversized: true,
    include_symlinks: true,
    precompose_unicode: process.platform === "darwin",
    include_devcouncil: true,
    ignore_dot_ignore: true,
  });
  const out = run(dcgrep, ["files"], body);
  if (out.status !== 0) throw new Error(`dcgrep files exited ${out.status}: ${out.stderr}`);
  const reply = JSON.parse(out.stdout.toString("utf8"));
  if (!reply.ok || !Array.isArray(reply.paths)) throw new Error("dcgrep files: not ok");
  return reply.paths.map(String);
}

/** @returns {string[]} */
function gitFiles() {
  return run("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).stdout
    .toString("utf8").split("\0").filter(Boolean);
}

/** @param {() => unknown} fn */
function time(fn) {
  const start = performance.now();
  fn();
  return performance.now() - start;
}

/** @param {number[]} samples @returns {Summary} */
function summary(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  return { n: sorted.length, min: sorted[0], p50: sorted[Math.floor((sorted.length - 1) / 2)], max: sorted[sorted.length - 1] };
}

/**
 * Interleaved A/B with alternating order, then the null arm, after a warm-up of each.
 * @param {() => unknown} a dcgrep @param {() => unknown} b git @returns {Arms}
 */
function measure(a, b) {
  a(); b();
  /** @type {number[]} */ const dc = [];
  /** @type {number[]} */ const git = [];
  /** @type {number[]} */ const nul = [];
  for (let i = 0; i < rounds; i++) {
    if (i % 2 === 0) { dc.push(time(a)); git.push(time(b)); }
    else { git.push(time(b)); dc.push(time(a)); }
  }
  for (let i = 0; i < rounds; i++) nul.push(time(a));
  return { dcgrep: summary(dc), git: summary(git), null: summary(nul) };
}

const health = JSON.parse(run(dcgrep, ["health"]).stdout.toString("utf8"));
/** @type {Report} */
const report = {
  repo,
  dcgrep: { path: dcgrep, version: String(health.version), schema_version: Number(health.schema_version) },
  git: run("git", ["--version"]).stdout.toString().trim(),
  rounds,
  load_start: loadavg()[0],
  load_end: 0,
  cases: [],
  listing: null,
};

for (const c of CASES) {
  const dc = dcSearch(c, 5000).count;
  const git = gitCount(c);
  // Both at or past dcgrep's ceiling is parity too: the limit, not the engine, decides the count.
  const equal = dc === git || (dc >= 5000 && git >= 5000);
  const parity = { dcgrep_matches: dc, git_matches: git, equal };
  report.cases.push(equal
    ? { ...c, parity, timed: true, ms: measure(() => dcSearch(c, LIMIT), () => gitSearch(c, LIMIT)) }
    : { ...c, parity, timed: false });
}

{
  const dc = new Set(dcFiles().map((p) => p.replace(/\/$/, "")));
  const tracked = run("git", ["ls-files", "-z", "--cached"]).stdout.toString("utf8").split("\0").filter(Boolean);
  for (const path of tracked) dc.add(path);
  const git = new Set(gitFiles().map((p) => p.replace(/\/$/, "")));
  const equal = dc.size === git.size && [...dc].every((p) => git.has(p));
  report.listing = {
    parity: { dcgrep_plus_index: dc.size, git: git.size, equal },
    timed: equal,
    ...(equal ? { ms: measure(dcFiles, gitFiles) } : {}),
  };
}
report.load_end = loadavg()[0];

/** @param {Summary} s */
const fmt = (s) => `${s.min.toFixed(1)} / ${s.p50.toFixed(1)}`;
/** @param {string} head @param {Arms | undefined} ms */
const line = (head, ms) => ms
  ? `${head}  dcgrep ${fmt(ms.dcgrep)}  git ${fmt(ms.git)}  null ${fmt(ms.null)}  ×${(ms.git.min / ms.dcgrep.min).toFixed(1)}`
  : `${head}  MISMATCH, not timed`;
console.log(`dcgrep ${report.dcgrep.version} · ${report.git} · ${repo}`);
console.log(`rounds ${rounds} · load ${report.load_start.toFixed(1)} → ${report.load_end.toFixed(1)} · ms as min / p50`);
for (const c of report.cases) {
  console.log(line(`${c.name.padEnd(26)} parity ${c.parity.dcgrep_matches}/${c.parity.git_matches}`, c.ms));
}
const listing = report.listing;
console.log(line(`${"listing".padEnd(26)} parity ${listing.parity.dcgrep_plus_index}/${listing.parity.git}`, listing.ms));
if (jsonOut) writeFileSync(jsonOut, `${JSON.stringify(report, null, 2)}\n`);
process.exit(report.cases.some((c) => !c.timed) || !listing.timed ? 1 : 0);
