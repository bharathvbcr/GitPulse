import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

// The app-level wiring of terminal incidents: the real controller, the real
// hosted-Claude record and resumers, and the real toast store. Only the
// transcript IPC stands in, answering every conversation it is asked about.

const ipc = vi.hoisted(() => ({ asked: [] as string[][] }));

vi.mock("../ipc/invoke", () => ({
  invoke: async (command: string, args: { sessionIds: string[] }) => {
    if (command !== "cmd_terminal_conversations") throw new Error(`unexpected IPC ${command}`);
    ipc.asked.push(args.sessionIds);
    return args.sessionIds.map((id) => ({ session_id: id, transcript: "found" }));
  },
}));

const { watchIncidents } = await import("./incidentWatch");
const { INCIDENT_EVENT } = await import("./incident");
const { hostedClaude, resumers } = await import("./hostedClaude");
const { toastStore } = await import("../stores/toastStore");

type Handler = (event: { payload: unknown }) => void;

const uuid = (i: number) => `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`;

/** A `listen` that records the channel and hands back the handler. */
function fakeListen() {
  const handlers = new Map<string, Handler>();
  const unlisten = vi.fn();
  const listen = async <T>(event: string, handler: (event: { payload: T }) => void) => {
    handlers.set(event, (e) => handler({ payload: e.payload as T }));
    return unlisten;
  };
  return { listen, handlers, unlisten };
}

function endedTab(i: number, resume: () => Promise<"started" | "deferred" | "failed">) {
  hostedClaude.begin({ sessionId: uuid(i), repoPath: "/r", startDir: null, mode: null, startedAt: i });
  hostedClaude.bind(uuid(i), `term-${i}`);
  hostedClaude.ended(uuid(i), "Terminated: 15");
  resumers.register(`term-${i}`, resume);
}

function incident(id: string, sessions: string[]) {
  return {
    id,
    signal: "Terminated: 15",
    at_ms: 1_791_000_000_000,
    session_ids: sessions,
    sender: { pid: 89829, name: "pkill" },
    sender_note: "launchd: sent by pkill[89829]",
  };
}

afterEach(() => {
  toastStore.clear();
  ipc.asked.length = 0;
  for (let i = 1; i <= 4; i++) resumers.unregister(`term-${i}`);
});

describe("watching for terminal incidents", () => {
  it("listens on the incident event and returns its unlisten", async () => {
    const { listen, handlers, unlisten } = fakeListen();
    const stop = await watchIncidents(listen);
    expect([...handlers.keys()]).toEqual([INCIDENT_EVENT]);
    stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("puts the offer up until dismissed, and reports what taking it did, failures included", async () => {
    endedTab(1, async () => "started");
    endedTab(2, async () => {
      throw new Error("the pty is gone");
    });
    endedTab(3, async () => "deferred");
    const { listen, handlers } = fakeListen();
    await watchIncidents(listen);

    handlers.get(INCIDENT_EVENT)?.({ payload: incident("inc-1", ["term-1", "term-2", "term-3"]) });
    await vi.waitFor(() => expect(get(toastStore)).toHaveLength(1));
    const [offer] = get(toastStore);
    expect(ipc.asked).toEqual([[uuid(1), uuid(2), uuid(3)]]);
    expect(offer.kind).toBe("warning");
    // Duration 0: the offer stays until dismissed, never counts itself away.
    expect(offer.duration).toBe(0);
    expect(offer.message).toContain("sent by pkill (pid 89829)");
    expect(offer.action?.label).toBe("Resume 3");

    await offer.action?.onClick();
    const after = get(toastStore).filter((toast) => toast.id !== offer.id);
    expect(after.map((toast) => [toast.kind, toast.message])).toEqual([
      ["error", "A Claude conversation couldn't be resumed: the pty is gone"],
      [
        "info",
        "Resumed 1 of 3 Claude conversations: 1 waiting for a free terminal slot (each starts when one frees); 1 couldn't start (each tab says why).",
      ],
    ]);
  });

  it("says nothing more after the offer when every conversation was relaunched", async () => {
    endedTab(4, async () => "started");
    const { listen, handlers } = fakeListen();
    await watchIncidents(listen);
    handlers.get(INCIDENT_EVENT)?.({ payload: incident("inc-2", ["term-4"]) });
    await vi.waitFor(() => expect(get(toastStore)).toHaveLength(1));
    await get(toastStore)[0].action?.onClick();
    expect(get(toastStore)).toHaveLength(1);
  });

  it("offers nothing to resume, and asks nothing, when no listed tab held a conversation", async () => {
    const { listen, handlers } = fakeListen();
    await watchIncidents(listen);
    handlers.get(INCIDENT_EVENT)?.({ payload: incident("inc-3", ["term-9"]) });
    await vi.waitFor(() => expect(get(toastStore)).toHaveLength(1));
    expect(get(toastStore)[0].action).toBeUndefined();
    expect(ipc.asked).toEqual([]);
  });

  it("ignores a payload that is not an incident", async () => {
    const { listen, handlers } = fakeListen();
    await watchIncidents(listen);
    handlers.get(INCIDENT_EVENT)?.({ payload: { nope: true } });
    await new Promise((done) => setTimeout(done, 20));
    expect(get(toastStore)).toEqual([]);
  });
});
