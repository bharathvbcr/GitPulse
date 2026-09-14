import { describe, expect, it } from "vitest";
import {
  FILTER_THRESHOLD,
  linkSummary,
  outsiderLine,
  repositoryRows,
  shouldOfferFilter,
  summaryLine,
} from "./taskRepositories";

const catalog = [
  { id: "r0", name: "GitPulse" },
  { id: "r1", name: "Manvi" },
  { id: "r2", name: "ScholarLM" },
  { id: "r3", name: "DevPrism" },
];

describe("repositoryRows", () => {
  it("keeps catalog order so a checked row does not jump out from under the pointer", () => {
    const rows = repositoryRows(catalog, ["r3"], "r3", null, "");
    expect(rows.map((row) => row.id)).toEqual(["r0", "r1", "r2", "r3"]);
  });

  it("marks the linked rows and the single primary among them", () => {
    const rows = repositoryRows(catalog, ["r1", "r3"], "r3", null, "");
    expect(rows.filter((row) => row.linked).map((row) => row.id)).toEqual(["r1", "r3"]);
    expect(rows.filter((row) => row.primary).map((row) => row.id)).toEqual(["r3"]);
  });

  it("never marks an unlinked row primary, even when the id still points at it", () => {
    // `primary` can lag a just-unchecked row for a tick; the row must not
    // claim to be the primary of a task it is no longer linked to.
    const rows = repositoryRows(catalog, ["r1"], "r3", null, "");
    expect(rows.some((row) => row.primary)).toBe(false);
  });

  it("filters by name, case-insensitively, on a plain substring", () => {
    expect(repositoryRows(catalog, [], "", null, "sm").map((row) => row.id)).toEqual(["r3"]);
    expect(repositoryRows(catalog, [], "", null, "PULSE").map((row) => row.id)).toEqual(["r0"]);
  });

  it("treats regex punctuation as literal text rather than a pattern", () => {
    // The filter runs over the whole catalog on every keystroke. A reader's
    // "(" must cost nothing and match nothing, not compile.
    expect(repositoryRows(catalog, [], "", null, "(").map((row) => row.id)).toEqual([]);
    expect(repositoryRows([{ id: "r9", name: "app (old)" }], [], "", null, "(old)").map((row) => row.id))
      .toEqual(["r9"]);
  });

  it("never hides a linked repository behind the filter, and says the row was kept", () => {
    const rows = repositoryRows(catalog, ["r3"], "r3", null, "manvi");
    expect(rows.map((row) => row.id)).toEqual(["r1", "r3"]);
    expect(rows.find((row) => row.id === "r3")?.keptByLink).toBe(true);
    expect(rows.find((row) => row.id === "r1")?.keptByLink).toBe(false);
  });

  it("marks workspace members only when membership is known", () => {
    expect(repositoryRows(catalog, [], "", ["r1"], "").filter((row) => row.member).map((row) => row.id))
      .toEqual(["r1"]);
    // `null` is "no home workspace, or membership unread" — not "member of none".
    expect(repositoryRows(catalog, [], "", null, "").some((row) => row.member)).toBe(false);
  });

  it("offers a filter only once the catalog outgrows the list", () => {
    expect(shouldOfferFilter(FILTER_THRESHOLD)).toBe(false);
    expect(shouldOfferFilter(FILTER_THRESHOLD + 1)).toBe(true);
  });
});

describe("linkSummary", () => {
  it("resolves the primary name and counts the links", () => {
    const summary = linkSummary(catalog, ["r1", "r3"], "r3", null);
    expect(summary).toEqual({ linked: 2, primaryName: "DevPrism", outsiders: [], unknown: [] });
  });

  it("reports links this page of the catalog cannot name, rather than dropping them", () => {
    const summary = linkSummary(catalog, ["r1", "r99"], "r99", null);
    expect(summary.unknown).toEqual(["r99"]);
    expect(summary.linked).toBe(2);
    expect(summary.primaryName).toBe("r99");
  });

  it("lists links outside the home workspace, and reports none when membership is unknown", () => {
    expect(linkSummary(catalog, ["r0", "r1"], "r0", ["r1"]).outsiders).toEqual(["r0"]);
    expect(linkSummary(catalog, ["r0", "r1"], "r0", null).outsiders).toEqual([]);
  });
});

describe("summary lines", () => {
  it("says a task still needs a repository rather than reporting an empty link set", () => {
    expect(summaryLine(linkSummary(catalog, [], "", null)))
      .toBe("No repository linked yet — a task needs one.");
  });

  it("never names a primary the task has not chosen", () => {
    expect(summaryLine(linkSummary(catalog, ["r1"], "", null)))
      .toBe("1 repository linked · no primary chosen");
    expect(summaryLine(linkSummary(catalog, ["r1", "r3"], "r1", null)))
      .toBe("2 repositories linked · primary Manvi");
  });

  it("counts the outsiders in the workspace it names", () => {
    expect(outsiderLine(["r0"], "Developer tools")).toBe("1 linked repository is not in Developer tools.");
    expect(outsiderLine(["r0", "r2"], "Developer tools")).toBe("2 linked repositories are not in Developer tools.");
  });
});
