import { describe, expect, it } from "vitest";
import type { Task, TaskCard } from "./client";
import {
  allLoadedCards,
  canQuickEnhance,
  canStartEnhanceFromDraft,
  cardChrome,
  cardMatchesFacet,
  collectFacetOptions,
  dueInputValue,
  dueState,
  emptyFacet,
  enhanceableFields,
  facetActive,
  hiddenTaskDetails,
  parseDueInput,
  visibleHiddenDetails,
} from "./taskOrganize";

function card(over: Partial<TaskCard> = {}): TaskCard {
  return {
    id: "t1", revision: 1, updated_at: 1, title: "A", kind: "bug", status: "inbox",
    priority: 1, severity: "high", owner: "Pat", due_at: 100, labels: ["ui", "drag", "extra"],
    repository_ids: ["r1", "r2"], primary_repository_id: "r1", home_workspace_id: null, position: 1,
    ...over,
  };
}

const task: Task = {
  ...card(),
  description: "Keep the original error E42",
  acceptance_criteria: ["Reproduce", " "],
  locked_fields: ["title"],
};

describe("facets", () => {
  it("starts inactive and becomes active on any constraint", () => {
    expect(facetActive(emptyFacet())).toBe(false);
    expect(facetActive({ ...emptyFacet(), kind: "bug" })).toBe(true);
  });

  it("filters by each facet independently and refuses mismatches", () => {
    const now = 50;
    expect(cardMatchesFacet(card(), emptyFacet(), now)).toBe(true);
    expect(cardMatchesFacet(card(), { ...emptyFacet(), priority: 0 }, now)).toBe(false);
    expect(cardMatchesFacet(card(), { ...emptyFacet(), priority: "1" as unknown as number }, now)).toBe(true);
    expect(cardMatchesFacet(card(), { ...emptyFacet(), kind: "feature" }, now)).toBe(false);
    expect(cardMatchesFacet(card(), { ...emptyFacet(), owner: "Pat" }, now)).toBe(true);
    expect(cardMatchesFacet(card({ owner: null }), { ...emptyFacet(), owner: "Pat" }, now)).toBe(false);
    expect(cardMatchesFacet(card(), { ...emptyFacet(), label: "missing" }, now)).toBe(false);
    expect(cardMatchesFacet(card(), { ...emptyFacet(), due: "overdue" }, 200)).toBe(true);
    expect(cardMatchesFacet(card({ due_at: null }), { ...emptyFacet(), due: "none" }, now)).toBe(true);
  });

  it("collects sorted unique kinds, owners and labels from loaded cards", () => {
    const options = collectFacetOptions([
      card({ kind: "bug", owner: "Zed", labels: ["b"] }),
      card({ id: "t2", kind: "feature", owner: "Ann", labels: ["a", "b"] }),
      card({ id: "t3", kind: "bug", owner: "  ", labels: [] }),
    ]);
    expect(options.kinds).toEqual(["bug", "feature"]);
    expect(options.owners).toEqual(["Ann", "Zed"]);
    expect(options.labels).toEqual(["a", "b"]);
  });
});

describe("due and chrome", () => {
  it("classifies due dates without inventing a due for empty or hostile values", () => {
    expect(dueState(null, 10)).toBe("none");
    expect(dueState(0, 10)).toBe("none");
    expect(dueState(-1, 10)).toBe("none");
    expect(dueState(5, 10)).toBe("overdue");
    expect(dueState(10 + 60, 10)).toBe("soon");
    expect(dueState(10 + 8 * 24 * 60 * 60, 10)).toBe("later");
  });

  it("counts extra repos and labels without restating the card face", () => {
    const chrome = cardChrome(card(), 200);
    expect(chrome.extraRepos).toBe(1);
    expect(chrome.extraLabels).toBe(1);
    expect(chrome.owner).toBe("Pat");
    expect(chrome.due).toBe("overdue");
  });
});

describe("hidden details and enhance gate", () => {
  it("surfaces fields the board card hides, including empty ones", () => {
    const details = hiddenTaskDetails(task, (id) => ({ r2: "Manvi" }[id]));
    const byKey = Object.fromEntries(details.map((row) => [row.key, row]));
    expect(byKey.description.value).toContain("E42");
    expect(byKey.criteria.value).toContain("Reproduce");
    expect(byKey.repos.value).toBe("Manvi");
    expect(byKey.locks.value).toBe("Title");
    expect(hiddenTaskDetails({ ...task, description: "", acceptance_criteria: [] }, () => undefined)
      .filter((row) => row.key === "description")[0].empty).toBe(true);
    const emptyish = hiddenTaskDetails({ ...task, owner: null, severity: null }, () => undefined);
    expect(visibleHiddenDetails(emptyish, false).every((row) => !row.empty)).toBe(true);
    expect(visibleHiddenDetails(emptyish, true).length).toBe(emptyish.length);
  });

  it("round-trips due dates and refuses hostile datetime-local values", () => {
    expect(parseDueInput("")).toBeNull();
    expect(parseDueInput("not-a-date")).toBeNull();
    expect(parseDueInput("x".repeat(40))).toBeNull();
    const parsed = parseDueInput("2026-09-09T15:30");
    expect(parsed).toBeGreaterThan(0);
    expect(dueInputValue(parsed).startsWith("2026-09-09T15:30")).toBe(true);
    expect(dueInputValue(null)).toBe("");
    expect(dueInputValue(-1)).toBe("");
    expect(canStartEnhanceFromDraft({ title: "", repository_ids: ["r"] })).toMatch(/title/);
    expect(canStartEnhanceFromDraft({ title: "Keep", repository_ids: [] })).toMatch(/repository/);
    expect(canStartEnhanceFromDraft({ title: "Keep", repository_ids: ["r"] })).toBeNull();
  });

  it("will not start an enhancement when Manvi or fields are unavailable", () => {
    expect(enhanceableFields(task)).toEqual(["description"]);
    expect(canQuickEnhance(task, null, null).ok).toBe(false);
    expect(canQuickEnhance(task, { provider: "", model: "m" }, null).ok).toBe(false);
    expect(canQuickEnhance(task, { provider: "local", model: "m" }, "Manvi down").ok).toBe(false);
    expect(canQuickEnhance({ locked_fields: ["title", "description"] }, { provider: "local", model: "m" }, null))
      .toMatchObject({ ok: false });
    expect(canQuickEnhance(task, { provider: "local", model: "m" }, null)).toEqual({
      ok: true,
      fields: ["description"],
    });
  });
});

describe("allLoadedCards", () => {
  it("deduplicates cards that appear if a walk revisits a page", () => {
    const cards = allLoadedCards({
      inbox: { items: [card(), card({ id: "t2" })] },
      ready: { items: [card({ id: "t1", status: "ready" })] },
    });
    expect(cards.map((c) => c.id)).toEqual(["t1", "t2"]);
  });
});
