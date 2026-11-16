import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  PATTERNS,
  TIMEZONES,
  VITEST_ENTRY,
  main,
  resolvePatternFiles,
  verifyZoneSupport,
} from "./test-timezones.mjs";

const tempDirs: string[] = [];

function scratchRepo(files: string[]): string {
  const dir = mkdtempSync(path.join(tmpdir(), "gp-tz-runner-"));
  tempDirs.push(dir);
  mkdirSync(path.join(dir, path.dirname(VITEST_ENTRY)), { recursive: true });
  writeFileSync(path.join(dir, VITEST_ENTRY), "// stub\n", "utf8");
  for (const rel of files) {
    const full = path.join(dir, rel);
    mkdirSync(path.dirname(full), { recursive: true });
    writeFileSync(full, "// test\n", "utf8");
  }
  return dir;
}

afterEach(() => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop()!;
    rmSync(dir, { recursive: true, force: true });
  }
});

describe("test-timezones configuration and ICU support", () => {
  it("covers America/New_York, Pacific/Auckland, and Asia/Kolkata", () => {
    expect(TIMEZONES).toContain("America/New_York");
    expect(TIMEZONES).toContain("Pacific/Auckland");
    expect(TIMEZONES).toContain("Asia/Kolkata");
  });

  it("includes the due-date, pulse metrics, and commit cadence globs and resolves real files", () => {
    expect(PATTERNS).toContain("src/lib/workbench/taskDue*.test.ts");
    expect(PATTERNS).toContain("src/lib/pulse/metrics*.test.ts");
    expect(PATTERNS).toContain("src/lib/metrics/commitCadence*.test.ts");

    const files = resolvePatternFiles();
    expect(files).toContain("src/lib/workbench/taskDue.test.ts");
    expect(files).toContain("src/lib/workbench/taskQuickAdd.test.ts");
    expect(files).toContain("src/lib/pulse/metrics.test.ts");
    expect(files).toContain("src/lib/metrics/commitCadence.test.ts");
  });

  it("fails closed when a pattern matches zero files or its directory is missing", () => {
    const emptyRepo = scratchRepo([]);
    expect(() => resolvePatternFiles(emptyRepo, ["src/lib/workbench/taskDue*.test.ts"])).toThrow(
      /Pattern directory missing/,
    );

    const noMatches = scratchRepo(["src/lib/workbench/unrelated.test.ts"]);
    expect(() => resolvePatternFiles(noMatches, ["src/lib/workbench/taskDue*.test.ts"])).toThrow(
      /Pattern matched no files/,
    );
  });

  it("applies every configured IANA timezone in a real Node child process", () => {
    for (const tz of TIMEZONES) {
      const result = verifyZoneSupport(tz);
      expect(result.ok, result.detail).toBe(true);
    }
  });

  it("rejects mismatched ICU resolved timezones", () => {
    const result = verifyZoneSupport("America/New_York", {
      spawn: (() => ({
        pid: 1,
        output: [],
        stdout: "UTC\n",
        stderr: "",
        status: 0,
        signal: null,
      })) as never,
    });
    expect(result.ok).toBe(false);
    expect(result.detail).toContain("got UTC");
  });
});

describe("test-timezones runner behavior", () => {
  const realFiles = resolvePatternFiles();

  it("runs every configured timezone even when an earlier timezone fails", () => {
    const repoRoot = scratchRepo(realFiles);
    const seenZones: string[] = [];
    const logs: string[] = [];
    const errors: string[] = [];

    const code = main([], {
      repoRoot,
      verifyZone: (tz) => ({ ok: true, detail: tz }),
      log: (...args) => void logs.push(args.join(" ")),
      error: (...args) => void errors.push(args.join(" ")),
      spawn: ((_cmd: string, _args: readonly string[], opts: { env?: NodeJS.ProcessEnv }) => {
        const tz = opts.env?.TZ ?? "";
        seenZones.push(tz);
        return {
          pid: 1,
          output: [],
          stdout: "",
          stderr: "",
          status: tz === "America/New_York" ? 1 : 0,
          signal: null,
        };
      }) as never,
    });

    expect(code).toBe(1);
    expect(seenZones).toEqual([...TIMEZONES]);
    expect(errors.some((line) => line.includes("America/New_York:failed"))).toBe(true);
  });

  it("returns exit 2 when ICU does not honor TZ rather than reporting a pass", () => {
    const repoRoot = scratchRepo(realFiles);
    const errors: string[] = [];
    let vitestCalled = false;

    const code = main([], {
      repoRoot,
      verifyZone: (tz) => ({ ok: false, detail: `ICU ignored ${tz}` }),
      log: () => {},
      error: (...args) => void errors.push(args.join(" ")),
      spawn: (() => {
        vitestCalled = true;
        return { pid: 1, output: [], stdout: "", stderr: "", status: 0, signal: null };
      }) as never,
    });

    expect(code).toBe(2);
    expect(vitestCalled).toBe(false);
    expect(errors.some((line) => line.includes("could not verify all timezones"))).toBe(true);
  });

  it("answers --help with exit 0 and rejects unknown flags with exit 2", () => {
    const logs: string[] = [];
    const errors: string[] = [];
    expect(main(["--help"], { log: (...a) => void logs.push(a.join(" ")) })).toBe(0);
    expect(logs.join("\n")).toContain("Usage:");
    expect(main(["--unknown"], { error: (...a) => void errors.push(a.join(" ")) })).toBe(2);
    expect(errors.join("\n")).toContain("FAIL:");
  });
});
