import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { latestEnhancement, listEnhancements, WorkbenchError } from "./client";
import { refine, storedRefine, task } from "./proposals.fixtures";
import {
  applyConflict, applyRefine, confirmReviewed, dismissRefine, refineDiff, refineResult,
  requestRefine, selectableParts, MAX_REVIEWED_RELATED,
} from "./proposals";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
beforeEach(() => { native.mockReset(); });

const wire = (n = 0) => JSON.parse(String((native.mock.calls[n]?.[1] as { input: string }).input));
const method = (n = 0) => (native.mock.calls[n]?.[1] as { method: string }).method;

describe("refinement", () => {
  it("sends the repository, the task and its revision; a proposal that claims it applied is refused", async () => {
    native.mockResolvedValueOnce(JSON.stringify(refine())).mockResolvedValueOnce(JSON.stringify(storedRefine()));
    const result = await requestRefine("/r", task, null, "q1");
    expect(method()).toBe("enhancements.refine");
    expect(wire()).toEqual({ repo_path: "/r", id: "t1", request_id: "q1", expected_revision: 4 });
    expect(result.criteria).toHaveLength(2);
    expect(() => refineResult(refine({ applied: true }))).toThrow(WorkbenchError);
    expect(() => refineResult(refine({ proposal: { acceptance_criteria: "x" } }))).toThrow(WorkbenchError);
  });
  it("shows the proposal as a diff against the task and flags a task that moved", () => {
    const result = refineResult(refine());
    const diff = refineDiff(task, result);
    expect(diff.stale).toBe(false);
    expect(diff.criteria.map((c) => c.state)).toEqual(["present", "add"]);
    expect(diff.duplicates[0]?.state).toBe("add");
    expect(refineDiff({ ...task, revision: 5 }, result).stale).toBe(true);
    expect(refineDiff({ ...task, links: [{ kind: "duplicate_of", item_id: "t9" }] }, result).duplicates[0]?.state).toBe("present");
  });
  it("reads the stored proposal back for the revision an accept is conditional on", async () => {
    native.mockResolvedValueOnce(JSON.stringify(refine())).mockResolvedValueOnce(JSON.stringify(storedRefine()));
    const result = await requestRefine("/r", task, null, "q1");
    expect(method(1)).toBe("enhancements.get");
    expect(wire(1)).toEqual({ id: "enh-1" });
    expect(result.enhancement_id).toBe("enh-1");
    expect(result.enhancement_revision).toBe(2);
  });
  it.each([
    ["not ready", storedRefine({ state: "dismissed" })],
    ["another task's", storedRefine({ task_id: "t2" })],
    ["drafted against another revision", storedRefine({ source_revision: 3 })],
    ["a text proposal", storedRefine({ kind: "text", fields: ["title"] })],
  ])("a reply naming a stored proposal that is %s is not a proposal", async (_name, stored) => {
    native.mockResolvedValueOnce(JSON.stringify(refine())).mockResolvedValueOnce(JSON.stringify(stored));
    await expect(requestRefine("/r", task, null, "q1")).rejects.toBeInstanceOf(WorkbenchError);
  });
  it("a reply without a stored proposal id is refused", () => {
    const { enhancement_id: _gone, ...bare } = refine();
    expect(() => refineResult(bare)).toThrow(WorkbenchError);
  });
  it("accepts the selected parts of the stored proposal at the reviewed revisions", async () => {
    const result = { ...refineResult(refine()), enhancement_revision: 2 };
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, outcome: "applied", revision: 5, criteria_added: 1, links_added: 0, created_item_ids: ["ft-a"] }));
    const receipt = await applyRefine("/r", result, ["acceptance_criteria", "split_into"]);
    expect(receipt).toEqual({ outcome: "applied", revision: 5, criteriaAdded: 1, linksAdded: 0, createdTaskIds: ["ft-a"] });
    expect(method()).toBe("proposals.apply");
    expect(wire()).toEqual({ repo_path: "/r", id: "enh-1", expected_revision: 2, expected_task_revision: 4, fields: ["acceptance_criteria", "split_into"], reviewed_related: [] });
    expect(wire()).not.toHaveProperty("split_ids");
  });
  it("a split that looks like open work holds the accept until the person names those tasks", async () => {
    const result = { ...refineResult(refine()), enhancement_revision: 2 };
    native.mockResolvedValueOnce(JSON.stringify({ ok: false, outcome: "related_tasks_exist", scan_complete: true,
      splits: [{ index: 0, title: "Indexer", related: [{ item_id: "ft-2", title: "Index things", status: "ready" }] }] }));
    expect(await applyRefine("/r", result, ["split_into"])).toEqual({ outcome: "related_tasks_exist",
      splits: [{ index: 0, title: "Indexer", related: [{ item_id: "ft-2", title: "Index things", status: "ready" }] }] });
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, outcome: "applied", revision: 5, criteria_added: 0, links_added: 0, created_item_ids: ["ft-a"] }));
    expect((await applyRefine("/r", result, ["split_into"], ["ft-2"])).outcome).toBe("applied");
    expect(wire(1)).toMatchObject({ reviewed_related: ["ft-2"] });
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, outcome: "related_tasks_exist", splits: [] }));
    await expect(applyRefine("/r", result, ["split_into"])).rejects.toBeInstanceOf(WorkbenchError);
  });
  it("a stale task is a visible refusal", async () => {
    const result = { ...refineResult(refine()), enhancement_revision: 2 };
    native.mockRejectedValueOnce({ code: "revision_conflict", message: "the task changed before refinement acceptance" });
    await expect(applyRefine("/r", result, ["acceptance_criteria"])).rejects.toMatchObject({ code: "revision_conflict" });
  });
  it("rejecting a proposal dismisses the stored one at its revision", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true }));
    await dismissRefine({ ...refineResult(refine()), enhancement_revision: 2 });
    expect(method()).toBe("enhancements.dismiss");
    expect(wire()).toMatchObject({ id: "enh-1", expected_revision: 2 });
  });
});

describe("stored refinements stay out of the text suggestions list", () => {
  const text = { id: "e0", revision: 2, updated_at: 1, state: "ready", task_id: "t1", source_revision: 4, fields: ["title"], provider: "p", model: "m", automatic: false, created_at: 1, expires_at: 2 };
  const rows = (items: unknown[]) => JSON.stringify({ ok: true, items, shown: items.length, total: items.length, has_more: false, next_cursor: null });
  it("a ready refinement newer than a text proposal does not hide it, and is not decoded as one", async () => {
    native.mockResolvedValueOnce(rows([storedRefine().item, text]));
    expect((await listEnhancements("t1")).items.map((row) => row.id)).toEqual(["e0"]);
    native.mockResolvedValueOnce(rows([storedRefine().item, text]));
    expect((await latestEnhancement("t1", ["ready"]))?.id).toBe("e0");
    expect(wire(1)).toMatchObject({ limit: 30, states: ["ready"] });
  });
});

describe("the reviewed set accumulates across refusals", () => {
  const page = (from: number, to: number) => Array.from({ length: to - from }, (_, n) => ({ item_id: `ft-${from + n}` }));
  it("adds what the person was just shown to what they confirmed before, never replacing it", () => {
    const first = confirmReviewed([], page(0, 25));
    expect(first).toHaveLength(25);
    const second = confirmReviewed(first, page(25, 50));
    expect(second).toHaveLength(50);
    expect(second.slice(0, 25)).toEqual(first);
  });
  it("lists an id once however often it is shown, and ignores blank ids", () => {
    expect(confirmReviewed(["ft-1"], [{ item_id: "ft-1" }, { item_id: " ft-1 " }, { item_id: "ft-2" }, { item_id: "" }, { item_id: "   " }])).toEqual(["ft-1", "ft-2"]);
    expect(confirmReviewed([], [])).toEqual([]);
  });
  it("never carries more than the host accepts in one call", () => {
    const carried = confirmReviewed(confirmReviewed([], page(0, MAX_REVIEWED_RELATED - 1)), page(MAX_REVIEWED_RELATED - 1, MAX_REVIEWED_RELATED + 40));
    expect(carried).toHaveLength(MAX_REVIEWED_RELATED);
    expect(new Set(carried).size).toBe(MAX_REVIEWED_RELATED);
  });
});

describe("the refine diff compares criteria trimmed, on both sides", () => {
  const result = (criteria: string[]) => refineResult(refine({ proposal: { acceptance_criteria: criteria.map((text) => ({ text, cites: [] })), split_into: [], duplicate_of: [] } }));
  it("a criterion differing only in whitespace is already on the task", () => {
    const diff = refineDiff({ ...task, acceptance_criteria: ["  Existing\n"] }, result(["Existing", "\tExisting  ", "Fresh"]));
    expect(diff.criteria.map((c) => [c.text, c.state])).toEqual([["Existing", "present"], ["Fresh", "add"]]);
  });
  it("a whitespace-only criterion is not a criterion, and a repeat within the proposal is one", () => {
    const diff = refineDiff(task, result(["   ", "", "Fresh", " Fresh "]));
    expect(diff.criteria.map((c) => c.text)).toEqual(["Fresh"]);
  });
});

describe("locked fields are never selectable", () => {
  it("acceptance_criteria locked on the task is neither offered nor ticked", () => {
    const locked = { ...task, locked_fields: ["acceptance_criteria" as const] };
    const parts = selectableParts(locked, refineResult(refine()));
    expect(parts.locked).toEqual(["acceptance_criteria"]);
    expect(parts.initial).toEqual([]);
  });
  it("an unlocked task starts with criteria ticked only when something is new", () => {
    expect(selectableParts(task, refineResult(refine())).initial).toEqual(["acceptance_criteria"]);
    const nothingNew = refineResult(refine({ proposal: { acceptance_criteria: [{ text: " Existing ", cites: [] }], split_into: [], duplicate_of: [] } }));
    expect(selectableParts(task, nothingNew).initial).toEqual([]);
  });
  it("title and description locks do not lock a refinement part", () => {
    expect(selectableParts({ ...task, locked_fields: ["title", "description"] }, refineResult(refine())).locked).toEqual([]);
  });
});

describe("a refused apply says which side moved", () => {
  const reviewed = { ...refineResult(refine()), enhancement_revision: 2 };
  it("names a proposal that was settled elsewhere before it names the task", () => {
    expect(applyConflict(reviewed, task, { state: "dismissed", revision: 3 })).toMatchObject({ cause: "proposal" });
    expect(applyConflict(reviewed, { ...task, revision: 9 }, { state: "accepted", revision: 3 }).message).toMatch(/already applied/i);
    expect(applyConflict(reviewed, task, { state: "dismissed", revision: 3 }).message).toMatch(/dismissed/i);
    expect(applyConflict(reviewed, task, { state: "ready", revision: 3 })).toMatchObject({ cause: "proposal" });
  });
  it("names the task when the proposal is as it was reviewed", () => {
    const conflict = applyConflict(reviewed, { ...task, revision: 5 }, { state: "ready", revision: 2 });
    expect(conflict.cause).toBe("task");
    expect(conflict.message).toMatch(/task changed/i);
  });
  it("says what it does not know when neither could be re-read", () => {
    const conflict = applyConflict(reviewed, null, null);
    expect(conflict.cause).toBe("unknown");
    expect(conflict.message).toMatch(/nothing was applied/i);
  });
});

describe("adversarial: the apply path's reviewed set across several held splits", () => {
  // A host that behaves as intake.rs does for `proposals.apply`: each split
  // names the first 25 of its related tasks the caller has not reviewed, and
  // the accept goes through only when no split names any.
  const hostWith = (perSplit: { item_id: string }[][]) => {
    native.mockReset();
    native.mockImplementation(async (_cmd, args) => {
      const sent = JSON.parse(String((args as { input: string }).input)) as { reviewed_related: string[] };
      const splits = perSplit.map((related, index) => ({ index, title: `S${index}`,
        related: related.filter((r) => !sent.reviewed_related.includes(r.item_id)).slice(0, 25).map((r) => ({ ...r, title: r.item_id, status: "todo" })) }))
        .filter((split) => split.related.length > 0);
      return JSON.stringify(splits.length ? { ok: false, outcome: "related_tasks_exist", scan_complete: true, splits }
        : { ok: true, outcome: "applied", revision: 5, criteria_added: 0, links_added: 0, created_item_ids: perSplit.map((_, n) => `ft-s${n}`) });
    });
  };
  const ids = (prefix: string, n: number) => Array.from({ length: n }, (_, i) => ({ item_id: `${prefix}-${i}` }));
  const confirmUntilApplied = async () => {
    const result = { ...refineResult(refine()), enhancement_revision: 2 };
    let reviewed: string[] = [];
    let outcome = await applyRefine("/r", result, ["split_into"], reviewed);
    let rounds = 0;
    while (outcome.outcome === "related_tasks_exist") {
      if (++rounds > 10) throw new Error("the review does not converge");
      // Exactly what TaskRefine's "apply anyway" passes: every split's related rows.
      reviewed = confirmReviewed(reviewed, outcome.splits.flatMap((split) => split.related));
      outcome = await applyRefine("/r", result, ["split_into"], reviewed);
    }
    return { rounds, reviewed: reviewed.length, outcome: outcome.outcome };
  };
  it("two splits with 30 disjoint related tasks each converge in two confirmations", async () => {
    hostWith([ids("a", 30), ids("b", 30)]);
    expect(await confirmUntilApplied()).toEqual({ rounds: 2, reviewed: 60, outcome: "applied" });
  });
  it("splits that share their related tasks are confirmed once, not once per split", async () => {
    const shared = ids("s", 60);
    hostWith([shared, shared, shared]);
    expect(await confirmUntilApplied()).toEqual({ rounds: 3, reviewed: 60, outcome: "applied" });
  });
  it("a page whose rows repeat an id already confirmed never grows the set past the distinct ids", async () => {
    const reviewed = confirmReviewed(confirmReviewed([], ids("x", 25)), [...ids("x", 25), ...ids("y", 5)]);
    expect(reviewed).toHaveLength(30);
    expect(new Set(reviewed).size).toBe(30);
  });
});
