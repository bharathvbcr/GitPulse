import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
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

describe("gusset:check GITPULSE_VENDOR_GIT_TIMEOUT_MS", () => {
  // Both checkouts point at directories that do not exist, so a run that is not
  // refused for the bound stops at the missing checkout, before `go` or bash.
  function run(bound: string) {
    const missing = join(tmpdir(), "gitpulse-no-such-checkout");
    return spawnSync(process.execPath, [fileURLToPath(new URL("./gusset-check.mjs", import.meta.url))], {
      encoding: "utf8",
      timeout: wallClockBudget(30_000),
      env: {
        ...process.env,
        GITPULSE_VENDOR_GIT_TIMEOUT_MS: bound,
        GITPULSE_DEVCOUNCIL_ROOT: missing,
        GITPULSE_GUSSET_ROOT: missing,
      },
    });
  }

  it.each(["0", "abc", "-5", "600001", "1.5", ""])("refuses %j with a FAIL line that names it, and exits 2", (bound) => {
    const result = run(bound);
    expect(result.stderr).toContain("FAIL: GITPULSE_VENDOR_GIT_TIMEOUT_MS=");
    expect(result.status).toBe(2);
  });

  it("does not refuse a usable bound", () => {
    const result = run("5000");
    expect(result.stderr).not.toContain("GITPULSE_VENDOR_GIT_TIMEOUT_MS");
    expect(result.stderr).toContain("is missing");
    expect(result.status).toBe(2);
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
        // The module directory is joined with the host's separator, as go.work
        // takes it on each platform; the checkout roots pass through as given.
        `replace github.com/bharathvbcr/DevCouncil/backend/go_orchestrator => ${JSON.stringify(join("/Code/devtools/DevCouncil", "backend", "go_orchestrator"))}`,
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
