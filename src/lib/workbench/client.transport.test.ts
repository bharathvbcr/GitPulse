import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  WorkbenchError, explainError, getTask, getWorkspace, listRepositories,
  listTasks, listWorkspaces, putTask, putWorkspace, registerRepository,
  request, taskDraft, taskWrite, workspaceDraft,
  getEnhancement, listEnhancements, changeEnhancement, enhancementConfiguration,
  generateEnhancement, deleteTask, deleteWorkspace,
} from "./client";
import type { Task, Workspace } from "./client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
const repo = { id: "repo_1", revision: 1, updated_at: 100, name: "Manvi", identity_key: "local:/code/Manvi/.git", remote_url: null };
const group: Workspace = { id: "workspace_1", revision: 3, updated_at: 120, name: "Agentic tools", description: "Shared tooling", icon: "layers", color: "blue", position: 1, pinned: true, archived: false, repository_ids: ["repo_1", "repo_2"] };
const item: Task = { id: "task_1", revision: 7, updated_at: 130, title: "Preserve repository scope", description: "Keep the original error E42 and both repository links.", acceptance_criteria: ["Both repository checks pass"], kind: "bug", status: "review", priority: 1, severity: "high", owner: "Bharath", due_at: 2_000, labels: ["regression"], repository_ids: ["repo_1", "repo_2"], primary_repository_id: "repo_1", home_workspace_id: "workspace_1", position: 2 };

function reply(items: unknown[], total = items.length, cursor: string | null = null): string {
  return JSON.stringify({ ok: true, items, shown: items.length, total, has_more: cursor !== null, next_cursor: cursor });
}

beforeEach(() => native.mockReset());

describe("native workbench boundary", () => {
  it("registers a path as data with independent record and request identities", async () => {
    native.mockResolvedValue(JSON.stringify({ repository: repo }));
    const path = "/tmp/repository with spaces/$(touch NEVER_EXECUTE)";
    expect(await registerRepository(path)).toEqual(repo);
    const call = native.mock.calls[0];
    expect(call?.[0]).toBe("cmd_workbench_register_repository");
    const args = call?.[1];
    expect(args).toMatchObject({ repoPath: path, id: expect.any(String), requestId: expect.any(String) });
    if (typeof args !== "object" || args === null || !("id" in args) || !("requestId" in args)) throw new Error("Missing registration identity");
    expect(args.id).not.toBe(args.requestId);
  });

  it("paginates repositories and archived groups without replacing server totals", async () => {
    native.mockResolvedValueOnce(reply([repo], 401, "repo-cursor"));
    expect(await listRepositories()).toMatchObject({ total: 401, shown: 1, next_cursor: "repo-cursor" });
    native.mockResolvedValueOnce(reply([], 401));
    await listRepositories("repo-cursor");
    native.mockResolvedValueOnce(reply([{ ...group, repository_count: 2 }], 301, "group-cursor"));
    expect(await listWorkspaces()).toMatchObject({ total: 301, items: [{ pinned: true, repository_count: 2 }] });
    native.mockResolvedValueOnce(reply([], 301));
    await listWorkspaces("group-cursor");
    expect(native.mock.calls.map((call) => call[1])).toEqual([
      { method: "repositories.list", input: JSON.stringify({ limit: 200 }) },
      { method: "repositories.list", input: JSON.stringify({ limit: 200, cursor: "repo-cursor" }) },
      { method: "workspaces.list", input: JSON.stringify({ limit: 200, include_archived: true }) },
      { method: "workspaces.list", input: JSON.stringify({ limit: 200, include_archived: true, cursor: "group-cursor" }) },
    ]);
  });

  it("keeps opaque cursors, search text and scope through task pagination", async () => {
    native.mockResolvedValueOnce(reply([item], 101, "opaque:cursor"));
    const first = await listTasks({ kind: "workspace", id: group.id }, "review", '"E42" path:foo');
    expect(first).toMatchObject({ total: 101, shown: 1, next_cursor: "opaque:cursor" });
    expect(first.items[0]).not.toHaveProperty("description");
    native.mockResolvedValueOnce(reply([], 101));
    await listTasks({ kind: "workspace", id: group.id }, "review", '"E42" path:foo', first.next_cursor ?? undefined);
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", {
      method: "items.list", input: JSON.stringify({ workspace_id: group.id, status: "review", query: '"E42" path:foo', limit: 30, cursor: "opaque:cursor" }),
    });
  });

  it("loads complete records before edits and retains membership in group replacements", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item }));
    expect(await getTask(item.id)).toEqual(item);
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: group }));
    const loaded = await getWorkspace(group.id);
    expect(loaded).toEqual(group);
    const draft = workspaceDraft(loaded);
    expect(draft).not.toHaveProperty("revision");
    const input = { ...draft, name: "Developer tools", id: group.id, expected_revision: group.revision, request_id: "group-write-1" };
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: { ...group, name: input.name, revision: 4 } }));
    expect(await putWorkspace(input)).toMatchObject({ revision: 4, name: input.name, repository_ids: ["repo_1", "repo_2"] });
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", { method: "workspaces.put", input: JSON.stringify(input) });
  });

  it("preserves full evidence and the identical receipt on an uncertain write retry", async () => {
    const body = taskWrite(item.id, item.revision, { ...taskDraft(item), status: "done" });
    native.mockRejectedValueOnce(new Error("Connection closed after write"));
    await expect(putTask(body)).rejects.toMatchObject({ code: "transport_error" });
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: { ...item, status: "done", revision: 8 } }));
    expect(await putTask(body)).toMatchObject({ revision: 8, description: item.description, due_at: item.due_at, repository_ids: item.repository_ids });
    expect(native.mock.calls[0]).toEqual(native.mock.calls[1]);
    expect(body).toMatchObject({ expected_revision: 7, request_id: expect.any(String), acceptance_criteria: item.acceptance_criteria, primary_repository_id: item.primary_repository_id });
  });

  it("wakes only committed text saves and keeps a worker failure separate from the saved task", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item, automatic_enhancement_queued: true }));
    native.mockRejectedValueOnce({ code: "not_installed", message: "Manvi is unavailable" });
    expect(await putTask({ request_id: "save-once" })).toEqual(item);
    await vi.waitFor(() => expect(native).toHaveBeenCalledTimes(2));
    expect(native.mock.calls[1]).toEqual(["cmd_workbench_request", { method: "enhancements.wake", input: "{}" }]);
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item, automatic_enhancement_queued: false }));
    expect(await putTask({ request_id: "move-once" })).toEqual(item);
    expect(native).toHaveBeenCalledTimes(3);
  });

  it("deletes a task with the caller-supplied request identity and refuses a failed receipt", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: { ...item, deleted: true, revision: 8 }, sequence: 1 }));
    await deleteTask(item.id, item.revision, "del-1");
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", {
      method: "items.delete",
      input: JSON.stringify({ id: item.id, expected_revision: item.revision, request_id: "del-1" }),
    });
    native.mockResolvedValueOnce(JSON.stringify({ ok: false }));
    await expect(deleteTask(item.id, item.revision, "del-1")).rejects.toMatchObject({ code: "protocol_error" });
  });

  it("deletes a workspace through the same receipt gate", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: { ...group, deleted: true }, sequence: 2 }));
    await deleteWorkspace(group.id, group.revision, "ws-del");
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", {
      method: "workspaces.delete",
      input: JSON.stringify({ id: group.id, expected_revision: group.revision, request_id: "ws-del" }),
    });
  });

  it.each(["revision_conflict", "not_found", "busy", "store_error"])("preserves native %s errors for recovery decisions", async (code) => {
    native.mockRejectedValueOnce({ code, message: "A specific storage failure" });
    await expect(getTask(item.id)).rejects.toMatchObject({ code, message: "A specific storage failure" });
    expect(native).toHaveBeenCalledTimes(1);
  });

  it.each([undefined, null, 5, { ok: true }, "{truncated", "x".repeat(2 * 1024 * 1024 + 1)])("rejects malformed or oversized native envelopes %#", async (value) => {
    native.mockResolvedValueOnce(value);
    await expect(request("items.get", { id: item.id })).rejects.toMatchObject({ code: "protocol_error" });
  });

  it.each([
    { ok: false, item },
    { ok: true, item: { ...item, revision: 0 } },
    { ok: true, item: { ...item, description: undefined } },
    { ok: true, item: { ...item, due_at: -1 } },
    { ok: true, item: { ...item, acceptance_criteria: [42] } },
    { ok: true, item: { ...item, repository_ids: ["repo_1", "repo_1"] } },
  ])("refuses failed or incomplete records even when JSON parsing succeeds %#", async (response) => {
    native.mockResolvedValueOnce(JSON.stringify(response));
    await expect(getTask(item.id)).rejects.toBeInstanceOf(WorkbenchError);
  });

  it("shows truthful error messages when a transport has no structured error code", () => {
    expect(explainError(new Error("Disconnected"))).toBe("Disconnected");
    expect(explainError({ message: "Unavailable" })).toBe("Unavailable");
    expect(explainError("Timed out")).toBe("Timed out");
    expect(explainError(null)).toBe("Task storage is unavailable. Try again.");
  });

  it("loads review details separately from bounded enhancement history and reuses uncertain acceptance identities", async () => {
    const proposal = { id: "e", revision: 3, updated_at: 140, created_at: 135, expires_at: 255, task_id: item.id, source_revision: item.revision, source: item, fields: ["title"], state: "ready", provider: "local", model: "saved-model", automatic: false, proposed: { title: "Preserve scope across both repositories" } };
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: proposal }));
    expect(await getEnhancement("e")).toMatchObject({ source: item, state: "ready" });
    native.mockResolvedValueOnce(reply([proposal], 42, "next"));
    const first = await listEnhancements(item.id);
    expect(first).toMatchObject({ total: 42, shown: 1, next_cursor: "next" });
    expect(first.items[0]).not.toHaveProperty("source");
    native.mockResolvedValueOnce(reply([], 42));
    await listEnhancements(item.id, "next");
    expect(native).toHaveBeenLastCalledWith("cmd_workbench_request", { method: "enhancements.list", input: JSON.stringify({ task_id: item.id, newest: true, limit: 30, cursor: "next" }) });
    const input = { id: "e", request_id: "accept-once", expected_revision: 3, expected_task_revision: 7, fields: ["title"] };
    native.mockRejectedValueOnce({ code: "transport_error", message: "Reply lost after commit" });
    await expect(changeEnhancement("enhancements.accept", input)).rejects.toMatchObject({ code: "transport_error" });
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: { ...proposal, state: "accepted", revision: 4, accepted_fields: ["title"] } }));
    expect(await changeEnhancement("enhancements.accept", input)).toMatchObject({ state: "accepted", accepted_fields: ["title"] });
    expect(native.mock.calls[3]).toEqual(native.mock.calls[4]);
  });

  it("reads explicit configuration without inferring provider health or initiating generation", async () => {
    const settings = { ok: true, provider: "local", model: "", model_source: "none", providers: ["local", "anthropic"] };
    native.mockResolvedValueOnce(JSON.stringify(settings));
    expect(await enhancementConfiguration()).toEqual({ provider: "local", model: "", model_source: "none", providers: settings.providers });
    expect(native.mock.calls).toEqual([["cmd_workbench_request", { method: "enhancements.configuration", input: "{}" }]]);
    for (const change of [{ ok: false }, { providers: [] }, { providers: ["local", "local"] }, { provider: "missing" }, { model: "x".repeat(513) }, { providers: [42] }, { model_source: null }]) {
      native.mockResolvedValueOnce(JSON.stringify({ ...settings, ...change }));
      await expect(enhancementConfiguration()).rejects.toMatchObject({ code: "protocol_error" });
    }
  });

  it("threads the local model selection into configuration and generate host calls", async () => {
    const selection = { base_url: "http://127.0.0.1:11434/v1", model: "qwen" };
    const settings = { ok: true, provider: "local", model: "qwen", model_source: "env", providers: ["local"] };
    native.mockResolvedValueOnce(JSON.stringify(settings));
    expect(await enhancementConfiguration(selection)).toMatchObject({ model: "qwen", model_source: "env" });
    expect(JSON.parse(String((native.mock.calls[0]?.[1] as { input: string }).input))).toEqual({ model: selection });

    const proposal = {
      id: "e", revision: 2, updated_at: 140, created_at: 135, expires_at: 255, task_id: item.id,
      source_revision: item.revision, source: item, fields: ["title"], state: "running",
      provider: "local", model: "qwen", automatic: false,
      worker_id: "worker-1", failure: "", accepted_fields: [], edited_fields: [], outcome_uncertain: false,
      rationale: "",
    };
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: proposal }));
    await generateEnhancement({ id: "e", request_id: "r", expected_revision: 1 }, selection);
    expect(JSON.parse(String((native.mock.calls[1]?.[1] as { input: string }).input))).toMatchObject({
      id: "e", request_id: "r", expected_revision: 1, model: selection,
    });
  });
});
