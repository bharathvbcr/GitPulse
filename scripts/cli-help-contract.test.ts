import { describe, expect, it } from "vitest";
import * as checkIpcContract from "./check-ipc-contract.mjs";
import * as checkCoverageTypes from "./check-coverage-types.mjs";
import * as checkReleaseVersion from "./check-release-version.mjs";
import * as checkReleaseAssets from "./check-release-assets.mjs";
import * as checkCoverageFloor from "./check-coverage-floor.mjs";
import * as releaseNotes from "./release-notes.mjs";

/**
 * Backlog A2. Every script entry point must answer `--help` with usage and
 * exit 0 — the distinction between "helped" and "failed" is the point, so the
 * exit code is asserted, not just the output.
 */
const ENTRY_POINTS: Array<[string, { main: (argv: string[]) => number }]> = [
  ["check-ipc-contract", checkIpcContract],
  ["check-coverage-types", checkCoverageTypes],
  ["check-release-version", checkReleaseVersion],
  ["check-release-assets", checkReleaseAssets],
  ["check-coverage-floor", checkCoverageFloor],
  ["release-notes", releaseNotes],
];

function captureLog(run: () => number): { code: number; text: string } {
  const logged: string[] = [];
  const original = console.log;
  console.log = (...args: unknown[]) => void logged.push(args.join(" "));
  try {
    return { code: run(), text: logged.join("\n") };
  } finally {
    console.log = original;
  }
}

describe("CLI help contract", () => {
  for (const [name, module] of ENTRY_POINTS) {
    it(`${name} answers --help with usage and exit 0`, () => {
      const long = captureLog(() => module.main(["--help"]));
      const short = captureLog(() => module.main(["-h"]));
      expect(long.code).toBe(0);
      expect(short.code).toBe(0);
      expect(long.text).toContain("Usage:");
      expect(long.text.length).toBeGreaterThan(40);
    });
  }

  it("still rejects an unknown flag as an error, not as help", () => {
    const original = console.error;
    console.error = () => {};
    try {
      expect(checkIpcContract.main(["--not-a-flag"])).toBe(2);
    } finally {
      console.error = original;
    }
  });
});
