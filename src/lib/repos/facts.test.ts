import { describe, expect, it } from "vitest";
import { toWipInput, unknownFacts, type RepoFacts } from "./facts";
import { IDLE_OPERATION } from "./operation";
import { WATCH_ACTIVE, WATCH_UNKNOWN } from "./watchState";
import { repoWip } from "./wipSummary";

function facts(overrides: Partial<RepoFacts> = {}): RepoFacts {
  return { ...unknownFacts("/repo/alpha", "alpha"), hydrated: true, ...overrides };
}

describe("unknownFacts", () => {
  it("reports every count as zero but marks itself unhydrated", () => {
    const blank = unknownFacts("/repo/beta", "beta");
    expect(blank.changedFiles).toBe(0);
    expect(blank.unpushedCommits).toBe(0);
    expect(blank.stashEntries).toBe(0);
    // The zeros above are only safe because this flag says they are not
    // measurements. Flipping it would turn "we never looked" into "clean".
    expect(blank.hydrated).toBe(false);
    expect(blank.operation).toEqual(IDLE_OPERATION);
    expect(blank.watch).toEqual(WATCH_UNKNOWN);
  });

  it("carries the identity it was given", () => {
    const blank = unknownFacts("/repo/beta", "beta");
    expect(blank.path).toBe("/repo/beta");
    expect(blank.label).toBe("beta");
  });
});

describe("toWipInput", () => {
  it("passes the risk-bearing counts through unchanged", () => {
    const input = toWipInput(
      facts({
        changedFiles: 7,
        conflictedFiles: 2,
        unpushedCommits: 3,
        stashEntries: 1,
      }),
    );
    expect(input).toMatchObject({
      path: "/repo/alpha",
      label: "alpha",
      changedFiles: 7,
      conflictedFiles: 2,
      unpushedCommits: 3,
      stashEntries: 1,
      hydrated: true,
    });
  });

  it("folds an unreadable stash into loadFailed", () => {
    // An unreadable stash list is work that exists nowhere else, invisible.
    // The risk model has one "could not read" channel and it must land there,
    // or a forgotten stash renders as no stash.
    const input = toWipInput(facts({ stashFailed: true }));
    expect(input.loadFailed).toBe(true);
    expect(repoWip(input).severity).toBe("unknown");
  });

  it("keeps loadFailed true when the snapshot itself failed", () => {
    const input = toWipInput(facts({ loadFailed: true, loadError: "boom" }));
    expect(input.loadFailed).toBe(true);
  });

  it("does not invent loadFailed for a healthy repository", () => {
    const input = toWipInput(facts({ watch: WATCH_ACTIVE }));
    expect(input.loadFailed).toBe(false);
    expect(repoWip(input).reasons).toEqual([]);
  });

  it("drops the fields the risk model does not read", () => {
    const input = toWipInput(facts({ additions: 40, deletions: 9, behindCommits: 5 }));
    expect(Object.keys(input).sort()).toEqual([
      "changedFiles",
      "conflictedFiles",
      "hydrated",
      "label",
      "loadFailed",
      "operation",
      "path",
      "stashEntries",
      "unpushedCommits",
    ]);
  });
});
