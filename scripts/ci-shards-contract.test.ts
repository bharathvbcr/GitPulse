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

/** What ci.yml says about the shards, or why it says nothing usable. */
export function shardProblems(ci: string, nextest: string): string[] {
  const problems: string[] = [];

  const runs = ci.split("\n").filter((line) => /\bcargo nextest run\b/.test(line));
  if (runs.length !== 1) problems.push(`expected exactly one \`cargo nextest run\` line in ci.yml, found ${runs.length}`);
  const run = runs[0] ?? "";
  const denominator = /--partition hash:\$\{\{ matrix\.shard \}\}\/(\d+)/.exec(run)?.[1];
  if (!denominator) problems.push("the nextest run is not `--partition hash:${{ matrix.shard }}/N`, so one job would run the whole suite");
  if (!/--profile ci\b/.test(run)) problems.push("the nextest run does not use `--profile ci`, which is what keeps a shard serial");
  if (!run.includes("--no-fail-fast")) problems.push("the nextest run can stop at its first failure");

  // The matrix: the macOS entries' `shard:` values.
  const matrix = ci.match(/- os: macos-latest\n\s+shard: (\d+)/g) ?? [];
  const shards = matrix.map((entry) => Number(/shard: (\d+)/.exec(entry)![1])).sort((a, b) => a - b);
  const count = Number(denominator);
  if (!denominator || shards.join(",") !== Array.from({ length: count }, (_, i) => i + 1).join(",")) {
    problems.push(`the macOS matrix shards [${shards}] are not exactly 1..${denominator ?? "N"}`);
  }

  // Every other spelling of the count.
  const named = /format\(', shard \{0\}\/(\d+)', matrix\.shard\)/.exec(ci)?.[1];
  if (named !== denominator) problems.push(`the job name says /${named}, the partition /${denominator}`);
  const check = ci.split("\n").find((line) => line.includes("scripts/check-shard-coverage.mjs")) ?? "";
  if (!check) problems.push("no step runs scripts/check-shard-coverage.mjs");
  const checked = /--shards (\d+)/.exec(check)?.[1];
  if (checked !== denominator) problems.push(`the completeness check expects ${checked} shards, the partition has ${denominator}`);
  const reports = [...check.matchAll(/junit-shard-(\d+)\/junit\.xml/g)].map((m) => Number(m[1])).sort((a, b) => a - b);
  if (reports.join(",") !== shards.join(",")) problems.push(`the completeness check reads reports [${reports}], the matrix runs shards [${shards}]`);
  const uploads = (ci.match(/name: junit-shard-\$\{\{ matrix\.shard \}\}/g) ?? []).length;
  if (uploads !== 1) problems.push("the shards do not each upload a `junit-shard-<n>` report");

  // The completeness job has to run after a red shard, not only a green one.
  const job = /\n  rust-test-complete:\n([\s\S]*?)(?:\n  [a-z-]+:\n|$)/.exec(ci)?.[1] ?? "";
  if (!/needs: rust-test\b/.test(job)) problems.push("rust-test-complete does not need rust-test");
  if (!/if: \$\{\{ !cancelled\(\) \}\}/.test(job)) problems.push("rust-test-complete is skipped after a failed shard, so a red shard could hide a missing one");

  // nextest pinned, and the profile a shard runs under is serial with a report.
  if (!/tool: cargo-nextest@\d+\.\d+\.\d+/.test(ci)) problems.push("cargo-nextest is not pinned to a version");
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

  it("macOS is the only leg that shards, so Linux and Windows keep the in-process serial gate", () => {
    const runs = ci.split("\n").filter((line) => /\bcargo test --manifest-path src-tauri\/Cargo\.toml --locked --no-fail-fast/.test(line));
    expect(runs.length).toBeGreaterThan(0);
    for (const line of runs) expect(line).toContain("--test-threads=1");
    expect(ci).toMatch(/Rust Unit & Integration Tests\n\s+if: \$\{\{ !cancelled\(\) && matrix\.os != 'macos-latest' \}\}/);
  });

  describe("the rules fail on the shapes they are for", () => {
    const broken = (from: string | RegExp, to: string) => {
      const mutated = ci.replace(from, to);
      expect(mutated, `the mutation ${String(from)} matched nothing`).not.toBe(ci);
      return shardProblems(mutated, nextest);
    };

    it("a matrix shorter than the denominator", () => {
      expect(broken(/- os: macos-latest\n\s+shard: 3\n/, "")).toEqual(expect.arrayContaining([expect.stringContaining("not exactly 1..3")]));
    });
    it("a denominator that grew without the matrix", () => {
      expect(broken("hash:${{ matrix.shard }}/3", "hash:${{ matrix.shard }}/4").length).toBeGreaterThan(0);
    });
    it("a completeness check that expects fewer shards", () => {
      expect(broken("--shards 3", "--shards 2")).toEqual(expect.arrayContaining([expect.stringContaining("expects 2 shards")]));
    });
    it("a run that is not partitioned", () => {
      expect(broken(/ --partition hash:\$\{\{ matrix\.shard \}\}\/3/, "")).toEqual(expect.arrayContaining([expect.stringContaining("whole suite")]));
    });
    it("a completeness job that waits for green", () => {
      expect(broken(/(rust-test-complete:[\s\S]*?)if: \$\{\{ !cancelled\(\) \}\}/, "$1if: ${{ success() }}")).toEqual(
        expect.arrayContaining([expect.stringContaining("skipped after a failed shard")]),
      );
    });
    it("a profile that is no longer serial", () => {
      expect(shardProblems(ci, nextest.replace(/(\[profile\.ci\]\ntest-threads = )1/, "$14"))).toEqual(
        expect.arrayContaining([expect.stringContaining("test-threads = 1")]),
      );
    });
  });
});
