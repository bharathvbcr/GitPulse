import { readFileSync } from "node:fs";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import { evaluateCoverage, parseLcov } from "./check-coverage-floor.mjs";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(new URL("./check-coverage-floor.mjs", import.meta.url));
const tempDirs: string[] = [];

afterAll(async () => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop();
    if (dir) await rm(dir, { recursive: true, force: true });
  }
});

function report({
  source = "src/example.ts",
  lineCounts = [1, 0],
  branchHits = [1, 1],
  terminate = true,
}: {
  source?: string;
  lineCounts?: number[];
  branchHits?: Array<number | "-">;
  terminate?: boolean;
} = {}) {
  const lines = lineCounts.map((count, index) => `DA:${index + 1},${count}`);
  const branches = branchHits.map(
    (count, index) => `BRDA:${index + 1},0,${index},${count}`,
  );
  const linesFound = lineCounts.length;
  const linesHit = lineCounts.filter((count) => count > 0).length;
  const branchesFound = branchHits.length;
  const branchesHit = branchHits.filter((count) => count !== "-" && count > 0).length;
  return [
    "TN:",
    `SF:${source}`,
    ...branches,
    `BRF:${branchesFound}`,
    `BRH:${branchesHit}`,
    ...lines,
    `LF:${linesFound}`,
    `LH:${linesHit}`,
    terminate ? "end_of_record" : "",
  ]
    .filter((line) => line !== "")
    .join("\n");
}

async function scratchRoot() {
  const root = await mkdtemp(path.join(tmpdir(), "gitpulse-coverage-floor-"));
  tempDirs.push(root);
  await mkdir(path.join(root, "coverage"), { recursive: true });
  return root;
}

async function runScript(args: string[]) {
  try {
    const { stdout, stderr } = await execFileAsync(process.execPath, [scriptPath, ...args]);
    return { code: 0, output: stdout + stderr };
  } catch (err) {
    const failure = err as { code?: number | null; stdout?: string; stderr?: string };
    return {
      code: failure.code ?? 1,
      output: (failure.stdout ?? "") + (failure.stderr ?? ""),
    };
  }
}

describe("coverage floor contract", () => {
  it("parses line and branch totals from complete LCOV records", () => {
    const parsed = parseLcov(report({ lineCounts: [4, 0, 2], branchHits: [1, 0, "-"] }));
    expect(parsed.records).toHaveLength(1);
    expect(parsed.totals).toEqual({
      lines: { found: 3, hit: 2 },
      branches: { found: 3, hit: 1 },
    });
  });

  it("accepts a report whose LF/LH disagree with its DA records", () => {
    // `cargo llvm-cov` computes LF/LH from its own line model and emits DA
    // only for instrumented lines, so a valid report carries LF above the DA
    // count and LH below the number of DA entries showing hits — real output
    // for ops.rs was LF 407 / 385 DA entries / LH 352 against 354 DA hits.
    // Treating that shape as corruption failed the gate on a genuine report.
    const llvmCovShape = [
      "TN:",
      "SF:src-tauri/src/example.rs",
      "DA:1,1",
      "DA:2,1",
      "DA:3,0",
      "LF:10",
      "LH:1",
      "end_of_record",
    ].join("\n");
    const parsed = parseLcov(llvmCovShape);
    // The summaries are what the floors are computed from, so they win.
    expect(parsed.totals.lines).toEqual({ found: 10, hit: 1 });
  });

  it("parses a record captured verbatim from cargo llvm-cov", () => {
    // scripts/fixtures/llvm-cov-real.info is the exact ops.rs record that
    // broke check:coverage: LF 407 with 385 DA entries, and LH 352 while 354
    // DA entries show hits. Both bugs in this parser came from inventing an
    // invariant and testing it against data invented by the same hand, so this
    // case is the producer's own bytes.
    const fixture = readFileSync(
      new URL("./fixtures/llvm-cov-real.info", import.meta.url),
      "utf8",
    );
    const parsed = parseLcov(fixture);
    expect(parsed.totals.lines).toEqual({ found: 407, hit: 352 });
  });

  it("parses a record captured verbatim from vitest v8", () => {
    // The two producers disagree: every one of the 103 records in a real v8
    // report satisfies LF == DA count and LH == DA hits, which is why the
    // invariant that broke on llvm-cov held for the frontend. Pinning both
    // means a change that suits one producer cannot silently break the other.
    const fixture = readFileSync(
      new URL("./fixtures/vitest-v8-real.info", import.meta.url),
      "utf8",
    );
    const parsed = parseLcov(fixture);
    expect(parsed.totals.lines).toEqual({ found: 18, hit: 18 });
    expect(parsed.totals.branches?.found).toBe(22);
  });

  it("accepts the saturated counters llvm-cov emits", () => {
    // llvm-cov writes u64::MAX for a counter that saturated or underflowed;
    // four such lines appear in this repository's own Rust report. They exceed
    // JavaScript's safe integer range, and rejecting them failed the gate on a
    // report cargo-llvm-cov had just produced.
    const saturated = [
      "TN:",
      "SF:src-tauri/src/example.rs",
      "DA:1,18446744073709551615",
      "DA:2,0",
      "BRF:2",
      "BRH:1",
      "BRDA:1,0,0,18446744073709551615",
      "BRDA:1,0,1,0",
      "LF:2",
      "LH:1",
      "end_of_record",
    ].join("\n");
    const parsed = parseLcov(saturated);
    expect(parsed.totals.lines).toEqual({ found: 2, hit: 1 });
    expect(parsed.totals.branches).toEqual({ found: 2, hit: 1 });
  });

  it("still rejects an execution count that is not a number at all", () => {
    // Relaxing the range must not relax the shape.
    for (const bad of ["abc", "-5", "1.5", ""]) {
      expect(() =>
        parseLcov(`TN:\nSF:a.rs\nDA:1,${bad}\nLF:1\nLH:1\nend_of_record`),
      ).toThrow();
    }
  });

  it("rejects forged summaries, missing data, duplicate lines, and truncation", () => {
    // Still corruption: a summary that is internally impossible.
    expect(() => parseLcov(report().replace("LH:1", "LH:99"))).toThrow(/LH cannot exceed LF/);
    expect(() => parseLcov("TN:\nSF:src/example.ts\nLF:1\nLH:1\nend_of_record")).toThrow(
      /no DA data records/,
    );
    expect(() => parseLcov(report().replace("LF:2\nLH:1", "DA:1,1\nLF:3\nLH:2"))).toThrow(
      /duplicate DA line/,
    );
    expect(() => parseLcov(report({ terminate: false }))).toThrow(/unterminated/);
  });

  it("returns a failing result when a report is below its declared floor", () => {
    const parsed = parseLcov(report({ lineCounts: [1, 0, 0, 0] }));
    const result = evaluateCoverage(parsed, { lines: 50, branches: 0 });
    expect(result.ok).toBe(false);
    expect(result.failures).toEqual(["lines 25.00% is below required 50.00%"]);
  });

  it("passes complete reports and distinguishes floor failures from malformed input", async () => {
    const root = await scratchRoot();
    await writeFile(
      path.join(root, "coverage", "lcov.info"),
      report({
        source: "src/frontend.ts",
        lineCounts: Array.from({ length: 10 }, () => 1),
        branchHits: [1, 1],
      }),
    );
    await writeFile(
      path.join(root, "lcov.info"),
      report({ source: "src-tauri/src/lib.rs", lineCounts: [1, 1, 1, 1, 0], branchHits: [] }),
    );

    const passing = await runScript(["--root", root]);
    expect(passing.code).toBe(0);
    expect(passing.output).toContain("OK: coverage floors hold");

    await writeFile(
      path.join(root, "coverage", "lcov.info"),
      report({ source: "src/frontend.ts", lineCounts: [1, 0, 0, 0] }),
    );
    const belowFloor = await runScript(["--root", root]);
    expect(belowFloor.code).toBe(1);
    expect(belowFloor.output).toMatch(/Frontend lines 25\.00% is below required 90\.00%/);

    await writeFile(
      path.join(root, "coverage", "lcov.info"),
      report({ source: "src/frontend.ts", terminate: false }),
    );
    const malformed = await runScript(["--root", root]);
    expect(malformed.code).toBe(2);
    expect(malformed.output).toMatch(/Frontend invalid:.*unterminated/);
  });
});
