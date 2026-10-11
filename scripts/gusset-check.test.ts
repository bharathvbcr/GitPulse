import { describe, expect, it } from "vitest";
import { wallClockBudget } from "../src/lib/testing/wallClock";
import {
  CHECK_TIMEOUT_MS,
  ENV_SCRIPT_TIMEOUT_MS,
  goVersionOf,
  parseEnvLines,
  spawnBounded,
  workspaceFor,
} from "./gusset-check.mjs";

describe("gusset:check child processes", () => {
  it("kills a child that outlives its deadline and reports it as an error", () => {
    const started = Date.now();
    const result = spawnBounded("node", ["-e", "setTimeout(() => {}, 60000)"], { stdio: "ignore" }, 300);
    expect((result.error as NodeJS.ErrnoException | undefined)?.code).toBe("ETIMEDOUT");
    expect(Date.now() - started).toBeLessThan(wallClockBudget(10_000));
  });

  it("passes a child that finishes in time through unchanged", () => {
    const result = spawnBounded("node", ["-e", "process.stdout.write('A=1')"], { encoding: "utf8" }, 30_000);
    expect(result.error).toBeUndefined();
    expect(parseEnvLines(String(result.stdout))).toEqual({ A: "1" });
  });

  it("bounds both children with a finite deadline", () => {
    for (const ms of [ENV_SCRIPT_TIMEOUT_MS, CHECK_TIMEOUT_MS]) {
      expect(Number.isFinite(ms)).toBe(true);
      expect(ms).toBeGreaterThan(0);
    }
  });
});

describe("gusset:check checkout resolution", () => {
  const goDir = "/Code/devtools/GitPulse/go";

  it("overrides nothing when DevCouncil and gusset sit where go.mod points", () => {
    expect(
      workspaceFor({ goDir, devcouncil: "/Code/devtools/DevCouncil", gusset: "/Code/devtools/gusset", goVersion: "1.26.6" }),
    ).toBeNull();
  });

  it("points a nested worktree's go/ at the checkouts actually found, through go.work replaces", () => {
    const nested = "/Code/devtools/GitPulse/.claude/worktrees/lane/go";
    const work = workspaceFor({
      goDir: nested,
      devcouncil: "/Code/devtools/DevCouncil",
      gusset: "/Code/devtools/gusset",
      goVersion: "1.26.6",
    });
    expect(work).toBe(
      [
        "go 1.26.6",
        "",
        `use "${nested}"`,
        "",
        'replace github.com/bharathvbcr/DevCouncil/backend/go_orchestrator => "/Code/devtools/DevCouncil/backend/go_orchestrator"',
        'replace github.com/bharathvbcr/gusset => "/Code/devtools/gusset"',
        "",
      ].join("\n"),
    );
  });

  it("reads go.mod's go version and refuses a go.mod without one", () => {
    expect(goVersionOf("module m\n\ngo 1.26.6\n")).toBe("1.26.6");
    expect(() => goVersionOf("module m\n")).toThrow(/no go version/);
  });
});
