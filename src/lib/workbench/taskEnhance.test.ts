import { describe, expect, it } from "vitest";
import type { Task } from "./client";
import { acceptEnhancementInput, createEnhancementInput, reviewable } from "./taskEnhance";

const task: Task = {
  id: "t1", revision: 4, updated_at: 1, title: "Keep E42", description: "evidence",
  kind: "bug", status: "ready", priority: 1, severity: null, owner: null, due_at: null,
  labels: [], acceptance_criteria: [], repository_ids: ["r"], primary_repository_id: "r",
  home_workspace_id: null, position: 1, locked_fields: ["title"],
};

describe("createEnhancementInput", () => {
  it("drops locked fields, empty providers and oversize models", () => {
    expect(createEnhancementInput(task, ["title", "description"], "local", "m", { id: "e", requestId: "r" }))
      .toMatchObject({ fields: ["description"], task_id: "t1", source_revision: 4, expected_revision: 0 });
    expect(createEnhancementInput(task, ["title"], "local", "m", { id: "e", requestId: "r" })).toBeNull();
    expect(createEnhancementInput(task, ["description"], "  ", "m", { id: "e", requestId: "r" })).toBeNull();
    expect(createEnhancementInput(task, ["description"], "local", "x".repeat(513), { id: "e", requestId: "r" })).toBeNull();
    expect(createEnhancementInput(task, ["description"], "local", "m", { id: "", requestId: "r" })).toBeNull();
  });
});

describe("reviewable", () => {
  it("is true only for a ready proposal", () => {
    expect(reviewable(null)).toBe(false);
    expect(reviewable({ state: "ready" } as never)).toBe(true);
    expect(reviewable({ state: "pending" } as never)).toBe(false);
  });
});

describe("acceptEnhancementInput", () => {
  it("accepts only ready, unlocked, proposed fields with a real request identity", () => {
    const ready = { id: "e", revision: 3, state: "ready", fields: ["title", "description"] } as never;
    expect(acceptEnhancementInput(ready, task, ["title", "description"], "r")).toMatchObject({
      fields: ["description"],
      expected_task_revision: 4,
    });
    expect(acceptEnhancementInput(ready, task, ["title"], "r")).toBeNull();
    expect(acceptEnhancementInput({ id: "e", revision: 3, state: "pending", fields: ["description"] } as never, task, ["description"], "r")).toBeNull();
    expect(acceptEnhancementInput(ready, task, ["description"], "")).toBeNull();
  });
});
