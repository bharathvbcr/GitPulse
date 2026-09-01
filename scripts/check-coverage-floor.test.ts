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

  it("rejects forged summaries, missing data, duplicate lines, and truncation", () => {
    expect(() => parseLcov(report({ lineCounts: [1, 0] }).replace("LH:1", "LH:0"))).toThrow(
      /LH cannot be below DA hit count/,
    );
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
