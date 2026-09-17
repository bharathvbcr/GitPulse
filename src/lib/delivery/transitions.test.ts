import { describe, expect, it } from "vitest";
import type { MonitorPhase } from "./phase";
import {
  anyInFlight,
  failuresSince,
  settledSince,
  successesSince,
  verdictRate,
  type PhasedRow,
} from "./transitions";

const row = (id: string, phase: MonitorPhase): PhasedRow => ({ id, phase });

const ALL_PHASES: MonitorPhase[] = ["in_flight", "settled_ok", "settled_bad", "unknown"];

describe("the baseline observation announces nothing", () => {
  it("reports no transition when there is no previous snapshot", () => {
    // Mounting the panel must not announce every failure already in the
    // window as though it had just happened.
    expect(settledSince([], [row("1", "settled_bad"), row("2", "settled_ok")])).toEqual([]);
    expect(failuresSince([], [row("1", "settled_bad")])).toEqual([]);
  });

  it("reports no transition for a row appearing already settled", () => {
    // It was never observed moving, so its outcome is history, not news.
    const previous = [row("1", "settled_ok")];
    const next = [row("2", "settled_bad"), row("1", "settled_ok")];
    expect(failuresSince(previous, next)).toEqual([]);
  });
});

describe("what counts as a transition", () => {
  it("detects in-flight becoming a failure", () => {
    const transitions = failuresSince([row("1", "in_flight")], [row("1", "settled_bad")]);
    expect(transitions).toHaveLength(1);
    expect(transitions[0]).toMatchObject({ from: "in_flight", to: "settled_bad" });
    expect(transitions[0].row.id).toBe("1");
  });

  it("detects in-flight becoming a success", () => {
    const transitions = successesSince([row("1", "in_flight")], [row("1", "settled_ok")]);
    expect(transitions).toHaveLength(1);
    expect(transitions[0].to).toBe("settled_ok");
  });

  it("treats in-flight becoming unjudgeable as settled but as neither verdict", () => {
    // A state this build has never heard of stops the poll — it has stopped
    // moving — but it is not a failure to announce and not a success to count.
    const previous = [row("1", "in_flight")];
    const next = [row("1", "unknown")];
    expect(settledSince(previous, next)).toHaveLength(1);
    expect(failuresSince(previous, next)).toEqual([]);
    expect(successesSince(previous, next)).toEqual([]);
  });

  it("does not announce the same outcome twice", () => {
    // Settled-to-settled is not a transition, which is what makes an
    // "already announced" set unnecessary — and that set is the thing that
    // would grow for the lifetime of the window.
    const settled = [row("1", "settled_bad")];
    expect(failuresSince(settled, settled)).toEqual([]);
  });

  it("reports nothing for a row that vanished from the listing", () => {
    // Rows fall off the display cap for reasons unrelated to their outcome.
    expect(settledSince([row("1", "in_flight")], [row("2", "settled_ok")])).toEqual([]);
  });

  it("is keyed by id, not by position", () => {
    // The listing is newest-first and capped, so one new run shifts every row
    // down. An index-to-index comparison reports the whole list as changed on
    // every push.
    const previous = [row("a", "settled_ok"), row("b", "settled_bad"), row("c", "in_flight")];
    const next = [row("z", "in_flight"), row("a", "settled_ok"), row("b", "settled_bad")];
    expect(settledSince(previous, next)).toEqual([]);
  });

  it("survives a duplicated id without crashing or double-reporting", () => {
    const previous = [row("1", "in_flight"), row("1", "settled_ok")];
    const next = [row("1", "settled_bad")];
    // First-wins on the previous snapshot: the listings are newest-first, so
    // the earlier row is the newer one and is what we compare against.
    expect(failuresSince(previous, next)).toHaveLength(1);
  });

  it("never reports a transition into in-flight", () => {
    // A row going backwards means the source re-ran it or we read a stale
    // page. Either way it is not something settling, and announcing it would
    // be announcing an event that did not happen.
    for (const from of ALL_PHASES) {
      expect(settledSince([row("1", from)], [row("1", "in_flight")]), from).toEqual([]);
    }
  });

  it("covers every phase pair without inventing a transition", () => {
    // Exhaustive rather than hand-picked: the only pairs that may produce a
    // transition are in_flight -> settled.
    for (const from of ALL_PHASES) {
      for (const to of ALL_PHASES) {
        const found = settledSince([row("1", from)], [row("1", to)]);
        const expected = from === "in_flight" && to !== "in_flight" ? 1 : 0;
        expect(found.length, `${from} -> ${to}`).toBe(expected);
      }
    }
  });
});

describe("the poll gate", () => {
  it("is true only while something is moving", () => {
    expect(anyInFlight([])).toBe(false);
    expect(anyInFlight([row("1", "settled_ok"), row("2", "unknown")])).toBe(false);
    expect(anyInFlight([row("1", "settled_ok"), row("2", "in_flight")])).toBe(true);
  });

  it("does not treat an unjudgeable row as work in progress", () => {
    // It would never change into a phase we recognise, so polling for it is
    // a timer that runs until the window closes.
    expect(anyInFlight([row("1", "unknown")])).toBe(false);
  });
});

describe("pass rate", () => {
  it("returns null rather than zero on an empty sample", () => {
    // The DORA cards learned this the hard way: a rate of zero over an empty
    // sample renders identically to a measured total failure.
    expect(verdictRate([])).toMatchObject({ ratePct: null, judged: 0 });
  });

  it("returns null when nothing in the sample can be judged", () => {
    const result = verdictRate([row("1", "in_flight"), row("2", "unknown")]);
    expect(result.ratePct).toBeNull();
    expect(result.judged).toBe(0);
    expect(result.unjudged).toBe(2);
  });

  it("excludes in-flight and unjudgeable rows from the denominator", () => {
    const result = verdictRate([
      row("1", "settled_ok"),
      row("2", "settled_bad"),
      row("3", "in_flight"),
      row("4", "unknown"),
    ]);
    // Two judged rows, one passed: 50%, not 25% of four.
    expect(result).toMatchObject({ ratePct: 50, judged: 2, passed: 1, failed: 1, unjudged: 2 });
  });

  it("reports a measured zero as zero, not as null", () => {
    // The distinction the null case exists to protect: a real, measured total
    // failure must still read as 0%.
    expect(verdictRate([row("1", "settled_bad")])).toMatchObject({ ratePct: 0, judged: 1 });
  });

  it("keeps one decimal place and never exceeds 100", () => {
    const rows = [...Array(3)].map((_, i) => row(`p${i}`, "settled_ok"));
    expect(verdictRate(rows).ratePct).toBe(100);
    expect(verdictRate([...rows, row("f", "settled_bad")]).ratePct).toBe(75);
    // 2/3 must not round to an integer and lose the distinction from 67%.
    expect(verdictRate([row("a", "settled_ok"), row("b", "settled_ok"), row("c", "settled_bad")]).ratePct).toBe(
      66.7,
    );
  });

  it("accounts for every row exactly once", () => {
    // The invariant that stops a row from being silently dropped from both
    // the numerator and the count of exclusions.
    const rows = ALL_PHASES.flatMap((phase, i) => [row(`${phase}-${i}a`, phase), row(`${phase}-${i}b`, phase)]);
    const result = verdictRate(rows);
    expect(result.judged + result.unjudged).toBe(rows.length);
    expect(result.passed + result.failed).toBe(result.judged);
  });
});
