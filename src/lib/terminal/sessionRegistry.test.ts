import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";
import {
  createSessionRegistry,
  sessionByNativeId,
  sessionsByRepo,
  type TerminalSessionRecord,
} from "./sessionRegistry";
import { DEFAULT_TERMINAL_SESSIONS, MAX_TERMINAL_SESSIONS, currentSessionLimit, setTerminalSessionLimit } from "./sessionLimit";

function record(overrides: Partial<TerminalSessionRecord> = {}): TerminalSessionRecord {
  return {
    key: "session-a",
    repoPath: "/repo/a",
    label: "Shell",
    status: "running",
    close: vi.fn(),
    ...overrides,
  };
}

describe("terminal session registry", () => {
  it("publishes session insertions and updates to subscribers", () => {
    const registry = createSessionRegistry();
    const snapshots: Array<readonly TerminalSessionRecord[]> = [];
    registry.subscribe((value) => snapshots.push(value));

    const lease = registry.reserve(record({ key: "a", status: "starting" }));
    expect(snapshots.at(-1)?.map((entry) => entry.status)).toEqual(["starting"]);

    lease.update("ready");
    expect(snapshots.at(-1)?.map((entry) => entry.status)).toEqual(["ready"]);
    expect(snapshots).toHaveLength(3);
    expect(snapshots[0]).toEqual([]);
  });

  it("releases a session and removes it from published state", () => {
    const registry = createSessionRegistry();
    const updates: Array<number> = [];
    registry.subscribe((value) => updates.push(value.length));

    const lease = registry.reserve(record({ key: "a", status: "ready" }));
    lease.release();

    expect(updates).toEqual([0, 1, 0]);
    expect(get(registry)).toHaveLength(0);
  });

  it("ignores updates and releases when already disposed", () => {
    const registry = createSessionRegistry();
    const lease = registry.reserve(record({ key: "a", status: "starting" }));
    lease.release();
    lease.update("stale");
    lease.release();

    expect(get(registry)).toHaveLength(0);
  });

  it("rejects duplicate keys", () => {
    const registry = createSessionRegistry();
    registry.reserve(record({ key: "a" }));
    expect(() => registry.reserve(record({ key: "a", status: "other" }))).toThrow(
      "This terminal already owns a session slot",
    );
  });

  it("rejects reservations above the terminal capacity", () => {
    const registry = createSessionRegistry();
    for (let index = 0; index < DEFAULT_TERMINAL_SESSIONS; index += 1) {
      registry.reserve(record({ key: String(index) }));
    }

    expect(() => registry.reserve(record({ key: String(DEFAULT_TERMINAL_SESSIONS + 1) }))).toThrow(
      `All ${DEFAULT_TERMINAL_SESSIONS} terminal sessions are in use across repositories`,
    );
    expect(get(registry)).toHaveLength(DEFAULT_TERMINAL_SESSIONS);
  });

  it("stops at the user's live limit and names it", () => {
    const registry = createSessionRegistry();
    try {
      setTerminalSessionLimit(DEFAULT_TERMINAL_SESSIONS + 6);
      for (let index = 0; index < DEFAULT_TERMINAL_SESSIONS + 6; index += 1) registry.reserve(record({ key: String(index) }));
      expect(() => registry.reserve(record({ key: "over" }))).toThrow(`All ${DEFAULT_TERMINAL_SESSIONS + 6} terminal sessions`);
      // Lowered below what is open: nothing new opens until enough close.
      setTerminalSessionLimit(4);
      expect(() => registry.reserve(record({ key: "lowered" }))).toThrow("All 4 terminal sessions");
      // A value the backend would not accept falls back to the default.
      for (const bad of [0, MAX_TERMINAL_SESSIONS + 1, 2.5, "40", null]) {
        setTerminalSessionLimit(bad);
        expect(currentSessionLimit(), String(bad)).toBe(DEFAULT_TERMINAL_SESSIONS);
      }
      setTerminalSessionLimit(MAX_TERMINAL_SESSIONS);
      expect(currentSessionLimit()).toBe(MAX_TERMINAL_SESSIONS);
    } finally {
      setTerminalSessionLimit(undefined);
    }
  });

  it("carries a reveal through status updates, so a jump target survives a restart", () => {
    // `update` rebuilds the record; a spread that dropped `reveal` would make
    // the Sessions list's "Go to" button silently go dead the first time the
    // shell changed status.
    const registry = createSessionRegistry();
    const reveal = vi.fn();
    const lease = registry.reserve(record({ key: "a", status: "starting", reveal }));
    lease.update("running");
    get(registry)[0].reveal?.();
    expect(reveal).toHaveBeenCalledOnce();
  });

  it("keeps reserved slots across status transitions while open", () => {
    const registry = createSessionRegistry();
    const lease = registry.reserve(record({ key: "a", status: "ready" }));
    lease.update("busy");
    lease.update("ready");
    lease.update("closing");

    expect(get(registry)).toHaveLength(1);
    expect(get(registry)[0].status).toBe("closing");
  });
});

describe("sessionsByRepo", () => {
  it("counts sessions per repository", () => {
    const counts = sessionsByRepo([
      { repoPath: "/r/a" },
      { repoPath: "/r/b" },
      { repoPath: "/r/a" },
      { repoPath: "/r/a" },
    ]);
    expect(counts.get("/r/a")).toBe(3);
    expect(counts.get("/r/b")).toBe(1);
  });

  it("omits a repository with no sessions rather than reporting zero", () => {
    // The badge renders on truthiness, so a stored 0 would draw an empty
    // terminal glyph on every repository the user has ever visited.
    const counts = sessionsByRepo([{ repoPath: "/r/a" }]);
    expect(counts.has("/r/b")).toBe(false);
    expect(counts.get("/r/b")).toBeUndefined();
    expect([...counts.keys()]).toEqual(["/r/a"]);
  });

  it("is empty for no sessions", () => {
    expect(sessionsByRepo([]).size).toBe(0);
  });

  it("skips a blank repoPath instead of counting it as a repository", () => {
    const counts = sessionsByRepo([{ repoPath: "" }, { repoPath: "/r/a" }]);
    expect(counts.has("")).toBe(false);
    expect(counts.get("/r/a")).toBe(1);
  });

  it("does not fold paths that differ only by case or trailing separator", () => {
    // Deliberate: every producer hands out the path the repository store
    // already resolved, so folding here would invent a second identity rule.
    // Pinned so a future change to that assumption is a failing test, not a
    // badge that quietly counts the wrong repository.
    const counts = sessionsByRepo([
      { repoPath: "/r/A" },
      { repoPath: "/r/a" },
      { repoPath: "/r/a/" },
    ]);
    expect(counts.size).toBe(3);
  });

  it("reads a live registry's published array", () => {
    const registry = createSessionRegistry();
    registry.reserve(record({ key: "a", repoPath: "/r/one" }));
    registry.reserve(record({ key: "b", repoPath: "/r/two" }));
    registry.reserve(record({ key: "c", repoPath: "/r/one" }));
    expect(sessionsByRepo(get(registry))).toEqual(new Map([["/r/one", 2], ["/r/two", 1]]));
  });
});


/**
 * A clicked notification names the backend's PTY id, which is the only handle
 * the OS ever saw. Without this bridge a banner opens the window and then does
 * nothing, because the tab key it would need is a renderer invention.
 */
describe("finding the tab a notification belongs to", () => {
  it("publishes the backend id once the PTY has one", () => {
    const registry = createSessionRegistry();
    const slot = registry.reserve(record());
    expect(get(registry)[0].sessionId).toBeUndefined();
    slot.identify("term-9-1a");
    expect(get(registry)[0].sessionId).toBe("term-9-1a");
    expect(sessionByNativeId(get(registry), "term-9-1a")?.key).toBe("session-a");
  });

  it("keeps the id across a status change", () => {
    // The status updater used to rebuild the row from the record it captured
    // at reservation time, which would have discarded an id learned later.
    const registry = createSessionRegistry();
    const slot = registry.reserve(record());
    slot.identify("term-9-1a");
    slot.update("exited");
    expect(get(registry)[0]).toMatchObject({ status: "exited", sessionId: "term-9-1a" });
  });

  it("answers null for an id nothing is running", () => {
    const registry = createSessionRegistry();
    const slot = registry.reserve(record());
    slot.identify("term-9-1a");
    expect(sessionByNativeId(get(registry), "term-9-2b")).toBeNull();
    expect(sessionByNativeId(get(registry), "")).toBeNull();
    slot.release();
    expect(sessionByNativeId(get(registry), "term-9-1a")).toBeNull();
  });

  it("never matches a session that has no id yet", () => {
    // Every unstarted session has an undefined id; an empty query must not
    // collide with all of them at once.
    const registry = createSessionRegistry();
    registry.reserve(record({ key: "a" }));
    registry.reserve(record({ key: "b" }));
    expect(sessionByNativeId(get(registry), undefined as unknown as string)).toBeNull();
  });

  it("ignores an identify after release", () => {
    const registry = createSessionRegistry();
    const slot = registry.reserve(record());
    slot.release();
    slot.identify("term-9-1a");
    expect(get(registry)).toHaveLength(0);
  });
});

describe("one record per native session", () => {
  it("a record that identifies a session displaces any other holding it, and nothing else", () => {
    const registry = createSessionRegistry();
    const close = async () => {};
    const adopted = registry.reserve({ key: "detached:term-1", repoPath: "/r", label: "Claude", status: "running", close });
    adopted.identify("term-1");
    const other = registry.reserve({ key: "tab-2", repoPath: "/r", label: "Shell", status: "running", close });
    other.identify("term-2");
    const tab = registry.reserve({ key: "tab-1", repoPath: "/r", label: "Claude", status: "starting", close });
    tab.identify("term-1");
    expect(get(registry).map((record) => record.key).sort()).toEqual(["tab-1", "tab-2"]);
    // The displaced holder releasing late frees nothing that is not its own.
    adopted.release();
    expect(get(registry).map((record) => record.key).sort()).toEqual(["tab-1", "tab-2"]);
  });
});
