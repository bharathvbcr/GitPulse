import { describe, expect, it } from "vitest";
import {
  DEFAULT_TASK_REPOSITORY_SORT,
  FILTER_THRESHOLD,
  TASK_REPOSITORY_SORTS,
  TASK_REPOSITORY_SORT_LABELS,
  groupRows,
  isTaskRepositorySort,
  linkSummary,
  outsiderLine,
  repositoryRows,
  shouldOfferFilter,
  sortNavigatorRepositories,
  summaryLine,
  triggerChips,
  type SortableNavigatorRepository,
  type TaskRepositorySort,
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

describe("triggerChips", () => {
  it("draws the primary first, so the closed trigger answers both questions", () => {
    const { chips, overflow } = triggerChips(catalog, ["r0", "r1", "r3"], "r3");
    expect(chips.map((chip) => chip.id)).toEqual(["r3", "r0", "r1"]);
    expect(chips.filter((chip) => chip.primary).map((chip) => chip.id)).toEqual(["r3"]);
    expect(overflow).toBe(0);
  });

  it("collapses the remainder to a count, never the primary", () => {
    // The trigger has a fixed width and the dock goes down to 380px, so some
    // chips have to go. Hiding the primary would leave it answering the easy
    // question and dropping the one that decides where an agent runs.
    const { chips, overflow } = triggerChips(catalog, ["r0", "r1", "r2", "r3"], "r3", 2);
    expect(chips.map((chip) => chip.id)).toEqual(["r3", "r0"]);
    expect(chips[0].primary).toBe(true);
    expect(overflow).toBe(2);
  });

  it("always leaves room for at least one chip", () => {
    for (const limit of [0, -5, Number.NaN]) {
      const { chips } = triggerChips(catalog, ["r0", "r3"], "r3", limit);
      expect(chips).toHaveLength(1);
      expect(chips[0].primary).toBe(true);
    }
  });

  it("skips a linked id this page of the catalog cannot name", () => {
    // Drawing it would put a raw id in the trigger; `linkSummary().unknown`
    // and the notice under the trigger are where that belongs.
    const { chips, overflow } = triggerChips(catalog, ["r0", "gone"], "r0");
    expect(chips.map((chip) => chip.id)).toEqual(["r0"]);
    expect(overflow).toBe(0);
    expect(linkSummary(catalog, ["r0", "gone"], "r0", null).unknown).toEqual(["gone"]);
  });

  it("marks no primary when the task has none", () => {
    const { chips } = triggerChips(catalog, ["r0", "r1"], "");
    expect(chips.some((chip) => chip.primary)).toBe(false);
    expect(chips.map((chip) => chip.id)).toEqual(["r0", "r1"]);
  });

  it("has nothing to draw with nothing linked", () => {
    expect(triggerChips(catalog, [], "")).toEqual({ chips: [], overflow: 0 });
  });
});

describe("groupRows", () => {
  it("splits rows into the three answers a reader is choosing between", () => {
    const rows = repositoryRows(catalog, ["r3"], "r3", ["r0", "r3"], "");
    const groups = groupRows(rows);
    expect(groups.map((group) => group.id)).toEqual(["linked", "workspace", "other"]);
    expect(groups[0].rows.map((row) => row.id)).toEqual(["r3"]);
    expect(groups[1].rows.map((row) => row.id)).toEqual(["r0"]);
    expect(groups[2].rows.map((row) => row.id)).toEqual(["r1", "r2"]);
  });

  it("keeps catalog order inside each group", () => {
    const rows = repositoryRows(catalog, ["r3", "r1"], "r3", null, "");
    expect(groupRows(rows)[0].rows.map((row) => row.id)).toEqual(["r1", "r3"]);
  });

  it("drops an empty group rather than drawing a heading over nothing", () => {
    expect(groupRows(repositoryRows(catalog, [], "", null, "")).map((group) => group.id)).toEqual(["other"]);
  });

  it("has no workspace group when the task has no home workspace", () => {
    expect(groupRows(repositoryRows(catalog, ["r0"], "r0", null, "")).map((group) => group.id)).toEqual(["linked", "other"]);
  });

  it("loses no row", () => {
    const rows = repositoryRows(catalog, ["r3"], "r3", ["r0"], "");
    expect(groupRows(rows).flatMap((group) => group.rows)).toHaveLength(rows.length);
  });
});

describe("isTaskRepositorySort", () => {
  it("accepts only the supported repository sort modes", () => {
    expect(DEFAULT_TASK_REPOSITORY_SORT).toBe("name-asc");
    expect(TASK_REPOSITORY_SORTS).toEqual(["name-asc", "name-desc", "open", "recent"]);
    for (const sort of TASK_REPOSITORY_SORTS) {
      expect(isTaskRepositorySort(sort)).toBe(true);
      expect(TASK_REPOSITORY_SORT_LABELS[sort]).toBeTruthy();
    }
    for (const bad of ["", "repo", "toString", "__proto__", null, undefined, 0, 1, {}, []]) {
      expect(isTaskRepositorySort(bad)).toBe(false);
    }
  });
});

describe("sortNavigatorRepositories", () => {
  const repo = (over: Partial<SortableNavigatorRepository> & Pick<SortableNavigatorRepository, "id" | "name">): SortableNavigatorRepository => ({
    identity_key: `local:/repos/${over.id}/.git`,
    updated_at: 100,
    ...over,
  });

  it("never mutates the input array and handles empty and single-item lists", () => {
    const empty = Object.freeze([] as SortableNavigatorRepository[]);
    expect(sortNavigatorRepositories(empty, "name-asc")).toEqual([]);

    const one = Object.freeze([repo({ id: "r1", name: "GitPulse" })]);
    const sorted = sortNavigatorRepositories(one, "recent");
    expect(sorted).toEqual(one);
    expect(sorted).not.toBe(one);
  });

  it("sorts by name ascending with case-insensitive natural ordering and deterministic tie-breakers", () => {
    const list = Object.freeze([
      repo({ id: "r4", name: "Beta", identity_key: "local:/b/.git" }),
      repo({ id: "r3", name: "app-10" }),
      repo({ id: "r2", name: "app-2" }),
      repo({ id: "r1", name: "alpha" }),
      repo({ id: "r5b", name: "same", identity_key: "local:/same-1/.git" }),
      repo({ id: "r5a", name: "Same", identity_key: "local:/same-2/.git" }),
      repo({ id: "r6b", name: "KeyTie", identity_key: "local:/k-2/.git" }),
      repo({ id: "r6a", name: "KeyTie", identity_key: "local:/k-1/.git" }),
      repo({ id: "r7b", name: "Dup", identity_key: "local:/dup/.git" }),
      repo({ id: "r7a", name: "Dup", identity_key: "local:/dup/.git" }),
    ]);

    expect(sortNavigatorRepositories(list, "name-asc").map((item) => item.id)).toEqual([
      "r1",
      "r2",
      "r3",
      "r4",
      "r7a",
      "r7b",
      "r6a",
      "r6b",
      "r5a",
      "r5b",
    ]);
  });

  it("sorts by name descending with natural numeric order and deterministic tie-breakers", () => {
    const list = Object.freeze([
      repo({ id: "r1", name: "Alpha" }),
      repo({ id: "r2", name: "app-2" }),
      repo({ id: "r3", name: "app-10" }),
      repo({ id: "r4", name: "Zeta" }),
      repo({ id: "r5b", name: "Dup", identity_key: "local:/dup-b/.git" }),
      repo({ id: "r5a", name: "Dup", identity_key: "local:/dup-a/.git" }),
      repo({ id: "r6b", name: "Tie", identity_key: "local:/tie/.git" }),
      repo({ id: "r6a", name: "Tie", identity_key: "local:/tie/.git" }),
    ]);

    expect(sortNavigatorRepositories(list, "name-desc").map((item) => item.id)).toEqual([
      "r4",
      "r6a",
      "r6b",
      "r5a",
      "r5b",
      "r3",
      "r2",
      "r1",
    ]);
  });

  it("sorts open repositories first in tab-strip order, then closed repositories A to Z", () => {
    const list = Object.freeze([
      repo({ id: "r-closed-b", name: "Zebra", identity_key: "local:/repos/zebra/.git" }),
      repo({ id: "r-manvi", name: "Manvi", identity_key: "local:/repos/manvi/.git" }),
      repo({ id: "r-gitpulse", name: "GitPulse", identity_key: "local:/repos/GitPulse/.git" }),
      repo({ id: "r-remote", name: "Remote Docs", identity_key: "remote:github.com/org/docs" }),
      repo({ id: "r-closed-a", name: "Alpha", identity_key: "local:/repos/alpha/.git" }),
    ]);
    const tabs = [
      { path: "/repos/gitpulse" },
      { path: "/repos/manvi" },
      { path: "/repos/gitpulse" },
      { path: "/repos/docs" },
    ];

    // Case-insensitive matching joins `/repos/gitpulse` with `local:/repos/GitPulse/.git`,
    // duplicate tabs keep their earliest index (0 before 1), and remote identities stay closed.
    expect(sortNavigatorRepositories(list, "open", tabs, { caseInsensitive: true }).map((item) => item.id)).toEqual([
      "r-gitpulse",
      "r-manvi",
      "r-closed-a",
      "r-remote",
      "r-closed-b",
    ]);

    // Case-sensitive filesystem leaves `GitPulse` vs `gitpulse` unmatched.
    expect(sortNavigatorRepositories(list, "open", tabs, { caseInsensitive: false }).map((item) => item.id)).toEqual([
      "r-manvi",
      "r-closed-a",
      "r-gitpulse",
      "r-remote",
      "r-closed-b",
    ]);
  });

  it("sorts by recently added descending, treating non-finite timestamps as 0 and breaking ties A to Z", () => {
    const list = Object.freeze([
      repo({ id: "r-old", name: "Beta", updated_at: 10 }),
      repo({ id: "r-tie-b", name: "Gamma", updated_at: 50 }),
      repo({ id: "r-tie-a", name: "Alpha", updated_at: 50 }),
      repo({ id: "r-nan", name: "Broken", updated_at: Number.NaN }),
      repo({ id: "r-neg", name: "NegativeInf", updated_at: Number.NEGATIVE_INFINITY }),
      repo({ id: "r-new", name: "Newest", updated_at: 200 }),
    ]);

    expect(sortNavigatorRepositories(list, "recent").map((item) => item.id)).toEqual([
      "r-new",
      "r-tie-a",
      "r-tie-b",
      "r-old",
      "r-nan",
      "r-neg",
    ]);
  });

  it("falls back safely to name-asc when passed an unknown sort key at runtime", () => {
    const list = Object.freeze([
      repo({ id: "r2", name: "Zeta" }),
      repo({ id: "r1", name: "Alpha" }),
    ]);

    expect(sortNavigatorRepositories(list, "unexpected" as unknown as TaskRepositorySort).map((item) => item.id)).toEqual([
      "r1",
      "r2",
    ]);
  });
});
