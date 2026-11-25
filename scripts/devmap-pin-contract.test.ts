import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * CI builds `devmap` from DevCouncil for the tests that drive the CLI (the
 * worktree-overlay and agent-bus radar cases fail with "devmap is not
 * installed" on a runner without it). The build is pinned to one commit, held
 * in `DEVMAP_REV`, and that pin is only worth anything if three things stay
 * true together: it is a full commit SHA (a branch or abbreviation moves under
 * the cache key), every install uses it, and every job that installs the CLI
 * also asks `check:vendor-schema` whether it still matches the vendored crates,
 * because a stale pin would otherwise pass quietly until a schema bump.
 *
 * Each rule is a function over the workflow text, so the self-test can show it
 * failing on the shape it is for.
 */
const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

/** The text of one top-level job. */
function jobText(ci: string, id: string): string {
  return new RegExp(`\\n  ${id}:\\n([\\s\\S]*?)(?=\\n  [a-z][a-z-]*:\\n|$)`).exec(ci)?.[1] ?? "";
}

/** The workflow's one top-level `DEVMAP_REV`, or undefined when it has none or several. */
function pinOf(workflow: string): string | undefined {
  const pins = [...workflow.matchAll(/^ {2}DEVMAP_REV: (\S+)\s*$/gm)].map((m) => m[1]);
  return pins.length === 1 ? pins[0] : undefined;
}

export function devmapPinProblems(ci: string, file = "ci.yml"): string[] {
  const problems: string[] = [];

  const pins = [...ci.matchAll(/^ {2}DEVMAP_REV: (\S+)\s*$/gm)].map((m) => m[1]);
  if (pins.length !== 1) problems.push(`expected exactly one DEVMAP_REV in ${file}, found ${pins.length}`);
  else if (!/^[0-9a-f]{40}$/.test(pins[0])) problems.push(`DEVMAP_REV ${pins[0]} is not a full 40-character commit SHA`);

  for (const line of ci.split("\n").filter((l) => l.includes("cargo install --git"))) {
    if (!line.includes('--rev "$DEVMAP_REV"')) problems.push(`an install does not use --rev "$DEVMAP_REV": ${line.trim()}`);
    if (!line.includes("--locked")) problems.push(`an install is not --locked: ${line.trim()}`);
  }

  const installing = [...ci.matchAll(/\n {2}([a-z][a-z-]*):\n/g)].map((m) => m[1]).filter((id) => jobText(ci, id).includes("name: Install devmap"));
  if (installing.length === 0) problems.push("no job installs devmap");
  // The worktree crate's cli_e2e tests drive the real CLI, so a job that runs them needs it too.
  for (const id of [...ci.matchAll(/\n {2}([a-z][a-z-]*):\n/g)].map((m) => m[1])) {
    const runsWorktreeCrate = /cargo test --manifest-path crates\/gitpulse-worktree\/Cargo\.toml/.test(jobText(ci, id));
    if (runsWorktreeCrate && !installing.includes(id)) problems.push(`${id} runs the worktree crate's tests without installing devmap`);
  }
  for (const id of installing) {
    const job = jobText(ci, id);
    const install = job.indexOf("name: Install devmap");
    const check = job.indexOf("bun run check:vendor-schema");
    if (check === -1) problems.push(`${id} installs devmap but never runs check:vendor-schema`);
    else if (check < install) problems.push(`${id} runs check:vendor-schema before it installs devmap`);
    // The restore and the save each name a key, and the two must both move with the pin.
    const keys = job.split("\n").filter((l) => /^\s+key: devmap-/.test(l));
    if (keys.length === 0 || keys.some((l) => !/\$\{\{ env\.DEVMAP_REV \}\}/.test(l))) problems.push(`${id} caches devmap on a key that does not include DEVMAP_REV`);
    // `actions/cache` saves in a post step that runs only if the whole job passed. These jobs'
    // suites are red on a runner more often than not, so the verified build is saved on its own.
    if (/uses: actions\/cache@/.test(job)) problems.push(`${id} caches devmap with actions/cache, which saves only when the whole job succeeds`);
    const save = job.indexOf("actions/cache/save@");
    if (save === -1) problems.push(`${id} never saves its devmap build with actions/cache/save`);
    else if (check !== -1 && save < check) problems.push(`${id} saves its devmap build before check:vendor-schema has passed`);
  }
  return problems;
}

/**
 * coverage.yml runs the same lib suite, instrumented, so it needs the same CLI.
 * A workflow cannot read another's env, so it carries its own copy of the pin,
 * and the copy is only safe while it equals ci.yml's: a stale one would build
 * an older CLI under a cache key ci.yml never writes.
 */
export function coverageDevmapProblems(coverage: string, ci: string): string[] {
  const problems = devmapPinProblems(coverage, "coverage.yml");
  const own = pinOf(coverage);
  const gating = pinOf(ci);
  if (own !== undefined && gating !== undefined && own !== gating) {
    problems.push(`coverage.yml's DEVMAP_REV ${own} differs from ci.yml's ${gating}`);
  }
  const install = coverage.indexOf("name: Install devmap");
  // The step, not the words: a comment near the top already mentions `cargo llvm-cov`.
  const suite = coverage.indexOf("name: Generate Rust Coverage");
  if (suite === -1) problems.push("coverage.yml has no 'Generate Rust Coverage' step; this contract needs updating");
  else if (install === -1 || install > suite) problems.push("coverage.yml runs the instrumented suite before it installs devmap");
  return problems;
}

describe("devmap pin contract", () => {
  const ci = read(".github/workflows/ci.yml");
  const coverage = read(".github/workflows/coverage.yml");

  it("coverage.yml installs the same pinned devmap, checked and kept, before its instrumented suite", () => {
    expect(coverageDevmapProblems(coverage, ci)).toEqual([]);
  });

  describe("the coverage rules fail on the shapes they are for", () => {
    const has = (problems: string[], text: string) => expect(problems).toEqual(expect.arrayContaining([expect.stringContaining(text)]));
    const mutate = (from: string | RegExp, to: string) => {
      const mutated = coverage.replace(from, to);
      expect(mutated, `the mutation ${String(from)} matched nothing`).not.toBe(coverage);
      return mutated;
    };

    it("a copy of the pin that drifted from ci.yml's", () => {
      has(coverageDevmapProblems(mutate(/DEVMAP_REV: [0-9a-f]{40}/, `DEVMAP_REV: ${"0".repeat(40)}`), ci), "differs from ci.yml's");
    });
    it("a coverage job with no devmap install", () => {
      has(coverageDevmapProblems(mutate("name: Install devmap", "name: Fetch devmap"), ci), "before it installs devmap");
    });
    it("an install that ignores the pin or is never checked", () => {
      has(coverageDevmapProblems(mutate('--rev "$DEVMAP_REV" ', ""), ci), 'does not use --rev "$DEVMAP_REV"');
      has(coverageDevmapProblems(mutate(/\n {6}- name: Installed devmap matches the vendored schema\n {8}run: bun run check:vendor-schema\n/, "\n"), ci), "never runs check:vendor-schema");
    });
  });

  it("pins devmap to one full commit, installs it from that pin, and checks it against the vendored schema", () => {
    expect(devmapPinProblems(ci)).toEqual([]);
  });

  it("builds from the commit the vendored devmap-store schema number comes from", () => {
    // The pin's schema is not readable here (DevCouncil is a different repository),
    // so the number the workflow comment states is held against the vendored one.
    const vendored = /pub const CURRENT_SCHEMA_VERSION:\s*i32\s*=\s*(\d+)/.exec(read("src-tauri/vendored/devmap-store/src/schema.rs"))?.[1];
    const stated = /DevCouncil commit `devmap` is built from[\s\S]*?store schema (\d+)/.exec(ci)?.[1];
    expect(vendored, "vendored schema number not found").toBeDefined();
    expect(stated, `ci.yml states schema ${stated}, the vendored crates are at ${vendored}`).toBe(vendored);
  });

  describe("the rules fail on the shapes they are for", () => {
    const broken = (from: string | RegExp, to: string) => {
      const mutated = ci.replace(from, to);
      expect(mutated, `the mutation ${String(from)} matched nothing`).not.toBe(ci);
      return devmapPinProblems(mutated);
    };
    const has = (problems: string[], text: string) => expect(problems).toEqual(expect.arrayContaining([expect.stringContaining(text)]));

    it("an abbreviated or branch pin", () => {
      has(broken(/DEVMAP_REV: [0-9a-f]{40}/, "DEVMAP_REV: main"), "not a full 40-character");
      has(broken(/DEVMAP_REV: [0-9a-f]{40}/, "DEVMAP_REV: b32d2862"), "not a full 40-character");
    });
    it("an install that ignores the pin", () => {
      has(broken('--rev "$DEVMAP_REV" devmap-cli --locked', "devmap-cli --locked"), 'does not use --rev "$DEVMAP_REV"');
    });
    it("an install that is not --locked", () => {
      has(broken('devmap-cli --locked --root "$root"', 'devmap-cli --root "$root"'), "not --locked");
    });
    it("a job that installs devmap but never checks the schema", () => {
      has(broken(/\n {6}- name: Installed devmap matches the vendored schema\n {8}run: bun run check:vendor-schema\n/, "\n"), "never runs check:vendor-schema");
    });
    it("a cache key that does not move with the pin", () => {
      has(broken(/key: devmap-\$\{\{ runner\.os \}\}-\$\{\{ runner\.arch \}\}-\$\{\{ env\.DEVMAP_REV \}\}/g, "key: devmap-${{ runner.os }}"), "does not include DEVMAP_REV");
      // Only the save's key: a restore on the pin and a save under another key is a cache that never hits.
      has(broken(/(actions\/cache\/save@[^\n]*\n\s+with:\n\s+path: [^\n]*\n\s+key: devmap-)\$\{\{ runner\.os \}\}-\$\{\{ runner\.arch \}\}-\$\{\{ env\.DEVMAP_REV \}\}/, "$1${{ runner.os }}"), "does not include DEVMAP_REV");
    });
    it("a job that runs the worktree crate's tests without devmap", () => {
      has(broken(/(rust-test-macos-extras:[\s\S]*?)- name: Install devmap/, "$1- name: Fetch devmap"), "runs the worktree crate's tests without installing devmap");
    });
    it("a cache that is saved only when the whole job passes", () => {
      has(broken("uses: actions/cache/restore@", "uses: actions/cache@"), "saves only when the whole job succeeds");
    });
    it("a build that is restored but never saved, or saved before it is checked", () => {
      has(broken(/\n {6}- name: Keep the verified devmap build\n[\s\S]*?\n\n/, "\n\n"), "never saves its devmap build");
      has(broken(/(\n {6}- name: Cache devmap\n)/, "\n      - name: Early\n        uses: actions/cache/save@x\n$1"), "before check:vendor-schema has passed");
    });
  });
});
