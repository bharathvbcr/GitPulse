import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { getTaskBrief, taskBrief, type Task } from "./client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const task: Task = { id: "task", revision: 2, updated_at: 12, title: "Preserve E42", description: "exact error: E42\n$(touch NEVER_EXECUTE) 🧪", kind: "bug", status: "ready", priority: 0, severity: "high", owner: "Pat", due_at: 12345, labels: ["regression"], repository_ids: ["r2", "r1"], primary_repository_id: "r1", home_workspace_id: "w", position: 0, acceptance_criteria: ["Keep both repositories"], locked_fields: ["title"] };
const reference = (id: string) => ({ id, revision: 7, updated_at: 10, name: `Repository ${id}` });
const brief = { id: task.id, revision: task.revision, updated_at: task.updated_at, format_version: 1, task, repositories: [reference("r2"), reference("r1")], workspace: { ...reference("w"), name: "Workspace" }, markdown: `# Task brief v1\nTask: task (revision 2)\n${task.description}\nr2 [r2]\n` };
beforeEach(() => native.mockReset());

describe("canonical saved task briefs", () => {
  it("loads the exact revision once and preserves canonical text without local formatting", async () => {
    native.mockResolvedValue(JSON.stringify({ ok: true, item: brief }));
    const result = await getTaskBrief(task.id, task.revision);
    expect(result).toEqual(brief);
    expect(result.markdown).toContain("revision 2");
    expect(result.markdown).toContain("exact error: E42");
    expect(result.markdown).toContain("r2 [r2]");
    expect(native.mock.calls).toEqual([["cmd_workbench_request", { method: "items.brief.get", input: '{"id":"task","expected_revision":2}' }]]);
  });
  it("requires the complete ordered repository vector and matching workspace", () => {
    for (const mutation of [
      { repositories: [] }, { repositories: [reference("r2")] },
      { repositories: [reference("r1"), reference("r2")] },
      { repositories: [reference("r2"), reference("r2")] },
      { workspace: null }, { workspace: reference("other") },
      { repositories: [reference("r2"), { ...reference("r1"), revision: 0 }] },
      { repositories: [reference("r2"), { ...reference("r1"), name: "🧪".repeat(76) }] },
      { repositories: [reference("r2"), { ...reference("r1"), name: "\0" }] },
    ]) expect(() => taskBrief({ ...brief, ...mutation })).toThrow("invalid response");
    expect(taskBrief({ ...brief, task: { ...task, home_workspace_id: null }, workspace: null }).workspace).toBeNull();
  });
  it("rejects version mismatches and malformed or oversized canonical text", () => {
    for (const mutation of [{ id: "other" }, { revision: 3 }, { updated_at: 13 }, { format_version: 2 }, { markdown: "" }, { markdown: "\0" }, { markdown: "🧪".repeat(524289) }]) {
      expect(() => taskBrief({ ...brief, ...mutation })).toThrow("invalid response");
    }
    const markdown = "🧪".repeat(16_384);
    expect(taskBrief({ ...brief, markdown }).markdown).toBe(markdown);
  });
  it("refuses a structurally valid response for a different request", async () => {
    native.mockResolvedValue(JSON.stringify({ ok: true, item: brief }));
    await expect(getTaskBrief("other", 2)).rejects.toMatchObject({ code: "protocol_error" });
    await expect(getTaskBrief(task.id, 3)).rejects.toMatchObject({ code: "protocol_error" });
  });
  it("preserves stale and unavailable errors without falling back to old editor text", async () => {
    native.mockRejectedValueOnce({ code: "revision_conflict", message: "Reload the task" });
    await expect(getTaskBrief(task.id, 1)).rejects.toMatchObject({ code: "revision_conflict", message: "Reload the task" });
    native.mockRejectedValueOnce(new Error("Store unavailable"));
    await expect(getTaskBrief(task.id, 2)).rejects.toMatchObject({ code: "transport_error" });
    expect(native).toHaveBeenCalledTimes(2);
  });
});
