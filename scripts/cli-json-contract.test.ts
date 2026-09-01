import { describe, expect, it } from "vitest";
import * as checkIpcContract from "./check-ipc-contract.mjs";
import * as checkCoverageTypes from "./check-coverage-types.mjs";
import * as checkReleaseVersion from "./check-release-version.mjs";
import * as checkCoverageFloor from "./check-coverage-floor.mjs";

/**
 * Backlog A1. `--json` must emit the result object the checker already builds,
 * suppress the text report, and leave exit codes identical: 0 holds,
 * 1 violated, 2 internal error. CI cannot annotate a pull request by
 * re-parsing prose.
 */
const CHECKERS: Array<[string, { main: (argv: string[]) => number }]> = [
  ["check-ipc-contract", checkIpcContract],
  ["check-coverage-types", checkCoverageTypes],
  ["check-release-version", checkReleaseVersion],
  ["check-coverage-floor", checkCoverageFloor],
];

function capture(run: () => number): { code: number; text: string } {
  const logged: string[] = [];
  const original = console.log;
  console.log = (...args: unknown[]) => void logged.push(args.join(" "));
  try {
    return { code: run(), text: logged.join("\n") };
  } finally {
    console.log = original;
  }
}

describe("CLI --json contract", () => {
  for (const [name, module] of CHECKERS) {
    it(`${name} emits parseable JSON and suppresses the text report`, () => {
      const json = capture(() => module.main(["--json"]));
      const text = capture(() => module.main([]));

      expect(json.code).toBe(text.code);
      const parsed = JSON.parse(json.text);
      const records = Array.isArray(parsed) ? parsed : [parsed];
      expect(records.length).toBeGreaterThan(0);
      for (const record of records) expect(typeof record.ok).toBe("boolean");

      // the human report must not leak into the machine-readable stream
      expect(json.text).not.toContain("OK:");
      expect(json.text).not.toContain("FAIL:");
      expect(text.text).not.toContain('"ok":');
    });
  }

  it("keeps exit codes identical in both modes when the contract is violated", () => {
    // A tag no manifest names is a violation, not an internal error.
    const text = capture(() => checkReleaseVersion.main(["--tag", "v9.9.9"]));
    const json = capture(() => checkReleaseVersion.main(["--tag", "v9.9.9", "--json"]));
    expect(text.code).toBe(1);
    expect(json.code).toBe(1);
    expect(JSON.parse(json.text).ok).toBe(false);
  });

  it("keeps exit codes identical in both modes for an internal error", () => {
    // A bad flag cannot be reported in the result object, so it is the 2 case.
    // (An unreadable manifest is deliberately *not* one: the checker reports
    // it as a violation with <unreadable> values and exits 1.)
    const original = console.error;
    console.error = () => {};
    try {
      expect(checkReleaseVersion.main(["--bogus-flag"])).toBe(2);
      expect(checkReleaseVersion.main(["--bogus-flag", "--json"])).toBe(2);
      expect(checkIpcContract.main(["--bogus-flag", "--json"])).toBe(2);
    } finally {
      console.error = original;
    }
  });

  it("reports an unreadable root as a violation in both modes, not a crash", () => {
    const text = capture(() => checkReleaseVersion.main(["--root", "/nonexistent/repo"]));
    const json = capture(() => checkReleaseVersion.main(["--root", "/nonexistent/repo", "--json"]));
    expect(text.code).toBe(1);
    expect(json.code).toBe(1);
    expect(JSON.parse(json.text).ok).toBe(false);
  });

  it("keeps an unreadable coverage report visible in JSON rather than absent", () => {
    // The floor checker's whole point is that a report which could not be read
    // never looks like one that passed; --json must carry that too.
    const original = console.error;
    console.error = () => {};
    let text = "";
    const log = console.log;
    console.log = (...args: unknown[]) => void (text += args.join(" "));
    try {
      const code = checkCoverageFloor.main(["--frontend", "/nonexistent/lcov.info", "--json"]);
      expect(code).toBe(2);
      const parsed = JSON.parse(text);
      expect(parsed.ok).toBe(false);
      expect(parsed.invalid).toBe(true);
      expect(parsed.reports.some((r: { ok: boolean }) => r.ok === false)).toBe(true);
    } finally {
      console.error = original;
      console.log = log;
    }
  });
});
