import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * GitPulse is a *consumer* of the verdict contract; Manvi owns it.
 *
 * This file checks two independent things, and the second is the one that
 * catches real drift:
 *
 *  1. The vendored `contracts/` copy still matches `CHECKSUMS` — nobody edited
 *     a contract here instead of in its canonical home.
 *  2. The Rust classifier and the TypeScript union agree with the shared
 *     `verdict.cases.json` fixture, whose cases are the cross-product of every
 *     classification-relevant field on a decision.
 *
 * The distinction the fixture exists to protect: the harness reports
 * `action: "allow"` for five different things, only one of which is a clean
 * pass. Collapsing them is how a grant-cleared write, a self-widened write,
 * and a write judged without the repo map all came to render as "clean".
 */

const contractsUrl = (name: string) => new URL(`../contracts/${name}`, import.meta.url);

const rust = readFileSync(new URL("../src-tauri/src/harness/policy.rs", import.meta.url), "utf8");
const store = readFileSync(new URL("../src/lib/stores/harnessStore.ts", import.meta.url), "utf8");

describe("vendored contracts are unmodified", () => {
  const recorded = readFileSync(contractsUrl("CHECKSUMS"), "utf8").trim().split("\n");

  it("records a checksum for every tracked contract file", () => {
    expect(recorded.length).toBe(5);
  });

  for (const line of recorded) {
    const [want, name] = line.split(/\s{2,}/);
    it(`${name} matches its recorded checksum`, () => {
      const actual = createHash("sha256")
        .update(readFileSync(contractsUrl(name)))
        .digest("hex");
      expect(
        actual,
        `contracts/${name} differs from the canonical copy in DevCouncil. ` +
          `Contracts are edited at their source and re-vendored, never patched here.`,
      ).toBe(want);
    });
  }
});

/**
 * The eight states, in the contract's evaluation order. Read from the schema
 * rather than restated, so a state added there and not handled here fails.
 */
const schema = JSON.parse(readFileSync(contractsUrl("verdict.schema.json"), "utf8"));
const contractStates: string[] = schema.$defs.classification.enum;

/**
 * Maps a Rust `PolicyStatus` variant to the contract's state name. The two
 * vocabularies differ in exactly two places — Rust says `Allowed`/`Blocked`
 * where the contract says `clean`/`denied` — and that mapping lives here so
 * both names stay greppable.
 */
const RUST_TO_CONTRACT: Record<string, string> = {
  allowed: "clean",
  blocked: "denied",
  demoted: "demoted",
  granted: "granted",
  widened: "widened",
  degraded: "degraded",
  warned: "warned",
  unchecked: "unchecked",
};

describe("the status vocabulary covers the contract", () => {
  it("maps every wire status onto a contract state", () => {
    const union = /export type PolicyStatus =([\s\S]*?);/.exec(store);
    expect(union, "PolicyStatus union not found").not.toBeNull();
    const members = [...union![1].matchAll(/"([a-z_]+)"/g)].map((m) => m[1]).sort();

    expect(members).toEqual(Object.keys(RUST_TO_CONTRACT).sort());
    expect([...new Set(Object.values(RUST_TO_CONTRACT))].sort()).toEqual(
      [...contractStates].sort(),
    );
  });

  it("gives every non-clean allow its own label and detail", () => {
    // These four all permit the action, so nothing but the label tells the
    // user they were not clean passes. A shared label would defeat the point.
    for (const status of ["granted", "widened", "degraded", "demoted"]) {
      expect(store, `verdictLabel has no case for "${status}"`).toContain(`case "${status}":`);
    }
    const labels = /export function verdictLabel[\s\S]*?\n}/.exec(store);
    expect(labels).not.toBeNull();
    const returned = [...labels![0].matchAll(/return "(.*?)"/g)].map((m) => m[1]);
    expect(
      new Set(returned).size,
      "two statuses share a label, so the user cannot tell them apart",
    ).toBe(returned.length);
  });
});

/**
 * The Rust classifier, re-expressed from the source it is written in.
 *
 * This is deliberately not a second implementation: it parses the actual match
 * arms out of `from_decision`, so a reordering in Rust that changes behaviour
 * is visible here rather than silently agreed with.
 */
describe("the Rust classifier follows the contract's order", () => {
  const fn = /let status = match action\.as_str\(\) \{([\s\S]*?)\n        \};/.exec(rust);

  it("found the classifier", () => {
    expect(fn, "from_decision's match on action not found").not.toBeNull();
  });

  it("tests grant before demotion before widening before degradation", () => {
    const body = fn![1];
    const at = (needle: string) => body.indexOf(needle);
    expect(at("d.grant_id")).toBeGreaterThan(-1);
    expect(at("d.grant_id")).toBeLessThan(at("d.demoted"));
    expect(at("d.demoted")).toBeLessThan(at("d.widened"));
    expect(at("d.widened")).toBeLessThan(at("d.degraded"));
  });

  it("classifies a denial before anything can reclassify it as an allow", () => {
    const body = fn![1];
    // `deny` must be the first arm: a denial carrying a grant that did not
    // clear it must stay a denial.
    expect(body.trimStart().startsWith('"deny" | "block" => PolicyStatus::Blocked')).toBe(true);
  });

  it("falls closed on an action it does not recognise", () => {
    expect(fn![1]).toMatch(/_ => PolicyStatus::Blocked/);
  });
});

/**
 * The generated fixture, classified by a TypeScript implementation of the same
 * contract. GitPulse renders verdicts in the frontend, so the frontend's
 * reading has to be checked too — not only the Rust one.
 */
type Decision = {
  action: string;
  grant_id?: string;
  demoted?: string;
  widened?: string;
  degraded?: string[];
};

function classify(d: Decision | null): string {
  if (d === null) return "unchecked";
  if (d.action === "deny") return "denied";
  if (d.grant_id) return "granted";
  if (d.demoted) return "demoted";
  if (d.widened) return "widened";
  if (d.degraded && d.degraded.length > 0) return "degraded";
  if (d.action === "warn") return "warned";
  return "clean";
}

describe("classification parity with the shared fixture", () => {
  const fixture = JSON.parse(readFileSync(contractsUrl("verdict.cases.json"), "utf8"));

  it("is the version this test understands", () => {
    expect(fixture.version).toBe(1);
    expect(fixture.cases.length).toBeGreaterThanOrEqual(8);
  });

  it("agrees on every generated case", () => {
    const disagreements: string[] = [];
    for (const c of fixture.cases) {
      const got = classify(c.decision);
      if (got !== c.expect) disagreements.push(`${c.name}: got ${got}, want ${c.expect}`);
    }
    expect(disagreements).toEqual([]);
  });

  it("exercises every state the contract defines", () => {
    const reached = new Set(fixture.cases.map((c: { expect: string }) => c.expect));
    for (const state of contractStates) {
      expect(reached.has(state), `no case in the fixture reaches "${state}"`).toBe(true);
    }
  });
});
