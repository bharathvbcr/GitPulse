import { beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({ invoke: [] as [string, unknown][], listeners: new Map<string, (event: { payload: unknown }) => void>() }));

vi.mock("../ipc/invoke", () => ({
  invoke: vi.fn(async (command: string, args?: unknown) => {
    calls.invoke.push([command, args]);
    return null;
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    calls.listeners.set(name, handler);
    return () => calls.listeners.delete(name);
  }),
}));

import { MAVI_EVENT, MAVI_MODES, MAVI_STATUS_EVENT, maviClient, onMaviEvents, onMaviStatus } from "./client";

beforeEach(() => {
  calls.invoke.length = 0;
  calls.listeners.clear();
});

describe("maviClient", () => {
  it("sends every command under its cmd_mavi_ name with the host's argument names", async () => {
    const request = { repoPath: "/r", mode: "ask" as const };
    await maviClient.start(request);
    await maviClient.send("s", "hi");
    await maviClient.answer("s", "r1", "allow");
    await maviClient.answer("s", "r2", "deny", "no");
    await maviClient.interrupt("s");
    await maviClient.stop("s");
    await maviClient.list();
    await maviClient.events("s", 4);
    await maviClient.fileDiff("s", "a.ts");
    await maviClient.sshHosts();
    await maviClient.sshProbe("gpu", "~/x");
    await maviClient.conversationCheck("/r", "c");
    expect(calls.invoke).toEqual([
      ["cmd_mavi_start", { request }],
      ["cmd_mavi_send", { id: "s", text: "hi" }],
      // The host reads an absent message as null, never as undefined.
      ["cmd_mavi_answer", { id: "s", requestId: "r1", decision: "allow", message: null }],
      ["cmd_mavi_answer", { id: "s", requestId: "r2", decision: "deny", message: "no" }],
      ["cmd_mavi_interrupt", { id: "s" }],
      ["cmd_mavi_stop", { id: "s" }],
      ["cmd_mavi_list", undefined],
      ["cmd_mavi_events", { id: "s", after: 4 }],
      ["cmd_mavi_file_diff", { id: "s", path: "a.ts" }],
      ["cmd_mavi_ssh_hosts", undefined],
      ["cmd_mavi_ssh_probe", { host: "gpu", dir: "~/x" }],
      ["cmd_mavi_conversation_check", { repoPath: "/r", conversationId: "c" }],
    ]);
  });

  it("orders the modes from least authority to most, Bypass last", () => {
    expect(MAVI_MODES.map((m) => m.id)).toEqual(["inspect", "ask", "edit", "auto_review", "preapproved", "bypass"]);
  });

  it("unpacks the host's event payloads for the store", async () => {
    const batches: [string, unknown][] = [];
    const statuses: unknown[] = [];
    const offEvents = await onMaviEvents((id, entries) => batches.push([id, entries]));
    await onMaviStatus((summary) => statuses.push(summary));
    calls.listeners.get(MAVI_EVENT)!({ payload: { id: "s", entries: [{ seq: 1, line: "{}" }] } });
    calls.listeners.get(MAVI_STATUS_EVENT)!({ payload: { id: "s", phase: "idle" } });
    expect(batches).toEqual([["s", [{ seq: 1, line: "{}" }]]]);
    expect(statuses).toEqual([{ id: "s", phase: "idle" }]);
    offEvents();
    expect(calls.listeners.has(MAVI_EVENT)).toBe(false);
  });
});
