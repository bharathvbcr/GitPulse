import { describe, expect, it } from "vitest";
import {
  classifyFileChange,
  dirtyAncestorSet,
  isUntrackedStatusCode,
  mergeListedAndStatusPaths,
  statusMatchesScope,
  statusLiveKey,
  statusPathKey,
  statusBadgeClass,
  statusBadgeLabel,
  summarizeStatuses,
  type StatusLike,
} from "./fileStatus";

function status(over: Partial<StatusLike> & Pick<StatusLike, "path">): StatusLike {
  return {
    status_code: " M",
    is_staged: false,
    is_conflicted: false,
    additions: 0,
    deletions: 0,
    ...over,
  };
}

describe("classifyFileChange", () => {
  it("prefers conflict over staged/untracked", () => {
    expect(
      classifyFileChange(
        status({ path: "a.ts", is_conflicted: true, is_staged: true, status_code: "UU" }),
      ),
    ).toBe("conflict");
  });

  it("treats ?? and ? as untracked even when spaces pad porcelain", () => {
    expect(isUntrackedStatusCode("??")).toBe(true);
    expect(isUntrackedStatusCode("?")).toBe(true);
    expect(isUntrackedStatusCode(" ?")).toBe(true);
    expect(isUntrackedStatusCode("M ")).toBe(false);
    expect(classifyFileChange(status({ path: "n.ts", status_code: "??" }))).toBe("untracked");
    expect(classifyFileChange(status({ path: "m.ts", is_staged: true, status_code: "M " }))).toBe(
      "staged",
    );
    expect(classifyFileChange(status({ path: "u.ts", status_code: " M" }))).toBe("unstaged");
    expect(classifyFileChange(null)).toBe("clean");
  });
});

describe("statusMatchesScope", () => {
  it("keeps all rows on the all scope and filters otherwise", () => {
    const staged = status({ path: "a.ts", is_staged: true, status_code: "M " });
    expect(statusMatchesScope(staged, "all")).toBe(true);
    expect(statusMatchesScope(staged, "staged")).toBe(true);
    expect(statusMatchesScope(staged, "modified")).toBe(false);
    expect(statusMatchesScope(undefined, "staged")).toBe(false);
    expect(statusMatchesScope(undefined, "all")).toBe(true);
  });
});

describe("summarizeStatuses", () => {
  it("counts each class and sums churn without laundering missing numbers", () => {
    const summary = summarizeStatuses([
      status({ path: "a.ts", is_staged: true, status_code: "M ", additions: 2, deletions: 1 }),
      status({ path: "b.ts", status_code: " M", additions: 3, deletions: 0 }),
      status({ path: "c.ts", status_code: "??" }),
      status({ path: "d.ts", is_conflicted: true, status_code: "UU" }),
    ]);
    expect(summary).toEqual({
      staged: 1,
      unstaged: 1,
      untracked: 1,
      conflicted: 1,
      additions: 5,
      deletions: 1,
      dirty: 4,
    });
  });
});

describe("statusPathKey and mergeListedAndStatusPaths", () => {
  it("keys listing membership on sorted paths, ignoring churn", () => {
    expect(
      statusPathKey([
        status({ path: "b.ts", additions: 9 }),
        status({ path: "a.ts", additions: 1 }),
      ]),
    ).toBe("a.ts\nb.ts");
    expect(statusPathKey([])).toBe("");
  });

  it("changes the live key when staged flags or churn change, not only paths", () => {
    const first = [status({ path: "a.ts", additions: 1 })];
    const staged = [status({ path: "a.ts", is_staged: true, status_code: "M ", additions: 1 })];
    const moreChurn = [status({ path: "a.ts", additions: 4 })];
    expect(statusLiveKey(first)).not.toBe(statusLiveKey(staged));
    expect(statusLiveKey(first)).not.toBe(statusLiveKey(moreChurn));
    expect(statusLiveKey([])).toBe("");
  });

  it("unions status-only paths (deleted/renamed) onto the listing", () => {
    expect(mergeListedAndStatusPaths(["a.ts", "b.ts"], ["b.ts", "gone.ts"])).toEqual([
      "a.ts",
      "b.ts",
      "gone.ts",
    ]);
    expect(mergeListedAndStatusPaths(["a.ts"], [])).toEqual(["a.ts"]);
  });
});

describe("dirtyAncestorSet", () => {
  it("marks every ancestor dir of a dirty file", () => {
    expect([...dirtyAncestorSet(["src/lib/a.ts"])].sort()).toEqual(["src", "src/lib"]);
    expect(dirtyAncestorSet(["README.md"]).size).toBe(0);
  });
});

describe("statusBadgeClass and statusBadgeLabel", () => {
  it("returns distinct styling and labels for each file change kind", () => {
    expect(statusBadgeClass("conflict")).toContain("rose");
    expect(statusBadgeClass("staged")).toContain("emerald");
    expect(statusBadgeClass("untracked")).toContain("cyan");
    expect(statusBadgeClass("unstaged")).toContain("amber");
    expect(statusBadgeClass("clean")).toBe("");

    expect(statusBadgeLabel("conflict")).toBe("!C");
    expect(statusBadgeLabel("staged")).toBe("S");
    expect(statusBadgeLabel("untracked")).toBe("U");
    expect(statusBadgeLabel("unstaged")).toBe("M");
    expect(statusBadgeLabel("clean")).toBe("");

    expect(statusBadgeLabel("conflict", true)).toBe("CONFLICT");
    expect(statusBadgeLabel("staged", true)).toBe("STAGED");
    expect(statusBadgeLabel("untracked", true)).toBe("UNTRACKED");
    expect(statusBadgeLabel("unstaged", true)).toBe("MODIFIED");
  });
});

