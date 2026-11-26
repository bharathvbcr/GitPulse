import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { REPO_ROOT, TempDirManager } from "./helpers/env";

interface RunnerOutput {
  exitCode: number;
  stdout: string;
  stderr: string;
}

interface Sandbox {
  tmpRoot: string;
  childTmpDir: string;
  runnerPath: string;
  sentinelFiles: string[];
  hashes: Map<string, string>;
}

function sha256File(filePath: string): string {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

function runRunner(sandbox: Sandbox, args: string[]): Promise<RunnerOutput> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [sandbox.runnerPath, ...args], {
      cwd: sandbox.tmpRoot,
      env: {
        ...process.env,
        TMPDIR: sandbox.childTmpDir,
        TEMP: sandbox.childTmpDir,
        TMP: sandbox.childTmpDir,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdoutChunks: Buffer[] = [];
    const stderrChunks: Buffer[] = [];
    child.stdout.on("data", (chunk: Buffer) => stdoutChunks.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderrChunks.push(chunk));
    child.once("error", reject);
    child.once("close", (code, signal) => {
      resolve({
        exitCode: code ?? (signal ? 1 : 0),
        stdout: Buffer.concat(stdoutChunks).toString("utf8"),
        stderr: Buffer.concat(stderrChunks).toString("utf8"),
      });
    });
  });
}

function assertSandboxClean(sandbox: Sandbox): void {
  for (const filePath of sandbox.sentinelFiles) {
    expect(sha256File(filePath), `Test file modified on disk: ${filePath}`).toBe(
      sandbox.hashes.get(filePath),
    );
  }
  expect(existsSync(path.join(sandbox.tmpRoot, "true"))).toBe(false);
  const leftoverTempDirs = readdirSync(sandbox.childTmpDir).filter((name) =>
    name.startsWith("gp-e2e-json-"),
  );
  expect(leftoverTempDirs).toEqual([]);
}

describe("E2E runner CLI (--list --json isolation & fail-closed guards)", () => {
  const temps = new TempDirManager();
  let sandbox: Sandbox;

  beforeEach(() => {
    const tmpRoot = temps.create("gp-e2e-runner-cli-");
    const childTmpDir = path.join(tmpRoot, "tmp");
    const e2eDir = path.join(tmpRoot, "tests", "e2e-r1-r4");
    mkdirSync(childTmpDir, { recursive: true });
    mkdirSync(e2eDir, { recursive: true });
    symlinkSync(path.join(REPO_ROOT, "node_modules"), path.join(tmpRoot, "node_modules"), "dir");

    const runnerPath = path.join(e2eDir, "run.mjs");
    copyFileSync(path.join(REPO_ROOT, "tests", "e2e-r1-r4", "run.mjs"), runnerPath);

    writeFileSync(
      path.join(e2eDir, "vitest.config.ts"),
      `import { defineConfig } from "vitest/config";
export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/e2e-r1-r4/**/*.test.ts"],
  },
});
`,
      "utf8",
    );

    const sentinels: Array<[string, string]> = [
      ["tier1-a.test.ts", "[Tier 1] [R1] sentinel a"],
      ["tier1-b.test.ts", "[Tier 1] [R2] sentinel b"],
      ["tier2-a.test.ts", "[Tier 2] [R1] sentinel a"],
      ["tier2-b.test.ts", "[Tier 2] [R4] sentinel b"],
      ["tier3-a.test.ts", "[Tier 3] [R3] sentinel a"],
      ["tier4-a.test.ts", "[Tier 4] [R2] sentinel a"],
    ];

    const sentinelFiles: string[] = [];
    const hashes = new Map<string, string>();
    for (const [name, suiteName] of sentinels) {
      const fullPath = path.join(e2eDir, name);
      writeFileSync(
        fullPath,
        `import { describe, expect, it } from "vitest";
describe(${JSON.stringify(suiteName)}, () => {
  it("passes", () => {
    expect(1).toBe(1);
  });
});
`,
        "utf8",
      );
      sentinelFiles.push(fullPath);
      hashes.set(fullPath, sha256File(fullPath));
    }

    sandbox = { tmpRoot, childTmpDir, runnerPath, sentinelFiles, hashes };
  });

  afterEach(() => {
    temps.cleanup();
  });

  it("lists tiers 1..4 and unfiltered --list --json without overwriting test files or creating ./true", async () => {
    const unfiltered = await runRunner(sandbox, ["--list", "--json"]);
    expect(unfiltered.exitCode, unfiltered.stderr).toBe(0);
    const parsedUnfiltered = JSON.parse(unfiltered.stdout) as Array<{ file: string; name: string }>;
    expect(Array.isArray(parsedUnfiltered)).toBe(true);
    expect(parsedUnfiltered).toHaveLength(6);
    for (const entry of parsedUnfiltered) {
      expect(entry.file).toMatch(/\.test\.ts$/);
      expect(entry.name).toMatch(/sentinel [ab] > passes$/);
    }
    assertSandboxClean(sandbox);

    const expected: Array<[string, number]> = [
      ["1", 2],
      ["2", 2],
      ["3", 1],
      ["4", 1],
    ];
    for (const [tier, count] of expected) {
      const res = await runRunner(sandbox, ["--tier", tier, "--list", "--json"]);
      expect(res.exitCode, res.stderr).toBe(0);
      const parsed = JSON.parse(res.stdout) as Array<{ file: string; name: string }>;
      expect(Array.isArray(parsed)).toBe(true);
      expect(parsed).toHaveLength(count);
      for (const entry of parsed) {
        expect(path.basename(entry.file)).toMatch(new RegExp(`^tier${tier}-`));
      }
      assertSandboxClean(sandbox);
    }
  });

  it("supports positional file filters and --json=true without mutating disk or creating ./true", async () => {
    const afterSep = await runRunner(sandbox, [
      "--list",
      "--json",
      "--",
      "tests/e2e-r1-r4/tier1-a.test.ts",
    ]);
    expect(afterSep.exitCode, afterSep.stderr).toBe(0);
    const parsedAfterSep = JSON.parse(afterSep.stdout) as Array<{ file: string }>;
    expect(parsedAfterSep).toHaveLength(1);
    expect(path.basename(parsedAfterSep[0]!.file)).toBe("tier1-a.test.ts");
    assertSandboxClean(sandbox);

    const beforeFlags = await runRunner(sandbox, [
      "tests/e2e-r1-r4/tier2-a.test.ts",
      "--list",
      "--json",
    ]);
    expect(beforeFlags.exitCode, beforeFlags.stderr).toBe(0);
    const parsedBeforeFlags = JSON.parse(beforeFlags.stdout) as Array<{ file: string }>;
    expect(parsedBeforeFlags).toHaveLength(1);
    expect(path.basename(parsedBeforeFlags[0]!.file)).toBe("tier2-a.test.ts");
    assertSandboxClean(sandbox);

    const jsonEqualsTrue = await runRunner(sandbox, ["--tier", "1", "--list", "--json=true"]);
    expect(jsonEqualsTrue.exitCode, jsonEqualsTrue.stderr).toBe(0);
    const parsedEqualsTrue = JSON.parse(jsonEqualsTrue.stdout) as Array<{ file: string }>;
    expect(parsedEqualsTrue).toHaveLength(2);
    assertSandboxClean(sandbox);
  });

  it("returns [] on unmatched -t in --list --json and fails closed on --json, --output-file, and --reporter bypasses", async () => {
    const emptyMatch = await runRunner(sandbox, [
      "--tier",
      "1",
      "--list",
      "--json",
      "-t",
      "NO_SUCH_TEST_XYZ",
    ]);
    expect(emptyMatch.exitCode, emptyMatch.stderr).toBe(0);
    expect(JSON.parse(emptyMatch.stdout)).toEqual([]);
    assertSandboxClean(sandbox);

    const bypassAfterSep = await runRunner(sandbox, [
      "--list",
      "--",
      "--json",
      "tests/e2e-r1-r4/tier1-a.test.ts",
    ]);
    expect(bypassAfterSep.exitCode).not.toBe(0);
    assertSandboxClean(sandbox);

    const badJsonArg = await runRunner(sandbox, [
      "--tier",
      "1",
      "--list",
      "--json=tests/e2e-r1-r4/tier1-a.test.ts",
    ]);
    expect(badJsonArg.exitCode).not.toBe(0);
    assertSandboxClean(sandbox);

    const blockedOutputCases: Array<{ fileName: string; buildArgs: (outPath: string) => string[] }> = [
      {
        fileName: "blocked-kebab-eq.json",
        buildArgs: (outPath) => ["--", `--output-file=${outPath}`],
      },
      {
        fileName: "blocked-kebab-space.json",
        buildArgs: (outPath) => ["--", "--output-file", outPath],
      },
      {
        fileName: "blocked-kebab-dotted.json",
        buildArgs: (outPath) => ["--", `--output-file.json=${outPath}`],
      },
      {
        fileName: "blocked-kebab-json.json",
        buildArgs: (outPath) => ["--json", "--", `--output-file=${outPath}`],
      },
      {
        fileName: "blocked-camel-eq.json",
        buildArgs: (outPath) => ["--", `--outputFile=${outPath}`],
      },
      {
        fileName: "blocked-camel-dotted.json",
        buildArgs: (outPath) => ["--", `--outputFile.json=${outPath}`],
      },
    ];
    for (const item of blockedOutputCases) {
      const blockedPath = path.join(sandbox.tmpRoot, item.fileName);
      const blockedRun = await runRunner(sandbox, item.buildArgs(blockedPath));
      expect(blockedRun.exitCode, item.fileName).not.toBe(0);
      expect(existsSync(blockedPath), `${item.fileName} must not exist on disk`).toBe(false);
      assertSandboxClean(sandbox);
    }

    for (const badReporterArgs of [
      ["--", "--reporter=verbose"],
      ["--", "--reporters=json"],
    ]) {
      const blockedReporter = await runRunner(sandbox, badReporterArgs);
      expect(blockedReporter.exitCode, badReporterArgs.join(" ")).not.toBe(0);
      assertSandboxClean(sandbox);
    }
  });
});
