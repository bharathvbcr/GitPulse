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

  it("counts one checkout spelled two ways as one, by the tab strip's identity rule", () => {
    // This used to pin exact-string keys on the claim that every producer
    // hands out one spelling. It does not: a task launch's checkout, an
    // adopted session's host path and a tab's path can differ by case on a
    // case-insensitive volume or by a trailing separator, and the strip and
    // the Agents plane already treat those as one checkout (`identityKey`).
    // A badge keyed differently counted the same checkout as two.
    const insensitive = { caseInsensitive: true };
    const counts = sessionsByRepo([
      { repoPath: "/r/A" },
      { repoPath: "/r/a" },
      { repoPath: "/r/a/" },
      { repoPath: "/r//a" },
    ], insensitive);
    expect(counts.size).toBe(1);
    expect(counts.get("/r/a")).toBe(4);
    expect(counts.get("/R/A/")).toBe(4);
    expect(counts.has("/r/A")).toBe(true);
    // The first spelling seen names the entry, so `keys()` stays a real path.
    expect([...counts.keys()]).toEqual(["/r/A"]);
  });

  it("keeps case apart on a case-sensitive volume, and folds only separators there", () => {
    const counts = sessionsByRepo([{ repoPath: "/r/A" }, { repoPath: "/r/a" }, { repoPath: "/r/a/" }], { caseInsensitive: false });
    expect(counts.size).toBe(2);
    expect(counts.get("/r/A")).toBe(1);
    expect(counts.get("/r/a")).toBe(2);
    expect(counts.get("/R/a")).toBeUndefined();
  });

  it("maps random spellings of the same checkout to the same sessions (seeded)", () => {
    let seed = 0x5eed1e55;
    const random = () => {
      seed = (seed + 0x6d2b79f5) | 0;
      let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
    const pick = <T,>(list: readonly T[]) => list[Math.floor(random() * list.length)];
    const flipCase = (text: string) => [...text].map((ch) => (random() < 0.5 ? ch.toUpperCase() : ch.toLowerCase())).join("");
    const respell = (path: string, caseInsensitive: boolean) => {
      let out = caseInsensitive ? flipCase(path) : path;
      if (random() < 0.3) out = out.replace(/\//g, "\\");
      // An inner separator only: a doubled LEADING one is a UNC path, which
      // is a different place.
      if (random() < 0.3) out = out.replace(/(.)[\\/]/, "$1//");
      if (random() < 0.4) out += pick(["/", "//", "\\"]);
      return out;
    };
    const checkouts = ["/Users/me/Code/app", "/Users/me/Code/app/.claude/worktrees/fix-1", "/Volumes/Work/api", "/srv/repo"];
    for (let round = 0; round < 200; round += 1) {
      const caseInsensitive = random() < 0.5;
      const options = { caseInsensitive };
      const expected = new Map<string, number>();
      const records: { repoPath: string }[] = [];
      const total = 1 + Math.floor(random() * 12);
      for (let i = 0; i < total; i += 1) {
        const checkout = pick(checkouts);
        records.push({ repoPath: respell(checkout, caseInsensitive) });
        expected.set(checkout, (expected.get(checkout) ?? 0) + 1);
      }
      const counts = sessionsByRepo(records, options);
      expect(counts.size, JSON.stringify(records)).toBe(expected.size);
      for (const [checkout, count] of expected) {
        // Asked under yet another spelling, the answer is the same count.
        expect(counts.get(respell(checkout, caseInsensitive)), `${checkout} in ${JSON.stringify(records)}`).toBe(count);
      }
    }
  });

  it("reads a live registry's published array", () => {
    const registry = createSessionRegistry();
    registry.reserve(record({ key: "a", repoPath: "/r/one" }));
    registry.reserve(record({ key: "b", repoPath: "/r/two" }));
    registry.reserve(record({ key: "c", repoPath: "/r/one" }));
    expect([...sessionsByRepo(get(registry), { caseInsensitive: false })]).toEqual([["/r/one", 2], ["/r/two", 1]]);
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
