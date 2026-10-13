#!/usr/bin/env node
/**
 * Spike ft-7450105d: what `git status` costs with git's own defaults against a
 * repository whose *user* has turned on `core.untrackedCache`, `core.fsmonitor`,
 * or both, to decide whether the Health panel should ever suggest it.
 *
 *   node scripts/bench-git-status-config.mjs --repo <path> [--repo <path> ...]
 *        [--synthetic <files>] [--rounds 9] [--untracked 0] [--json <file>]
 *
 * Owner decision: GitPulse never writes repo config and never passes
 * `-c core.untrackedCache` / `-c core.fsmonitor`. So this script only ever
 * changes configuration inside THROWAWAY CLONES that it makes under `tmp/`;
 * each `--repo` is a read-only clone source and is never touched. Every config
 * write is `git config --local` run with its working directory inside a scratch
 * clone (checked before each call), and no call names `--global` or `--system`.
 *
 * Every scratch clone is cleaned up on exit, including on SIGINT / SIGTERM, and
 * the fsmonitor daemon (which outlives the process that started it, and is not
 * covered by procguard) is stopped with `git fsmonitor--daemon stop` first. The
 * report states how many daemon processes existed before the run and after
 * cleanup; a difference is printed as a failure and the exit code is 1.
 *
 * Measured, per repository and variant:
 *   - "cold": the first `git status` after the clone is configured;
 *   - "warm": interleaved rounds (each round runs every variant once, the order
 *     rotated), min-of-N and p50, after the user's own priming `git status`
 *     runs (which is what populates the untracked cache and starts the daemon);
 *   - two invocation modes: "gitpulse" (the exact `STATUS_ARGV` and child
 *     environment, `GIT_OPTIONAL_LOCKS=0`, which stops status refreshing the
 *     index) and "plain" (what the user's own terminal runs);
 *   - the daemon's cost: processes started, resident memory, accumulated CPU
 *     time, and idle wakeups per second sampled with `top` while the tree is quiet.
 */
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { cpus, devNull, loadavg } from "node:os";
import { performance } from "node:perf_hooks";

const here = dirname(fileURLToPath(import.meta.url));
const scratchRoot = resolve(here, "..", "tmp");

const VARIANTS = [
  { name: "default", config: [] },
  { name: "untrackedCache", config: [["core.untrackedCache", "true"]] },
  { name: "fsmonitor", config: [["core.fsmonitor", "true"]] },
  {
    name: "both",
    config: [
      ["core.untrackedCache", "true"],
      ["core.fsmonitor", "true"],
    ],
  },
];

/** `GitReader::STATUS_ARGV` (src-tauri/src/engine/git_reader.rs). */
const GITPULSE_STATUS = ["-c", "core.quotepath=off", "status", "--porcelain=v1", "-u", "-z"];
const PLAIN_STATUS = ["status", "--porcelain=v1", "-u", "-z"];

function parseArgs(argv) {
  const out = { repos: [], rounds: 9, untracked: 0, synthetic: 0, json: null };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--repo") out.repos.push(resolve(argv[++i]));
    else if (arg === "--rounds") out.rounds = Number(argv[++i]);
    else if (arg === "--untracked") out.untracked = Number(argv[++i]);
    else if (arg === "--synthetic") out.synthetic = Number(argv[++i]);
    else if (arg === "--json") out.json = resolve(argv[++i]);
    else throw new Error(`unknown argument ${arg}`);
  }
  if (out.repos.length === 0 && out.synthetic === 0) {
    throw new Error("pass at least one --repo <clone source> or --synthetic <files>");
  }
  if (!Number.isInteger(out.synthetic) || out.synthetic < 0) throw new Error("--synthetic must be >= 0");
  if (!Number.isInteger(out.rounds) || out.rounds < 1) throw new Error("--rounds must be >= 1");
  if (!Number.isInteger(out.untracked) || out.untracked < 0) throw new Error("--untracked must be >= 0");
  return out;
}

/** Environment for a measured child: no user or system config can change the variant. */
function gitEnv(extra = {}) {
  return {
    ...process.env,
    GIT_CONFIG_GLOBAL: devNull,
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_TERMINAL_PROMPT: "0",
    LC_ALL: "C",
    ...extra,
  };
}

function insideScratch(path) {
  return resolve(path).startsWith(scratchRoot + sep);
}

function git(cwd, args, env = gitEnv()) {
  const result = spawnSync("git", args, { cwd, env, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  if (result.error) throw result.error;
  return result;
}

/** The only place configuration is written: `--local`, inside a scratch clone. */
function setLocalConfig(cloneDir, key, value) {
  if (!insideScratch(cloneDir)) throw new Error(`refusing to write config outside the scratch root: ${cloneDir}`);
  const result = git(cloneDir, ["config", "--local", key, value]);
  if (result.status !== 0) throw new Error(`git config ${key} failed: ${result.stderr}`);
}

function daemonPids() {
  const result = spawnSync("pgrep", ["-f", "fsmonitor--daemon run"], { encoding: "utf8" });
  return new Set(
    (result.stdout ?? "")
      .split("\n")
      .map((line) => Number(line.trim()))
      .filter((pid) => Number.isInteger(pid) && pid > 0 && pid !== process.pid),
  );
}

/** Daemons holding a file under `dir` open: the ones this run started. */
function daemonsUnder(dir) {
  const owned = [];
  for (const pid of daemonPids()) {
    // The daemon's own working directory is the user's home; what ties it to
    // a repository is the IPC socket it holds open under that repository's `.git`.
    const result = spawnSync("lsof", ["-p", String(pid), "-Fn"], { encoding: "utf8" });
    const names = (result.stdout ?? "").split("\n").filter((line) => line.startsWith("n"));
    if (names.some((line) => line.slice(1).startsWith(dir + sep))) owned.push(pid);
  }
  return owned;
}

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function timeStatus(cloneDir, mode) {
  const argv = mode === "gitpulse" ? GITPULSE_STATUS : PLAIN_STATUS;
  const env = mode === "gitpulse" ? gitEnv({ GIT_OPTIONAL_LOCKS: "0" }) : gitEnv();
  const started = performance.now();
  const result = git(cloneDir, argv, env);
  const elapsed = performance.now() - started;
  if (result.status !== 0) throw new Error(`git ${argv.join(" ")} failed in ${cloneDir}: ${result.stderr}`);
  const entries = result.stdout.split("\0").filter(Boolean).length;
  return { ms: elapsed, entries };
}

function quantile(sorted, q) {
  if (sorted.length === 0) return null;
  return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
}

function summarize(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  return { n: sorted.length, min: sorted[0], p50: quantile(sorted, 0.5), max: sorted[sorted.length - 1] };
}

function processCost(pids) {
  if (pids.length === 0) return { processes: 0, rss_kb: 0, cpu_seconds: 0 };
  const result = spawnSync("ps", ["-o", "rss=,cputime=", "-p", pids.join(",")], { encoding: "utf8" });
  let rss = 0;
  let cpu = 0;
  for (const line of (result.stdout ?? "").split("\n")) {
    const parts = line.trim().split(/\s+/);
    if (parts.length < 2) continue;
    rss += Number(parts[0]) || 0;
    // cputime is [[dd-]hh:]mm:ss.cc
    const clock = parts[1].split(":").map(Number);
    cpu += clock.reduce((acc, part) => acc * 60 + part, 0);
  }
  return { processes: pids.length, rss_kb: rss, cpu_seconds: cpu };
}

/** Idle wakeups per second for `pids` over `seconds`, from `top`'s IDLEW column. */
function idleWakeups(pids, seconds) {
  if (pids.length === 0) return null;
  let total = 0;
  for (const pid of pids) {
    const result = spawnSync(
      "top",
      ["-l", "2", "-s", String(seconds), "-pid", String(pid), "-stats", "pid,idlew"],
      { encoding: "utf8" },
    );
    const lines = (result.stdout ?? "").split("\n").filter((line) => new RegExp(`^${pid}\\s`).test(line.trim()));
    const last = lines[lines.length - 1];
    if (!last) return null;
    const value = Number(last.trim().split(/\s+/)[1]);
    if (!Number.isFinite(value)) return null;
    total += value;
  }
  return total / seconds;
}

function populateUntracked(cloneDir, count) {
  if (count === 0) return;
  const dir = join(cloneDir, "spike-untracked");
  mkdirSync(dir, { recursive: true });
  for (let group = 0; group < Math.ceil(count / 100); group += 1) {
    const sub = join(dir, `d${group}`);
    mkdirSync(sub, { recursive: true });
    for (let i = 0; i < Math.min(100, count - group * 100); i += 1) {
      writeFileSync(join(sub, `f${i}.txt`), `untracked ${group}/${i}\n`);
    }
  }
}

/**
 * A scratch repository of `files` tracked files (fixed names and content, a
 * fixed identity and date), standing in for a repository too large to keep
 * around. Built inside the scratch root; it is a clone source like any other.
 */
function makeSyntheticRepo(dir, files) {
  mkdirSync(dir, { recursive: true });
  const env = gitEnv({
    GIT_AUTHOR_NAME: "spike",
    GIT_AUTHOR_EMAIL: "spike@example.invalid",
    GIT_COMMITTER_NAME: "spike",
    GIT_COMMITTER_EMAIL: "spike@example.invalid",
    GIT_AUTHOR_DATE: "2026-01-01T00:00:00Z",
    GIT_COMMITTER_DATE: "2026-01-01T00:00:00Z",
  });
  for (const args of [["init", "--quiet"], ["config", "--local", "commit.gpgsign", "false"], ["config", "--local", "gc.auto", "0"], ["config", "--local", "maintenance.auto", "false"]]) {
    if (git(dir, args, env).status !== 0) throw new Error(`git ${args.join(" ")} failed in the synthetic source`);
  }
  const perDir = 200;
  for (let group = 0; group < Math.ceil(files / perDir); group += 1) {
    const sub = join(dir, "src", `pkg${String(group).padStart(4, "0")}`);
    mkdirSync(sub, { recursive: true });
    for (let i = 0; i < Math.min(perDir, files - group * perDir); i += 1) {
      writeFileSync(join(sub, `file${i}.rs`), `pub fn f${group}_${i}() -> u32 { ${group * 1000 + i} }\n`);
    }
  }
  for (const args of [["add", "-A"], ["commit", "--quiet", "-m", "synthetic"]]) {
    if (git(dir, args, env).status !== 0) throw new Error(`git ${args.join(" ")} failed in the synthetic source`);
  }
}

function stopDaemon(cloneDir) {
  // `stop` on a clone with no daemon exits non-zero; that is fine.
  spawnSync("git", ["fsmonitor--daemon", "stop"], { cwd: cloneDir, env: gitEnv(), encoding: "utf8" });
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  mkdirSync(scratchRoot, { recursive: true });
  const workDir = mkdtempSync(join(scratchRoot, "git-status-spike-"));
  const clones = []; // { dir }
  let cleaned = false;
  const daemonsBefore = daemonPids();
  /** Pids of daemons this run's clones started (matched by working directory). */
  const mine = new Set();

  function cleanup() {
    if (cleaned) return;
    cleaned = true;
    for (const clone of clones) stopDaemon(clone.dir);
    rmSync(workDir, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 });
  }
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
    process.on(signal, () => {
      cleanup();
      process.exit(130);
    });
  }

  const report = {
    git: git(process.cwd(), ["--version"]).stdout.trim(),
    rounds: args.rounds,
    untracked_files_added: args.untracked,
    host: { cpus: cpus().length, loadavg_start: loadavg() },
    repos: [],
  };
  let failure = null;
  try {
    const sources = args.repos.map((path) => ({ path, label: path.split(sep).filter(Boolean).pop() }));
    if (args.synthetic > 0) {
      const path = join(workDir, "synthetic-source");
      makeSyntheticRepo(path, args.synthetic);
      sources.push({ path, label: `synthetic-${args.synthetic}` });
    }
    for (const { path: source, label: name } of sources) {
      const tracked = git(source, ["ls-files", "-z"]).stdout.split("\0").filter(Boolean).length;
      const row = { repo: name, tracked_files: tracked, variants: {}, daemon: null };
      const variantClones = [];
      const repoDir = join(workDir, `clones-${name}`);
      mkdirSync(repoDir, { recursive: true });
      for (const variant of VARIANTS) {
        const dir = join(repoDir, variant.name);
        const cloned = spawnSync("git", ["clone", "--quiet", "--local", source, dir], {
          env: gitEnv(),
          encoding: "utf8",
        });
        if (cloned.status !== 0) throw new Error(`clone of ${source} failed: ${cloned.stderr}`);
        clones.push({ dir });
        populateUntracked(dir, args.untracked);
        variantClones.push({ variant, dir });
      }

      // The user turns the features on in their own repository (here: a scratch clone).
      for (const { variant, dir } of variantClones) {
        for (const [key, value] of variant.config) setLocalConfig(dir, key, value);
      }

      // Cold: the first GitPulse-style status after configuration, before any priming.
      for (const { variant, dir } of variantClones) {
        const cold = timeStatus(dir, "gitpulse");
        row.variants[variant.name] = { cold_ms: cold.ms, entries: cold.entries, gitpulse: [], plain: [] };
      }

      // The user's own `git status` runs (twice) prime the untracked cache and start the daemon.
      for (const { dir } of variantClones) {
        timeStatus(dir, "plain");
        timeStatus(dir, "plain");
      }
      // Is the untracked cache really in effect? Its index extension is `UNTR`.
      for (const { variant, dir } of variantClones) {
        row.variants[variant.name].index_has_untracked_cache = readFileSync(join(dir, ".git", "index")).includes("UNTR");
      }
      // Give a freshly started daemon a moment to finish its initial scan.
      await new Promise((done) => setTimeout(done, 1500));

      // Warm, interleaved: one sample of every variant per round, rotating who goes first.
      for (const mode of ["gitpulse", "plain"]) {
        for (let round = 0; round < args.rounds; round += 1) {
          for (let k = 0; k < variantClones.length; k += 1) {
            const { variant, dir } = variantClones[(k + round) % variantClones.length];
            row.variants[variant.name][mode].push(timeStatus(dir, mode).ms);
          }
        }
      }
      for (const entry of Object.values(row.variants)) {
        entry.gitpulse = summarize(entry.gitpulse);
        entry.plain = summarize(entry.plain);
      }

      // Daemon cost, from the daemons this run started (not the user's own).
      const started = daemonsUnder(repoDir);
      for (const pid of started) mine.add(pid);
      const cost = processCost(started);
      row.daemon = {
        ...cost,
        idle_wakeups_per_second: idleWakeups(started, 5),
        note: "daemons started by this repository's fsmonitor and both clones only",
      };
      report.repos.push(row);

      // Done with this repository: stop its daemons and drop its clones now.
      for (const { dir } of variantClones) stopDaemon(dir);
      rmSync(repoDir, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 });
    }
  } catch (error) {
    failure = error;
  } finally {
    cleanup();
  }

  // Daemons are matched by pid, so wait briefly for the stopped ones to exit.
  await new Promise((done) => setTimeout(done, 1000));
  const leaked = [...mine].filter(alive);
  report.host.loadavg_end = loadavg();
  report.daemons_before = daemonsBefore.size;
  report.daemons_leaked = leaked.length;

  if (failure) {
    console.error(`spike failed: ${failure.message}`);
    process.exitCode = 1;
  }
  printReport(report);
  if (args.json) writeFileSync(args.json, `${JSON.stringify(report, null, 2)}\n`);
  if (leaked.length > 0) {
    console.error(`FAIL: ${leaked.length} fsmonitor daemon(s) outlived the run: ${leaked.join(", ")}`);
    process.exitCode = 1;
  }
}

function fmt(ms) {
  return ms === null || ms === undefined ? "n/a" : `${ms.toFixed(1)}`;
}

function printReport(report) {
  console.log(
    `${report.git}; ${report.rounds} interleaved rounds; ${report.untracked_files_added} untracked files added per clone; ` +
      `${report.host.cpus} cpus, load average ${report.host.loadavg_start[0].toFixed(1)} at start`,
  );
  for (const row of report.repos) {
    console.log(`\n${row.repo} (${row.tracked_files} tracked files). ms, min / p50 of warm rounds; cold = first status`);
    console.log("variant         cold   gitpulse(min/p50)   plain(min/p50)   (* = UNTR extension present in the index)");
    const base = row.variants.default;
    for (const [name, entry] of Object.entries(row.variants)) {
      const speed = (mode) => (base[mode].min > 0 ? ` ${(entry[mode].min / base[mode].min).toFixed(2)}x` : "");
      console.log(
        `${name.padEnd(15)}${entry.index_has_untracked_cache ? "*" : " "}${fmt(entry.cold_ms).padStart(6)}   ${fmt(entry.gitpulse.min).padStart(7)} / ${fmt(entry.gitpulse.p50).padEnd(7)}${speed("gitpulse").padEnd(7)} ${fmt(entry.plain.min).padStart(7)} / ${fmt(entry.plain.p50).padEnd(7)}${speed("plain")}`,
      );
    }
    const d = row.daemon;
    console.log(
      `fsmonitor daemons started: ${d.processes}, rss ${d.rss_kb} KB, cpu ${d.cpu_seconds.toFixed(2)} s, idle wakeups/s ${d.idle_wakeups_per_second === null ? "n/a" : d.idle_wakeups_per_second.toFixed(1)}`,
    );
  }
  console.log(`\nfsmonitor daemons before: ${report.daemons_before}; leaked after cleanup: ${report.daemons_leaked}`);
}

await main();
