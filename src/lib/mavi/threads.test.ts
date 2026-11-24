import { describe, expect, it } from "vitest";
import { readThreadRecords, statusTone, targetLabel, THREADS_STORAGE_KEY, titleFrom, writeThreadRecords, type ThreadRecord } from "./threads";

class MemoryStorage {
  map = new Map<string, string>();
  getItem(key: string) {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string) {
    this.map.set(key, value);
  }
}

const UUID = "0d235a3d-03cf-44a9-ad04-effb59321456";

const record = (key: string, extra: Partial<ThreadRecord> = {}): ThreadRecord => ({
  key,
  sessionId: "mavi-1",
  title: "Fix it",
  target: { kind: "local", root: "/r" },
  mode: "ask",
  model: null,
  conversationId: UUID,
  createdAt: 1,
  lastActivityAt: 1,
  seenAt: 1,
  movedToTerminalAt: null,
  ...extra,
});

describe("thread records in storage", () => {
  it("round-trip, newest first", () => {
    const storage = new MemoryStorage();
    writeThreadRecords(storage, [record("old", { lastActivityAt: 1 }), record("new", { lastActivityAt: 9 })]);
    expect(readThreadRecords(storage).map((r) => r.key)).toEqual(["new", "old"]);
  });

  it("drops unusable records and fields rather than repairing them", () => {
    const storage = new MemoryStorage();
    storage.setItem(
      THREADS_STORAGE_KEY,
      JSON.stringify([
        null,
        { key: 1, target: { kind: "local", root: "/r" } },
        { key: "no-target" },
        { key: "bad-target", target: { kind: "ftp" } },
        // A session id the host never issues, a conversation id that is not a
        // uuid, and missing fields: kept as a record, with those fields cleared.
        { key: "loose", target: { kind: "ssh", host: "gpu", dir: "~/x" }, sessionId: "s1", conversationId: "nope", title: 7 },
      ]),
    );
    expect(readThreadRecords(storage)).toEqual([
      {
        key: "loose",
        sessionId: null,
        title: "Untitled thread",
        target: { kind: "ssh", host: "gpu", dir: "~/x" },
        mode: "ask",
        model: null,
        conversationId: null,
        createdAt: 0,
        lastActivityAt: 0,
        seenAt: 0,
        movedToTerminalAt: null,
      },
    ]);
  });

  it("reads nothing from no storage, an empty key, or text that is not a list", () => {
    const storage = new MemoryStorage();
    expect(readThreadRecords(null)).toEqual([]);
    expect(readThreadRecords(storage)).toEqual([]);
    storage.setItem(THREADS_STORAGE_KEY, "{not json");
    expect(readThreadRecords(storage)).toEqual([]);
    storage.setItem(THREADS_STORAGE_KEY, JSON.stringify({ key: "x" }));
    expect(readThreadRecords(storage)).toEqual([]);
    // Writing without storage, or into storage that refuses, does not throw.
    writeThreadRecords(null, [record("a")]);
    writeThreadRecords({ getItem: () => null, setItem: () => { throw new Error("full"); } }, [record("a")]);
  });
});

describe("labels", () => {
  it("titles a thread by its first non-empty line, bounded", () => {
    expect(titleFrom("\n  Fix   the build  \nmore")).toBe("Fix the build");
    expect(titleFrom("   ")).toBe("Untitled thread");
    expect(titleFrom("x".repeat(100))).toHaveLength(64);
  });

  it("names a target by its folder or host and directory", () => {
    expect(targetLabel({ kind: "local", root: "/code/payments/" })).toBe("payments");
    expect(targetLabel({ kind: "local", root: "C:\\code\\app" })).toBe("app");
    expect(targetLabel({ kind: "ssh", host: "gpu", dir: "~/ingest" })).toBe("gpu:~/ingest");
  });

  it("gives every status a tone", () => {
    expect(["needs_you", "working", "background", "failed", "review", "done", "idle", "not_running"].map((s) => statusTone(s as never))).toEqual([
      "warn",
      "accent",
      "accent",
      "danger",
      "ok",
      "ok",
      "muted",
      "muted",
    ]);
  });
});
