import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { parseWorkflowYaml } from "./check-workflows.mjs";
import { REQUIRED_WORKFLOW_JOBS } from "./release-state.mjs";

/**
 * All three OS Rust legs (Linux, Windows, macOS) run as nextest shards
 * (`--partition hash:M/N`), and each OS's shard count is spelled in five places
 * that nothing compiles against each other: the matrix, the partition
 * denominator, the job name, the completeness check's report list and its
 * `--shards`. A matrix of two with a denominator of three leaves a third of the
 * suite unrun while every job is green, which is the failure
 * `scripts/check-shard-coverage.mjs` exists to catch at run time; this catches
 * the edit that causes it before a push.
 *
 * Each rule is a function over the workflow text, so the self-test can show the
 * rule failing on the shape it is for.
 */
const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

/** The text of one top-level job. */
function jobText(ci: string, id: string): string {
  return new RegExp(`\\n  ${id}:\\n([\\s\\S]*?)(?=\\n  [a-z][a-z-]*:\\n|$)`).exec(ci)?.[1] ?? "";
}

const SHARDED_OSES = [
  { key: "linux", label: "Linux" },
  { key: "windows", label: "Windows" },
  { key: "macos", label: "macOS" },
] as const;

/** What ci.yml says about the shards, or why it says nothing usable. */
export function shardProblems(ci: string, nextest: string): string[] {
  const problems: string[] = [];
  const complete = jobText(ci, "rust-test-complete");
  if (!complete) problems.push("ci.yml has no `rust-test-complete` job");
  if (jobText(ci, "rust-test")) problems.push("ci.yml still defines an unsharded `rust-test` job alongside the sharded OS jobs");

  const allRuns = ci.split("\n").filter((line) => /\bcargo nextest run\b/.test(line));
  if (allRuns.length !== SHARDED_OSES.length) {
    problems.push(`expected ${SHARDED_OSES.length} \`cargo nextest run\` lines in ci.yml, found ${allRuns.length}`);
  }

  for (const { key, label } of SHARDED_OSES) {
    const shardsId = `rust-test-${key}`;
    const extrasId = `rust-test-${key}-extras`;
    const shards = jobText(ci, shardsId);
    const extras = jobText(ci, extrasId);
    if (!shards) problems.push(`ci.yml has no \`${shardsId}\` job`);
    if (!extras) problems.push(`ci.yml has no \`${extrasId}\` job`);

    const runs = shards.split("\n").filter((line) => /\bcargo nextest run\b/.test(line));
    if (runs.length !== 1) problems.push(`${shardsId}: expected exactly one \`cargo nextest run\` line, found ${runs.length}`);
    const run = runs[0] ?? "";
    const denominator = /--partition hash:\$\{\{ matrix\.shard \}\}\/(\d+)/.exec(run)?.[1];
    if (!denominator) problems.push(`${shardsId}: the nextest run is not \`--partition hash:\${{ matrix.shard }}/N\`, so one job would run the whole suite`);
    if (!/--profile ci\b/.test(run)) problems.push(`${shardsId}: the nextest run does not use \`--profile ci\`, which is what keeps a shard serial`);
    if (!run.includes("--no-fail-fast")) problems.push(`${shardsId}: the nextest run can stop at its first failure`);

    // The matrix axis: a plain list, so the release-gate rule can read it.
    const axis = /\n\s+shard: \[([^\]]*)\]/.exec(shards)?.[1];
    const listed = (axis ?? "").split(",").map((v) => Number(v.trim())).filter((n) => Number.isInteger(n)).sort((a, b) => a - b);
    const count = Number(denominator);
    if (axis === undefined || !denominator || listed.join(",") !== Array.from({ length: count }, (_, i) => i + 1).join(",")) {
      problems.push(`${shardsId}: the shard axis [${listed}] is not exactly 1..${denominator ?? "N"}`);
    }

    // Every other spelling of the count.
    const named = new RegExp(`name: Rust Tests \\(${label}, shard \\$\\{\\{ matrix\\.shard \\}\\}\\/(\\d+)\\)`).exec(shards)?.[1];
    if (named !== denominator) problems.push(`${shardsId}: the job name says /${named}, the partition /${denominator}`);
    const check = complete.split("\n").find((line) => line.includes("scripts/check-shard-coverage.mjs") && line.includes(`list-${key}/nextest-list.json`)) ?? "";
    if (!check) problems.push(`rust-test-complete: no step runs scripts/check-shard-coverage.mjs for ${key}`);
    const checked = /--shards (\d+)/.exec(check)?.[1];
    if (checked !== denominator) problems.push(`${shardsId}: the completeness check expects ${checked} shards, the partition has ${denominator}`);
    const reportRe = new RegExp(`junit-${key}-shard-(\\d+)\\/junit\\.xml`, "g");
    const reports = [...check.matchAll(reportRe)].map((m) => Number(m[1])).sort((a, b) => a - b);
    if (reports.join(",") !== listed.join(",")) problems.push(`${shardsId}: the completeness check reads reports [${reports}], the matrix runs shards [${listed}]`);
    const uploadRe = new RegExp(`name: junit-${key}-shard-\\$\\{\\{ matrix\\.shard \\}\\}`, "g");
    if ((shards.match(uploadRe) ?? []).length !== 1) problems.push(`${shardsId}: the shards do not each upload a \`junit-${key}-shard-<n>\` report`);

    // The completeness job waits for every job that feeds it.
    if (!new RegExp(`needs: \\[[^\\]]*\\b${shardsId}\\b[^\\]]*\\]`).test(complete) || !new RegExp(`needs: \\[[^\\]]*\\b${extrasId}\\b[^\\]]*\\]`).test(complete)) {
      problems.push(`rust-test-complete does not need both ${shardsId} and ${extrasId}`);
    }
    if (!extras.includes(`name: nextest-list-${key}`)) problems.push(`${extrasId} does not upload the suite list \`nextest-list-${key}\``);
    if (!complete.includes(`name: nextest-list-${key}`)) problems.push(`rust-test-complete does not download \`nextest-list-${key}\``);
    if (!/shell: bash\n\s+run: cargo nextest list /.test(extras)) problems.push(`${extrasId} does not write nextest-list.json through bash (UTF-8)`);

    // nextest pinned in both jobs for this OS.
    if (!/tool: cargo-nextest@\d+\.\d+\.\d+/.test(shards) || !/tool: cargo-nextest@\d+\.\d+\.\d+/.test(extras)) {
      problems.push(`cargo-nextest is not pinned to a version in both ${label} jobs`);
    }
  }

  // The completeness job never skips on its own: a skipped job reads as a pass to release-state.mjs.
  if (/^    if:/m.test(complete)) problems.push("rust-test-complete has a job-level `if`, which can skip it and leave the run green");

  for (const line of ci.split("\n").filter((l) => /\bcargo test --manifest-path src-tauri\/Cargo\.toml --locked/.test(l))) {
    if (!line.includes("--test-threads=1")) problems.push("a `cargo test` gate line lost `--test-threads=1`");
  }

  const profile = /\n\[profile\.ci\]\n([^[]*)/.exec(nextest)?.[1] ?? "";
  if (!/^test-threads\s*=\s*1\s*$/m.test(profile)) problems.push("[profile.ci] is not `test-threads = 1`");
  if (!/^fail-fast\s*=\s*false\s*$/m.test(profile)) problems.push("[profile.ci] does not run past a failure");
  if (!/\[profile\.ci\.junit\]\s*\npath = "junit\.xml"/.test(nextest)) problems.push("[profile.ci.junit] does not write junit.xml, which the shards upload");
  return problems;
}

/** Expand every non-dispatch-only job's rendered GitHub Actions display names from a workflow file. */
function expectedPushJobNames(yamlText: string): string[] {
  const root = parseWorkflowYaml(yamlText);
  if (!(root instanceof Map)) throw new Error("workflow root must be a mapping");
  const jobs = root.get("jobs");
  if (!(jobs instanceof Map)) throw new Error("workflow has no jobs mapping");
  const names: string[] = [];
  for (const [jobId, job] of jobs) {
    if (!(job instanceof Map)) continue;
    const jobIf = job.get("if");
    if (jobIf && !Array.isArray(jobIf) && !(jobIf instanceof Map) && jobIf.kind === "scalar" && jobIf.value.includes("workflow_dispatch")) {
      continue;
    }
    const nameNode = job.get("name");
    const template = nameNode && !Array.isArray(nameNode) && !(nameNode instanceof Map) && nameNode.kind === "scalar"
      ? nameNode.value
      : jobId;
    const strategy = job.get("strategy");
    const matrix = strategy instanceof Map ? strategy.get("matrix") : undefined;
    let combos: Record<string, string>[] = [{}];
    if (matrix instanceof Map) {
      for (const [axis, axisNode] of matrix) {
        let values: string[] = [];
        if (Array.isArray(axisNode)) {
          values = axisNode.flatMap((item) => (!Array.isArray(item) && !(item instanceof Map) && item.kind === "scalar" ? [item.value] : []));
        } else if (!(axisNode instanceof Map) && axisNode.kind === "flow" && axisNode.items) {
          values = axisNode.items;
        }
        if (values.length > 0) {
          combos = combos.flatMap((combo) => values.map((val) => ({ ...combo, [axis]: val })));
        }
      }
    }
    for (const combo of combos) {
      const rendered = template.replace(/\$\{\{\s*matrix\.([A-Za-z_][\w-]*)\s*\}\}/g, (_, axis) => combo[axis] ?? "");
      names.push(rendered);
    }
  }
  return names.sort();
}

describe("ci shards contract", () => {
  const ci = read(".github/workflows/ci.yml");
  const coverage = read(".github/workflows/coverage.yml");
  const nextest = read("src-tauri/.config/nextest.toml");

  it("spells the shard count the same way everywhere across Linux, Windows, and macOS", () => {
    expect(shardProblems(ci, nextest)).toEqual([]);
  });

  it("keeps release-state.mjs REQUIRED_WORKFLOW_JOBS in exact lockstep with ci.yml and coverage.yml", () => {
    expect([...REQUIRED_WORKFLOW_JOBS["ci.yml"]].sort()).toEqual(expectedPushJobNames(ci));
    expect([...REQUIRED_WORKFLOW_JOBS["coverage.yml"]].sort()).toEqual(expectedPushJobNames(coverage));
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
      has(broken("shard: [1, 2, 3, 4]", "shard: [1, 2, 3]"), "not exactly 1..4");
    });
    it("a denominator that grew without the axis", () => {
      expect(broken("hash:${{ matrix.shard }}/3", "hash:${{ matrix.shard }}/4").length).toBeGreaterThan(0);
      expect(broken("hash:${{ matrix.shard }}/4", "hash:${{ matrix.shard }}/5").length).toBeGreaterThan(0);
    });
    it("a completeness check that expects fewer shards", () => {
      has(broken("--shards 3", "--shards 2"), "expects 2 shards");
      has(broken("--shards 4", "--shards 3"), "expects 3 shards");
    });
    it("a run that is not partitioned", () => {
      has(broken(/ --partition hash:\$\{\{ matrix\.shard \}\}\/3/, ""), "whole suite");
      has(broken(/ --partition hash:\$\{\{ matrix\.shard \}\}\/4/, ""), "whole suite");
    });
    it("a completeness job that can skip itself", () => {
      has(broken("  rust-test-complete:\n    name:", "  rust-test-complete:\n    if: ${{ !cancelled() }}\n    name:"), "job-level `if`");
    });
    it("a completeness job that no longer waits for one OS's extras", () => {
      has(broken(", rust-test-windows-extras", ""), "does not need both rust-test-windows and rust-test-windows-extras");
    });
    it("a profile that is no longer serial", () => {
      has(shardProblems(ci, nextest.replace(/(\[profile\.ci\]\ntest-threads = )1/, "$14")), "test-threads = 1");
    });
  });
});
