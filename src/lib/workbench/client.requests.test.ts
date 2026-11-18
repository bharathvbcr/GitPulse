import { beforeEach, describe, expect, it, vi } from "vitest";

const native = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: native }));

import {
  WorkbenchError, bindTerminalRunThread, getNotificationSettings, listPendingDecisions, organizeTasks, suggestTasks, task, taskOrganization,
} from "./client";

const SESSION = "6F0A6D1E-1111-4222-8333-444455556661";
const emptyPage = { ok: true, items: [], shown: 0, total: 0, has_more: false, next_cursor: null };
const settings = {
  id: "profile", revision: 1, updated_at: 0, profile_id: "0123456789abcdef0123456789abcdef", enabled: true, sound: false,
  background: false, after_sequence: 0, quiet_start: null, quiet_end: null, muted_workspace_ids: [], muted_repository_ids: [], muted_task_ids: [],
};
const sent = () => native.mock.calls.map((call) => ({ command: call[0], ...(call[1] as { method: string; input: string }) }));

beforeEach(() => native.mockReset());

describe("workbench requests that decode their own replies", () => {
  it("asks for a run's pending requests only, thirty at a time", async () => {
    native.mockResolvedValue(JSON.stringify(emptyPage));
    const result = await listPendingDecisions("run-1");
    expect(result.items).toEqual([]);
    expect(sent()).toEqual([{ command: "cmd_workbench_request", method: "decisions.list", input: JSON.stringify({ run_id: "run-1", state: "pending", limit: 30 }) }]);
  });

  it("reads the profile's notification settings", async () => {
    native.mockResolvedValue(JSON.stringify({ ok: true, item: settings }));
    const read = await getNotificationSettings();
    expect(read).toMatchObject({ enabled: true, profile_id: settings.profile_id, quiet_start: null });
    expect(sent()[0]).toMatchObject({ method: "notifications.settings.get", input: JSON.stringify({ id: "profile" }) });
  });

  it("refuses notification settings that are not the profile's", async () => {
    native.mockResolvedValue(JSON.stringify({ ok: true, item: { ...settings, id: "other" } }));
    await expect(getNotificationSettings()).rejects.toThrow(/invalid response/);
  });

  it("binds a discovered provider conversation to its run under its lower-case id, and ignores an id that is not one", async () => {
    native.mockResolvedValue(JSON.stringify({ ok: true }));
    await bindTerminalRunThread("run-1", "session-1", "not-a-session");
    expect(native).not.toHaveBeenCalled();
    await bindTerminalRunThread("run-1", "session-1", SESSION);
    expect(sent()).toEqual([{
      command: "cmd_workbench_request",
      method: "runs.bind_terminal_thread",
      input: JSON.stringify({ id: "run-1", session_id: "session-1", provider_thread_id: SESSION.toLowerCase() }),
    }]);
  });

  it("refuses a request the host answered with an error, by its code", async () => {
    native.mockRejectedValueOnce({ code: "not_found", message: "gone" });
    const failure = await bindTerminalRunThread("run-1", "s", SESSION).catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(WorkbenchError);
    expect((failure as WorkbenchError).code).toBe("not_found");
    expect((failure as WorkbenchError).message).toBe("gone");
  });
});

describe("task detail decoding", () => {
  const full = { id: "t1", revision: 2, updated_at: 1, title: "Preserve task evidence", description: "exact error: E42", acceptance_criteria: ["Round-trip evidence"], kind: "bug", status: "ready", priority: 1, severity: null, owner: null, due_at: null, labels: ["regression"], repository_ids: ["r1", "r2"], primary_repository_id: "r1", home_workspace_id: null, position: 10 };

  it("reads a checklist and links on a task, and refuses a malformed entry", () => {
    const decoded = task({ ...full, checklist: [{ text: "write it", done: true }], links: [{ kind: "blocks", item_id: "t2" }] });
    expect(decoded.checklist).toEqual([{ text: "write it", done: true }]);
    expect(decoded.links).toEqual([{ kind: "blocks", item_id: "t2" }]);
    expect(() => task({ ...full, checklist: [{ text: "   ", done: false }] })).toThrow("invalid response");
    expect(() => task({ ...full, checklist: "no" })).toThrow("invalid response");
    expect(() => task({ ...full, links: [{ kind: "friends", item_id: "t2" }] })).toThrow("invalid response");
    expect(() => task({ ...full, links: [{ kind: "blocks", item_id: "../x" }] })).toThrow("invalid response");
    expect(() => task({ ...full, links: {} })).toThrow("invalid response");
  });
});

describe("suggest and organize", () => {
  const organization = {
    ok: true, method: "deterministic", generated_at: 5,
    changes: [{ id: "a", title: "A", status: "backlog", revision: 2, from: 1, to: 0, rule: "r", reason: "because" }],
    order: [{ status: "backlog", ids: ["a"] }],
    groups: [{ kind: "duplicate", keep: "a", members: ["a", "b"] }],
    coverage: { read: 2, unread: 0, unhydrated: 0, unrevisioned: 0, complete: true },
    order_note: "n", differs: ["x"],
  };

  it("decodes an organization with its rows, groups and coverage", () => {
    const decoded = taskOrganization(organization);
    expect(decoded.changes[0]).toEqual({ id: "a", title: "A", status: "backlog", revision: 2, from: 1, to: 0, rule: "r", reason: "because" });
    expect(decoded.order).toEqual([{ status: "backlog", ids: ["a"] }]);
    expect(decoded.groups).toEqual([{ kind: "duplicate", keep: "a", members: ["a", "b"] }]);
    expect(decoded.coverage).toEqual({ read: 2, unread: 0, unhydrated: 0, unrevisioned: 0, complete: true, note: "" });
    expect(decoded.differs).toEqual(["x"]);
  });

  it("refuses an organization with a priority out of range, an unknown status or a method that is not deterministic", () => {
    expect(() => taskOrganization({ ...organization, changes: [{ ...organization.changes[0], to: 9 }] })).toThrow("invalid response");
    expect(() => taskOrganization({ ...organization, changes: [{ ...organization.changes[0], revision: 0 }] })).toThrow("invalid response");
    expect(() => taskOrganization({ ...organization, order: [{ status: "nonsense", ids: [] }] })).toThrow("invalid response");
    expect(() => taskOrganization({ ...organization, method: "model" })).toThrow("invalid response");
  });

  it("asks the host to organize a repository's tasks and decodes the answer", async () => {
    native.mockResolvedValue(JSON.stringify(organization));
    const decoded = await organizeTasks({ repository_id: "r1" });
    expect(decoded.generated_at).toBe(5);
    expect(sent()[0]).toMatchObject({ method: "tasks.organize", input: JSON.stringify({ repository_id: "r1" }) });
  });

  it("refuses a suggest answer that is not the exact shape, rather than listing part of it", async () => {
    native.mockResolvedValue(JSON.stringify({ ok: false }));
    await expect(suggestTasks({ limit: 3, repository_id: "r1" })).rejects.toThrow(/invalid response/);
    expect(sent()[0]).toMatchObject({ method: "tasks.suggest", input: JSON.stringify({ limit: 3, repository_id: "r1" }) });
  });
});
