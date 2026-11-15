import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { GATES, cargoMetadata, commandLine, main, productionScratchContext, runGates, skipReason, summarize } from "./ci-local.mjs";

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
 * `cargo llvm-cov nextest` leaves ~50 GiB in `<target>/llvm-cov-target`. The
 * runner removes it itself, once the last gate that reads what the test gate
 * produced has run. These tests drive `runGates` over a temp repository and
 * fake gates, so nothing here builds, or deletes, anything real.
 */

const KEEP = "CI_LOCAL_KEEP_COVERAGE_BUILD";
const MANIFEST = "src-tauri/Cargo.toml";

type Gate = (typeof GATES)[number];
type Outcome = ReturnType<typeof runGates>[number];
const gate = (id: string, extra: Partial<Gate> = {}): Gate => ({
  id,
  name: id,
  program: "fake",
  args: [],
  ...extra,
});
const OWNER = gate("owner", {
  produces: ["lcov.info"],
  scratch: [{ kind: "cargo-llvm-cov", manifest: MANIFEST }],
});
const READER = gate("reader", { reads: ["lcov.info"] });
const OTHER = gate("other");

const ok = { ok: true, detail: "exit 0", ms: 1 };
const bad = { ok: false, detail: "exit 101", ms: 1 };

let root: string;
let outside: string;
const cleanups: string[] = [];

beforeEach(() => {
  root = realpathSync(mkdtempSync(join(tmpdir(), "ci-local-scratch-root-")));
  outside = realpathSync(mkdtempSync(join(tmpdir(), "ci-local-scratch-out-")));
  cleanups.push(root, outside);
});
afterEach(() => {
  for (const dir of cleanups.splice(0)) rmSync(dir, { recursive: true, force: true });
});

const plant = (dir: string, files: Record<string, string> = { "debug/deps/x.rlib": "x".repeat(4096) }) => {
  for (const [name, body] of Object.entries(files)) {
    mkdirSync(join(dir, name, ".."), { recursive: true });
    writeFileSync(join(dir, name), body);
  }
};
const defaultDir = () => join(root, "src-tauri", "target", "llvm-cov-target");

/** What `cargo metadata` would say, for a repo whose target dir is src-tauri/target. */
const metadata = (targetDirectory = join(root, "src-tauri", "target"), buildDirectory = targetDirectory) =>
  () => ({ targetDirectory, buildDirectory });

const scratchOptions = (env: Record<string, string> = {}, cargoMetadata = metadata()) => ({
  env,
  cargoMetadata,
  home: join(outside, "home"),
});

const run = (
  gates: Gate[],
  options: {
    env?: Record<string, string>;
    cargoMetadata?: ReturnType<typeof metadata>;
    results?: Record<string, typeof ok>;
    onRun?: (id: string) => void;
  } = {},
) => {
  const lines: string[] = [];
  const outcomes = runGates(gates, {
    platform: "darwin",
    root,
    log: (line: string) => lines.push(line),
    scratch: scratchOptions(options.env, options.cargoMetadata),
    run: (g) => {
      options.onRun?.(g.id);
      return options.results?.[g.id] ?? ok;
    },
  });
  return { outcomes, lines, report: summarize(outcomes) };
};

const firstCleanup = (outcomes: Outcome[]) => {
  const [found] = outcomes.flatMap((o) => o.cleanups ?? []);
  if (found === undefined) throw new Error("no cleanup was recorded on any outcome");
  return found;
};

describe("the coverage build is removed once nothing needs it", () => {
  it("keeps it through the gate that reads the report, then deletes it, leaving lcov.info", () => {
    const seen: Record<string, boolean[]> = {};
    run([OWNER, OTHER, READER, OTHER].map((g, i) => (i === 3 ? gate("after") : g)), {
      onRun: (id) => {
        if (id === "owner") {
          plant(defaultDir());
          writeFileSync(join(root, "lcov.info"), "TN:\nend_of_record\n");
        }
        seen[id] = [existsSync(defaultDir()), existsSync(join(root, "lcov.info"))];
      },
    });
    // The reader still sees both; the next gate sees no build directory and the report.
    expect(seen.reader).toEqual([true, true]);
    expect(seen.other).toEqual([true, true]);
    expect(seen.after).toEqual([false, true]);
    expect(existsSync(defaultDir())).toBe(false);
    expect(readFileSync(join(root, "lcov.info"), "utf8")).toContain("end_of_record");
  });

  it("deletes it right after its gate when no later gate reads that gate's reports", () => {
    const seen: Record<string, boolean> = {};
    run([OWNER, gate("next")], {
      onRun: (id) => {
        if (id === "owner") plant(defaultDir());
        seen[id] = existsSync(defaultDir());
      },
    });
    expect(seen.next).toBe(false);
  });

  it("deletes it when the gate that wrote it failed, and when the reader failed", () => {
    run([OWNER, READER], {
      results: { owner: bad, reader: bad },
      onRun: (id) => id === "owner" && plant(defaultDir()),
    });
    expect(existsSync(defaultDir())).toBe(false);
  });

  it("deletes it when a later gate throws and the run is abandoned", () => {
    expect(() =>
      run([OWNER, READER, gate("boom")], {
        onRun: (id) => {
          if (id === "owner") plant(defaultDir());
          if (id === "boom") throw new Error("runner blew up");
        },
      }),
    ).toThrow("runner blew up");
    expect(existsSync(defaultDir())).toBe(false);
  });

  it("deletes it when the run is cut short before the reader ever runs", () => {
    expect(() =>
      run([OWNER, gate("boom"), READER], {
        onRun: (id) => {
          if (id === "owner") plant(defaultDir());
          if (id === "boom") throw new Error("interrupted");
        },
      }),
    ).toThrow("interrupted");
    expect(existsSync(defaultDir())).toBe(false);
  });

  it("does nothing when the gate was skipped, so nothing it did not make is deleted", () => {
    plant(defaultDir());
    const wrongPlatform: Gate = { ...OWNER, onlyOn: "linux" };
    runGates([wrongPlatform], {
      platform: "darwin",
      root,
      log: () => {},
      scratch: scratchOptions(),
      run: () => ok,
    });
    expect(existsSync(defaultDir())).toBe(true);
  });

  it("does nothing unless the caller opts in with a scratch context", () => {
    plant(defaultDir());
    runGates([OWNER, READER], { platform: "darwin", root, log: () => {}, run: () => ok });
    expect(existsSync(defaultDir())).toBe(true);
  });

  it("reports what it removed and how much, on the gate that owned it and in the summary", () => {
    const { outcomes, lines, report } = run([OWNER, READER], {
      onRun: (id) => id === "owner" && plant(defaultDir(), { "a.bin": "x".repeat(10_000), "b/c.bin": "y".repeat(20_000) }),
    });
    const done = firstCleanup(outcomes);
    expect(done.status).toBe("removed");
    expect(done.dir).toBe(defaultDir());
    expect(done.bytes ?? 0).toBeGreaterThanOrEqual(30_000);
    expect(report).toMatch(/removed .*llvm-cov-target/);
    expect(report).toMatch(/freed/i);
    expect(lines.join("\n")).toMatch(/removed/);
    // Cleanup is not a gate: the gate list and its counts are unchanged.
    expect(outcomes).toHaveLength(2);
    expect(report).toContain("2 passed");
  });

  it("says plainly when there was nothing to remove", () => {
    const { outcomes, report } = run([OWNER]);
    expect(firstCleanup(outcomes).status).toBe("absent");
    expect(report).toMatch(/nothing to remove/i);
  });
});

describe("keeping the build for an incremental rebuild", () => {
  it(`${KEEP} leaves the directory and says it was kept`, () => {
    const { outcomes, report } = run([OWNER, READER], {
      env: { [KEEP]: "1" },
      onRun: (id) => id === "owner" && plant(defaultDir()),
    });
    expect(existsSync(defaultDir())).toBe(true);
    expect(firstCleanup(outcomes).status).toBe("kept");
    expect(report).toContain(KEEP);
  });

  it.each(["0", "false", ""])("treats %j as not asking to keep it", (value) => {
    run([OWNER], { env: { [KEEP]: value }, onRun: (id) => id === "owner" && plant(defaultDir()) });
    expect(existsSync(defaultDir())).toBe(false);
  });
});

describe("the directory deleted is the one the coverage build used", () => {
  it("follows CARGO_LLVM_COV_TARGET_DIR, relative to the repository, and leaves the default alone", () => {
    const moved = join(root, "elsewhere", "cov");
    const { outcomes } = run([OWNER], {
      env: { CARGO_LLVM_COV_TARGET_DIR: "elsewhere/cov" },
      onRun: () => {
        plant(moved, { "CACHEDIR.TAG": "Signature: 8a477f597d28d172789f06886806bc55\n", "debug/x": "x" });
        plant(defaultDir());
      },
    });
    expect(existsSync(moved)).toBe(false);
    expect(existsSync(defaultDir())).toBe(true);
    expect(firstCleanup(outcomes).dir).toBe(moved);
  });

  it("follows the cargo target directory when it is moved, as CARGO_TARGET_DIR would", () => {
    const moved = join(outside, "shared-target");
    run([OWNER], {
      env: { CARGO_TARGET_DIR: moved },
      cargoMetadata: metadata(moved),
      onRun: () => plant(join(moved, "llvm-cov-target")),
    });
    expect(existsSync(join(moved, "llvm-cov-target"))).toBe(false);
  });

  it("deletes only the llvm-cov-target subdirectory of the cargo target directory", () => {
    run([OWNER], {
      onRun: () => {
        plant(defaultDir());
        plant(join(root, "src-tauri", "target", "debug"), { "keep.bin": "k" });
      },
    });
    expect(existsSync(join(root, "src-tauri", "target", "debug", "keep.bin"))).toBe(true);
  });

  // cargo-llvm-cov 0.9.1 reads CARGO_LLVM_COV_BUILD_DIR only beside CARGO_LLVM_COV_TARGET_DIR,
  // and uses the build directory only when cargo's build.build-dir is configured (which
  // `cargo metadata` shows as build_directory differing from target_directory).
  const BUILD_ENV = {
    CARGO_LLVM_COV_TARGET_DIR: "tgt/llvm-cov-target",
    CARGO_LLVM_COV_BUILD_DIR: "build-scratch/llvm-cov-target",
  };
  const configuredBuildDir = () => metadata(join(root, "src-tauri", "target"), join(root, "cargo-build"));

  it("follows CARGO_LLVM_COV_BUILD_DIR when cargo's build-dir is configured", () => {
    const build = join(root, "build-scratch", "llvm-cov-target");
    run([OWNER], {
      env: BUILD_ENV,
      cargoMetadata: configuredBuildDir(),
      onRun: () => {
        plant(join(root, "tgt", "llvm-cov-target"));
        plant(build);
      },
    });
    expect(existsSync(build)).toBe(false);
    expect(existsSync(join(root, "tgt", "llvm-cov-target"))).toBe(false);
  });

  it("leaves CARGO_LLVM_COV_BUILD_DIR alone when cargo's build-dir is not configured, as the build never used it", () => {
    const build = join(root, "build-scratch", "llvm-cov-target");
    run([OWNER], {
      env: BUILD_ENV,
      onRun: () => {
        plant(join(root, "tgt", "llvm-cov-target"));
        plant(build);
      },
    });
    expect(existsSync(join(root, "tgt", "llvm-cov-target"))).toBe(false);
    expect(existsSync(build)).toBe(true);
  });

  it("leaves CARGO_LLVM_COV_BUILD_DIR alone when CARGO_LLVM_COV_TARGET_DIR is not set, as the build never read it", () => {
    const build = join(root, "build-scratch", "llvm-cov-target");
    run([OWNER], {
      env: { CARGO_LLVM_COV_BUILD_DIR: "build-scratch/llvm-cov-target" },
      cargoMetadata: configuredBuildDir(),
      onRun: () => {
        plant(defaultDir());
        plant(build);
      },
    });
    expect(existsSync(defaultDir())).toBe(false);
    expect(existsSync(build)).toBe(true);
  });

  it("does not touch cargo's build directory when CARGO_LLVM_COV_TARGET_DIR moved the build elsewhere", () => {
    const cargoBuild = join(root, "cargo-build", "llvm-cov-target");
    run([OWNER], {
      env: { CARGO_LLVM_COV_TARGET_DIR: "tgt/llvm-cov-target" },
      cargoMetadata: configuredBuildDir(),
      onRun: () => {
        plant(join(root, "tgt", "llvm-cov-target"));
        plant(cargoBuild);
      },
    });
    expect(existsSync(cargoBuild)).toBe(true);
  });

  it("follows a separate cargo build directory when cargo reports one", () => {
    const buildBase = join(root, "builds");
    run([OWNER], {
      cargoMetadata: metadata(join(root, "src-tauri", "target"), buildBase),
      onRun: () => {
        plant(defaultDir());
        plant(join(buildBase, "llvm-cov-target"));
      },
    });
    expect(existsSync(join(buildBase, "llvm-cov-target"))).toBe(false);
    expect(existsSync(defaultDir())).toBe(false);
  });
});

describe("it refuses rather than delete what it cannot identify, and says so", () => {
  const first = firstCleanup;

  it("does not follow a symlink at the path", () => {
    const precious = join(outside, "precious");
    plant(precious, { "keep.txt": "do not delete" });
    const { outcomes, report } = run([OWNER], {
      onRun: () => {
        mkdirSync(join(root, "src-tauri", "target"), { recursive: true });
        symlinkSync(precious, defaultDir());
      },
    });
    expect(readFileSync(join(precious, "keep.txt"), "utf8")).toBe("do not delete");
    expect(lstatSync(defaultDir()).isSymbolicLink()).toBe(true);
    expect(first(outcomes).status).toBe("refused");
    expect(report).toMatch(/NOT REMOVED/);
    expect(report).toMatch(/symlink/i);
  });

  it("does not delete through a symlinked parent that leads outside the repository", () => {
    const real = join(outside, "real-target");
    plant(join(real, "llvm-cov-target"), { "keep.txt": "do not delete" });
    const { outcomes } = run([OWNER], {
      onRun: () => {
        mkdirSync(join(root, "src-tauri"), { recursive: true });
        symlinkSync(real, join(root, "src-tauri", "target"));
      },
    });
    expect(readFileSync(join(real, "llvm-cov-target", "keep.txt"), "utf8")).toBe("do not delete");
    expect(first(outcomes).status).toBe("refused");
  });

  it("does not delete a directory that contains the repository", () => {
    const parent = join(root, "..");
    const { outcomes } = run([OWNER], { env: { CARGO_LLVM_COV_TARGET_DIR: parent } });
    expect(existsSync(root)).toBe(true);
    expect(first(outcomes).status).toBe("refused");
  });

  it("does not delete the repository root itself", () => {
    plant(root, { "CACHEDIR.TAG": "x", "src/keep.txt": "keep" });
    const { outcomes } = run([OWNER], { env: { CARGO_LLVM_COV_TARGET_DIR: "." } });
    expect(existsSync(join(root, "src", "keep.txt"))).toBe(true);
    expect(first(outcomes).status).toBe("refused");
  });

  it("does not delete the home directory", () => {
    const home = join(outside, "home");
    plant(home, { "CACHEDIR.TAG": "x", "doc.txt": "mine" });
    const { outcomes } = run([OWNER], { env: { CARGO_LLVM_COV_TARGET_DIR: home } });
    expect(readFileSync(join(home, "doc.txt"), "utf8")).toBe("mine");
    expect(first(outcomes).status).toBe("refused");
  });

  it("does not delete the real home directory when none is injected", () => {
    const { outcomes } = (() => {
      const lines: string[] = [];
      const outcomes = runGates([OWNER], {
        platform: "darwin",
        root,
        log: (l) => lines.push(l),
        scratch: { env: { CARGO_LLVM_COV_TARGET_DIR: homedir() }, cargoMetadata: metadata() },
        run: () => ok,
      });
      return { outcomes };
    })();
    expect(existsSync(homedir())).toBe(true);
    expect(first(outcomes).status).toBe("refused");
  });

  it("does not delete outside the repository when only a config file moved the target", () => {
    const moved = join(outside, "config-target");
    plant(join(moved, "llvm-cov-target"), { "keep.txt": "do not delete" });
    const { outcomes, report } = run([OWNER], { cargoMetadata: metadata(moved) });
    expect(readFileSync(join(moved, "llvm-cov-target", "keep.txt"), "utf8")).toBe("do not delete");
    expect(first(outcomes).status).toBe("refused");
    expect(report).toMatch(/outside the repository/i);
  });

  it("does not delete a differently named CARGO_LLVM_COV_TARGET_DIR that is not a cargo target directory", () => {
    const named = join(outside, "documents");
    plant(named, { "report.txt": "mine" });
    const { outcomes } = run([OWNER], { env: { CARGO_LLVM_COV_TARGET_DIR: named } });
    expect(readFileSync(join(named, "report.txt"), "utf8")).toBe("mine");
    expect(first(outcomes).status).toBe("refused");
  });

  it("does not delete the regular cargo target directory even when named explicitly", () => {
    const regular = join(root, "src-tauri", "target");
    plant(regular, { "CACHEDIR.TAG": "x", "debug/keep.bin": "k" });
    const { outcomes } = run([OWNER], { env: { CARGO_LLVM_COV_TARGET_DIR: regular } });
    expect(existsSync(join(regular, "debug", "keep.bin"))).toBe(true);
    expect(first(outcomes).status).toBe("refused");
  });

  it("refuses a variable that is set but empty", () => {
    plant(defaultDir());
    const { outcomes } = run([OWNER], { env: { CARGO_LLVM_COV_TARGET_DIR: "" } });
    expect(existsSync(defaultDir())).toBe(true);
    expect(first(outcomes).status).toBe("refused");
  });

  it("deletes nothing and fails loudly when the location cannot be resolved", () => {
    plant(defaultDir());
    const { outcomes, report } = run([OWNER], {
      cargoMetadata: () => {
        throw new Error("cargo metadata timed out");
      },
    });
    expect(existsSync(defaultDir())).toBe(true);
    expect(first(outcomes).status).toBe("failed");
    expect(report).toMatch(/NOT REMOVED/);
    expect(report).toContain("cargo metadata timed out");
  });

  it("reports a failed removal as failed, never as removed", () => {
    plant(defaultDir());
    const lines: string[] = [];
    const outcomes = runGates([OWNER], {
      platform: "darwin",
      root,
      log: (l) => lines.push(l),
      scratch: {
        ...scratchOptions(),
        remove: () => {
          throw new Error("EBUSY");
        },
      },
      run: () => ok,
    });
    expect(first(outcomes).status).toBe("failed");
    expect(summarize(outcomes)).toMatch(/NOT REMOVED/);
    expect(summarize(outcomes)).not.toMatch(/\bremoved\b.*llvm-cov-target/);
  });

  it("does not let a refused cleanup turn a passing gate into a failing one", () => {
    const { outcomes } = run([OWNER], { cargoMetadata: metadata(join(outside, "t")) });
    expect(outcomes[0].status).toBe("passed");
  });
});

describe("the declaration on the real gate list", () => {
  it("declares the coverage build as the test gate's scratch, and the floor as its reader", () => {
    const owner = GATES.find((g) => g.id === "cargo-tests");
    const floor = GATES.find((g) => g.id === "coverage-floor");
    expect(owner?.scratch).toEqual([{ kind: "cargo-llvm-cov", manifest: MANIFEST }]);
    expect(floor?.reads).toEqual(expect.arrayContaining(["lcov.info", "coverage/lcov.info"]));
  });

  it("only reads what an earlier gate produces", () => {
    const seen = new Set<string>();
    for (const g of GATES) {
      for (const read of g.reads ?? []) expect(seen.has(read)).toBe(true);
      for (const made of g.produces ?? []) seen.add(made);
    }
  });

  it("keeps the build through the coverage floor and removes it before the run ends", () => {
    const order = GATES.map((g) => g.id);
    const seen: Record<string, boolean> = {};
    runGates(GATES, {
      platform: "darwin",
      root,
      log: () => {},
      scratch: scratchOptions(),
      run: (g) => {
        if (g.id === "cargo-tests") plant(defaultDir());
        seen[g.id] = existsSync(defaultDir());
        return ok;
      },
    });
    const owner = order.indexOf("cargo-tests");
    const floor = order.indexOf("coverage-floor");
    for (const id of order.slice(owner, floor + 1)) expect(seen[id]).toBe(true);
    expect(existsSync(defaultDir())).toBe(false);
  });
});

describe("the wiring main uses, and the cargo query behind it", () => {
  const answer = (stdout: string, status: number | null = 0, error?: Error) => () => ({ stdout, status, error });

  it("reads target and build directories from cargo metadata", () => {
    const got = cargoMetadata(MANIFEST, answer(JSON.stringify({ target_directory: "/t", build_directory: "/b" })));
    expect(got).toEqual({ targetDirectory: "/t", buildDirectory: "/b" });
  });

  it("takes the build directory to be the target directory when cargo does not report one", () => {
    expect(cargoMetadata(MANIFEST, answer(JSON.stringify({ target_directory: "/t" })))).toEqual({
      targetDirectory: "/t",
      buildDirectory: "/t",
    });
  });

  it("asks cargo for metadata only, never to build, over the given manifest", () => {
    const seen: string[][] = [];
    cargoMetadata(MANIFEST, (args) => {
      seen.push(args);
      return { stdout: JSON.stringify({ target_directory: "/t" }), status: 0 };
    });
    expect(seen).toEqual([["metadata", "--no-deps", "--format-version", "1", "--manifest-path", MANIFEST]]);
  });

  it.each([
    ["a timeout or spawn error", answer("", null, new Error("ETIMEDOUT"))],
    ["a non-zero exit", answer("", 101)],
    ["output that is not JSON", answer("not json")],
    ["JSON without a target_directory", answer("{}")],
    ["JSON that is not an object", answer("null")],
  ])("fails, rather than guess a directory, on %s", (_label, runner) => {
    expect(() => cargoMetadata(MANIFEST, runner)).toThrow();
  });

  it("hands main the real environment and cargo query, not a stand-in", () => {
    const context = productionScratchContext();
    expect(context.env).toBe(process.env);
    expect(context.cargoMetadata).toBe(cargoMetadata);
    expect(context.home).toBe(homedir());
  });

  it("main cleans up through the scratch context it is given, and reports it", () => {
    const lines: string[] = [];
    const code = main([], {
      gates: [OWNER, READER],
      root,
      log: (line) => lines.push(line),
      scratch: scratchOptions(),
      run: (g) => {
        if (g.id === "owner") plant(defaultDir());
        return ok;
      },
    });
    expect(code).toBe(0);
    expect(existsSync(defaultDir())).toBe(false);
    expect(lines.join("\n")).toMatch(/removed .*llvm-cov-target/);
  });

  it("main keeps the build when asked to, and still exits on the gates alone", () => {
    const lines: string[] = [];
    const code = main([], {
      gates: [OWNER, READER],
      root,
      log: (line) => lines.push(line),
      scratch: scratchOptions({ [KEEP]: "1" }),
      run: (g) => {
        if (g.id === "owner") plant(defaultDir());
        return g.id === "reader" ? bad : ok;
      },
    });
    expect(code).toBe(1);
    expect(existsSync(defaultDir())).toBe(true);
  });

  it("documents the opt-out in --help", () => {
    const lines: string[] = [];
    const original = console.log;
    console.log = (line: string) => lines.push(line);
    try {
      expect(main(["--help"])).toBe(0);
    } finally {
      console.log = original;
    }
    expect(lines.join("\n")).toContain(KEEP);
  });
});
