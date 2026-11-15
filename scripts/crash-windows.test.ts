import { describe, expect, it } from "vitest";
import {
  checkTable,
  findHooks,
  summary,
  testFacts,
  type Hook,
  type Row,
  type Table,
} from "./crash-windows";

/**
 * The guard's checking logic, as pure functions over strings. No file is read
 * here: every case builds a tiny Rust source and a tiny table, so a failure
 * names the rule that broke and nothing else.
 */

const OWNER = "src/owner.rs";
const SUITE = "src/suite.rs";

const OWNER_SOURCE = `
fn write_step() {}
#[cfg(all(test, unix))]
fn crash_point(written: i64) {
    let _ = std::env::var("GITPULSE_TEST_KILL_AFTER");
}
`;

const SUITE_SOURCE = `
#[cfg(test)]
mod tests {
    #[test]
    fn recovers_after_kill() {}

    #[test]
    fn recovers_after_error() {}
}
`;

const HOOKS: Hook[] = [
  { kind: "env", name: "GITPULSE_TEST_KILL_AFTER", file: OWNER },
  { kind: "fn", name: "crash_point", file: OWNER },
];

function covered(id: string, over: Partial<Row> = {}): Row {
  return {
    id,
    path: "ledger",
    boundary: { durableBefore: "rows inserted", notYetWritten: "commit" },
    injection: { kind: "hook", detail: "SIGKILL after N rows", hooks: HOOKS },
    tests: [{ file: SUITE, name: "recovers_after_kill", cfg: [] }],
    recovery: "the next pass records every row once",
    status: "covered",
    ...over,
  };
}

function uncovered(id: string, over: Partial<Row> = {}): Row {
  return {
    id,
    path: "ops-log",
    boundary: { durableBefore: "snapshot ref", notYetWritten: "ledger row" },
    injection: { kind: "none", detail: "no injection point", hooks: [] },
    tests: [],
    recovery: "the snapshot exists with no ledger row",
    status: "uncovered",
    reason: "no hook between update-ref and the ledger append",
    ...over,
  };
}

function table(rows: Row[], over: Partial<Table> = {}): Table {
  return {
    scope: { dirs: [], files: [OWNER] },
    paths: ["ledger", "ops-log", "worktree-merge"],
    ignoredHooks: [],
    rows,
    ...over,
  };
}

const SOURCES: Record<string, string> = { [OWNER]: OWNER_SOURCE, [SUITE]: SUITE_SOURCE };
const SCOPE = [OWNER];

function errors(t: Table, sources = SOURCES): string[] {
  return checkTable(t, sources, SCOPE).errors;
}

describe("a correct table", () => {
  it("passes, and counts covered and uncovered rows separately", () => {
    const report = checkTable(table([covered("LEDGER-A"), uncovered("OPSLOG-B")]), SOURCES, SCOPE);
    expect(report.errors).toEqual([]);
    expect(report.covered).toBe(1);
    expect(report.uncovered).toBe(1);
    expect(report.byPath).toEqual({
      ledger: { covered: 1, uncovered: 0 },
      "ops-log": { covered: 0, uncovered: 1 },
      "worktree-merge": { covered: 0, uncovered: 0 },
    });
  });

  it("never counts an uncovered row as covered, even when it cites a nearby test", () => {
    const row = uncovered("OPSLOG-B", {
      nearestTests: [{ file: SUITE, name: "recovers_after_error", cfg: [] }],
    });
    const report = checkTable(table([covered("LEDGER-A", { tests: [] }), row]), SOURCES, SCOPE);
    // The covering row is broken on purpose (no test), so it must not be counted
    // either; the uncovered row stands alone in its own tally.
    expect(report.errors.join("\n")).toMatch(/LEDGER-A.*covered row cites no test/s);
    expect([report.covered, report.uncovered]).toEqual([0, 1]);
  });

  it("tallies rows by engine when a row names one, so one engine's coverage cannot stand for another's", () => {
    const rows = [
      covered("WTMERGE-TD-A", { path: "worktree-merge", engine: "teardown" }),
      uncovered("WTMERGE-PIPE-A", { path: "worktree-merge", engine: "pipeline" }),
      uncovered("WTMERGE-PIPE-B", { path: "worktree-merge", engine: "pipeline" }),
      covered("LEDGER-A"),
    ];
    const report = checkTable(table(rows), SOURCES, SCOPE);
    expect(report.errors).toEqual([]);
    expect(report.byEngine).toEqual({
      "worktree-merge/teardown": { covered: 1, uncovered: 0 },
      "worktree-merge/pipeline": { covered: 0, uncovered: 2 },
    });
    expect(summary(report)).toContain("worktree-merge/pipeline: covered 0, uncovered 2");
  });

  it("states its scope and both counts in its summary", () => {
    const text = summary(checkTable(table([covered("LEDGER-A"), uncovered("OPSLOG-B")]), SOURCES, SCOPE));
    expect(text).toContain("covered 1");
    expect(text).toContain("uncovered 1");
    expect(text).toContain("scanned 1 file");
    expect(text).toContain(OWNER);
    expect(text).toMatch(/not the whole codebase/i);
  });
});

describe("an unlisted hook", () => {
  it("fails when a scope file gains a hook no row names", () => {
    const grown = `${OWNER_SOURCE}\npub(crate) fn inject_next_failure_for_test() {}\n`;
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [OWNER]: grown });
    expect(found.join("\n")).toMatch(/inject_next_failure_for_test.*no row names/s);
  });

  it("fails on an unlisted env hook", () => {
    const grown = `${OWNER_SOURCE}\nfn other() { std::env::var("GITPULSE_TEST_DIE_HERE"); }\n`;
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [OWNER]: grown });
    expect(found.join("\n")).toMatch(/GITPULSE_TEST_DIE_HERE.*no row names/s);
  });

  it("accepts a hook that is named only by an uncovered row", () => {
    const row = uncovered("LEDGER-B", {
      injection: { kind: "hook", detail: "reachable", hooks: HOOKS },
    });
    expect(errors(table([row]))).toEqual([]);
  });

  it("accepts an unlisted hook that the table explicitly sets aside with a reason", () => {
    const grown = `${OWNER_SOURCE}\nfn bench_for_test() {}\n`;
    const t = table([covered("LEDGER-A")], {
      ignoredHooks: [{ kind: "fn", name: "bench_for_test", file: OWNER, reason: "a benchmark, not a fault" }],
    });
    expect(errors(t, { ...SOURCES, [OWNER]: grown })).toEqual([]);
  });

  it("fails when a set-aside hook has no reason or no longer exists", () => {
    const noReason = table([covered("LEDGER-A")], {
      ignoredHooks: [{ kind: "fn", name: "crash_point", file: OWNER, reason: "" }],
    });
    expect(errors(noReason).join("\n")).toMatch(/set aside.*no reason/s);
    const gone = table([covered("LEDGER-A")], {
      ignoredHooks: [{ kind: "fn", name: "vanished_for_test", file: OWNER, reason: "x" }],
    });
    expect(errors(gone).join("\n")).toMatch(/vanished_for_test.*no longer exists/s);
  });

  it("does not see a hook that appears only in a comment, a doc comment or a string", () => {
    const quiet = `
// fn inject_in_line_comment_for_test() {}
/* fn inject_in_block_comment_for_test() {} /* nested */ still comment */
/// fn inject_in_doc_comment_for_test() {}
fn real() { let _ = "fn inject_in_string_for_test()"; }
#[ignore = "run alone with GITPULSE_TEST_WRITE_BUDGET=0"]
fn bench() {}
`;
    expect(findHooks(OWNER, quiet)).toEqual([]);
  });

  it("finds env hooks only as a whole string literal, and fn hooks by the documented names", () => {
    const source = `
fn a() { std::env::var("GITPULSE_TEST_ONE"); }
fn crash_point_after(n: i64) {}
fn inject_thing() {}
fn thing_for_test() {}
fn plain() {}
fn my_failpoint() {}
`;
    const names = findHooks(OWNER, source).map((hook) => `${hook.kind}:${hook.name}`);
    expect(names.sort()).toEqual(
      [
        "env:GITPULSE_TEST_ONE",
        "fn:crash_point_after",
        "fn:inject_thing",
        "fn:my_failpoint",
        "fn:thing_for_test",
      ].sort(),
    );
  });

  it("finds a closure seam: a *_with function that takes a dyn Fn parameter", () => {
    const source = `
fn remove_with(path: &str, delete: &mut dyn FnMut(&Path) -> std::io::Result<()>) {}
fn sum_with(a: u32, b: u32) -> u32 { a + b }
fn merge_and_teardown_with_seams(repo: &str, sleep: &mut dyn FnMut(Duration)) {}
`;
    const names = findHooks(OWNER, source).map((hook) => hook.name).sort();
    expect(names).toEqual(["merge_and_teardown_with_seams", "remove_with"]);
  });

  it("does not take a test function for a hook", () => {
    const source = `
#[test]
fn injects_a_failure_for_test() {}
`;
    expect(findHooks(OWNER, source)).toEqual([]);
  });
});

describe("a row that names a hook that is gone", () => {
  it("fails when the cited fn no longer exists in the cited file", () => {
    const shrunk = "fn write_step() {}\n";
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [OWNER]: shrunk });
    expect(found.join("\n")).toMatch(/LEDGER-A.*crash_point.*no longer exists/s);
  });

  it("fails when the cited env var no longer appears as a literal", () => {
    const renamed = OWNER_SOURCE.replace("GITPULSE_TEST_KILL_AFTER", "GITPULSE_TEST_RENAMED");
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [OWNER]: renamed });
    expect(found.join("\n")).toMatch(/LEDGER-A.*GITPULSE_TEST_KILL_AFTER.*no longer exists/s);
  });

  it("fails when the only trace left is a comment", () => {
    const commented = "// fn crash_point() { }\n// \"GITPULSE_TEST_KILL_AFTER\"\n";
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [OWNER]: commented });
    expect(found.join("\n")).toMatch(/crash_point.*no longer exists/s);
    expect(found.join("\n")).toMatch(/GITPULSE_TEST_KILL_AFTER.*no longer exists/s);
  });

  it("fails when the cited file was not loaded at all", () => {
    const found = errors(table([covered("LEDGER-A")]), { [SUITE]: SUITE_SOURCE });
    expect(found.join("\n")).toMatch(/LEDGER-A.*src\/owner\.rs.*not (loaded|found)/s);
  });
});

describe("a row that cites a test that is not there", () => {
  it("fails when the test function does not exist in the cited file", () => {
    const row = covered("LEDGER-A", { tests: [{ file: SUITE, name: "recovers_after_nothing", cfg: [] }] });
    expect(errors(table([row])).join("\n")).toMatch(/LEDGER-A.*recovers_after_nothing.*not found/s);
  });

  it("fails when the name occurs only in a comment", () => {
    const hollow = "// fn recovers_after_kill() {}\n";
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [SUITE]: hollow });
    expect(found.join("\n")).toMatch(/recovers_after_kill.*not found/s);
  });

  it("fails when the function exists but is not a test", () => {
    const helper = "fn recovers_after_kill() {}\n";
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [SUITE]: helper });
    expect(found.join("\n")).toMatch(/recovers_after_kill.*not a test/s);
  });

  it("fails when two functions share the name, since the row would be ambiguous", () => {
    const twice = `${SUITE_SOURCE}\n#[test]\nfn recovers_after_kill() {}\n`;
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [SUITE]: twice });
    expect(found.join("\n")).toMatch(/recovers_after_kill.*2 times|ambiguous/s);
  });

  it("fails for a missing nearest test too, not only for a covering one", () => {
    const row = uncovered("OPSLOG-B", { nearestTests: [{ file: SUITE, name: "ghost", cfg: [] }] });
    expect(errors(table([row])).join("\n")).toMatch(/OPSLOG-B.*ghost.*not found/s);
  });
});

describe("an ignored or compiled-out test", () => {
  const IGNORED = `
#[test]
#[ignore = "slow"]
fn recovers_after_kill() {}
`;
  const GATED = `
#[cfg(unix)]
#[test]
fn recovers_after_kill() {}
`;
  const MOD_GATED = `
#[cfg(target_os = "macos")]
mod mac_only {
    #[test]
    fn recovers_after_kill() {}
}
`;
  const FILE_GATED = `
#![cfg(unix)]
#[test]
fn recovers_after_kill() {}
`;

  it("fails when the test is #[ignore]d and the row does not say so", () => {
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [SUITE]: IGNORED });
    expect(found.join("\n")).toMatch(/LEDGER-A.*recovers_after_kill.*ignore/s);
  });

  it("passes when the row declares the ignore", () => {
    const row = covered("LEDGER-A", {
      tests: [{ file: SUITE, name: "recovers_after_kill", cfg: [], ignored: true }],
    });
    expect(errors(table([row]), { ...SOURCES, [SUITE]: IGNORED })).toEqual([]);
  });

  it("fails when the row claims an ignore that the source does not carry", () => {
    const row = covered("LEDGER-A", {
      tests: [{ file: SUITE, name: "recovers_after_kill", cfg: [], ignored: true }],
    });
    expect(errors(table([row])).join("\n")).toMatch(/LEDGER-A.*claims.*ignore/s);
  });

  it("fails when the test carries a platform cfg the row does not declare", () => {
    const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [SUITE]: GATED });
    expect(found.join("\n")).toMatch(/LEDGER-A.*recovers_after_kill.*cfg.*unix/s);
  });

  it("passes when the row declares that cfg, whatever the whitespace", () => {
    const row = covered("LEDGER-A", {
      tests: [{ file: SUITE, name: "recovers_after_kill", cfg: ["unix"] }],
    });
    expect(errors(table([row]), { ...SOURCES, [SUITE]: GATED })).toEqual([]);
    const spaced = GATED.replace("cfg(unix)", "cfg( unix )");
    expect(errors(table([row]), { ...SOURCES, [SUITE]: spaced })).toEqual([]);
  });

  it("sees a cfg on an enclosing module and on the whole file", () => {
    for (const source of [MOD_GATED, FILE_GATED]) {
      const found = errors(table([covered("LEDGER-A")]), { ...SOURCES, [SUITE]: source });
      expect(found.join("\n")).toMatch(/LEDGER-A.*cfg/s);
    }
    const mac = covered("LEDGER-A", {
      tests: [{ file: SUITE, name: "recovers_after_kill", cfg: ['target_os="macos"'] }],
    });
    expect(errors(table([mac]), { ...SOURCES, [SUITE]: MOD_GATED })).toEqual([]);
    const unix = covered("LEDGER-A", {
      tests: [{ file: SUITE, name: "recovers_after_kill", cfg: ["unix"] }],
    });
    expect(errors(table([unix]), { ...SOURCES, [SUITE]: FILE_GATED })).toEqual([]);
  });

  it("does not ask for a declaration for plain cfg(test)", () => {
    const facts = testFacts(SUITE_SOURCE, "recovers_after_kill");
    expect(facts.cfgs).toEqual([]);
    expect(facts.isTest).toBe(true);
  });

  it("sees an ignore that arrives through cfg_attr", () => {
    const viaCfgAttr = `
#[test]
#[cfg_attr(unix, ignore = "slow on unix")]
fn recovers_after_kill() {}
`;
    expect(testFacts(viaCfgAttr, "recovers_after_kill").ignored).toBe(true);
    const lint = `
#[test]
#[cfg_attr(not(unix), allow(dead_code))]
fn recovers_after_kill() {}
`;
    expect(testFacts(lint, "recovers_after_kill").ignored).toBe(false);
  });

  it("is not fooled by braces or semicolons inside an attribute string", () => {
    const tricky = `
#[test]
#[ignore = "run with {x}; then y"]
fn recovers_after_kill() {}
`;
    const facts = testFacts(tricky, "recovers_after_kill");
    expect(facts.ignored).toBe(true);
    expect(facts.isTest).toBe(true);
  });
});

describe("a duplicate id", () => {
  it("fails when two rows share an id, even in different paths", () => {
    const found = errors(table([covered("LEDGER-A"), uncovered("LEDGER-A")]));
    expect(found.join("\n")).toMatch(/duplicate id.*LEDGER-A/s);
  });

  it("counts a duplicated row once in neither tally it breaks", () => {
    const report = checkTable(table([covered("LEDGER-A"), covered("LEDGER-A")]), SOURCES, SCOPE);
    expect(report.errors.length).toBeGreaterThan(0);
  });
});

describe("a row that contradicts its own status", () => {
  it("fails a covered row with no tests", () => {
    const found = errors(table([covered("LEDGER-A", { tests: [] })]));
    expect(found.join("\n")).toMatch(/LEDGER-A.*covered.*no test/s);
  });

  it("fails a covered row whose injection is none", () => {
    const row = covered("LEDGER-A", { injection: { kind: "none", detail: "-", hooks: [] } });
    expect(errors(table([row])).join("\n")).toMatch(/LEDGER-A.*covered.*injection/s);
  });

  it("fails an uncovered row with no reason", () => {
    const found = errors(table([uncovered("OPSLOG-B", { reason: "  " })]));
    expect(found.join("\n")).toMatch(/OPSLOG-B.*uncovered.*reason/s);
  });

  it("fails an uncovered row that lists covering tests", () => {
    const row = uncovered("OPSLOG-B", { tests: [{ file: SUITE, name: "recovers_after_kill", cfg: [] }] });
    expect(errors(table([row])).join("\n")).toMatch(/OPSLOG-B.*uncovered.*tests/s);
  });

  it("fails a hook injection with no hook, and a hookless kind that lists one", () => {
    const noHooks = covered("LEDGER-A", { injection: { kind: "hook", detail: "-", hooks: [] } });
    expect(errors(table([noHooks])).join("\n")).toMatch(/LEDGER-A.*hook.*names no hook/s);
    const stray = covered("LEDGER-A", { injection: { kind: "state", detail: "-", hooks: HOOKS } });
    expect(errors(table([stray])).join("\n")).toMatch(/LEDGER-A.*state.*lists hooks/s);
  });

  it("fails an unknown path and an unknown status", () => {
    expect(errors(table([covered("LEDGER-A", { path: "elsewhere" })])).join("\n")).toMatch(/LEDGER-A.*path/s);
    const bad = covered("LEDGER-A", { status: "mostly" as Row["status"] });
    expect(errors(table([bad])).join("\n")).toMatch(/LEDGER-A.*status/s);
  });

  it("fails an empty scope, since a guard that scanned nothing passed nothing", () => {
    const found = checkTable(table([covered("LEDGER-A")]), SOURCES, []).errors;
    expect(found.join("\n")).toMatch(/scope.*empty|scanned no file/s);
  });
});
