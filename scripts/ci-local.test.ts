import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { devNull, tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  EXIT_TREE_UNSTABLE,
  GATES,
  commandLine,
  execute,
  main,
  runGates,
  skipReason,
  summarize,
  workingTreeId,
} from "./ci-local.mjs";

/**
 * The local gate has to be told what actually runs remotely, or it drifts into
 * answering a different question than the one the developer is asking it.
 *
 * These derive the workflows' own commands rather than restating them, so a
 * step added to CI fails here instead of passing locally and failing on the
 * runner — which is the specific way this repository lost a release: the
 * WKWebView sweep was a CI step with no local counterpart, so the engine that
 * produced the only failure was the one engine nobody could run before pushing.
 */
const workflow = (name: string) =>
  readFileSync(new URL(`../.github/workflows/${name}`, import.meta.url), "utf8");

/**
 * package.json script names a workflow (or a command line) invokes: `bun run x`.
 * Bare `bun test` is Bun's own test runner, not the `test` script, so it is
 * deliberately not matched.
 */
function npmScripts(text: string): Set<string> {
  const found = new Set<string>();
  for (const match of text.matchAll(/\bbun run ([\w:.-]+)/g)) {
    found.add(match[1]);
  }
  return found;
}

const PUSH_WORKFLOWS = ["ci.yml", "coverage.yml"];
const ALL_WORKFLOWS = ["ci.yml", "coverage.yml", "release.yml"];

/** Every npm script this runner would invoke, directly or by declared cover. */
function gateScripts(): Set<string> {
  const found = new Set<string>();
  for (const gate of GATES) {
    if (gate.program === "bun") {
      found.add(gate.args[0] === "run" ? gate.args[1] : gate.args[0]);
    }
    for (const covered of gate.covers ?? []) {
      for (const script of npmScripts(covered)) found.add(script);
    }
  }
  return found;
}

describe("ci:local mirrors the workflows it stands in for", () => {
  it("runs every npm gate the push workflows run", () => {
    const remote = new Set<string>();
    for (const file of PUSH_WORKFLOWS) {
      for (const script of npmScripts(workflow(file))) remote.add(script);
    }
    // `bun install` is dependency installation, not a gate, and is not a
    // `bun run` script, so it never enters this set. Nothing is exempt.
    expect(remote.size).toBeGreaterThan(5);

    const local = gateScripts();
    const missing = [...remote].filter((script) => !local.has(script)).sort();
    expect(missing).toEqual([]);
  });

  it("claims no coverage of a step the workflows no longer run", () => {
    const everywhere = new Set<string>();
    for (const file of ALL_WORKFLOWS) {
      for (const script of npmScripts(workflow(file))) everywhere.add(script);
    }
    const claimed = new Set<string>();
    for (const gate of GATES) {
      for (const covered of gate.covers ?? []) {
        for (const script of npmScripts(covered)) claimed.add(script);
      }
    }
    expect(claimed.size).toBeGreaterThan(5);
    const stale = [...claimed].filter((script) => !everywhere.has(script)).sort();
    expect(stale).toEqual([]);
  });

  it("carries the Rust gates ci.yml runs, over the same manifest", () => {
    const ci = workflow("ci.yml");
    expect(ci).toContain("cargo fmt");
    expect(ci).toContain("cargo clippy");
    const cargo = GATES.filter((gate) => gate.program === "cargo").map(commandLine);
    expect(cargo.some((line) => line.startsWith("cargo fmt"))).toBe(true);
    expect(cargo.some((line) => line.startsWith("cargo clippy"))).toBe(true);
    expect(cargo.some((line) => line.includes("llvm-cov"))).toBe(true);
    const manifests = ["src-tauri/Cargo.toml", "crates/gitpulse-worktree/Cargo.toml"];
    for (const line of cargo) {
      expect(manifests.some((manifest) => line.includes(manifest))).toBe(true);
    }
  });

  it("builds and tests crates/gitpulse-worktree, which is not a src-tauri workspace member", () => {
    // Before these gates nothing in CI or ci:local built the zero-copy build cache.
    const manifest = "crates/gitpulse-worktree/Cargo.toml";
    const crate = GATES.filter((gate) => gate.program === "cargo")
      .map(commandLine)
      .filter((line) => line.includes(manifest));
    for (const verb of ["fmt", "clippy", "test"]) {
      expect(crate.some((line) => line.startsWith(`cargo ${verb} `))).toBe(true);
    }
    // Locked, so the crate's own Cargo.lock is what CI resolves.
    for (const line of crate.filter((l) => !l.startsWith("cargo fmt"))) {
      expect(line).toContain("--locked");
    }
    // Each gate states the exact workflow step it covers, and ci.yml still has it.
    const ci = workflow("ci.yml");
    for (const gate of GATES.filter((g) => commandLine(g).includes(manifest))) {
      for (const covered of gate.covers ?? []) expect(ci).toContain(covered);
    }
  });

  it("runs the WKWebView sweep CI runs, and only where it can", () => {
    expect(workflow("ci.yml")).toContain("bun run test:webkit:all");
    const webkit = GATES.find((gate) => gate.id === "webkit-regressions");
    expect(webkit).toBeDefined();
    expect(webkit?.onlyOn).toBe("darwin");
    expect(skipReason(webkit!, "darwin")).toBeNull();
    expect(skipReason(webkit!, "linux")).toMatch(/needs darwin/);
  });

  it("gates vendored crates on local edits, never on upstream drift alone", () => {
    // Bare `--check` fails whenever DevCouncil commits, with no change here,
    // so it is the post-refresh question, not a gate. CI and this runner both
    // run the edit check, through the same script so they cannot diverge.
    const scripts = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"))
      .scripts as Record<string, string>;
    expect(scripts["check:vendor-edits"]).toBe("node scripts/vendor-crates.mjs --check --allow-drift");
    expect(scripts["vendor:check"]).toBe("node scripts/vendor-crates.mjs --check");
    expect(workflow("ci.yml")).toContain("bun run check:vendor-edits");
    const gate = GATES.find((g) => g.id === "vendor-edits");
    expect(gate && commandLine(gate)).toBe("bun run check:vendor-edits");
    expect(gate?.local).toBeUndefined();
  });

  it("gates the design rules in strict mode, through a package.json script that exists", () => {
    const gate = GATES.find((g) => g.id === "design-lint");
    expect(gate).toBeDefined();
    expect(commandLine(gate!)).toBe("bun run design:gate");
    const scripts = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")).scripts as Record<string, string>;
    expect(scripts["design:gate"]).toBe("node scripts/design-lint.mjs --strict");
    // A failing design gate fails the run, like any other gate.
    const outcomes = runGates(GATES, { run: (g) => ({ ok: g.id !== "design-lint", detail: "", ms: 0 }), platform: "darwin", log: () => {} });
    expect(outcomes.filter((o) => o.status === "failed").map((o) => o.gate.id)).toEqual(["design-lint"]);
  });

  it("passes the flags that stop one failing test binary hiding the rest", () => {
    // ci.yml spells out why: without --no-fail-fast cargo stops at the first
    // failing test binary, so the rest never run and the job reports one
    // failure when there may be several.
    const rust = GATES.find((gate) => gate.id === "cargo-tests");
    expect(commandLine(rust!)).toContain("--no-fail-fast");
    expect(commandLine(rust!)).toContain("--locked");
    expect(workflow("ci.yml")).toContain("--no-fail-fast");
  });
});

describe("a failing gate does not hide the ones after it", () => {
  const stub = (failing: string[]) => (gate: { id: string }) => ({
    ok: !failing.includes(gate.id),
    detail: failing.includes(gate.id) ? "exit 1" : "exit 0",
    ms: 1,
  });

  it("runs every later gate after one fails", () => {
    const ran: string[] = [];
    const outcomes = runGates(GATES, {
      platform: "darwin",
      log: () => {},
      run: (gate) => {
        ran.push(gate.id);
        return stub(["ipc-contract"])(gate);
      },
    });
    expect(ran).toEqual(GATES.map((gate) => gate.id));
    expect(outcomes.filter((o) => o.status === "failed").map((o) => o.gate.id)).toEqual([
      "ipc-contract",
    ]);
    expect(outcomes.filter((o) => o.status === "passed")).toHaveLength(GATES.length - 1);
  });

  it("reports every failure in one run, not one per push", () => {
    const outcomes = runGates(GATES, {
      platform: "darwin",
      log: () => {},
      run: stub(["ipc-contract", "cargo-clippy", "build"]),
    });
    const failed = outcomes.filter((o) => o.status === "failed").map((o) => o.gate.id).sort();
    expect(failed).toEqual(["build", "cargo-clippy", "ipc-contract"]);
    const report = summarize(outcomes);
    for (const id of failed) {
      expect(report).toContain(commandLine(GATES.find((gate) => gate.id === id)!));
    }
    expect(report).toContain("3 failed");
  });
});

describe("a gate that could not run never reads like one that passed", () => {
  it("marks the WKWebView sweep skipped off macOS and names it in the summary", () => {
    const outcomes = runGates(GATES, {
      platform: "linux",
      log: () => {},
      run: () => ({ ok: true, detail: "exit 0", ms: 1 }),
    });
    const webkit = outcomes.find((o) => o.gate.id === "webkit-regressions");
    expect(webkit?.status).toBe("skipped");
    expect(webkit?.status).not.toBe("passed");

    const report = summarize(outcomes);
    expect(report).toContain("SKIP");
    expect(report).toContain("1 skipped");
    // Counted is not enough: the reader has to see which coverage they lost.
    expect(report).toContain("not run here: Native renderer regressions (WKWebView)");
  });

  it("counts a skip apart from a pass in the totals", () => {
    const outcomes = runGates(GATES, {
      platform: "linux",
      log: () => {},
      run: () => ({ ok: true, detail: "exit 0", ms: 1 }),
    });
    const report = summarize(outcomes);
    expect(report).toContain(`${GATES.length - 1} passed`);
    expect(report).not.toContain(`${GATES.length} passed`);
  });
});

describe("a report left by an earlier run is not read as this run's", () => {
  const withRoot = (body: (root: string) => void) => {
    const root = mkdtempSync(join(tmpdir(), "ci-local-reports-"));
    try {
      body(root);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  };
  const plant = (root: string, report: string) => {
    mkdirSync(join(root, report, ".."), { recursive: true });
    writeFileSync(join(root, report), "TN:\nend_of_record\n");
  };

  it("removes a gate's reports before it runs, so a run that writes none leaves none for the floor", () => {
    withRoot((root) => {
      plant(root, "lcov.info");
      plant(root, join("coverage", "lcov.info"));
      const seenByFloor: boolean[] = [];
      runGates(GATES, {
        platform: "darwin",
        log: () => {},
        root,
        // Every gate "fails" without writing, as cargo llvm-cov does when a test fails.
        run: (gate) => {
          if (gate.id === "coverage-floor") {
            seenByFloor.push(existsSync(join(root, "lcov.info")), existsSync(join(root, "coverage", "lcov.info")));
          }
          return { ok: gate.id === "coverage-floor", detail: "exit 101", ms: 1 };
        },
      });
      expect(seenByFloor).toEqual([false, false]);
    });
  });

  it("keeps the report the gate itself writes", () => {
    withRoot((root) => {
      plant(root, "lcov.info");
      runGates(GATES, {
        platform: "darwin",
        log: () => {},
        root,
        run: (gate) => {
          if (gate.id === "cargo-tests") plant(root, "lcov.info");
          return { ok: true, detail: "exit 0", ms: 1 };
        },
      });
      expect(existsSync(join(root, "lcov.info"))).toBe(true);
    });
  });

  it("declares, before the floor, every report check:coverage reads", () => {
    const floor = GATES.findIndex((gate) => gate.id === "coverage-floor");
    const produced = GATES.slice(0, floor).flatMap((gate) => gate.produces ?? []);
    // scripts/check-coverage-floor.mjs's defaults.
    expect(produced).toEqual(expect.arrayContaining(["coverage/lcov.info", "lcov.info"]));
  });

  it("leaves files alone when no root is given", () => {
    withRoot((root) => {
      plant(root, "lcov.info");
      runGates(GATES, { platform: "darwin", log: () => {}, run: () => ({ ok: true, detail: "exit 0", ms: 1 }) });
      expect(existsSync(join(root, "lcov.info"))).toBe(true);
    });
  });
});

/**
 * Real throwaway repositories, with Git isolated from this machine's config and
 * from any GIT_* variables the test runner inherited (a hook would set GIT_DIR).
 */
const sandboxes: string[] = [];
afterEach(() => {
  for (const dir of sandboxes.splice(0)) rmSync(dir, { recursive: true, force: true });
});
const sandbox = () => {
  const dir = mkdtempSync(join(tmpdir(), "ci-local-tree-test-"));
  sandboxes.push(dir);
  return dir;
};
const isolatedEnv = (): NodeJS.ProcessEnv => {
  const env: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (!key.startsWith("GIT_")) env[key] = value;
  }
  return { ...env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: devNull };
};
const git = (cwd: string, ...args: string[]): string => {
  const result = spawnSync(
    "git",
    ["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false", ...args],
    { cwd, env: isolatedEnv(), encoding: "utf8" },
  );
  if (result.status !== 0) throw new Error(`git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout;
};
const makeRepo = () => {
  const repo = join(sandbox(), "repo");
  mkdirSync(repo);
  git(repo, "init", "-q");
  writeFileSync(join(repo, "tracked.txt"), "one\n");
  writeFileSync(join(repo, ".gitignore"), "ignored.log\n");
  git(repo, "add", "tracked.txt", ".gitignore");
  git(repo, "commit", "-q", "-m", "first");
  return repo;
};
const readTree = (repo: string, extra: { tmpRoot?: string; run?: never } = {}) =>
  workingTreeId(repo, { env: isolatedEnv(), ...extra });
const idOf = (repo: string): string => {
  const reading = readTree(repo);
  if (!reading.ok) throw new Error(reading.reason);
  return reading.id;
};

describe("workingTreeId reads the tree that is on disk", () => {
  it("is the same on two calls when nothing changed", () => {
    const repo = makeRepo();
    expect(idOf(repo)).toBe(idOf(repo));
    expect(idOf(repo)).toMatch(/^[0-9a-f]{40,64}$/);
  });

  it("equals HEAD's tree on a clean checkout", () => {
    const repo = makeRepo();
    expect(idOf(repo)).toBe(git(repo, "rev-parse", "HEAD^{tree}").trim());
  });

  it("changes when a tracked file is edited", () => {
    const repo = makeRepo();
    const before = idOf(repo);
    writeFileSync(join(repo, "tracked.txt"), "two\n");
    expect(idOf(repo)).not.toBe(before);
  });

  it("changes when an untracked file appears", () => {
    const repo = makeRepo();
    const before = idOf(repo);
    writeFileSync(join(repo, "new.txt"), "x\n");
    expect(idOf(repo)).not.toBe(before);
  });

  it("does not change when only an ignored file appears", () => {
    const repo = makeRepo();
    const before = idOf(repo);
    writeFileSync(join(repo, "ignored.log"), "noise\n");
    expect(idOf(repo)).toBe(before);
  });

  it("works with no index to copy, building one from the files", () => {
    const repo = join(sandbox(), "fresh");
    mkdirSync(repo);
    git(repo, "init", "-q");
    writeFileSync(join(repo, "a.txt"), "a\n");
    expect(existsSync(join(repo, ".git", "index"))).toBe(false);
    const first = idOf(repo);
    expect(first).toMatch(/^[0-9a-f]{40,64}$/);
    expect(existsSync(join(repo, ".git", "index"))).toBe(false);
    writeFileSync(join(repo, "a.txt"), "b\n");
    expect(idOf(repo)).not.toBe(first);
  });

  it("leaves the real index and git status exactly as they were", () => {
    const repo = makeRepo();
    writeFileSync(join(repo, "tracked.txt"), "edited\n");
    writeFileSync(join(repo, "new.txt"), "x\n");
    git(repo, "add", "new.txt");
    writeFileSync(join(repo, "untracked.txt"), "u\n");
    const status = () => git(repo, "status", "--porcelain=v2", "--untracked-files=all");
    status(); // let git refresh its own stat cache once, so the next two reads are comparable
    const indexBefore = readFileSync(join(repo, ".git", "index"));
    const headBefore = git(repo, "rev-parse", "HEAD");
    const statusBefore = status();
    idOf(repo);
    expect(readFileSync(join(repo, ".git", "index")).equals(indexBefore)).toBe(true);
    expect(status()).toBe(statusBefore);
    expect(git(repo, "rev-parse", "HEAD")).toBe(headBefore);
  });

  it("removes its temporary index afterwards", () => {
    const repo = makeRepo();
    const scratch = sandbox();
    expect(readTree(repo, { tmpRoot: scratch }).ok).toBe(true);
    expect(readdirSync(scratch)).toEqual([]);
  });

  it("removes its temporary index when a step fails, and reports unknown", () => {
    const repo = makeRepo();
    const scratch = sandbox();
    const reading = workingTreeId(repo, {
      env: isolatedEnv(),
      tmpRoot: scratch,
      run: (args: string[], where: { cwd: string; env: NodeJS.ProcessEnv }) => {
        if (args.includes("write-tree")) {
          // The private index exists by now; the failure must not leave it behind.
          expect(readdirSync(scratch)).toHaveLength(1);
          return { status: 128, stdout: "", stderr: "fatal: forced\n", error: undefined };
        }
        const real = spawnSync("git", args, { cwd: where.cwd, env: where.env, encoding: "utf8" });
        return { status: real.status, stdout: real.stdout, stderr: real.stderr, error: undefined };
      },
    });
    expect(reading.ok).toBe(false);
    expect(reading.ok === false && reading.reason).toContain("write-tree");
    expect(readdirSync(scratch)).toEqual([]);
  });

  it("removes its temporary index when the program cannot be started", () => {
    const repo = makeRepo();
    const scratch = sandbox();
    const reading = workingTreeId(repo, {
      env: isolatedEnv(),
      tmpRoot: scratch,
      run: (args: string[], where: { cwd: string; env: NodeJS.ProcessEnv }) => {
        if (args.includes("add")) throw new Error("spawn exploded");
        const real = spawnSync("git", args, { cwd: where.cwd, env: where.env, encoding: "utf8" });
        return { status: real.status, stdout: real.stdout, stderr: real.stderr, error: undefined };
      },
    });
    expect(reading).toEqual({ ok: false, reason: "spawn exploded" });
    expect(readdirSync(scratch)).toEqual([]);
  });

  it("is unknown outside a repository, never an id", () => {
    const outside = sandbox();
    const scratch = sandbox();
    const reading = workingTreeId(outside, {
      env: { ...isolatedEnv(), GIT_CEILING_DIRECTORIES: tmpdir() },
      tmpRoot: scratch,
    });
    expect(reading.ok).toBe(false);
    expect("id" in reading).toBe(false);
    expect(readdirSync(scratch)).toEqual([]);
  });

  it("is unknown for a directory that does not exist", () => {
    const missing = join(sandbox(), "nope");
    expect(workingTreeId(missing, { env: isolatedEnv() }).ok).toBe(false);
  });
});

describe("the summary says which tree was tested", () => {
  const good = (id: string) => ({ ok: true as const, id });
  const bad = (reason: string) => ({ ok: false as const, reason });
  const readings = (...all: Array<ReturnType<typeof good> | ReturnType<typeof bad>>) => {
    let next = 0;
    return () => all[next++];
  };
  const passing = () => ({ ok: true, detail: "exit 0", ms: 1 });
  const A = "a".repeat(40);
  const B = "b".repeat(40);
  const run = (options: Parameters<typeof execute>[0]) => {
    const lines: string[] = [];
    const code = execute({ platform: "darwin", run: passing, log: (line: string) => lines.push(line), ...options });
    const text = lines.join("\n");
    const last = text.split("\n").filter((line) => line.trim() !== "").at(-1);
    return { code, text, last };
  };

  it("names the tree when both readings agree", () => {
    const { text, code } = run({ readTree: readings(good(A), good(A)) });
    expect(code).toBe(0);
    expect(text).toContain(`tested tree: ${A}`);
    expect(text).not.toContain("CHANGED");
  });

  it("gives both ids and says the result describes neither tree when the tree moved", () => {
    const { text, last } = run({ readTree: readings(good(A), good(B)) });
    expect(text).toContain("TREE CHANGED DURING THE RUN");
    expect(text).toContain(`at start: ${A}`);
    expect(text).toContain(`at end:   ${B}`);
    expect(text).toContain("The working tree changed while the gates ran, so this result does not describe either tree.");
    expect(text).not.toContain("tested tree:");
    // A reader who sees only the last line cannot take it for a clean pass.
    expect(last).toMatch(/INCONCLUSIVE, not a clean pass/);
  });

  it("says the tree could not be read, and why, rather than omitting the line", () => {
    const first = run({ readTree: readings(bad("not a git repository"), good(A)) });
    expect(first.text).toContain("tree: could not be read");
    expect(first.text).toContain("before the first gate: not a git repository");
    const second = run({ readTree: readings(good(A), bad("git add failed: boom")) });
    expect(second.text).toContain("after the last gate: git add failed: boom");
    const both = run({ readTree: readings(bad("x"), bad("y")) });
    expect(both.text).toContain("before the first gate: x");
    expect(both.text).toContain("after the last gate: y");
  });

  it("keeps summarize(outcomes) alone free of tree lines", () => {
    const outcomes = runGates(GATES, { platform: "darwin", log: () => {}, run: passing });
    expect(summarize(outcomes)).not.toMatch(/tested tree|TREE CHANGED|tree: could not/);
  });
});

describe("exit codes with and without --strict-tree", () => {
  const good = (id: string) => ({ ok: true as const, id });
  const A = "a".repeat(40);
  const B = "b".repeat(40);
  const pass = () => ({ ok: true, detail: "exit 0", ms: 1 });
  const failing = (id: string) => (gate: { id: string }) => ({ ok: gate.id !== id, detail: "exit 1", ms: 1 });
  const seq = (...all: Array<{ ok: boolean; id?: string; reason?: string }>) => {
    let next = 0;
    return () => all[next++] as ReturnType<typeof good>;
  };
  const base = { platform: "darwin" as const, log: () => {} };

  it("default: 0 on a pass and 1 on a failure, whatever the tree did", () => {
    for (const trees of [
      [good(A), good(A)],
      [good(A), good(B)],
      [{ ok: false, reason: "no" }, good(A)],
    ]) {
      expect(execute({ ...base, run: pass, readTree: seq(...trees) })).toBe(0);
      expect(execute({ ...base, run: failing("build"), readTree: seq(...trees) })).toBe(1);
    }
  });

  it("--strict-tree: unchanged tree keeps 0 and 1", () => {
    expect(execute({ ...base, strictTree: true, run: pass, readTree: seq(good(A), good(A)) })).toBe(0);
    expect(execute({ ...base, strictTree: true, run: failing("build"), readTree: seq(good(A), good(A)) })).toBe(1);
  });

  it("--strict-tree: a changed or unreadable tree exits with a code distinct from a gate failure", () => {
    expect(EXIT_TREE_UNSTABLE).not.toBe(0);
    expect(EXIT_TREE_UNSTABLE).not.toBe(1);
    expect(EXIT_TREE_UNSTABLE).not.toBe(2);
    expect(execute({ ...base, strictTree: true, run: pass, readTree: seq(good(A), good(B)) })).toBe(EXIT_TREE_UNSTABLE);
    expect(
      execute({ ...base, strictTree: true, run: pass, readTree: seq({ ok: false, reason: "no" }, good(A)) }),
    ).toBe(EXIT_TREE_UNSTABLE);
    expect(
      execute({ ...base, strictTree: true, run: pass, readTree: seq(good(A), { ok: false, reason: "no" }) }),
    ).toBe(EXIT_TREE_UNSTABLE);
  });

  it("--strict-tree: a gate failure still exits 1 when the tree also moved", () => {
    expect(execute({ ...base, strictTree: true, run: failing("build"), readTree: seq(good(A), good(B)) })).toBe(1);
  });

  it("main accepts --strict-tree, documents it, and still rejects other options", () => {
    const logged: string[] = [];
    const realLog = console.log;
    const realError = console.error;
    console.log = (line: string) => logged.push(String(line));
    console.error = () => {};
    try {
      expect(main(["--help"])).toBe(0);
      expect(main(["--bogus"])).toBe(2);
    } finally {
      console.log = realLog;
      console.error = realError;
    }
    const help = logged.join("\n");
    expect(help).toContain("--strict-tree");
    expect(help).toMatch(/Exit codes:.*3/);
    const header = readFileSync(new URL("./ci-local.mjs", import.meta.url), "utf8").split("*/")[0];
    expect(header).toContain("--strict-tree");
    expect(header).toMatch(/Exit codes:.*3/s);
  });
});

describe("Tested-tree: appears only for a complete, passing, stable run", () => {
  const A = "c".repeat(40);
  const B = "d".repeat(40);
  const stable = () => ({ ok: true as const, id: A });
  const pass = () => ({ ok: true, detail: "exit 0", ms: 1 });
  const capture = (options: Parameters<typeof execute>[0]) => {
    const lines: string[] = [];
    execute({ platform: "darwin", run: pass, log: (line: string) => lines.push(line), ...options });
    return lines.join("\n");
  };
  const evidence = /^Tested-tree: /m;

  it("is the last line, with the tree id, when every gate ran and passed on a stable tree", () => {
    const text = capture({ readTree: stable });
    expect(text).toMatch(evidence);
    expect(text.trimEnd().split("\n").at(-1)).toBe(`Tested-tree: ${A}`);
  });

  it("is absent when a gate failed", () => {
    expect(capture({ readTree: stable, run: (g: { id: string }) => ({ ok: g.id !== "build", detail: "exit 1", ms: 1 }) })).not.toMatch(evidence);
  });

  it("is absent when a gate was skipped", () => {
    expect(capture({ readTree: stable, platform: "linux" })).not.toMatch(evidence);
  });

  it("is absent when only some of the gates were given", () => {
    expect(capture({ readTree: stable, gates: GATES.slice(0, 3) })).not.toMatch(evidence);
  });

  it("is absent when the tree changed or could not be read", () => {
    let n = 0;
    expect(capture({ readTree: () => ({ ok: true as const, id: n++ === 0 ? A : B }) })).not.toMatch(evidence);
    let m = 0;
    expect(
      capture({ readTree: () => (m++ === 0 ? { ok: true as const, id: A } : { ok: false as const, reason: "x" }) }),
    ).not.toMatch(evidence);
  });
});
