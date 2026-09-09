import { describe, expect, it } from "vitest";
import { enhancement, enhancementSummary } from "./client";

const source = { id: "t", revision: 2, updated_at: 10, title: "Resolve E42", description: "Keep evidence", kind: "bug", status: "backlog", priority: 1, severity: null, owner: null, due_at: null, labels: [], acceptance_criteria: ["Reproduce the failure"], repository_ids: ["r"], primary_repository_id: "r", home_workspace_id: null, position: 1, locked_fields: [] };
const pending = { id: "e", revision: 1, updated_at: 10, created_at: 10, expires_at: 130, task_id: "t", source_revision: 2, source, fields: ["title", "description"], state: "pending", provider: "local", model: "configured-model", automatic: false };

describe("versioned enhancement boundary", () => {
  it("requires full source for review and keeps summaries separate", () => {
    expect(enhancement(pending).source).toEqual(source);
    const { source: _source, ...summary } = pending;
    expect(enhancementSummary(summary)).not.toHaveProperty("source");
    expect(() => enhancement(summary)).toThrow();
  });

  it("refuses unrelated source, missing worker ownership and invented states", () => {
    for (const change of [
      { task_id: "other" }, { source_revision: 3 }, { state: "probably_complete" },
      { source: { ...source, revision: 0 } }, { fields: [] }, { fields: ["title", "title"] },
      { fields: ["skip_permissions"] }, { provider: "" }, { model: "" },
      { state: "running" }, { state: "cancel_requested", worker_id: "" },
      { state: "failed" }, { state: "interrupted", outcome_uncertain: false },
    ]) expect(() => enhancement({ ...pending, ...change })).toThrow();
  });

  it("preserves complete suggestions and selected acceptance without implying task completion", () => {
    const ready = { ...pending, revision: 3, state: "ready", proposed: { title: "Investigate E42", description: "Preserve the observed failure." }, rationale: "Clarified the action." };
    expect(enhancement(ready)).toMatchObject({ state: "ready", source: { status: "backlog" }, proposed: ready.proposed });
    for (const proposed of [{ title: "Changed" }, { ...ready.proposed, permissions: "skip" }, { ...ready.proposed, title: null }, { ...ready.proposed, title: "x".repeat(301) }]) {
      expect(() => enhancement({ ...ready, proposed })).toThrow();
    }
    expect(enhancement({ ...ready, state: "accepted", accepted_fields: ["title"] })).toMatchObject({ accepted_fields: ["title"] });
    expect(() => enhancement({ ...ready, state: "accepted", accepted_fields: ["permission"] })).toThrow();
  });

  it("retains cancellation and recovery uncertainty as distinct states", () => {
    expect(enhancement({ ...pending, state: "cancel_requested", worker_id: "worker", started_at: 20 }).state).toBe("cancel_requested");
    expect(enhancement({ ...pending, state: "interrupted", worker_id: "worker", failure: "Provider outcome is unknown", outcome_uncertain: true }).outcome_uncertain).toBe(true);
    expect(enhancement({ ...pending, state: "cancelled", worker_id: "worker", failure: "User cancelled" }).state).toBe("cancelled");
  });

  it("preserves original model text and refuses inconsistent edit attribution", () => {
    const original = { title: "Resolve E42", description: "Preserve evidence" };
    const revised = { ...pending, state: "ready", proposed: { ...original, title: "Investigate the E42 startup failure" }, original_proposed: original, edited_fields: ["title"] };
    expect(enhancement(revised)).toMatchObject({ original_proposed: original, edited_fields: ["title"], proposed: revised.proposed });
    expect(enhancementSummary(revised)).not.toHaveProperty("original_proposed");
    for (const changes of [{ original_proposed: undefined }, { original_proposed: { title: "missing description" } }, { edited_fields: [] }, { edited_fields: ["description"] }, { edited_fields: ["permission"] }, { original_proposed: { ...original, title: "x".repeat(301) } }]) {
      expect(() => enhancement({ ...revised, ...changes })).toThrow();
    }
  });
});
