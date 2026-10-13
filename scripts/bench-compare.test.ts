import { execFile } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import {
  DEFAULT_FLOOR_US,
  DEFAULT_RATIO,
  buildBaseline,
  compare,
  loadResults,
  validateReport,
} from "./bench-compare.mjs";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(new URL("./bench-compare.mjs", import.meta.url));
const baselinesDir = fileURLToPath(new URL("../src-tauri/benches/baselines", import.meta.url));
const tempDirs: string[] = [];

afterAll(async () => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop();
    if (dir) await rm(dir, { recursive: true, force: true });
  }
});

type Row = { n: number; time_gated?: boolean; min_ns: number; counters: Record<string, number> };

/** A schema-1 report as `benches/common/mod.rs` writes it. */
function report(
  bench: string,
  rows: Record<string, Partial<Row>>,
  overrides: Record<string, unknown> = {},
) {
  const filled: Record<string, Row> = {};
  for (const [name, row] of Object.entries(rows)) {
    filled[name] = { n: 7, time_gated: true, min_ns: 1_000_000, counters: { rows: 100 }, ...row };
  }
  return {
    schema: 1,
    bench,
    commit: "abc123",
    os: "macos",
    arch: "aarch64",
    cpu_count: 8,
    runner_class: "macos-latest",
    smoke: false,
    host: { start: { load: { status: "value", value: [1.5, 1.2, 1.1] } } },
    rows: filled,
    ...overrides,
  };
}

const baselineOf = (...reports: ReturnType<typeof report>[]) =>
  buildBaseline(Object.fromEntries(reports.map((r) => [r.bench, r])));
const runOf = (...reports: ReturnType<typeof report>[]) =>
  Object.fromEntries(reports.map((r) => [r.bench, r]));

describe("compare: the regression gate", () => {
  const base = report("graph_diff", { "solve 10k": { min_ns: 10_000_000 } });

  it("fails a synthetic 2x regression in min-of-N", () => {
    const slow = report("graph_diff", { "solve 10k": { min_ns: 20_000_000 } });
    const verdict = compare(baselineOf(base), runOf(slow));
    expect(verdict.code).toBe(1);
    expect(verdict.failures.join("\n")).toMatch(/solve 10k.*2\.00x/);
  });

  it("passes a noise-level change in either direction", () => {
    for (const min_ns of [10_800_000, 9_000_000, 10_000_000]) {
      const verdict = compare(baselineOf(base), runOf(report("graph_diff", { "solve 10k": { min_ns } })));
      expect(verdict.code, `min_ns ${min_ns}`).toBe(0);
      expect(verdict.failures).toEqual([]);
    }
  });

  it("passes an improvement", () => {
    const fast = report("graph_diff", { "solve 10k": { min_ns: 2_000_000 } });
    expect(compare(baselineOf(base), runOf(fast)).code).toBe(0);
  });

  it("fails exactly past the documented ratio", () => {
    const at = report("graph_diff", { "solve 10k": { min_ns: 10_000_000 * DEFAULT_RATIO } });
    const past = report("graph_diff", { "solve 10k": { min_ns: 10_000_000 * DEFAULT_RATIO + 1_000 } });
    expect(compare(baselineOf(base), runOf(at)).code).toBe(0);
    expect(compare(baselineOf(base), runOf(past)).code).toBe(1);
  });

  it("does not fail a tiny row on an absolute change under the floor", () => {
    const tiny = report("graph_diff", { "validate": { min_ns: 2_000 } });
    const jitter = report("graph_diff", {
      "validate": { min_ns: 2_000 + (DEFAULT_FLOOR_US * 1000) / 2 }, // 26x, but 25 us
    });
    expect(compare(baselineOf(tiny), runOf(jitter)).code).toBe(0);
    const real = report("graph_diff", { "validate": { min_ns: 2_000 + DEFAULT_FLOOR_US * 1000 + 1 } });
    expect(compare(baselineOf(tiny), runOf(real)).code).toBe(1);
  });

  it("fails when a work counter changes, even if the time improved", () => {
    const fewer = report("graph_diff", { "solve 10k": { min_ns: 1_000_000, counters: { rows: 99 } } });
    const verdict = compare(baselineOf(base), runOf(fewer));
    expect(verdict.code).toBe(1);
    expect(verdict.failures.join("\n")).toMatch(/work counter rows changed 100 -> 99/);
  });

  it("fails when a counter appears or disappears", () => {
    const extra = report("graph_diff", { "solve 10k": { min_ns: 10_000_000, counters: { rows: 100, spawns: 3 } } });
    expect(compare(baselineOf(base), runOf(extra)).failures.join("\n")).toMatch(/spawns changed \(absent\) -> 3/);
    const gone = report("graph_diff", { "solve 10k": { min_ns: 10_000_000, counters: {} } });
    expect(compare(baselineOf(base), runOf(gone)).failures.join("\n")).toMatch(/rows changed 100 -> \(absent\)/);
  });

  it("does not gate the wall clock of a row that opted out, but still gates its counters", () => {
    const flaky = report("pty", { flood: { time_gated: false, min_ns: 1_000_000 } });
    const slower = report("pty", { flood: { time_gated: false, min_ns: 9_000_000 } });
    expect(compare(baselineOf(flaky), runOf(slower)).code).toBe(0);
    const changed = report("pty", { flood: { time_gated: false, min_ns: 1_000_000, counters: { rows: 1 } } });
    expect(compare(baselineOf(flaky), runOf(changed)).code).toBe(1);
  });

  it("fails when a baselined row or bench disappears", () => {
    const two = report("graph_diff", { a: {}, b: {} });
    const one = report("graph_diff", { a: {} });
    expect(compare(baselineOf(two), runOf(one)).failures.join("\n")).toMatch(/graph_diff \/ b: baselined row is missing/);
    const other = report("watcher", { a: {} });
    expect(compare(baselineOf(two), runOf(other)).failures.join("\n")).toMatch(/graph_diff: baselined but produced no report/);
  });

  it("notes, rather than fails, a row with no baseline, unless strict", () => {
    const added = report("graph_diff", { "solve 10k": { min_ns: 10_000_000 }, fresh: {} });
    const lax = compare(baselineOf(base), runOf(added));
    expect(lax.code).toBe(0);
    expect(lax.notes.join("\n")).toMatch(/fresh: no baseline for this row/);
    expect(compare(baselineOf(base), runOf(added), { strictNew: true }).code).toBe(1);
  });

  it("refuses a run from another runner class instead of comparing across them", () => {
    const ubuntu = report("graph_diff", { "solve 10k": {} }, { runner_class: "ubuntu-latest" });
    const verdict = compare(baselineOf(base), runOf(ubuntu));
    expect(verdict.code).toBe(2);
    expect(verdict.failures.join("\n")).toMatch(/runner class mismatch/);
  });

  it("says when the host was loaded, so a slow wall clock is read for what it is", () => {
    const loaded = report("graph_diff", { "solve 10k": { min_ns: 10_000_000 } }, {
      host: { start: { load: { status: "value", value: [40, 30, 20] } } },
    });
    const verdict = compare(baselineOf(base), runOf(loaded));
    expect(verdict.notes.join("\n")).toMatch(/host load 40\.0 on 8 cpus/);
  });
});

describe("report validation and baseline building", () => {
  it("rejects a report with no rows, no min or the wrong schema", () => {
    expect(() => validateReport({ ...report("x", { a: {} }), rows: {} }, "f")).toThrow(/no rows/);
    expect(() => validateReport({ ...report("x", { a: {} }), schema: 2 }, "f")).toThrow(/schema/);
    const noMin = report("x", { a: {} });
    delete (noMin.rows.a as Partial<Row>).min_ns;
    expect(() => validateReport(noMin, "f")).toThrow(/no min_ns/);
  });

  it("refuses to fold two runner classes into one baseline", () => {
    const mac = report("a", { r: {} });
    const linux = report("b", { r: {} }, { runner_class: "ubuntu-latest" });
    expect(() => buildBaseline(runOf(mac, linux))).toThrow(/span runner classes/);
  });

  it("round-trips: a run compared with the baseline written from it passes", () => {
    const run = runOf(report("a", { r: { min_ns: 5_000_000 } }), report("b", { s: { counters: { ops: 3 } } }));
    expect(compare(buildBaseline(run), run).code).toBe(0);
  });
});

describe("the CLI", () => {
  async function workspace(reports: ReturnType<typeof report>[]) {
    const dir = await mkdtemp(path.join(tmpdir(), "bench-compare-"));
    tempDirs.push(dir);
    const results = path.join(dir, "results");
    await mkdir(results);
    for (const r of reports) await writeFile(path.join(results, `${r.bench}.json`), JSON.stringify(r));
    return { dir, results };
  }

  async function run(args: string[]) {
    try {
      const { stdout, stderr } = await execFileAsync(process.execPath, [scriptPath, ...args]);
      return { code: 0, stdout, stderr };
    } catch (error) {
      const e = error as { code: number; stdout: string; stderr: string };
      return { code: e.code, stdout: e.stdout, stderr: e.stderr };
    }
  }

  it("exits 2, never 0, when there is no baseline for the class", async () => {
    const { dir, results } = await workspace([report("graph_diff", { a: {} })]);
    const out = await run(["--results", results, "--baseline", path.join(dir, "missing.json")]);
    expect(out.code).toBe(2);
    expect(out.stderr).toMatch(/NO BASELINE for runner class macos-latest/);
  });

  it("writes a baseline, then passes a clean run and fails a 2x one (exit 0 then 1)", async () => {
    const { dir, results } = await workspace([report("graph_diff", { a: { min_ns: 8_000_000 } })]);
    const baseline = path.join(dir, "baseline.json");
    expect((await run(["--results", results, "--write-baseline", baseline])).code).toBe(0);
    expect((await run(["--results", results, "--baseline", baseline])).code).toBe(0);

    const slow = await workspace([report("graph_diff", { a: { min_ns: 16_000_000 } })]);
    const failed = await run(["--results", slow.results, "--baseline", baseline]);
    expect(failed.code).toBe(1);
    expect(failed.stderr).toMatch(/FAIL: graph_diff \/ a: min-of-7/);
  });

  it("exits 2 on an empty results directory and on malformed JSON", async () => {
    const dir = await mkdtemp(path.join(tmpdir(), "bench-compare-"));
    tempDirs.push(dir);
    const empty = path.join(dir, "empty");
    await mkdir(empty);
    expect((await run(["--results", empty, "--baseline", path.join(dir, "b.json")])).code).toBe(2);
    await writeFile(path.join(empty, "bad.json"), "{not json");
    expect((await run(["--results", empty, "--baseline", path.join(dir, "b.json")])).code).toBe(2);
    expect(() => loadResults(empty)).toThrow(/not valid JSON/);
  });

  it("exits 2 on a bad command line", async () => {
    expect((await run([])).code).toBe(2);
    expect((await run(["--results", "x", "--baseline", "a", "--write-baseline", "b"])).code).toBe(2);
    expect((await run(["--results", "x", "--baseline", "a", "--ratio", "-1"])).code).toBe(2);
  });
});

describe("committed baselines", () => {
  const files = existsSync(baselinesDir)
    ? readdirSync(baselinesDir).filter((name) => name.endsWith(".json"))
    : [];

  it("exist for at least one runner class", () => {
    expect(files.length).toBeGreaterThan(0);
  });

  it.each(files)("%s is a well-formed schema-1 baseline named for its class", (file) => {
    const baseline = JSON.parse(readFileSync(path.join(baselinesDir, file), "utf8"));
    expect(baseline.schema).toBe(1);
    expect(`${baseline.runner_class}.json`).toBe(file);
    expect(Object.keys(baseline.benches).length).toBeGreaterThan(0);
    for (const [bench, entry] of Object.entries<any>(baseline.benches)) {
      expect(Object.keys(entry.rows).length, bench).toBeGreaterThan(0);
      for (const [name, row] of Object.entries<any>(entry.rows)) {
        expect(row.min_ns, `${bench}/${name}`).toBeGreaterThan(0);
        expect(row.counters, `${bench}/${name}`).toBeTypeOf("object");
      }
    }
  });
});
