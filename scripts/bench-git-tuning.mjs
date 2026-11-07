#!/usr/bin/env node
/**
 * ft-6b68be32: what repository tuning buys, to set the thresholds below which
 * the Health panel offers nothing (src-tauri/src/storage/repo_tuning.rs).
 *
 *   node scripts/bench-git-tuning.mjs <packs|loose|files> [--sizes a,b,c]
 *        [--rounds 7] [--json <file>]
 *
 * Every fixture is a throwaway repository under `tmp/` (removed on exit); no
 * repository the user owns is read or written, and no `--global` or `--system`
 * config is touched. Fixtures are built with `git fast-import`, so they are
 * deterministic and quick.
 *
 *   packs  many packs vs one multi-pack-index vs one pack, on the object lookups
 *          a history walk and a status do (commit-graph off, so lookups matter)
 *   loose  a history held as loose objects vs the same history packed
 *   files  index format: default vs feature.manyFiles, status and a one-file add
 *
 * Timing: arms are interleaved (every round runs each arm once, the order
 * rotated), min-of-N and p50 are reported, plus the machine load at start and
 * end so a noisy run is visible. "ratio" is arm / baseline, below 1 is faster.
 */
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { cpus, loadavg } from "node:os";
import { performance } from "node:perf_hooks";

const here = dirname(fileURLToPath(import.meta.url));
const scratchRoot = process.env.BENCH_TMP ? resolve(process.env.BENCH_TMP) : resolve(here, "..", "tmp");
mkdirSync(scratchRoot, { recursive: true });
const work = mkdtempSync(join(scratchRoot, "bench-tuning-"));
const cleanup = () => rmSync(work, { recursive: true, force: true });
process.on("exit", cleanup);
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => process.exit(130));

const GIT_ENV = {
  ...process.env,
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_OPTIONAL_LOCKS: "0",
  GIT_TERMINAL_PROMPT: "0",
};

/**
 * @param {string} cwd
 * @param {string[]} args
 * @param {string} [input]
 * @returns {string}
 */
function git(cwd, args, input) {
  const r = spawnSync("git", args, { cwd, env: GIT_ENV, input, encoding: "utf8", maxBuffer: 1 << 28 });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${r.stderr}`);
  return r.stdout;
}

/**
 * An ordinary git run: unlike [git], it may refresh and rewrite the index.
 * @param {string} cwd
 * @param {string[]} args
 */
function gitRefreshing(cwd, args) {
  /** @type {Record<string, string | undefined>} */
  const env = { ...GIT_ENV };
  delete env.GIT_OPTIONAL_LOCKS;
  const r = spawnSync("git", args, { cwd, env, encoding: "utf8", maxBuffer: 1 << 28 });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${r.stderr}`);
}

/**
 * The index format version from its header.
 * @param {string} dir
 * @returns {number}
 */
function indexVersion(dir) {
  return readFileSync(join(dir, ".git/index")).readUInt32BE(4);
}

/**
 * @param {string} cwd
 * @param {string[]} args
 * @returns {number}
 */
function timeGit(cwd, args) {
  const t0 = performance.now();
  const r = spawnSync("git", args, { cwd, env: GIT_ENV, stdio: ["ignore", "ignore", "pipe"] });
  const ms = performance.now() - t0;
  if (r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${r.stderr}`);
  return ms;
}

/** A fast-import stream: `commits` commits, each touching `perCommit` of `files` paths. */
/**
 * @param {number} commits
 * @param {number} files
 * @param {number} perCommit
 * @returns {string}
 */
function importStream(commits, files, perCommit) {
  const out = [];
  let seed = 12345;
  const rnd = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff);
  for (let c = 0; c < commits; c++) {
    out.push(`commit refs/heads/main\ncommitter b <b@x> ${1700000000 + c} +0000\ndata 3\nc${c % 10}\n`);
    const touched = c === 0 ? files : perCommit;
    for (let i = 0; i < touched; i++) {
      const f = c === 0 ? i : rnd() % files;
      const body = `commit ${c} file ${f} ${rnd()}\n`.padEnd(64, ".") + "\n";
      out.push(`M 100644 inline d${f % 100}/f${f}.txt\ndata ${Buffer.byteLength(body)}\n${body}`);
    }
    out.push("\n");
  }
  return out.join("");
}

/**
 * @param {string} name
 * @returns {string}
 */
function freshRepo(name) {
  const dir = join(work, name);
  mkdirSync(dir, { recursive: true });
  git(dir, ["init", "-q", "-b", "main"]);
  git(dir, ["config", "gc.auto", "0"]);
  git(dir, ["config", "core.commitGraph", "false"]);
  git(dir, ["config", "gc.writeCommitGraph", "false"]);
  return dir;
}

/**
 * @param {string} dir
 * @param {number} commits
 * @param {number} files
 * @param {number} perCommit
 * @param {string[]} [fastImportArgs]
 */
function populate(dir, commits, files, perCommit, fastImportArgs = []) {
  git(dir, ["fast-import", "--quiet", ...fastImportArgs], importStream(commits, files, perCommit));
  git(dir, ["reset", "-q", "--hard", "main"]);
}

/** @param {string} dir */
function packCount(dir) {
  return readdirSync(join(dir, ".git/objects/pack")).filter((n) => n.endsWith(".pack")).length;
}

/** Turns every pack of the repository into loose objects. */
/** @param {string} dir */
function explode(dir) {
  const packDir = join(dir, ".git/objects/pack");
  for (const name of readdirSync(packDir).filter((n) => n.endsWith(".pack"))) {
    const bytes = readFileSync(join(packDir, name));
    rmSync(join(packDir, name));
    rmSync(join(packDir, name.replace(/\.pack$/, ".idx")));
    const un = spawnSync("git", ["unpack-objects", "-q"], { cwd: dir, env: GIT_ENV, input: bytes, maxBuffer: 1 << 28 });
    if (un.status !== 0) throw new Error(`unpack-objects: ${un.stderr}`);
  }
}

/**
 * What `git maintenance start` would schedule, run once by hand on a fixture.
 * @param {string} dir
 * @param {string} task
 */
function maintain(dir, task) {
  git(dir, ["maintenance", "run", `--task=${task}`]);
}

/** Rewrites the history as `count` packs, each holding a slice of it. */
/**
 * @param {string} dir
 * @param {number} count
 */
function splitIntoPacks(dir, count) {
  explode(dir);
  const commits = git(dir, ["rev-list", "--reverse", "main"]).trim().split("\n");
  const step = Math.ceil(commits.length / count);
  let previous = null;
  for (let i = 0; i < commits.length; i += step) {
    const tip = commits[Math.min(i + step, commits.length) - 1];
    const revs = git(dir, ["rev-list", "--objects", previous ? `${previous}..${tip}` : tip]);
    git(dir, ["pack-objects", "-q", ".git/objects/pack/pack"], revs);
    previous = tip;
  }
  git(dir, ["prune-packed"]);
}

/** @param {string} dir */
function looseCount(dir) {
  const objects = join(dir, ".git/objects");
  let n = 0;
  for (const d of readdirSync(objects)) if (/^[0-9a-f]{2}$/.test(d)) n += readdirSync(join(objects, d)).length;
  return n;
}

/**
 * @param {number[]} samples
 * @returns {{ n: number, min: number, p50: number, max: number }}
 */
function summarize(samples) {
  const s = [...samples].sort((a, b) => a - b);
  return { n: s.length, min: s[0], p50: s[Math.floor(s.length / 2)], max: s[s.length - 1] };
}
/**
 * Interleaved, order rotated.
 * @param {Array<{ name: string, dir: string }>} arms
 * @param {Array<{ name: string, args: string[] }>} ops
 * @param {number} rounds
 */
function race(arms, ops, rounds) {
  /** @type {Record<string, Record<string, number[]>>} */
  const results = {};
  for (const op of ops) {
    results[op.name] = Object.fromEntries(arms.map((a) => [a.name, []]));
    // one unrecorded warm-up per arm so the first read of the fixture is not charged
    for (const a of arms) timeGit(a.dir, op.args);
    for (let r = 0; r < rounds; r++) {
      for (let i = 0; i < arms.length; i++) {
        const a = arms[(i + r) % arms.length];
        results[op.name][a.name].push(timeGit(a.dir, op.args));
      }
    }
  }
  return Object.fromEntries(
    Object.entries(results).map(([op, byArm]) => [op, Object.fromEntries(Object.entries(byArm).map(([k, v]) => [k, summarize(v)]))]),
  );
}

const READ_OPS = [
  { name: "rev-list --count HEAD", args: ["rev-list", "--count", "HEAD"] },
  { name: "log -n 200 --stat", args: ["log", "-n", "200", "--stat", "--format=%H"] },
  { name: "log -- d7/f7.txt", args: ["log", "--format=%H", "--", "d7/f7.txt"] },
  { name: "rev-list --objects HEAD", args: ["rev-list", "--objects", "HEAD"] },
  { name: "status", args: ["status", "--porcelain"] },
];

/**
 * @param {string} name
 * @param {string | null} fallback
 * @returns {string | null}
 */
function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
}

const mode = process.argv[2];
const sizes = (arg("sizes", "") || "").split(",").filter(Boolean).map(Number);
const rounds = Number(arg("rounds", "7"));
/** @type {any} */
const report = { mode, rounds, git: git(work, ["--version"]).trim(), cpus: cpus().length, load_start: loadavg(), runs: [] };

if (mode === "packs") {
  // `sizes` are pack counts; the history is the same in every arm.
  const packTargets = sizes.length ? sizes : [1, 20, 60, 150, 400];
  const commits = Number(arg("commits", "4000"));
  const files = Number(arg("files", "2000"));
  for (const target of packTargets) {
    const many = freshRepo(`packs-${target}`);
    populate(many, commits, files, 4);
    splitIntoPacks(many, target);
    const midx = freshRepo(`midx-${target}`);
    populate(midx, commits, files, 4);
    splitIntoPacks(midx, target);
    git(midx, ["multi-pack-index", "write"]);
    const one = freshRepo(`one-${target}`);
    populate(one, commits, files, 4);
    git(one, ["repack", "-a", "-d", "-q"]);
    // The action itself: the incremental-repack task `git maintenance start` schedules.
    const kept = freshRepo(`maintained-${target}`);
    populate(kept, commits, files, 4);
    splitIntoPacks(kept, target);
    maintain(kept, "incremental-repack");
    const arms = [
      { name: "many-packs", dir: many },
      { name: "midx", dir: midx },
      { name: "maintained", dir: kept },
      { name: "one-pack", dir: one },
    ];
    const result = race(arms, READ_OPS, rounds);
    report.runs.push({ packs: packCount(many), packs_after_maintenance: packCount(kept), commits, files, result });
  }
} else if (mode === "loose") {
  // `sizes` are commit counts; loose objects are about commits * (1 + 5).
  const commitTargets = sizes.length ? sizes : [100, 500, 1500, 4000];
  const files = Number(arg("files", "500"));
  for (const commits of commitTargets) {
    const packed = freshRepo(`packed-${commits}`);
    populate(packed, commits, files, 4);
    const loose = freshRepo(`loose-${commits}`);
    populate(loose, commits, files, 4);
    explode(loose);
    // The action itself: the loose-objects task `git maintenance start` schedules.
    const kept = freshRepo(`maintained-loose-${commits}`);
    populate(kept, commits, files, 4);
    explode(kept);
    maintain(kept, "loose-objects");
    const arms = [
      { name: "loose", dir: loose },
      { name: "maintained", dir: kept },
      { name: "packed", dir: packed },
    ];
    const result = race(arms, READ_OPS, rounds);
    report.runs.push({ loose_objects: looseCount(loose), loose_after_maintenance: looseCount(kept), packs_after_maintenance: packCount(kept), commits, files, result });
  }
} else if (mode === "files") {
  // Arm A is a plain repository, arm B the same repository with
  // feature.manyFiles, and arm C a second plain copy: the identical-arm control
  // that gives the noise floor. Every arm gets one ordinary, index-refreshing
  // `git status` (no GIT_OPTIONAL_LOCKS), which is when git rewrites the index
  // in the format the config asks for; only then are GitPulse-style reads
  // (GIT_OPTIONAL_LOCKS=0) timed, so a gain that is just "the index was
  // refreshed once" shows up in arm A as well and cancels.
  const fileTargets = sizes.length ? sizes : [20000, 40000, 100000];
  for (const files of fileTargets) {
    const plain = freshRepo(`plain-${files}`);
    const control = freshRepo(`plain-b-${files}`);
    const many = freshRepo(`many-${files}`);
    // Attribution arm: only the untracked cache, the one part of
    // feature.manyFiles that survived the control.
    const untracked = freshRepo(`untracked-${files}`);
    for (const dir of [plain, control, many, untracked]) populate(dir, 1, files, 0);
    git(many, ["config", "feature.manyFiles", "true"]);
    git(untracked, ["config", "core.untrackedCache", "true"]);
    for (const dir of [plain, control, many, untracked]) {
      writeFileSync(join(dir, "extra.txt"), "x");
      gitRefreshing(dir, ["status", "--porcelain"]);
    }
    /** @param {string} d */
    const sizeOf = (d) => statSync(join(d, ".git/index")).size;
    const arms = [
      { name: "default", dir: plain },
      { name: "default-b (control)", dir: control },
      { name: "manyFiles", dir: many },
      { name: "untrackedCache only", dir: untracked },
    ];
    const ops = [
      { name: "status", args: ["status", "--porcelain"] },
      // GitPulse's own invocation (`GitReader::STATUS_ARGV`): all untracked files, NUL-separated.
      { name: "status (GitPulse argv)", args: ["-c", "core.quotepath=off", "status", "--porcelain=v1", "-u", "-z"] },
      { name: "status -u (all untracked)", args: ["status", "--porcelain", "-u"] },
      { name: "status -z", args: ["status", "--porcelain", "-z"] },
      { name: "ls-files", args: ["ls-files", "--stage"] },
      { name: "add extra.txt (index rewrite)", args: ["add", "extra.txt"] },
    ];
    const result = race(arms, ops, rounds);
    report.runs.push({
      tracked: files,
      index_version: { default: indexVersion(plain), manyFiles: indexVersion(many) },
      index_bytes: { default: sizeOf(plain), manyFiles: sizeOf(many) },
      result,
    });
  }
} else {
  console.error("usage: bench-git-tuning.mjs <packs|loose|files> [--sizes a,b] [--rounds N] [--json file]");
  process.exit(2);
}

report.load_end = loadavg();
const jsonPath = arg("json", null);
if (jsonPath) writeFileSync(jsonPath, JSON.stringify(report, null, 2));

/** @param {number} x */
const fmt = (x) => x.toFixed(1);
for (const run of report.runs) {
  const head = Object.entries(run).filter(([k]) => k !== "result" && typeof run[k] !== "object");
  console.log(`\n## ${head.map(([k, v]) => `${k}=${v}`).join(" ")}${run.index_bytes ? ` index_bytes=${JSON.stringify(run.index_bytes)}` : ""}`);
  console.log("| operation | arm | min ms | p50 ms | min ratio vs baseline |");
  console.log("| --- | --- | --- | --- | --- |");
  for (const [op, byArm] of Object.entries(run.result)) {
    const arms = Object.keys(byArm);
    const baseName = report.mode === "files" ? "default" : arms.includes("one-pack") ? "one-pack" : "packed";
    for (const arm of arms) {
      const s = byArm[arm];
      console.log(`| ${op} | ${arm} | ${fmt(s.min)} | ${fmt(s.p50)} | ${(s.min / byArm[baseName].min).toFixed(2)} |`);
    }
  }
}
console.log(`\ngit ${report.git}; ${report.cpus} cpus; load ${report.load_start.map((/** @type {number} */ x) => x.toFixed(1))} -> ${report.load_end.map((/** @type {number} */ x) => x.toFixed(1))}; rounds ${rounds}`);
