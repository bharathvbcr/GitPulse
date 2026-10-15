#!/usr/bin/env node
/**
 * The TUI-redraw bench for card ft-406d1f4a (xterm WebGL renderer). Run INSIDE
 * a GitPulse terminal session, so every byte takes the production path: real
 * PTY -> reader -> per-session output Channel -> xterm -> paint -> ack.
 *
 *   node scripts/tui-redraw-bench.mjs --mode redraw --arm webgl --transparency true \
 *        --webcontent-pid <pid> --out tmp/tui-bench.jsonl [--seconds 30] [--hz 60]
 *   node scripts/tui-redraw-bench.mjs --mode flood  ... [--mib 100]
 *   node scripts/tui-redraw-bench.mjs --mode idle   ... [--seconds 60]
 *   node scripts/tui-redraw-bench.mjs --summarize tmp/tui-bench.jsonl
 *
 * Modes, one JSON line appended to `--out` per run (stdout is the screen):
 *
 * - `redraw`: the alternate screen, every cell rewritten (`--hz` frames a
 *   second for `--seconds`), as an agent CLI repaints. A frame the PTY would
 *   not take (backpressure) is skipped and counted, never queued.
 * - `flood`: `--mib` MiB of coloured lines as fast as the PTY takes them.
 *   GitPulse holds the PTY until painted output is acknowledged, so the rate
 *   is end to end; it is timed until the terminal answers a DSR sent after
 *   the last byte, i.e. until everything before it was parsed.
 * - `idle`: nothing written for `--seconds`; the cursor still blinks.
 *
 * Every mode samples the WebContent process named by `--webcontent-pid`
 * with `ps -M`: CPU-seconds of its first thread (the main thread: threads are
 * listed in creation order — inferred from the ordering, not from a thread
 * name, which `ps` does not print) and of the whole process. `redraw` and
 * `flood` also send a DSR (`ESC [6n`) every 250 ms and time xterm's reply:
 * the parse-and-answer round trip on the same main thread a keystroke echo
 * waits for. It is a PROXY for keystroke-echo p95 — it does not include the
 * keydown or the final paint — and the summary labels it so.
 *
 * Not measured here: the rAF dropped-frame fraction, which needs an in-page
 * probe (planned with ft-cc4be67b). `--summarize` therefore never answers
 * "ships" unless records carry `dropped_frame_fraction`; it says what is
 * missing instead. `--arm` and `--transparency` are declared by the person
 * running it (Settings > GPU terminal rendering; the allowTransparency build);
 * the script cannot observe them.
 */
import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { cpus, loadavg } from "node:os";
import { performance } from "node:perf_hooks";
import { pathToFileURL } from "node:url";

export const ARMS = Object.freeze(["dom", "webgl"]);
export const MODES = Object.freeze(["redraw", "flood", "idle"]);
/** The win rule's N, per arm and mode. */
export const MIN_RUNS = 7;
const DSR = "\u001b[6n";
const DSR_EVERY_MS = 250;
const DSR_DRAIN_MS = 10_000;

/**
 * One run, as appended to the results file.
 * @typedef {{ at: string, mode: string, arm: string, transparency: boolean,
 *   main_thread_cpu_s: number, wall_s: number, dsr_n?: number,
 *   dsr_p95_ms?: number | null, dsr_lost?: number, mb_per_s?: number,
 *   dropped_frame_fraction?: number }} BenchRecord
 */

/** What one mode measured, before the CPU sample and host load are added. */
/** @typedef {{ wall_s: number, dsr?: number[], dsr_lost?: number, [key: string]: unknown }} ModeResult */

/** A min-of-N or best-of-N figure per arm; null when no run carried it. */
/** @typedef {{ dom: number | null, webgl: number | null }} Pair */

/**
 * `ps` CPU time — `M:SS.ss` or `H:MM:SS.ss` — in seconds; NaN when unreadable.
 * @param {string} text
 */
export function parseClock(text) {
  if (!/^\d+(?::\d+){1,2}\.\d+$/.test(text)) return Number.NaN;
  return text.split(":").reduce((total, part) => total * 60 + Number(part), 0);
}

/**
 * `ps -M -p <pid>` output: main-thread and whole-process CPU-seconds.
 * Columns are USER PID TT %CPU STAT PRI STIME UTIME COMMAND; only the first
 * row carries USER and COMMAND, so each row is read by its two time columns.
 * @param {string} text
 */
export function parseThreads(text) {
  /** @type {number[]} */
  const rows = [];
  for (const line of text.split("\n").slice(1)) {
    const match = /\s(\d+(?::\d+){1,2}\.\d+)\s+(\d+(?::\d+){1,2}\.\d+)(?:\s|$)/.exec(line);
    if (!match) continue;
    const seconds = parseClock(match[1]) + parseClock(match[2]);
    if (Number.isFinite(seconds)) rows.push(seconds);
  }
  if (!rows.length) return null;
  return { main: rows[0], total: rows.reduce((sum, value) => sum + value, 0), threads: rows.length };
}

/** @param {number} pid */
function readWebContent(pid) {
  const parsed = parseThreads(execFileSync("ps", ["-M", "-p", String(pid)], { encoding: "utf8" }));
  if (!parsed) throw new Error(`ps -M -p ${pid} listed no threads`);
  return parsed;
}

/**
 * One full-screen repaint: every row rewritten in place, colour shifted per frame.
 * @param {number} index @param {number} cols @param {number} rows
 */
export function frame(index, cols, rows) {
  const glyphs = "abcdefghijklmnopqrstuvwxyz0123456789 ─│╭╮✓✗";
  let out = "";
  for (let row = 0; row < rows; row += 1) {
    let line = "";
    for (let col = 0; col < cols; col += 1) line += glyphs[(index + row * 7 + col) % glyphs.length];
    out += `\u001b[${row + 1};1H\u001b[${31 + ((index + row) % 6)}m${line}`;
  }
  return `${out}\u001b[0m`;
}

/**
 * Cursor-position reports (`ESC [ row ; col R`) in a chunk of stdin.
 * @param {string} text
 */
export function countDsrReplies(text) {
  return (text.match(/\u001b\[\d+;\d+R/g) ?? []).length;
}

/** @param {number[]} sorted @param {number} q @returns {number | null} */
export function quantile(sorted, q) {
  if (!sorted.length) return null;
  return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
}

/** @param {string[]} argv @returns {Record<string, string>} */
function parseArgs(argv) {
  /** @type {Record<string, string>} */
  const args = { hz: "60", mib: "100" };
  for (let i = 0; i < argv.length; i += 1) {
    const key = argv[i];
    const value = argv[i + 1];
    if (!key.startsWith("--") || value === undefined) throw new Error(`expected --name value, got ${key}`);
    args[key.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase())] = value;
    i += 1;
  }
  return args;
}

/**
 * Sends a DSR every 250 ms and times each reply; replies arrive in order.
 * @param {NodeJS.ReadStream} stdin @param {NodeJS.WriteStream} stdout
 */
function dsrProbe(stdin, stdout) {
  /** @type {number[]} */
  const pending = [];
  /** @type {number[]} */
  const samples = [];
  /** @param {Buffer} chunk */
  const onData = (chunk) => {
    for (let n = countDsrReplies(chunk.toString("latin1")); n > 0; n -= 1) {
      const sent = pending.shift();
      if (sent !== undefined) samples.push(performance.now() - sent);
    }
  };
  stdin.on("data", onData);
  const send = () => { pending.push(performance.now()); stdout.write(DSR); };
  const timer = setInterval(send, DSR_EVERY_MS);
  return {
    samples,
    /** Stops probing, sends one last DSR, and resolves once every reply is in. */
    async drain() {
      clearInterval(timer);
      send();
      const deadline = performance.now() + DSR_DRAIN_MS;
      while (pending.length && performance.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 5));
      stdin.off("data", onData);
      return pending.length;
    },
  };
}

/**
 * @param {Record<string, string>} args @param {NodeJS.ReadStream} stdin @param {NodeJS.WriteStream} stdout
 * @returns {Promise<ModeResult>}
 */
async function runRedraw(args, stdin, stdout) {
  const seconds = Number(args.seconds ?? 30);
  const hz = Number(args.hz);
  const cols = stdout.columns;
  const rows = stdout.rows;
  stdout.write("\u001b[?1049h\u001b[?25l");
  const probe = dsrProbe(stdin, stdout);
  const period = 1000 / hz;
  const start = performance.now();
  let written = 0;
  let skipped = 0;
  let bytes = 0;
  let blocked = false;
  stdout.on("drain", () => { blocked = false; });
  for (let index = 0; ; index += 1) {
    const due = start + index * period;
    if (due - start >= seconds * 1000) break;
    const wait = due - performance.now();
    if (wait > 0) await new Promise((resolve) => setTimeout(resolve, wait));
    if (blocked) { skipped += 1; continue; }
    const text = frame(index, cols, rows);
    bytes += Buffer.byteLength(text);
    written += 1;
    if (!stdout.write(text)) blocked = true;
  }
  const lost = await probe.drain();
  const wall = (performance.now() - start) / 1000;
  stdout.write("\u001b[?25h\u001b[?1049l");
  return { wall_s: wall, cols, rows, hz, frames_written: written, frames_skipped: skipped, bytes, dsr: probe.samples, dsr_lost: lost };
}

/**
 * @param {Record<string, string>} args @param {NodeJS.ReadStream} stdin @param {NodeJS.WriteStream} stdout
 * @returns {Promise<ModeResult>}
 */
async function runFlood(args, stdin, stdout) {
  const target = Number(args.mib) * 1024 * 1024;
  /** @param {number} n */
  const line = (n) => `\u001b[3${n % 7}m${String(n).padStart(9, "0")} ${"flood ".repeat(11)}\u001b[0m\r\n`;
  const probe = dsrProbe(stdin, stdout);
  const start = performance.now();
  let bytes = 0;
  for (let n = 0; bytes < target; n += 1) {
    const text = line(n);
    bytes += Buffer.byteLength(text);
    if (!stdout.write(text)) await new Promise((resolve) => stdout.once("drain", resolve));
  }
  const lost = await probe.drain();
  const wall = (performance.now() - start) / 1000;
  return { wall_s: wall, bytes, mb_per_s: bytes / (1024 * 1024) / wall, dsr: probe.samples, dsr_lost: lost };
}

/**
 * @param {Record<string, string>} args @param {NodeJS.WriteStream} stdout
 * @returns {Promise<ModeResult>}
 */
async function runIdle(args, stdout) {
  const seconds = Number(args.seconds ?? 60);
  stdout.write(`idle for ${seconds} s — do not touch this window\r\n`);
  const start = performance.now();
  await new Promise((resolve) => setTimeout(resolve, seconds * 1000));
  return { wall_s: (performance.now() - start) / 1000 };
}

/** @param {Record<string, string>} args */
async function measure(args) {
  if (process.platform !== "darwin") throw new Error("ps -M per-thread times are read on macOS only");
  if (!MODES.includes(args.mode)) throw new Error(`--mode must be one of ${MODES.join(", ")}`);
  if (!ARMS.includes(args.arm)) throw new Error(`--arm must be one of ${ARMS.join(", ")} (Settings > GPU terminal rendering)`);
  if (!["true", "false"].includes(args.transparency)) throw new Error("--transparency must be true or false (the build's allowTransparency)");
  const pid = Number(args.webcontentPid);
  if (!Number.isInteger(pid) || pid <= 1) throw new Error("--webcontent-pid is the test app's WebContent process id");
  if (!args.out) throw new Error("--out <file.jsonl> is required; stdout is the screen under test");
  const { stdin, stdout } = process;
  if (!stdin.isTTY || !stdout.isTTY) throw new Error("run this inside a GitPulse terminal session, not through a pipe");
  stdin.setRawMode(true);
  stdin.resume();
  const load = loadavg()[0];
  const before = readWebContent(pid);
  /** @type {ModeResult} */
  let result;
  try {
    result = args.mode === "redraw" ? await runRedraw(args, stdin, stdout)
      : args.mode === "flood" ? await runFlood(args, stdin, stdout)
        : await runIdle(args, stdout);
  } finally {
    stdin.setRawMode(false);
    stdin.pause();
  }
  const after = readWebContent(pid);
  const { dsr, ...rest } = result;
  const sorted = [...(dsr ?? [])].sort((a, b) => a - b);
  const record = {
    at: new Date().toISOString(),
    mode: args.mode,
    arm: args.arm,
    transparency: args.transparency === "true",
    webcontent_pid: pid,
    main_thread_cpu_s: after.main - before.main,
    process_cpu_s: after.total - before.total,
    ...rest,
    dsr_n: sorted.length,
    dsr_p50_ms: quantile(sorted, 0.5),
    dsr_p95_ms: quantile(sorted, 0.95),
    host: { load_start: load, load_end: loadavg()[0], cpus: cpus().length },
  };
  appendFileSync(args.out, `${JSON.stringify(record)}\n`);
  stdout.write(`\r\n${args.mode}/${args.arm}: main thread ${record.main_thread_cpu_s.toFixed(2)} CPU-s over ${result.wall_s.toFixed(1)} s — appended to ${args.out}\r\n`);
}

/**
 * Finite numbers out of `values`.
 * @param {unknown[]} values @returns {number[]}
 */
const finite = (values) => values.filter((value) => typeof value === "number" && Number.isFinite(value)).map(Number);

/**
 * Lowest of `values`; null when none is a number.
 * @param {unknown[]} values @returns {number | null}
 */
function minOf(values) {
  const numbers = finite(values);
  return numbers.length ? Math.min(...numbers) : null;
}

/**
 * Highest of `values`; null when none is a number.
 * @param {unknown[]} values @returns {number | null}
 */
function maxOf(values) {
  const numbers = finite(values);
  return numbers.length ? Math.max(...numbers) : null;
}

/**
 * The win rule over a results file. Fails closed: anything missing or invalid
 * is a named reason the verdict is "undecided", never a pass.
 * @param {BenchRecord[]} records
 */
export function verdict(records) {
  /** @type {string[]} */
  const missing = [];
  const valid = records.filter((record) => !record.dsr_lost && (record.mode === "idle" || (record.dsr_n ?? 0) > 0));
  if (valid.length !== records.length) missing.push(`${records.length - valid.length} run(s) lost a DSR reply and were dropped`);
  /** @param {string} mode @param {string} arm */
  const pick = (mode, arm) => valid.filter((record) => record.mode === mode && record.arm === arm);
  for (const mode of MODES) {
    for (const arm of ARMS) {
      const n = pick(mode, arm).length;
      if (n < MIN_RUNS) missing.push(`${mode}/${arm}: ${n} run(s), need ${MIN_RUNS}`);
    }
    const order = valid.filter((record) => record.mode === mode).sort((a, b) => a.at.localeCompare(b.at)).map((record) => record.arm);
    let streak = 0;
    for (let i = 0; i < order.length; i += 1) {
      streak = i > 0 && order[i] === order[i - 1] ? streak + 1 : 1;
      if (streak > 2) { missing.push(`${mode}: not interleaved (${streak} ${order[i]} runs in a row)`); break; }
    }
  }
  if (new Set(valid.map((record) => record.transparency)).size > 1) missing.push("the arms ran with different allowTransparency settings");
  /** @param {string} mode @param {(record: BenchRecord) => unknown} read @param {(values: unknown[]) => number | null} best @returns {Pair} */
  const pair = (mode, read, best) => ({ dom: best(pick(mode, "dom").map(read)), webgl: best(pick(mode, "webgl").map(read)) });
  const figures = {
    redraw_main_cpu_s: pair("redraw", (record) => record.main_thread_cpu_s, minOf),
    redraw_dsr_p95_ms_proxy_for_echo: pair("redraw", (record) => record.dsr_p95_ms, minOf),
    dropped_frame_fraction: pair("redraw", (record) => record.dropped_frame_fraction, minOf),
    flood_mb_per_s_best: pair("flood", (record) => record.mb_per_s, maxOf),
    idle_main_cpu_pct_of_core: pair("idle", (record) => (100 * record.main_thread_cpu_s) / record.wall_s, minOf),
  };
  for (const [name, { dom, webgl }] of Object.entries(figures)) {
    if (dom !== null && webgl !== null) continue;
    missing.push(name === "dropped_frame_fraction"
      ? "dropped-frame fraction not measured (needs the rAF probe planned with ft-cc4be67b)"
      : `${name}: no figure for ${dom === null ? "dom" : "webgl"}`);
  }
  if (missing.length) return { decided: false, ships: null, reasons: missing, figures };
  /** @param {Pair} figure @returns {{ dom: number, webgl: number }} */
  const both = (figure) => ({ dom: Number(figure.dom), webgl: Number(figure.webgl) });
  const cpu = both(figures.redraw_main_cpu_s);
  const echo = both(figures.redraw_dsr_p95_ms_proxy_for_echo);
  const dropped = both(figures.dropped_frame_fraction);
  const flood = both(figures.flood_mb_per_s_best);
  const idle = both(figures.idle_main_cpu_pct_of_core);
  /** @type {string[]} */
  const reasons = [];
  if (!(cpu.webgl * 2 <= cpu.dom)) reasons.push(`main-thread CPU ${cpu.webgl.toFixed(2)} s vs DOM ${cpu.dom.toFixed(2)} s is not 2x lower`);
  if (echo.webgl > echo.dom) reasons.push("echo p95 (DSR proxy) is worse");
  if (dropped.webgl > dropped.dom) reasons.push("dropped-frame fraction is worse");
  if (flood.webgl < flood.dom) reasons.push("flood throughput is worse");
  if (idle.webgl > idle.dom + 1) reasons.push("60 s idle CPU is more than 1 pp of a core higher");
  return { decided: true, ships: reasons.length === 0, reasons, figures };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.summarize) {
    /** @type {BenchRecord[]} */
    const records = readFileSync(args.summarize, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
    const result = verdict(records);
    console.log(JSON.stringify(result, null, 2));
    process.exitCode = result.decided ? (result.ships ? 0 : 1) : 2;
    return;
  }
  await measure(args);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : error}\n`);
    process.exitCode = 2;
  });
}
