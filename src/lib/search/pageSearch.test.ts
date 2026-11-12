import { describe, expect, it } from "vitest";
import {
  pageScope,
  pageSearchFor,
  registerPageSearch,
  registerStandingFilter,
  standingFilterFor,
  type FilterProvider,
} from "./pageSearch.svelte";

function filter(label: string): FilterProvider {
  let q = "";
  return {
    kind: "filter",
    label,
    placeholder: label,
    query: () => q,
    setQuery: (next) => {
      q = next;
    },
  };
}

describe("page search registry", () => {
  it("names a place by view and section", () => {
    expect(pageScope("history", "graph")).toBe("history:graph");
    expect(pageScope("insights", null)).toBe("insights:");
  });

  it("answers only for the place asked about", () => {
    // The field asks for the place on screen. A diff mounted behind Graph must
    // not take the field while Graph is showing.
    const dispose = registerPageSearch("history:diff", filter("diff"));
    expect(pageSearchFor("history:diff")?.label).toBe("diff");
    expect(pageSearchFor("history:graph")).toBeNull();
    expect(pageSearchFor(null)).toBeNull();
    dispose();
    expect(pageSearchFor("history:diff")).toBeNull();
  });

  it("lets a later registration win, and the earlier disposer leave it alone", () => {
    // A viewer that remounts registers before the old instance's teardown
    // runs; that teardown must not remove the new provider.
    const first = registerPageSearch("code:explorer", filter("first"));
    const second = registerPageSearch("code:explorer", filter("second"));
    first();
    expect(pageSearchFor("code:explorer")?.label).toBe("second");
    second();
    expect(pageSearchFor("code:explorer")).toBeNull();
  });

  it("keeps each place's query its own", () => {
    // A find string typed in a diff must never become the commit filter on a
    // section switch.
    const graph = filter("graph");
    const diff = filter("diff");
    const disposers = [registerPageSearch("history:graph", graph), registerPageSearch("history:diff", diff)];
    pageSearchFor("history:diff")?.setQuery("needle");
    expect(pageSearchFor("history:graph")?.query()).toBe("");
    expect(pageSearchFor("history:diff")?.query()).toBe("needle");
    for (const dispose of disposers) dispose();
  });

  it("reports a view's standing filter until its owner goes", () => {
    let applied = true;
    const read = () => (applied ? { label: "Commits: “fix”", title: "Clear", clear: () => (applied = false) } : null);
    const dispose = registerStandingFilter("history", read);
    expect(standingFilterFor("history")?.label).toBe("Commits: “fix”");
    expect(standingFilterFor("code")).toBeNull();
    standingFilterFor("history")?.clear();
    expect(standingFilterFor("history")).toBeNull();
    const replaced = registerStandingFilter("history", () => null);
    dispose();
    // The stale disposer left the newer registration in place.
    expect(standingFilterFor("history")).toBeNull();
    replaced();
  });
});
