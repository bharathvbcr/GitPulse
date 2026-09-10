import { afterEach, describe, expect, it, vi } from "vitest";
import { WorkbenchError, type Enhancement, type Task } from "./client";
import { EnhancementAction, acceptEnhancementInput, startQuickEnhance } from "./taskEnhance";
import { TASK_ACTION_TIMEOUT_MS } from "./taskActions";

const task: Task = {
  id: "t", revision: 4, updated_at: 1, title: "Fix E42", description: "Keep evidence",
  kind: "bug", status: "ready", priority: 1, severity: null, owner: null, due_at: null,
  labels: [], acceptance_criteria: [], repository_ids: ["r"], primary_repository_id: "r",
  home_workspace_id: null, position: 1, locked_fields: [],
};
const proposal: Enhancement = {
  id: "e", revision: 3, updated_at: 1, task_id: "t", source_revision: 4, source: task,
  fields: ["title", "description"], state: "ready", provider: "local", model: "fixture",
  automatic: false, created_at: 1, expires_at: 200, worker_id: null, failure: "",
  accepted_fields: [], edited_fields: [], outcome_uncertain: false, original_proposed: null,
  proposed: {title: "Resolve E42", description: "Retain evidence"}, rationale: "",
};
const input = {id: "e", request_id: "accept", expected_revision: 3, expected_task_revision: 4, fields: ["title"]};
const accepted: Enhancement = {...proposal, revision: 4, state: "accepted", accepted_fields: ["title"]};
const saved: Task = {...task, title: "Resolve E42", revision: 5};
afterEach(() => vi.useRealTimers());

describe("enhancement transaction adversarial boundaries", () => {
  it.each(["transport_error", "worker_error", "store_error", "protocol_error"])("reuses the exact receipt after %s", async code => {
    const change = vi.fn().mockRejectedValueOnce(new WorkbenchError(code, "lost reply")).mockResolvedValue(accepted);
    const action = new EnhancementAction({change, read: vi.fn().mockResolvedValue(saved)});
    const original = structuredClone(input);
    await expect(action.run("enhancements.accept", original, task.id)).rejects.toThrow("lost reply");
    original.fields.push("description");
    original.expected_task_revision = 99;
    expect(action.pending?.input).toEqual(input);
    await expect(action.run("enhancements.accept", {...input, request_id: "different"}, task.id)).rejects.toThrow(/pending/);
    const pending = action.pending!;
    expect((await action.run(pending.method, pending.input, pending.taskID)).task).toEqual(saved);
    expect(change.mock.calls[1]).toEqual(change.mock.calls[0]);
    expect(action.pending).toBeNull();
  });

  it("only retries the read after acceptance committed and task refresh failed", async () => {
    const change = vi.fn().mockResolvedValue(accepted);
    const read = vi.fn().mockRejectedValueOnce(new WorkbenchError("transport_error", "read offline")).mockResolvedValue(saved);
    const action = new EnhancementAction({change, read});
    await expect(action.run("enhancements.accept", input, task.id)).rejects.toThrow("read offline");
    const pending = action.pending!;
    await action.run(pending.method, pending.input, pending.taskID);
    expect(change).toHaveBeenCalledTimes(1);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it.each(["create", "generate"])("retains the %s receipt across a lost reply without recreating the proposal", async phase => {
    const change = vi.fn().mockImplementation(async (method: string) => {
      if (method === `enhancements.${phase}` && change.mock.calls.filter(([m]) => m === method).length === 1) throw new WorkbenchError("transport_error", "lost");
      return {...proposal, revision: method === "enhancements.create" ? 1 : 2, state: method === "enhancements.create" ? "pending" : "running"};
    });
    const action = new EnhancementAction({change, read: vi.fn()});
    const create = {id: "e", request_id: "create", expected_revision: 0, task_id: "t", source_revision: 4};
    await expect(action.run("enhancements.create", create, task.id)).rejects.toThrow("lost");
    const pending = action.pending!;
    expect(pending.method).toBe(`enhancements.${phase}`);
    expect((await action.run(pending.method, pending.input, pending.taskID)).proposal.state).toBe("running");
    const repeats = change.mock.calls.filter(([method]) => method === `enhancements.${phase}`);
    expect(repeats[0]).toEqual(repeats[1]);
    expect(change).toHaveBeenCalledTimes(3);
  });

  it("refuses a 100-click burst while one acceptance is in flight", async () => {
    let release: (value: Enhancement) => void = () => { throw new Error("not started"); };
    const change = vi.fn(() => new Promise<Enhancement>(resolve => { release = resolve; }));
    const action = new EnhancementAction({change, read: vi.fn().mockResolvedValue(saved)});
    const first = action.run("enhancements.accept", input, task.id);
    const burst = await Promise.allSettled(Array.from({length: 100}, () => action.run("enhancements.accept", input, task.id)));
    expect(burst.every(result => result.status === "rejected")).toBe(true);
    release(accepted); await first;
    expect(change).toHaveBeenCalledTimes(1);
  });

  it("bounds a stalled mutation and keeps its original identity for reconciliation", async () => {
    vi.useFakeTimers();
    const action = new EnhancementAction({change: vi.fn(() => new Promise<Enhancement>(() => {})), read: vi.fn()});
    const result = expect(action.run("enhancements.accept", input, task.id)).rejects.toThrow(/timed out/);
    await vi.advanceTimersByTimeAsync(TASK_ACTION_TIMEOUT_MS);
    await result;
    expect(action.pending?.input).toEqual(input);
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each([{...accepted, id: "foreign"}, {...accepted, task_id: "foreign"}, {...accepted, revision: 3}])("rejects mismatched mutation confirmations", async result => {
    const read = vi.fn();
    const action = new EnhancementAction({change: vi.fn().mockResolvedValue(result), read});
    await expect(action.run("enhancements.accept", input, task.id)).rejects.toThrow(/confirmation/);
    expect(action.pending).not.toBeNull();
    expect(read).not.toHaveBeenCalled();
  });

  it.each([{...saved, id: "foreign"}, {...saved, revision: 4}])("rejects wrong or stale task confirmations", async result => {
    const action = new EnhancementAction({change: vi.fn().mockResolvedValue(accepted), read: vi.fn().mockResolvedValue(result)});
    await expect(action.run("enhancements.accept", input, task.id)).rejects.toThrow(/confirmation/);
    expect(action.pending?.result).toEqual(accepted);
  });

  it("a definite revision conflict releases the pending mutation without retries", async () => {
    const change = vi.fn().mockRejectedValue(new WorkbenchError("revision_conflict", "Task changed"));
    const action = new EnhancementAction({change, read: vi.fn()});
    await expect(action.run("enhancements.accept", input, task.id)).rejects.toThrow("Task changed");
    expect(action.pending).toBeNull();
    expect(change).toHaveBeenCalledTimes(1);
  });

  it("empty or fully locked field choices never widen into all fields", async () => {
    const change = vi.fn();
    const action = new EnhancementAction({change, read: vi.fn()});
    const config = {provider: "local", model: "fixture", providers: ["local"], model_source: "fixture"};
    await expect(startQuickEnhance(task, [], config, action)).rejects.toThrow(/Nothing/);
    await expect(startQuickEnhance({...task, locked_fields: ["title"]}, ["title"], config, action)).rejects.toThrow(/Nothing/);
    expect(change).not.toHaveBeenCalled();
    expect(acceptEnhancementInput(proposal, {...task, id: "other"}, ["title"], "r")).toBeNull();
    expect(acceptEnhancementInput(proposal, {...task, revision: 5}, ["title"], "r")).toBeNull();
  });
});
