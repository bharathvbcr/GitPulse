import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { CONTEXT_READ_MS, CONTROL_QUICK_MS, FAMILY_LOOKUP_MS, KEEP_MS, KILL_HARD_MS, KILL_SOFT_MS, RENDERER_DRAIN_MS } from "./inputQueue";

/**
 * The arithmetic written above `KILL_HARD_MS` (inputQueue.ts) is about the
 * backend's sleeps, and a number in a comment goes stale the day someone
 * lengthens a grace. This reads those sleeps out of the Rust source, the way
 * `SESSION_GONE_MARKERS` is pinned to its Rust twin, and holds each frontend
 * bound to them.
 */
const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "..");
const rust = readFileSync(join(root, "src-tauri", "src", "terminal", "mod.rs"), "utf8");

/** Text of a Rust item from `start` to the first closing brace in column 0. */
function item(start: string): string {
  const at = rust.indexOf(start);
  expect(at, `${start} not found in terminal/mod.rs`).toBeGreaterThanOrEqual(0);
  const rest = rust.slice(at);
  const end = rest.indexOf("\n}\n");
  expect(end).toBeGreaterThan(0);
  return rest.slice(0, end);
}

/** A process-table scan, as slow as the slowest `cmd_terminal_context` in a log taken at load ~400. */
const MEASURED_SCAN_MS = 1209;
/** Scans on `kill_session`'s critical path: the first listing, the grace loop's last, the two sweeps. */
const SCANS_ON_KILL_PATH = 4;
/** The two SIGKILL sweeps' pauses in `stop_session_jobs`, and portable_pty's 4 x 50 ms reap in `Child::kill` (an external crate: portable-pty 0.9.0, unix `ChildKiller for std::process::Child`). */
const SWEEP_AND_REAP_MS = 2 * 20 + 4 * 50;

function killSleepsMs(): number {
  const grace = /pub\(crate\) const HANGUP_GRACE: Duration = Duration::from_millis\((\d+)\);/.exec(rust);
  expect(grace, "HANGUP_GRACE moved or changed shape").not.toBeNull();
  const poll = /Instant::now\(\) \+ Duration::from_secs\((\d+)\)/.exec(item("pub fn kill_session("));
  expect(poll, "kill_session's cleanup poll moved or changed shape").not.toBeNull();
  return Number(grace![1]) + Number(poll![1]) * 1000 + SWEEP_AND_REAP_MS;
}

/** The top-level arguments of every `terminalDeadline(...)` call in `source`, found by walking the parentheses. */
function deadlineArguments(source: string): string[][] {
  const calls: string[][] = [];
  for (const start of source.matchAll(/terminalDeadline\(/g)) {
    const args: string[] = [];
    let depth = 1, current = "";
    for (let i = start.index + start[0].length; i < source.length && depth > 0; i++) {
      const ch = source[i];
      if ("([{".includes(ch)) depth++;
      else if (")]}".includes(ch)) depth--;
      if (depth === 0) break;
      if (ch === "," && depth === 1) { args.push(current); current = ""; } else current += ch;
    }
    args.push(current);
    calls.push(args);
  }
  return calls;
}

describe("the terminal command bounds against the backend they wait for", () => {
  it("a close is not called slow before its own grace could explain it, and not failed within its worst case", () => {
    const sleeps = killSleepsMs();
    expect(sleeps).toBe(4740);
    expect(KILL_SOFT_MS).toBeGreaterThan(sleeps);
    expect(KILL_HARD_MS).toBeGreaterThanOrEqual(sleeps + SCANS_ON_KILL_PATH * MEASURED_SCAN_MS);
    // Headroom, not just coverage: the worst case is not a measurement.
    expect(KILL_HARD_MS).toBeGreaterThanOrEqual(2.5 * (sleeps + SCANS_ON_KILL_PATH * MEASURED_SCAN_MS) - 1);
  });

  it("every other bound clears its measured or computed worst case with room", () => {
    expect(CONTEXT_READ_MS).toBeGreaterThanOrEqual(4 * MEASURED_SCAN_MS);
    expect(CONTROL_QUICK_MS).toBeGreaterThanOrEqual(10_000);
    expect(KEEP_MS).toBe(CONTROL_QUICK_MS + FAMILY_LOOKUP_MS);
    // No backend in the drain: it is the page parsing a 256 KiB window, so the
    // bound only has to outlast a starved page, and may not be the old 5 s.
    expect(RENDERER_DRAIN_MS).toBeGreaterThan(5000);
  });

  it("no terminal deadline is a bare number: each is a named bound with its reason", () => {
    const files = [
      join(here, "sessionLifecycle.ts"),
      join(here, "detachedSessions.ts"),
      join(root, "src", "lib", "components", "TerminalSession.svelte"),
    ];
    for (const file of files) {
      const found = deadlineArguments(readFileSync(file, "utf8")).filter((args) => /^\s*\d[\d_]*\s*$/.test(args[1] ?? ""));
      expect(found, `${file} has a numeric terminal deadline`).toEqual([]);
    }
  });

  it("the scan sees the calls it is looking at, including one with nested parentheses", () => {
    const calls = deadlineArguments('await terminalDeadline(Promise.resolve(hooks.reset()), 5000, "Draining", { a: 1 });');
    expect(calls).toHaveLength(1);
    expect(calls[0][1].trim()).toBe("5000");
  });
});
