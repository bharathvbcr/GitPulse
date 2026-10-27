import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CardVerdict from "./CardVerdict.svelte";
import { VERDICT_LABELS, type CardVerdict as Verdict } from "../workbench/verification";

const STATES: Verdict["state"][] = ["running", "passed", "failed", "degraded", "could_not_run"];
const html = (verdict: Verdict) => render(CardVerdict, { props: { verdict } }).body;

describe("the verdict chip on a board card", () => {
  it.each(STATES)("renders %s with its own label", (state) => {
    const out = html({ state, stale: false });
    expect(out).toContain(`data-testid="card-verdict" data-state="${state}"`);
    expect(out).toContain(VERDICT_LABELS[state]);
    expect(out).not.toContain("earlier commit");
  });

  it("keeps all five states visibly distinct", () => {
    expect(new Set(STATES.map((state) => VERDICT_LABELS[state])).size).toBe(STATES.length);
    expect(html({ state: "could_not_run", stale: false })).not.toMatch(/>(Passed|Failed)</);
  });

  it("marks a verdict that is for an earlier commit, for readers and for assistive tech", () => {
    const out = html({ state: "passed", stale: true });
    expect(out).toContain('data-stale="true"');
    expect(out).toContain("(for an earlier commit)");
  });
});

describe("the board reads verdicts once per load, batched, and says when it cannot", () => {
  const board = readFileSync(new URL("./TaskBoard.svelte", import.meta.url), "utf8");
  // One read per load, through getBoardVerdicts: it splits a board past one
  // host batch (256 cards, after Load more or Load all) into reads of 256.
  it("makes one batched call per load and no per-card read", () => {
    expect(board.match(/getBoardVerdicts\(/g)).toHaveLength(1);
    expect(board).not.toMatch(/\bgetVerdicts\(/);
    expect(board).not.toMatch(/getVerification\(/);
  });
  it("draws the chip on the board card and the list row, and an error line when the read fails", () => {
    expect(board.match(/<CardVerdict /g)).toHaveLength(2);
    expect(board).toContain('data-testid="board-verdicts-error"');
  });
});
