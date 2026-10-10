import { describe, expect, it } from "vitest";
import { enhancement, enhancementKind, enhancementSummary, page, storedRefinement, taskCard, task, taskDraft, taskWrite, scopeParams } from "./client";

const full = { id: "t1", revision: 2, updated_at: 1, title: "Preserve task evidence", description: "exact error: E42", acceptance_criteria: ["Round-trip evidence"], kind: "bug", status: "ready", priority: 1, severity: null, owner: null, due_at: null, labels: ["regression"], repository_ids: ["r1", "r2"], primary_repository_id: "r1", home_workspace_id: null, position: 10 };
describe("workbench boundary", () => {
  it("does not use a card as a full task for replacement", () => {
    const card = taskCard(full);
    expect(card).not.toHaveProperty("description");
    expect(() => task(card)).toThrow("invalid response");
    const body = taskWrite(full.id, full.revision, { ...taskDraft(task(full)), status: "review" });
    expect(body).toMatchObject({ description: "exact error: E42", acceptance_criteria: ["Round-trip evidence"], labels: ["regression"], repository_ids: ["r1", "r2"], expected_revision: 2, status: "review" });
    expect(body).not.toHaveProperty("updated_at");
    expect(body).not.toHaveProperty("due_at");
  });
  it("preserves original counts and rejects failed or inconsistent pages", () => {
    const response = { ok: true, items: [full], shown: 1, total: 100, has_more: true, next_cursor: "1:10:t1" };
    expect(page(response, taskCard).total).toBe(100);
    for (const mutation of [{ ok: false }, { shown: 0 }, { total: 0 }, { next_cursor: null }, { items: null }, { total: Infinity }, { has_more: "true" }]) expect(() => page({ ...response, ...mutation }, taskCard)).toThrow();
  });
  it("rejects malformed links, unknown state and non-versioned records", () => {
    for (const change of [{ revision: 0 }, { status: "probably_done" }, { priority: 5 }, { repository_ids: [] }, { repository_ids: ["r1", "r1"] }, { primary_repository_id: "other" }, { title: null }]) expect(() => taskCard({ ...full, ...change })).toThrow();
  });
  it("maps all three board scopes", () => {
    expect(scopeParams({ kind: "global" })).toEqual({});
    expect(scopeParams({ kind: "workspace", id: "w" })).toEqual({ workspace_id: "w" });
    expect(scopeParams({ kind: "repository", id: "r" })).toEqual({ repository_id: "r" });
  });
  it("preserves field locks through complete-record edits and refuses unknown lock targets", () => {
    const loaded = task({ ...full, locked_fields: ["title"] });
    expect(taskWrite(full.id, full.revision, taskDraft(loaded))).toHaveProperty("locked_fields", ["title"]);
    for (const locked_fields of [["title", "title"], ["push"], "title", ["title", "description", "title"]]) {
      expect(() => task({ ...full, locked_fields })).toThrow("invalid response");
    }
  });
  it("decodes the acceptance_criteria lock schema 12 added, so a locked task still loads", () => {
    const loaded = task({ ...full, locked_fields: ["title", "acceptance_criteria"] });
    expect(loaded.locked_fields).toEqual(["title", "acceptance_criteria"]);
    expect(taskWrite(full.id, full.revision, taskDraft(loaded))).toHaveProperty("locked_fields", ["title", "acceptance_criteria"]);
  });
  it("tells refinement proposals from text ones, and never reads one as the other", () => {
    const stored = { id: "e1", revision: 2, updated_at: 1, kind: "refine", state: "ready", task_id: "t1", source_revision: 2, fields: ["acceptance_criteria", "split_into"], provider: "p", model: "m", automatic: false, created_at: 1, expires_at: 2,
      proposed: { acceptance_criteria: ["a"], split_into: [{ title: "T", description: "D" }], duplicate_of: [] } };
    expect(enhancementKind(stored)).toBe("refine");
    expect(enhancementKind({ ...stored, kind: undefined })).toBe("text");
    expect(() => enhancementKind({ ...stored, kind: "other" })).toThrow("invalid response");
    expect(() => enhancementSummary(stored)).toThrow("invalid response");
    expect(() => enhancement({ ...stored, source: full })).toThrow("invalid response");
    expect(storedRefinement(stored).proposed.split_into).toEqual([{ title: "T", description: "D" }]);
    expect(storedRefinement({ ...stored, state: "accepted", accepted_fields: ["split_into"], created_item_ids: ["ft-a"], applied_task_revision: 3 })).toMatchObject({ created_item_ids: ["ft-a"], applied_task_revision: 3 });
    for (const bad of [{ fields: ["title"] }, { fields: [] }, { created_item_ids: [1] }, { kind: "text" }]) expect(() => storedRefinement({ ...stored, ...bad })).toThrow("invalid response");
  });
  it("round-trips pasted logs and treats a missing field as absent rather than invalid", () => {
    expect(task(full).logs).toBeUndefined();
    const dumped = "error: E42\n    at src/main.rs:12";
    const loaded = task({ ...full, logs: dumped });
    expect(loaded.logs).toBe(dumped);
    expect(taskWrite(full.id, full.revision, taskDraft(loaded))).toMatchObject({ logs: dumped });
    expect(taskWrite(full.id, full.revision, taskDraft(task(full)))).not.toHaveProperty("logs");
    expect(() => task({ ...full, logs: 12 })).toThrow("invalid response");
    expect(() => task({ ...full, logs: "a\0b" })).toThrow("invalid response");
  });
});
