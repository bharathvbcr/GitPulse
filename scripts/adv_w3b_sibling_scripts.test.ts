// Adversary tests for lane W3b item 3: `GITPULSE_VENDOR_GIT_TIMEOUT_MS` in the
// two scripts that reach `findSibling`.
import { spawnSync } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, describe, expect, it } from "vitest";
import { wallClockBudget } from "../src/lib/testing/wallClock";

const SCRIPTS = path.dirname(fileURLToPath(import.meta.url));
const tempDirs: string[] = [];
afterAll(() => {
  for (const dir of tempDirs) rmSync(dir, { recursive: true, force: true });
});

function scratch(): string {
  const dir = realpathSync(mkdtempSync(path.join(tmpdir(), "adv-w3b-")));
  tempDirs.push(dir);
  return dir;
}

/** A `git` that records that it ran, then hangs like a stuck lock would. */
function fakeGit(dir: string): { bin: string; marker: string } {
  const bin = path.join(dir, "bin");
  mkdirSync(bin);
  const marker = path.join(dir, "git-ran");
  const git = path.join(bin, "git");
  writeFileSync(git, `#!/bin/sh\necho "$@" >> "${marker}"\nexec sleep 20\n`);
  chmodSync(git, 0o755);
  return { bin, marker };
}

const SCRIPT_NAMES = ["check-schema-parity.mjs", "gusset-check.mjs"] as const;

function run(script: (typeof SCRIPT_NAMES)[number], env: NodeJS.ProcessEnv, args: string[] = [], root = SCRIPTS) {
  return spawnSync(process.execPath, [path.join(root, script), ...args], {
    encoding: "utf8",
    timeout: wallClockBudget(40_000),
    env,
  });
}

const BAD = [
  "",
  "0",
  "-1",
  "600001",
  "1e3",
  " 30",
  "30 ",
  "30ms",
  "0x10",
  "0b1",
  "+30",
  ".5",
  "1_000",
  "Infinity",
  "NaN",
  "30\n",
  "٣٠",
  "３０",
  "9".repeat(400),
  "00000000030",
];

describe("a bad bound is refused identically by both siblings, before any other effect", () => {
  for (const script of SCRIPT_NAMES) {
    it.each(BAD)(`${script}: %j gives exactly one FAIL line naming the variable, exit 2, no stack, git never spawned`, (value) => {
      const dir = scratch();
      const { bin, marker } = fakeGit(dir);
      const missing = path.join(dir, "no-such-checkout");
      const result = run(script, {
        ...process.env,
        PATH: `${bin}${path.delimiter}${process.env.PATH}`,
        GITPULSE_VENDOR_GIT_TIMEOUT_MS: value,
        GITPULSE_DEVCOUNCIL_ROOT: missing,
        GITPULSE_GUSSET_ROOT: missing,
      });
      const lines = result.stderr.split("\n").filter((line) => line.startsWith("FAIL:"));
      expect(lines, result.stderr).toHaveLength(1);
      expect(lines[0]).toContain("GITPULSE_VENDOR_GIT_TIMEOUT_MS=");
      expect(result.status).toBe(2);
      expect(result.stderr).not.toMatch(/^\s+at /m);
      expect(result.stdout).toBe("");
      expect(existsSync(marker)).toBe(false);
    });
  }

  it.each(["1", "30", "0000030", "600000"])("a good bound %j is not refused by either script", (value) => {
    for (const script of SCRIPT_NAMES) {
      const dir = scratch();
      const missing = path.join(dir, "no-such-checkout");
      const result = run(script, {
        ...process.env,
        GITPULSE_VENDOR_GIT_TIMEOUT_MS: value,
        GITPULSE_DEVCOUNCIL_ROOT: missing,
        GITPULSE_GUSSET_ROOT: missing,
      });
      expect(result.stderr, `${script} ${value}`).not.toContain("GITPULSE_VENDOR_GIT_TIMEOUT_MS");
    }
  });

  it("an unset bound is the default and not refused", () => {
    for (const script of SCRIPT_NAMES) {
      const dir = scratch();
      const missing = path.join(dir, "no-such-checkout");
      const env: NodeJS.ProcessEnv = {
        ...process.env,
        GITPULSE_DEVCOUNCIL_ROOT: missing,
        GITPULSE_GUSSET_ROOT: missing,
      };
      delete env.GITPULSE_VENDOR_GIT_TIMEOUT_MS;
      expect(run(script, env).stderr, script).not.toContain("GITPULSE_VENDOR_GIT_TIMEOUT_MS");
    }
  });

  it("--help still wins over a bad bound in check-schema-parity", () => {
    const result = run("check-schema-parity.mjs", { ...process.env, GITPULSE_VENDOR_GIT_TIMEOUT_MS: "abc" }, ["--help"]);
    expect(result.status).toBe(0);
    expect(result.stderr).toBe("");
  });
});

describe("a git that hangs inside findSibling", () => {
  // The scripts are copied under a scratch root that has no sibling checkout
  // on any ancestor, so `findSibling` falls back to `git rev-parse`, which the
  // fake hangs. The bound of 300 ms must cut it, and report it as a FAIL.
  function copied(): { root: string; env: NodeJS.ProcessEnv; marker: string } {
    const dir = scratch();
    const root = path.join(dir, "checkout");
    mkdirSync(path.join(root, "scripts"), { recursive: true });
    for (const file of readdirSync(SCRIPTS)) {
      if (file.endsWith(".mjs")) cpSync(path.join(SCRIPTS, file), path.join(root, "scripts", file));
    }
    const { bin, marker } = fakeGit(dir);
    const env: NodeJS.ProcessEnv = {
      ...process.env,
      PATH: `${bin}${path.delimiter}${process.env.PATH}`,
      GITPULSE_VENDOR_GIT_TIMEOUT_MS: "300",
    };
    delete env.GITPULSE_DEVCOUNCIL_ROOT;
    delete env.GITPULSE_GUSSET_ROOT;
    return { root, env, marker };
  }

  for (const script of SCRIPT_NAMES) {
    it(`${script} reports the cut as a FAIL line with exit 2, as vendor-crates.mjs does`, () => {
      const { root, env, marker } = copied();
      const started = Date.now();
      const result = run(script, env, script === "check-schema-parity.mjs" ? ["--app", "/nonexistent.app"] : [], path.join(root, "scripts"));
      expect(existsSync(marker), `the fake git was never reached: ${result.stderr} ${result.stdout} ${root}`).toBe(true);
      expect(Date.now() - started).toBeLessThan(wallClockBudget(15_000));
      expect(result.stderr).toMatch(/^FAIL:/m);
      expect(result.status).toBe(2);
      expect(result.stderr).not.toMatch(/^\s+at /m);
    });
  }
});
