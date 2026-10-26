import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { GATES, commandLine, runGates, skipReason, summarize } from "./ci-local.mjs";

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
    for (const line of cargo) expect(line).toContain("src-tauri/Cargo.toml");
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
