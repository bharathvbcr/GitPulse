import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("./ptyBus.tauri", () => ({ ptyBus: { subscribe: () => () => {} } }));
vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));

const { adoptDetachedSessions, handOverDetachedRun, parseListing, stopQuestion, DETACHED_STATUS } = await import("./detachedSessions");
const { createSessionRegistry } = await import("./sessionRegistry");
const { taskTerminalRequests, consumeTaskTerminalRequest } = await import("./taskLaunches");
import type { PtyBus, PtySessionHandlers } from "./ptyBus";
import type { TerminalListing } from "./runResult";

const row = (patch: Partial<TerminalListing> = {}): TerminalListing => ({
  id: "term-1-a", shell: "/usr/local/bin/claude", cwd: "/repos/app", repo: "/repos/app",
  launcher: "claude", run_id: null, detached: true, ...patch,
});

function fakeBus() {
  const handlers = new Map<string, PtySessionHandlers>();
  const bus: PtyBus = {
    prepare: async () => () => {},
    subscribe(id, session) { handlers.set(id, session); return () => { if (handlers.get(id) === session) handlers.delete(id); }; },
    pendingCount: () => 0,
  };
  return { bus, handlers };
}

function setup(rows: unknown, answer = true) {
  const registry = createSessionRegistry();
  const asked: { title: string; message: string }[] = [];
  const confirm = async (options: { title: string; message: string }) => { asked.push(options); return answer; };
  const { bus, handlers } = fakeBus();
  const calls: { command: string; args?: Record<string, unknown> }[] = [];
  const invoke = async <T,>(command: string, args?: Record<string, unknown>): Promise<T> => {
    calls.push({ command, args });
    return (command === "cmd_terminal_sessions" ? rows : null) as T;
  };
  return { registry, handlers, calls, asked, adopt: () => adoptDetachedSessions({ invoke, bus, registry, confirm }) };
}

afterEach(() => { for (const request of get(taskTerminalRequests)) consumeTaskTerminalRequest(request); });

describe("sessions a reloaded page left running", () => {
  it("are listed, counted against the shared limit, and say they are still running", async () => {
    const { registry, adopt } = setup([row(), row({ id: "term-1-b", launcher: null, shell: "/bin/zsh", detached: false })]);
    expect(await adopt()).toBe(1);
    const [record] = get(registry);
    expect(get(registry)).toHaveLength(1);
    expect(record).toMatchObject({ repoPath: "/repos/app", label: "Claude", sessionId: "term-1-a", status: DETACHED_STATUS });
    // A session this page's own tab holds is not adopted twice, and neither
    // is one already adopted.
    expect(await adopt()).toBe(0);
    expect(get(registry)).toHaveLength(1);
  });

  it("stop for real, and leave the list only once the host has stopped them", async () => {
    const { registry, calls, adopt } = setup([row()]);
    await adopt();
    await get(registry)[0].close();
    expect(calls.at(-1)).toEqual({ command: "cmd_terminal_kill", args: { sessionId: "term-1-a" } });
    expect(get(registry)).toEqual([]);
  });

  it("are handed to their repository's dock to be shown, and give up their slot to the tab", async () => {
    const { registry, handlers, adopt } = setup([row()]);
    await adopt();
    expect(handlers.has("term-1-a")).toBe(true);
    get(registry)[0].reveal?.();
    expect(get(registry)).toEqual([]);
    expect(handlers.has("term-1-a")).toBe(false);
    expect(get(taskTerminalRequests)).toEqual([
      { runId: "detached:term-1-a", repoPath: "/repos/app", provider: "claude", title: "Claude", attach: { sessionId: "term-1-a" } },
    ]);
  });

  it("a task attempt's is shown through its own launch path, which takes its session over", async () => {
    const { registry, adopt } = setup([row({ run_id: "run-1" })]);
    await adopt();
    expect(get(registry)[0]).toMatchObject({ taskRunId: "run-1", title: "Task attempt" });
    get(registry)[0].reveal?.();
    expect(get(taskTerminalRequests)).toEqual([{ runId: "run-1", repoPath: "/repos/app", provider: "claude", title: "Task attempt" }]);
  });

  it("leave the list when their process ends on its own", async () => {
    const { registry, handlers, adopt } = setup([row()]);
    await adopt();
    handlers.get("term-1-a")?.onExit({ id: "term-1-a", exit_code: 0, signal: "", error: null, reaped: true } as never);
    expect(get(registry)).toEqual([]);
  });

  it("a list that cannot be read is an error, never \"none\"", async () => {
    await expect(setup({ nope: true }).adopt()).rejects.toThrow(/invalid session list/);
    // A malformed row is skipped; it cannot be named, stopped or shown.
    const { registry, adopt } = setup([{ id: "x" }, row({ id: "" }), row({ repo: "\0" }), row({ detached: "yes" as never })]);
    expect(await adopt()).toBe(0);
    expect(get(registry)).toEqual([]);
  });

  // The same class e27e6daf fixed for tabs: a running agent stopped from
  // the Sessions list without a word.
  it("ask before the Sessions list stops one, and stop nothing when declined", async () => {
    const { registry, calls, asked, adopt } = setup([row({ run_id: "run-1" })], false);
    await adopt();
    const record = get(registry)[0];
    expect(await record.confirmClose?.()).toBe(false);
    expect(asked[0]).toMatchObject({ ...stopQuestion({ run_id: "run-1" }, "Claude"), confirmLabel: "Stop", destructive: true });
    expect(asked[0].message).toContain("ends that attempt");
    expect(calls.some((call) => call.command === "cmd_terminal_kill")).toBe(false);
    expect(stopQuestion({ run_id: null }, "Shell").message).toContain("still running from before the window reloaded");
  });

  // The advisor's case: "Open terminal" takes a reloaded task session over
  // through the task's own launch path, never through the adopted record.
  it("a task tab that takes a session over leaves it listed once, by the tab", async () => {
    const { registry, adopt } = setup([row({ run_id: "run-1" })]);
    await adopt();
    const tab = registry.reserve({ key: "tab-7", repoPath: "/repos/app", label: "Claude", status: "starting", close: async () => {} });
    tab.identify("term-1-a");
    expect(get(registry).map((record) => record.key)).toEqual(["tab-7"]);
  });

  it("hand an adopted task session over to its task, freeing its slot first", async () => {
    const { registry, adopt } = setup([row({ run_id: "run-1" })]);
    await adopt();
    expect(handOverDetachedRun("run-2", registry)).toBe(false);
    expect(get(registry)).toHaveLength(1);
    expect(handOverDetachedRun("run-1", registry)).toBe(true);
    expect(get(registry)).toEqual([]);
    expect(get(taskTerminalRequests).map((request) => request.runId)).toEqual(["run-1"]);
  });

  it("parses only a well-formed row", () => {
    expect(parseListing(row())).toEqual(row());
    expect(parseListing(row({ launcher: null, run_id: null }))).toMatchObject({ launcher: null, run_id: null });
    for (const bad of [null, [], "x", { ...row(), id: 7 }, { ...row(), run_id: "r".repeat(129) }, { ...row(), cwd: "" }]) {
      expect(parseListing(bad)).toBeNull();
    }
  });
});
