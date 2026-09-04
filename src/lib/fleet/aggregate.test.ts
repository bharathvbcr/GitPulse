import { describe, expect, it } from "vitest";
import {
  byUrgency,
  describeTally,
  fleetHeadline,
  isComplete,
  severityRank,
  tally,
  tallyScope,
} from "./aggregate";
import { UNSCANNED, failedCell, readCell, type FleetRow, type FleetSeverity } from "./types";

function row(overrides: Partial<FleetRow> = {}): FleetRow {
  return {
    path: "/repo/a",
    label: "a",
    presence: "open",
    branch: "main",
    severity: "clean",
    headline: "clean",
    changes: UNSCANNED,
    sync: UNSCANNED,
    watchWarning: null,
    work: UNSCANNED,
    activity: UNSCANNED,
    loc: UNSCANNED,
    storage: UNSCANNED,
    health: UNSCANNED,
    coverage: UNSCANNED,
    ...overrides,
  };
}

const bytes = (n: number, partial = false) =>
  readCell({ bytes: n, gitBytes: 0, reclaimableBytes: 0 }, 1, partial);

describe("tallyScope", () => {
  it("counts open repositories only", () => {
    const rows = [row(), row({ path: "/repo/b", presence: "recent" })];
    expect(tallyScope(rows).map((r) => r.path)).toEqual(["/repo/a"]);
  });
});

describe("tally", () => {
  it("sums the read cells and counts them", () => {
    const rows = [
      row({ storage: bytes(100) }),
      row({ path: "/repo/b", storage: bytes(250) }),
    ];
    const total = tally(rows, (r) => r.storage, (v) => v.bytes);
    expect(total).toMatchObject({ value: 350, counted: 2, eligible: 2, failed: 0, unscanned: 0 });
    expect(isComplete(total)).toBe(true);
  });

  it("keeps failed and unscanned in separate buckets", () => {
    const rows = [
      row({ storage: bytes(100) }),
      row({ path: "/repo/b", storage: failedCell("timed out") }),
      row({ path: "/repo/c", storage: UNSCANNED }),
    ];
    const total = tally(rows, (r) => r.storage, (v) => v.bytes);
    expect(total).toMatchObject({ value: 100, counted: 1, eligible: 3, failed: 1, unscanned: 1 });
    expect(isComplete(total)).toBe(false);
  });

  it("does not let recents rows inflate a workspace total", () => {
    const rows = [
      row({ storage: bytes(100) }),
      row({ path: "/repo/old", presence: "recent", storage: bytes(9_000) }),
    ];
    expect(tally(rows, (r) => r.storage, (v) => v.bytes)).toMatchObject({
      value: 100,
      eligible: 1,
    });
  });

  it("carries a partial contribution through to the total", () => {
    const rows = [row({ storage: bytes(100, true) })];
    const total = tally(rows, (r) => r.storage, (v) => v.bytes);
    expect(total.partial).toBe(true);
    // Every repository was counted, but a floor is not a total.
    expect(isComplete(total)).toBe(false);
  });

  it("treats a non-finite contribution as a failure rather than poisoning the sum", () => {
    const rows = [row({ storage: bytes(100) }), row({ path: "/repo/b", storage: bytes(Number.NaN) })];
    const total = tally(rows, (r) => r.storage, (v) => v.bytes);
    expect(total.value).toBe(100);
    expect(total.failed).toBe(1);
  });

  it("reports an empty scope as incomplete, not as a clean zero", () => {
    const total = tally([], (r) => r.storage, (v) => v.bytes);
    expect(total).toMatchObject({ value: 0, counted: 0, eligible: 0 });
    expect(isComplete(total)).toBe(false);
  });
});

describe("describeTally", () => {
  it("says nothing when the total really covers everything", () => {
    expect(describeTally({ value: 1, counted: 3, eligible: 3, failed: 0, unscanned: 0, partial: false })).toBe("");
  });

  it("names the shortfall rather than rounding it away", () => {
    expect(
      describeTally({ value: 1, counted: 20, eligible: 24, failed: 1, unscanned: 3, partial: false }),
    ).toBe("counted across 20 of 24 — 1 failed, 3 not scanned");
  });

  it("says a full count is still partial when a contribution was a floor", () => {
    expect(
      describeTally({ value: 1, counted: 3, eligible: 3, failed: 0, unscanned: 0, partial: true }),
    ).toBe("across all 3, some counts partial");
  });

  it("refuses to imply a total when nothing was counted", () => {
    expect(
      describeTally({ value: 0, counted: 0, eligible: 4, failed: 0, unscanned: 4, partial: false }),
    ).toBe("not scanned");
    expect(
      describeTally({ value: 0, counted: 0, eligible: 4, failed: 4, unscanned: 0, partial: false }),
    ).toBe("not counted — 4 repositories failed to scan");
  });

  it("says so when there is nothing in scope", () => {
    expect(
      describeTally({ value: 0, counted: 0, eligible: 0, failed: 0, unscanned: 0, partial: false }),
    ).toBe("no repositories in scope");
  });
});

describe("severityRank / byUrgency", () => {
  it("ranks worst first and clean last", () => {
    const order: FleetSeverity[] = [
      "conflicts",
      "operation",
      "unknown",
      "uncommitted",
      "unpushed",
      "stash",
      "clean",
    ];
    const ranks = order.map(severityRank);
    expect(ranks).toEqual([...ranks].sort((a, b) => a - b));
  });

  it("sorts an unrecognized severity last rather than above a conflict", () => {
    expect(severityRank("wat" as FleetSeverity)).toBeGreaterThan(severityRank("clean"));
  });

  it("puts unknown above merely-uncommitted, matching the risk model", () => {
    expect(severityRank("unknown")).toBeLessThan(severityRank("uncommitted"));
  });

  it("breaks severity ties by presence, then label", () => {
    const rows = [
      row({ path: "/z", label: "z", severity: "uncommitted" }),
      row({ path: "/old", label: "a", severity: "uncommitted", presence: "recent" }),
      row({ path: "/b", label: "b", severity: "conflicts" }),
    ];
    expect(byUrgency(rows).map((r) => r.label)).toEqual(["b", "z", "a"]);
  });

  it("does not mutate the array it was given", () => {
    const rows = [row({ label: "z", severity: "clean" }), row({ label: "a", severity: "conflicts" })];
    byUrgency(rows);
    expect(rows.map((r) => r.label)).toEqual(["z", "a"]);
  });
});

describe("fleetHeadline", () => {
  it("says so when nothing is open", () => {
    expect(fleetHeadline([]).sentence).toBe("No repositories are open.");
  });

  it("ignores recents when deciding whether the workspace is clean", () => {
    const headline = fleetHeadline([
      row(),
      row({ path: "/old", presence: "recent", severity: "unknown" }),
    ]);
    expect(headline.open).toBe(1);
    expect(headline.sentence).toBe("One repository open, and it is clean.");
  });

  it("declares all clear only when every open repository was examined and is clean", () => {
    expect(fleetHeadline([row(), row({ path: "/b" })]).sentence).toBe(
      "2 repositories open, all clean.",
    );
  });

  it("names the worst repository and counts the unreadable ones", () => {
    const headline = fleetHeadline([
      row({ path: "/a", label: "alpha", severity: "conflicts", headline: "2 files with conflicts" }),
      row({ path: "/b", label: "beta", severity: "unknown", headline: "state could not be read" }),
      row({ path: "/c", label: "gamma" }),
    ]);
    expect(headline.attention).toBe(2);
    expect(headline.unknown).toBe(1);
    expect(headline.sentence).toBe(
      "2 repositories of 3 need attention, 1 unreadable — alpha: 2 files with conflicts",
    );
  });

  it("never calls an unhydrated workspace clean", () => {
    const headline = fleetHeadline([row({ severity: "unknown", headline: "not loaded yet" })]);
    expect(headline.sentence).not.toContain("clean");
    expect(headline.sentence).toContain("not loaded yet");
  });
});
