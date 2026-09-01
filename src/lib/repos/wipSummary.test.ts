import { describe, expect, it } from "vitest";
import { IDLE_OPERATION, type RepoOperation } from "./operation";
import {
  bulkSkipReason,
  describeWorkspace,
  repoWip,
  shouldWarnBeforeClosing,
  summarizeWorkspace,
  type RepoWipInput,
} from "./wipSummary";

function input(extra: Partial<RepoWipInput> = {}): RepoWipInput {
  return {
    path: "/r/alpha",
    label: "alpha",
    changedFiles: 0,
    conflictedFiles: 0,
    unpushedCommits: 0,
    stashEntries: 0,
    operation: IDLE_OPERATION,
    loadFailed: false,
    hydrated: true,
    ...extra,
  };
}

function parked(kind: RepoOperation["kind"] = "Merge") {
  return {
    operation: {
      kind,
      current_step: null,
      total_steps: null,
      head_ref: "main",
      incoming_ref: null,
      conflicted_paths: [],
      conflicted_total: 0,
      available: ["abort" as const],
    },
    probeFailed: false,
  };
}

describe("repoWip", () => {
  it("reports nothing for a clean, fully loaded repository", () => {
    expect(repoWip(input()).reasons).toEqual([]);
    expect(repoWip(input()).severity).toBeNull();
  });

  it("counts uncommitted changes without double-counting conflicts", () => {
    // A conflicted file is already reported as a conflict; counting it again
    // as an uncommitted change would inflate every conflicted repository.
    const wip = repoWip(input({ changedFiles: 5, conflictedFiles: 2 }));
    const uncommitted = wip.reasons.find((r) => r.kind === "uncommitted");
    expect(uncommitted?.detail).toBe("3 uncommitted changes");
    expect(wip.reasons.find((r) => r.kind === "conflicts")?.detail).toBe("2 files with conflicts");
  });

  it("omits uncommitted entirely when every change is a conflict", () => {
    const wip = repoWip(input({ changedFiles: 2, conflictedFiles: 2 }));
    expect(wip.reasons.some((r) => r.kind === "uncommitted")).toBe(false);
  });

  it("ranks conflicts and parked operations above everything else", () => {
    const wip = repoWip(
      input({
        changedFiles: 9,
        conflictedFiles: 1,
        unpushedCommits: 4,
        stashEntries: 2,
        operation: parked(),
      }),
    );
    expect(wip.severity).toBe("conflicts");
    expect(wip.reasons.map((r) => r.kind)).toEqual([
      "conflicts",
      "operation",
      "uncommitted",
      "unpushed",
      "stash",
    ]);
  });

  it("treats a repository that could not be read as at risk, never as clean", () => {
    // The failure this guards: a repo whose status call threw renders exactly
    // like a clean one, and the user closes the window over unsaved work.
    const wip = repoWip(input({ loadFailed: true }));
    expect(wip.severity).toBe("unknown");
    expect(wip.reasons[0].detail).toContain("could not be read");
  });

  it("treats a not-yet-loaded repository as unknown rather than clean", () => {
    expect(repoWip(input({ hydrated: false })).severity).toBe("unknown");
  });

  it("treats a failed operation probe as unknown", () => {
    const wip = repoWip(input({ operation: { operation: null, probeFailed: true } }));
    expect(wip.severity).toBe("unknown");
  });

  it("ranks unknown above merely-uncommitted", () => {
    // Not knowing is a worse position to act from than knowing there is work.
    const wip = repoWip(input({ loadFailed: true, changedFiles: 3 }));
    expect(wip.reasons[0].kind).toBe("unknown");
  });

  it("uses singular wording for a single item", () => {
    const wip = repoWip(input({ changedFiles: 1, unpushedCommits: 1, stashEntries: 1 }));
    const details = wip.reasons.map((r) => r.detail);
    expect(details).toContain("1 uncommitted change");
    expect(details).toContain("1 unpushed commit");
    expect(details).toContain("1 stash entry");
  });

  it("pluralizes stash entries correctly rather than as 'stash entrys'", () => {
    expect(repoWip(input({ stashEntries: 3 })).reasons[0].detail).toBe("3 stash entries");
  });

  it("reports unpushed commits and stashes, which are invisible from the tab bar", () => {
    const wip = repoWip(input({ unpushedCommits: 7, stashEntries: 1 }));
    expect(wip.reasons.map((r) => r.kind)).toEqual(["unpushed", "stash"]);
  });
});

describe("summarizeWorkspace", () => {
  it("says nothing is open rather than claiming everything is clean", () => {
    const summary = summarizeWorkspace([]);
    expect(summary.allClear).toBe(false);
    expect(describeWorkspace(summary)).toBe("No repositories are open.");
  });

  it("declares all clear only when every repository was examined and clean", () => {
    const summary = summarizeWorkspace([input(), input({ path: "/r/b", label: "b" })]);
    expect(summary.allClear).toBe(true);
    expect(summary.repos).toEqual([]);
    expect(describeWorkspace(summary)).toBe("No uncommitted work across 2 repositories.");
  });

  it("never declares all clear while one repository is unknown", () => {
    const summary = summarizeWorkspace([input(), input({ path: "/r/b", label: "b", loadFailed: true })]);
    expect(summary.allClear).toBe(false);
    expect(summary.unknown).toBe(1);
  });

  it("lists the worst repository first and names it in the sentence", () => {
    const summary = summarizeWorkspace([
      input({ path: "/r/c", label: "carol", stashEntries: 1 }),
      input({ path: "/r/a", label: "alice", conflictedFiles: 2 }),
      input({ path: "/r/b", label: "bob", changedFiles: 4 }),
    ]);
    expect(summary.repos.map((r) => r.label)).toEqual(["alice", "bob", "carol"]);
    expect(describeWorkspace(summary)).toContain("alice: 2 files with conflicts");
  });

  it("orders alphabetically within one severity band, for a stable list", () => {
    const summary = summarizeWorkspace([
      input({ path: "/r/z", label: "zeta", changedFiles: 1 }),
      input({ path: "/r/a", label: "alpha", changedFiles: 1 }),
    ]);
    expect(summary.repos.map((r) => r.label)).toEqual(["alpha", "zeta"]);
  });

  it("counts examined repositories including the clean ones", () => {
    const summary = summarizeWorkspace([input(), input({ path: "/r/b", label: "b", changedFiles: 1 })]);
    expect(summary.examined).toBe(2);
    expect(summary.repos).toHaveLength(1);
  });

  it("uses the singular sentence for a lone clean repository", () => {
    expect(describeWorkspace(summarizeWorkspace([input()]))).toBe(
      "No uncommitted work — the repository is clean.",
    );
  });
});

describe("shouldWarnBeforeClosing", () => {
  it("warns on uncommitted work, conflicts, and parked operations", () => {
    for (const extra of [
      { changedFiles: 1 },
      { conflictedFiles: 1 },
      { operation: parked() },
    ]) {
      expect(
        shouldWarnBeforeClosing(summarizeWorkspace([input(extra)])),
        JSON.stringify(extra),
      ).toBe(true);
    }
  });

  it("warns when a repository's state is unknown", () => {
    // A confirmation the user dismisses is cheap; closing over an unread
    // repository is not.
    expect(shouldWarnBeforeClosing(summarizeWorkspace([input({ loadFailed: true })]))).toBe(true);
  });

  it("does not warn for work that is already durably committed", () => {
    // Unpushed commits and stashes survive closing the app; nagging about
    // them trains the user to dismiss the warning that matters.
    expect(
      shouldWarnBeforeClosing(summarizeWorkspace([input({ unpushedCommits: 5, stashEntries: 2 })])),
    ).toBe(false);
  });

  it("does not warn on a clean workspace", () => {
    expect(shouldWarnBeforeClosing(summarizeWorkspace([input()]))).toBe(false);
  });
});

describe("bulkSkipReason", () => {
  it("clears a healthy repository for unattended bulk work", () => {
    expect(bulkSkipReason(input())).toBeNull();
    expect(bulkSkipReason(input({ unpushedCommits: 3, stashEntries: 1 }))).toBeNull();
  });

  it("skips a repository whose state could not be read", () => {
    expect(bulkSkipReason(input({ loadFailed: true }))).toContain("could not be read");
  });

  it("skips a repository that has not finished loading", () => {
    expect(bulkSkipReason(input({ hydrated: false }))).toContain("not finished loading");
  });

  it("skips a repository parked mid-operation, naming the merge", () => {
    expect(bulkSkipReason(input({ operation: parked() }))).toContain("merge is in progress");
    expect(bulkSkipReason(input({ operation: parked("Rebase") }))).toContain(
      "git operation is in progress",
    );
  });

  it("skips a repository with outstanding conflicts", () => {
    expect(bulkSkipReason(input({ conflictedFiles: 2 }))).toBe("2 files still have conflicts.");
  });

  it("does not skip merely because the tree is dirty", () => {
    // A fetch is safe on a dirty tree, and skipping every repository with an
    // edit in it would make workspace-wide fetch useless in practice.
    expect(bulkSkipReason(input({ changedFiles: 12 }))).toBeNull();
  });
});
