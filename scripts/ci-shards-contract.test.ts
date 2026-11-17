import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The macOS Rust legs run as nextest shards (`--partition hash:M/N`), and a
 * shard count is spelled in five places that nothing compiles against each
 * other: the matrix, the partition denominator, the job name, the completeness
 * check's report list and its `--shards`. A matrix of two with a denominator of
 * three leaves a third of the suite unrun while every job is green, which is the
 * failure `scripts/check-shard-coverage.mjs` exists to catch at run time; this
 * catches the edit that causes it before a push.
 *
 * Each rule is a function over the workflow text, so the self-test can show the
 * rule failing on the shape it is for.
 */
const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

/** The text of one top-level job. */
function jobText(ci: string, id: string): string {
  return new RegExp(`\\n  ${id}:\\n([\\s\\S]*?)(?=\\n  [a-z][a-z-]*:\\n|$)`).exec(ci)?.[1] ?? "";
}

/** What ci.yml says about the shards, or why it says nothing usable. */
export function shardProblems(ci: string, nextest: string): string[] {
  const problems: string[] = [];
  const shards = jobText(ci, "rust-test-macos");
  const extras = jobText(ci, "rust-test-macos-extras");
  const complete = jobText(ci, "rust-test-complete");
  const gate = jobText(ci, "rust-test");
  for (const [name, text] of [["rust-test-macos", shards], ["rust-test-macos-extras", extras], ["rust-test-complete", complete], ["rust-test", gate]] as const) {
    if (!text) problems.push(`ci.yml has no \`${name}\` job`);
  }

  const runs = ci.split("\n").filter((line) => /\bcargo nextest run\b/.test(line));
  if (runs.length !== 1) problems.push(`expected exactly one \`cargo nextest run\` line in ci.yml, found ${runs.length}`);
  const run = runs[0] ?? "";
  const denominator = /--partition hash:\$\{\{ matrix\.shard \}\}\/(\d+)/.exec(run)?.[1];
  if (!denominator) problems.push("the nextest run is not `--partition hash:${{ matrix.shard }}/N`, so one job would run the whole suite");
  if (!/--profile ci\b/.test(run)) problems.push("the nextest run does not use `--profile ci`, which is what keeps a shard serial");
  if (!run.includes("--no-fail-fast")) problems.push("the nextest run can stop at its first failure");
  if (!shards.includes("cargo nextest run")) problems.push("the nextest run is not in rust-test-macos");

  // The matrix axis: a plain list, so the release-gate rule can read it.
  const axis = /\n\s+shard: \[([^\]]*)\]/.exec(shards)?.[1];
  const listed = (axis ?? "").split(",").map((v) => Number(v.trim())).filter((n) => Number.isInteger(n)).sort((a, b) => a - b);
  const count = Number(denominator);
  if (axis === undefined || !denominator || listed.join(",") !== Array.from({ length: count }, (_, i) => i + 1).join(",")) {
    problems.push(`the shard axis [${listed}] is not exactly 1..${denominator ?? "N"}`);
  }

  // Every other spelling of the count.
  const named = /name: Rust Tests \(macOS, shard \$\{\{ matrix\.shard \}\}\/(\d+)\)/.exec(shards)?.[1];
  if (named !== denominator) problems.push(`the job name says /${named}, the partition /${denominator}`);
  const check = ci.split("\n").find((line) => line.includes("scripts/check-shard-coverage.mjs")) ?? "";
  if (!check) problems.push("no step runs scripts/check-shard-coverage.mjs");
  const checked = /--shards (\d+)/.exec(check)?.[1];
  if (checked !== denominator) problems.push(`the completeness check expects ${checked} shards, the partition has ${denominator}`);
  const reports = [...check.matchAll(/junit-shard-(\d+)\/junit\.xml/g)].map((m) => Number(m[1])).sort((a, b) => a - b);
  if (reports.join(",") !== listed.join(",")) problems.push(`the completeness check reads reports [${reports}], the matrix runs shards [${listed}]`);
  if ((shards.match(/name: junit-shard-\$\{\{ matrix\.shard \}\}/g) ?? []).length !== 1) problems.push("the shards do not each upload a `junit-shard-<n>` report");

  // The completeness job waits for every job that feeds it and never skips on
  // its own: a skipped job reads as a pass to release-state.mjs.
  if (!/needs: \[[^\]]*rust-test-macos\b[^\]]*\]/.test(complete) || !/needs: \[[^\]]*rust-test-macos-extras\b[^\]]*\]/.test(complete)) {
    problems.push("rust-test-complete does not need both rust-test-macos and rust-test-macos-extras");
  }
  if (/^    if:/m.test(complete)) problems.push("rust-test-complete has a job-level `if`, which can skip it and leave the run green");
  if (!extras.includes("name: nextest-list")) problems.push("rust-test-macos-extras does not upload the suite list the check reads");

  // Linux and Windows keep the in-process serial gate and never shard.
  if (/macos/i.test(/\n\s+os: \[([^\]]*)\]/.exec(gate)?.[1] ?? "")) problems.push("rust-test lists macOS, which belongs to the sharded jobs");
  for (const line of ci.split("\n").filter((l) => /\bcargo test --manifest-path src-tauri\/Cargo\.toml --locked --no-fail-fast/.test(l))) {
    if (!line.includes("--test-threads=1")) problems.push("a `cargo test` gate line lost `--test-threads=1`");
  }

  // nextest pinned, and the profile a shard runs under is serial with a report.
  if (!/tool: cargo-nextest@\d+\.\d+\.\d+/.test(shards) || !/tool: cargo-nextest@\d+\.\d+\.\d+/.test(extras)) problems.push("cargo-nextest is not pinned to a version in both macOS jobs");
  const profile = /\n\[profile\.ci\]\n([^[]*)/.exec(nextest)?.[1] ?? "";
  if (!/^test-threads\s*=\s*1\s*$/m.test(profile)) problems.push("[profile.ci] is not `test-threads = 1`");
  if (!/^fail-fast\s*=\s*false\s*$/m.test(profile)) problems.push("[profile.ci] does not run past a failure");
  if (!/\[profile\.ci\.junit\]\s*\npath = "junit\.xml"/.test(nextest)) problems.push("[profile.ci.junit] does not write junit.xml, which the shards upload");
  return problems;
}

describe("ci shards contract", () => {
  const ci = read(".github/workflows/ci.yml");
  const nextest = read("src-tauri/.config/nextest.toml");

  it("spells the shard count the same way everywhere", () => {
    expect(shardProblems(ci, nextest)).toEqual([]);
  });

  describe("the rules fail on the shapes they are for", () => {
    const broken = (from: string | RegExp, to: string) => {
      const mutated = ci.replace(from, to);
      expect(mutated, `the mutation ${String(from)} matched nothing`).not.toBe(ci);
      return shardProblems(mutated, nextest);
    };
    const has = (problems: string[], text: string) => expect(problems).toEqual(expect.arrayContaining([expect.stringContaining(text)]));

    it("an axis shorter than the denominator", () => {
      has(broken("shard: [1, 2, 3]", "shard: [1, 2]"), "not exactly 1..3");
    });
    it("a denominator that grew without the axis", () => {
      expect(broken("hash:${{ matrix.shard }}/3", "hash:${{ matrix.shard }}/4").length).toBeGreaterThan(0);
    });
    it("a completeness check that expects fewer shards", () => {
      has(broken("--shards 3", "--shards 2"), "expects 2 shards");
    });
    it("a run that is not partitioned", () => {
      has(broken(/ --partition hash:\$\{\{ matrix\.shard \}\}\/3/, ""), "whole suite");
    });
    it("a completeness job that can skip itself", () => {
      has(broken("  rust-test-complete:\n    name:", "  rust-test-complete:\n    if: ${{ !cancelled() }}\n    name:"), "job-level `if`");
    });
    it("a completeness job that no longer waits for the extras", () => {
      has(broken("needs: [rust-test-macos, rust-test-macos-extras]", "needs: [rust-test-macos]"), "does not need both");
    });
    it("macOS back in the serial gate", () => {
      has(broken("os: [ubuntu-latest, windows-latest]", "os: [ubuntu-latest, macos-latest, windows-latest]"), "lists macOS");
    });
    it("a profile that is no longer serial", () => {
      has(shardProblems(ci, nextest.replace(/(\[profile\.ci\]\ntest-threads = )1/, "$14")), "test-threads = 1");
    });
  });
});
