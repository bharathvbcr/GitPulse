// Adversarial probes (W6-L2 reviewer): can `check:types` pass while the TS mirror
// of a `#[serde(flatten)]`ed struct is missing, misnamed or mistyped?
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { CONTRACTS, runTypeCheck } from "./check-coverage-types.mjs";

const dirs: string[] = [];
afterAll(async () => {
  for (const d of dirs) await rm(d, { recursive: true, force: true });
});

const ledger = CONTRACTS.find((c) => c.label === "ledger")!;
const freshness = CONTRACTS.find((c) => c.label === "ledger-freshness")!;

async function mutatedTs(edit: (source: string) => string) {
  const dir = await mkdtemp(path.join(tmpdir(), "gp-flatten-adv-"));
  dirs.push(dir);
  const copy = path.join(dir, "types.ts");
  const source = await readFile(ledger.tsPath, "utf8");
  const next = edit(source);
  expect(next, "the mutation must change the TS mirror").not.toBe(source);
  await writeFile(copy, next);
  return copy;
}

const mutations: Array<[string, (s: string) => string]> = [
  ["drop Freshness.stale_reason", (s) => s.replace("  stale_reason: string;\n", "")],
  ["drop Freshness.freshness_ok", (s) => s.replace("  freshness_ok: boolean;\n", "")],
  ["rename Freshness.stale_after_secs", (s) => s.replace("  stale_after_secs: number;", "  staleAfterSecs: number;")],
  ["retype Freshness.stale to string", (s) => s.replace("  stale: boolean;", "  stale: string;")],
  ["retype Freshness.catch_up (drop null)", (s) => s.replace("  catch_up: CatchUpSummary | null;", "  catch_up: CatchUpSummary;")],
  ["drop LedgerStatus.history_only_bindings", (s) => s.replace("  history_only_bindings: number;\n", "")],
  ["drop `extends Freshness` from LedgerStatus", (s) => s.replace("export interface LedgerStatus extends Freshness {", "export interface LedgerStatus {")],
  ["retype LedgerStatus.recording_state to number", (s) => s.replace("  recording_state: string;", "  recording_state: number;")],
  ["drop CatchUpSummary.stale_worktrees", (s) => s.replace("  stale_worktrees: number;\n", "")],
  ["retype CatchUpSummary.recorded", (s) => s.replace("  recorded: number;", "  recorded: string;")],
];

describe("check:types on a flattened LedgerStatus", () => {
  it("passes on the unmutated tree (control)", () => {
    expect(runTypeCheck(ledger).violations).toEqual([]);
    expect(runTypeCheck(freshness).violations).toEqual([]);
  });

  for (const [name, edit] of mutations) {
    it(`fails the ledger contract or the freshness contract when: ${name}`, async () => {
      const tsPath = await mutatedTs(edit);
      const a = runTypeCheck({ ...ledger, tsPath });
      const b = runTypeCheck({ ...freshness, tsPath });
      expect(
        [...a.violations, ...b.violations],
        `check:types accepted the mutation "${name}" in both contracts`,
      ).not.toEqual([]);
    });

    // Stronger: the contract that OWNS the flatten (LedgerStatus) should itself
    // notice a Freshness-field mutation, not rely on a second contract.
    if (name.includes("Freshness.")) {
      it(`the ledger contract alone fails when: ${name}`, async () => {
        const tsPath = await mutatedTs(edit);
        expect(
          runTypeCheck({ ...ledger, tsPath }).violations,
          `the "ledger" contract accepted "${name}"`,
        ).not.toEqual([]);
      });
    }
  }
});
