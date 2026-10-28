import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// ADVERSARY (W5a, ft-67f95e2d item 5): `bench-compare` fails a run on a changed
// work counter or a time over the ratio on a gated row. Both new benches have
// every time row `.ungated()`, so the counters are the whole gate. A counter
// that is a constant of the bench's own input (`NOTICES`, `SESSIONS`, `CALLS`,
// the payload length) cannot move when the code under test gets slower or stops
// delivering, so a bench whose counters are all inputs gates nothing.
//
// Measured, 2026-10-27 (docs/PERFORMANCE.md is not edited by this test):
// `alerts::try_offer` slowed by a 200 us sleep (119 ms -> 15.3 s at min) and
// `gitpulse-hook` by a 100 ms sleep (7.5 ms -> 119 ms at min) both compared
// "OK: 2 benches, 8 rows within 1.5x" against a baseline of the clean run.

const benches = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "benches");

/** What a counter's value may be and still be a property of the bench's input. */
const INPUT_ONLY = /^(?:[A-Z][A-Z0-9_]*|stdin_bytes|\w+\.len\(\))(?: as u64)?$/;

function counters(source: string): { name: string; value: string }[] {
  return [...source.matchAll(/\.counter\(\s*"([^"]+)"\s*,\s*([^)]*?)\)(?=\s*[.;])/g)].map((m) => ({ name: m[1], value: m[2].trim() }));
}

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const perf = readFileSync(join(root, "docs", "PERFORMANCE.md"), "utf8");
const workflow = readFileSync(join(root, ".github", "workflows", "benches.yml"), "utf8");

describe("docs/PERFORMANCE.md says what benches.yml does", () => {
  it("does not say the scheduled job fails on a runner with no committed baseline", () => {
    // The compare step prints a notice and exits 0 when the file is absent (4797eb40).
    expect(workflow).toMatch(/Bench baseline not committed[\s\S]*?exit 0/);
    expect(perf).not.toMatch(/scheduled job fails loudly on those classes/);
  });

  it("gives the notify bench the card its own section is filed under", () => {
    const heading = /### `gitpulse-hook notify` by stdin size \((S1-\d+)\)/.exec(perf)?.[1];
    const row = /^\| `notify_stdin_bench` \|.*\((S1-\d+)\) \|$/m.exec(perf)?.[1];
    expect(heading, "the notify section lost its card tag").toBeDefined();
    expect(row, "the notify bench row carries no card tag").toBeDefined();
    expect(row).toBe(heading);
  });
});

describe("a registered bench that is entirely ungated must gate something that can move", () => {
  for (const bench of ["alerts_worker_throughput", "notify_stdin_bench"]) {
    it(`${bench} has a gated counter that is measured, not an input of the bench`, () => {
      const source = readFileSync(join(benches, `${bench}.rs`), "utf8");
      const all = counters(source);
      expect(all.length, `${bench} declares no counters`).toBeGreaterThan(0);
      const ungated = source.includes(".ungated()");
      const measured = all.filter((counter) => !INPUT_ONLY.test(counter.value));
      expect(
        measured,
        `${bench}: every gated counter is a constant of the bench's input (${all.map((c) => `${c.name}=${c.value}`).join(", ")}), ` +
          `and its time rows are ${ungated ? "ungated" : "gated"}, so bench-compare cannot fail it`,
      ).not.toHaveLength(0);
    });
  }
});
