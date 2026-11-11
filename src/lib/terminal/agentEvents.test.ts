import { describe, expect, it } from "vitest";
import {
  createAgentEventBus,
  type TerminalAgentMetaPayload,
  type TerminalAgentTouchedFilePayload,
  type TerminalChildClosedPayload,
  type TerminalChildSpawnedPayload,
  type TerminalOpenFileRequestPayload,
} from "./agentEvents";
import type { EventListen } from "./ptyBus";

function makeHarness() {
  const listeners = new Map<string, (event: { payload: unknown }) => void>();
  let unlistenCount = 0;
  const listenFn: EventListen = async (event, handler) => {
    listeners.set(event, handler as (event: { payload: unknown }) => void);
    return () => {
      unlistenCount += 1;
      listeners.delete(event);
    };
  };
  return {
    listenFn,
    emit(event: string, payload: unknown) {
      listeners.get(event)?.({ payload });
    },
    has(event: string) {
      return listeners.has(event);
    },
    get unlistenCount() {
      return unlistenCount;
    },
  };
}

describe("agentEvents", () => {
  it("routes validated terminal-agent-touched-file events for both repo edits and plan artifacts", async () => {
    const harness = makeHarness();
    const bus = createAgentEventBus(harness.listenFn);
    const touched: TerminalAgentTouchedFilePayload[] = [];
    const children: TerminalChildSpawnedPayload[] = [];
    const opened: TerminalOpenFileRequestPayload[] = [];

    const unsub = bus.subscribePanelEvents({
      onChildSpawned: (p) => children.push(p),
      onOpenFile: (p) => opened.push(p),
      onTouchedFile: (p) => touched.push(p),
    });

    await Promise.resolve();
    expect(harness.has("terminal-agent-touched-file")).toBe(true);

    // Valid repo edit.
    harness.emit("terminal-agent-touched-file", {
      id: "term-1",
      repo_path: "/Users/dev/repo",
      path: "src/lib/app.ts",
      artifact: false,
    });
    // Valid Antigravity plan artifact edit.
    harness.emit("terminal-agent-touched-file", {
      id: "term-2",
      repo_path: "/Users/dev/repo",
      path: "implementation_plan.md",
      artifact: true,
    });
    // Adversarial / malformed payloads are dropped fail-closed.
    harness.emit("terminal-agent-touched-file", null);
    harness.emit("terminal-agent-touched-file", {
      id: "",
      repo_path: "/Users/dev/repo",
      path: "src/lib/app.ts",
      artifact: false,
    });
    harness.emit("terminal-agent-touched-file", {
      id: "term-1",
      repo_path: "/Users/dev/repo",
      path: "../outside.ts",
      artifact: false,
    });
    harness.emit("terminal-agent-touched-file", {
      id: "term-1",
      repo_path: "/Users/dev/repo",
      path: "/etc/passwd",
      artifact: false,
    });

    expect(touched).toEqual([
      {
        id: "term-1",
        repo_path: "/Users/dev/repo",
        path: "src/lib/app.ts",
        artifact: false,
      },
      {
        id: "term-2",
        repo_path: "/Users/dev/repo",
        path: "implementation_plan.md",
        artifact: true,
      },
    ]);

    unsub();
    expect(harness.has("terminal-agent-touched-file")).toBe(false);
  });

  it("replays buffered terminal-agent-meta to late subscribers and forgets on exit", async () => {
    const harness = makeHarness();
    const bus = createAgentEventBus(harness.listenFn);
    const earlyUnsub = bus.subscribePanelEvents({});
    await Promise.resolve();

    harness.emit("terminal-agent-meta", {
      id: "term-42",
      agent: "agy",
      conversation_id: "ec33ebf9-0cba-4100-8142-c61503f6c587",
      artifact_dir: "/Users/dev/.gemini/antigravity/brain/ec33ebf9",
    });

    const received: TerminalAgentMetaPayload[] = [];
    const subUnsub = bus.subscribeAgentMeta("term-42", (meta) => received.push(meta));
    await Promise.resolve();

    expect(received).toEqual([
      {
        id: "term-42",
        agent: "agy",
        conversation_id: "ec33ebf9-0cba-4100-8142-c61503f6c587",
        artifact_dir: "/Users/dev/.gemini/antigravity/brain/ec33ebf9",
      },
    ]);

    bus.forgetAgentMeta("term-42");
    subUnsub();
    earlyUnsub();
  });

  it("routes validated terminal-child-closed payloads and drops malformed events", async () => {
    const harness = makeHarness();
    const bus = createAgentEventBus(harness.listenFn);
    const closed: TerminalChildClosedPayload[] = [];

    const unsub = bus.subscribePanelEvents({
      onChildClosed: (p) => closed.push(p),
    });

    await Promise.resolve();
    expect(harness.has("terminal-child-closed")).toBe(true);

    harness.emit("terminal-child-closed", {
      id: "term-100-child",
      parent_session_id: "term-100-parent",
      repo_path: "/Users/dev/repo",
    });
    harness.emit("terminal-child-closed", null);
    harness.emit("terminal-child-closed", {
      id: "",
      parent_session_id: "term-100-parent",
      repo_path: "/Users/dev/repo",
    });
    harness.emit("terminal-child-closed", {
      id: "term-100-child",
      parent_session_id: "",
      repo_path: "/Users/dev/repo",
    });
    harness.emit("terminal-child-closed", {
      id: "term-100-child",
      parent_session_id: "term-100-parent",
      repo_path: "",
    });

    expect(closed).toEqual([
      {
        id: "term-100-child",
        parent_session_id: "term-100-parent",
        repo_path: "/Users/dev/repo",
      },
    ]);

    unsub();
    expect(harness.has("terminal-child-closed")).toBe(false);
  });
});
