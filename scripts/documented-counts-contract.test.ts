import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { parseRegisteredHandlers, DEFAULT_LIB_RS } from "./check-ipc-contract.mjs";
import * as checkCoverageTypes from "./check-coverage-types.mjs";
import { REGISTERED_VIEWS } from "../src/lib/views/viewRegistry";

/**
 * README, CONTRIBUTING and ARCHITECTURE state precise counts — 95 IPC
 * handlers, 62 compared fields, 13 views. Numbers in prose rot silently: the
 * Rust test count sat at "~200+" while the real figure was 890.
 *
 * Each count is taken from the canonical implementation rather than recounted
 * here, so this cannot drift from what the checkers actually measure.
 */
const DOCS = ["README.md", "CONTRIBUTING.md", "docs/ARCHITECTURE.md"] as const;

function read(relative: string): string {
  return readFileSync(new URL(`../${relative}`, import.meta.url), "utf8");
}

/** Every distinct number asserted about `thing` across the docs. */
function claimedCounts(pattern: RegExp): Map<string, Set<number>> {
  const found = new Map<string, Set<number>>();
  for (const doc of DOCS) {
    for (const match of read(doc).matchAll(pattern)) {
      const value = Number(match[1].replace(/,/g, ""));
      if (!Number.isFinite(value)) continue;
      if (!found.has(doc)) found.set(doc, new Set());
      found.get(doc)?.add(value);
    }
  }
  return found;
}

function expectAllClaim(pattern: RegExp, actual: number, label: string): void {
  const claims = claimedCounts(pattern);
  expect(claims.size, `no document states a ${label} count`).toBeGreaterThan(0);
  for (const [doc, values] of claims) {
    for (const value of values) {
      expect(value, `${doc} claims ${value} ${label}; the code has ${actual}`).toBe(actual);
    }
  }
}

describe("documented counts match the code", () => {
  it("states the real number of IPC handlers", () => {
    // Counted by the IPC checker's own parser, not by a second regex here.
    const { handlers, errors } = parseRegisteredHandlers(readFileSync(DEFAULT_LIB_RS, "utf8"));
    expect(errors.length, "the registry must parse cleanly for the count to mean anything").toBe(0);
    const actual = handlers.size;
    expect(actual).toBeGreaterThan(50);
    expectAllClaim(/(\d+)\s+(?:Rust commands|Registered Handlers|Handlers|handlers)/g, actual, "handler");
  });

  it("states the real number of registered views", () => {
    expectAllClaim(/(\d+)\s+(?:application )?views/g, REGISTERED_VIEWS.length, "view");
  });

  it("states the real number of type-checked fields", () => {
    // Sum the contracts the checker actually compares, via its own JSON mode.
    const logged: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => void logged.push(args.join(" "));
    let code: number;
    try {
      code = checkCoverageTypes.main(["--json"]);
    } finally {
      console.log = original;
    }
    expect(code).toBe(0);
    const reports = JSON.parse(logged.join("\n")) as Array<{ fieldCount?: number }>;
    const actual = reports.reduce((sum, report) => sum + (report.fieldCount ?? 0), 0);
    expect(actual, "the checker should report a field count").toBeGreaterThan(0);
    expectAllClaim(/(\d+)\s+(?:data )?fields/g, actual, "field");
  });

  it("states test-suite sizes as floors, so growth does not make them false", () => {
    // "1,890+" went stale as an exact-ish figure; a floor stays true while the
    // suite grows and only breaks if coverage genuinely shrinks.
    const contributing = read("CONTRIBUTING.md");
    expect(contributing).toContain("2,000+ tests");
    expect(contributing).toContain("850+ tests");
    expect(contributing).not.toContain("~200+");
  });
});
