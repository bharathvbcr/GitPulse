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

export function devmapPinProblems(ci: string): string[] {
  const problems: string[] = [];

  const pins = [...ci.matchAll(/^ {2}DEVMAP_REV: (\S+)\s*$/gm)].map((m) => m[1]);
  if (pins.length !== 1) problems.push(`expected exactly one DEVMAP_REV in ci.yml, found ${pins.length}`);
  else if (!/^[0-9a-f]{40}$/.test(pins[0])) problems.push(`DEVMAP_REV ${pins[0]} is not a full 40-character commit SHA`);

  for (const line of ci.split("\n").filter((l) => l.includes("cargo install --git"))) {
    if (!line.includes('--rev "$DEVMAP_REV"')) problems.push(`an install does not use --rev "$DEVMAP_REV": ${line.trim()}`);
    if (!line.includes("--locked")) problems.push(`an install is not --locked: ${line.trim()}`);
  }

  const installing = [...ci.matchAll(/\n {2}([a-z][a-z-]*):\n/g)].map((m) => m[1]).filter((id) => jobText(ci, id).includes("name: Install devmap"));
  if (installing.length === 0) problems.push("no job installs devmap");
  for (const id of installing) {
    const job = jobText(ci, id);
    const install = job.indexOf("name: Install devmap");
    const check = job.indexOf("bun run check:vendor-schema");
    if (check === -1) problems.push(`${id} installs devmap but never runs check:vendor-schema`);
    else if (check < install) problems.push(`${id} runs check:vendor-schema before it installs devmap`);
    if (!/key: devmap-.*\$\{\{ env\.DEVMAP_REV \}\}/.test(job)) problems.push(`${id} caches devmap on a key that does not include DEVMAP_REV`);
  }
  return problems;
}

describe("devmap pin contract", () => {
  const ci = read(".github/workflows/ci.yml");

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
      has(broken(/key: devmap-\$\{\{ runner\.os \}\}-\$\{\{ runner\.arch \}\}-\$\{\{ env\.DEVMAP_REV \}\}/, "key: devmap-${{ runner.os }}"), "does not include DEVMAP_REV");
    });
  });
});
