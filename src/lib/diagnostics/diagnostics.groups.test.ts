import { describe, expect, it } from "vitest";
import {
  createDiagnostics,
  diagnosticEntryFirstAt,
  formatDiagnosticRange,
  groupDiagnostics,
  normalizeDiagnosticMessage,
  type DiagnosticEntry,
} from "./diagnostics";

function entry(over: Partial<DiagnosticEntry> & Pick<DiagnosticEntry, "id" | "at">): DiagnosticEntry {
  return { severity: "error", source: "repo", message: "boom", count: 1, ...over };
}

describe("normalizeDiagnosticMessage", () => {
  it("collapses whitespace and drops trailing punctuation", () => {
    expect(normalizeDiagnosticMessage("clone  failed\n here. ")).toBe("clone failed here");
    expect(normalizeDiagnosticMessage("clone failed!")).toBe(normalizeDiagnosticMessage("clone failed"));
  });

  it("masks object ids and timestamps but not plain numbers or paths", () => {
    expect(normalizeDiagnosticMessage("bad object 3f2a9c1d4e at 2026-11-12T22:01:12Z")).toBe(
      normalizeDiagnosticMessage("bad object abcdef0123 at 2026-11-12T23:59:59Z"),
    );
    expect(normalizeDiagnosticMessage("exit code 128")).not.toBe(normalizeDiagnosticMessage("exit code 129"));
    expect(normalizeDiagnosticMessage("no such file /a/b")).not.toBe(
      normalizeDiagnosticMessage("no such file /a/c"),
    );
    // A long number or an ordinary word is not an object id.
    expect(normalizeDiagnosticMessage("took 1234567 bytes")).not.toBe(
      normalizeDiagnosticMessage("took 7654321 bytes"),
    );
    expect(normalizeDiagnosticMessage("defaced")).toBe("defaced");
  });
});

describe("groupDiagnostics", () => {
  it("folds interleaved identical entries into one group with count, first and last", () => {
    const a = "Cannot read properties of undefined (reading 'invoke')";
    const b = "Something else broke";
    const groups = groupDiagnostics([
      entry({ id: 5, at: 5000, message: a, count: 3, firstAt: 4000 }),
      entry({ id: 4, at: 3000, message: b }),
      entry({ id: 3, at: 2500, message: a, count: 2, firstAt: 2000 }),
      entry({ id: 2, at: 1500, message: b }),
      entry({ id: 1, at: 1000, message: a }),
    ]);
    expect(groups).toHaveLength(2);
    const [first, second] = groups;
    expect(first.message).toBe(a);
    expect(first.count).toBe(6);
    expect(first.firstAt).toBe(1000);
    expect(first.lastAt).toBe(5000);
    expect(first.entries.map((e) => e.id)).toEqual([5, 3, 1]);
    expect(first.varied).toBe(false);
    expect(second.message).toBe(b);
    expect(second.count).toBe(2);
  });

  it("keeps distinct messages, severities, sources and builds apart", () => {
    const groups = groupDiagnostics([
      entry({ id: 6, at: 6, message: "x" }),
      entry({ id: 5, at: 5, message: "y" }),
      entry({ id: 4, at: 4, message: "x", severity: "warning" }),
      entry({ id: 3, at: 3, message: "x", source: "console" }),
      entry({ id: 2, at: 2, message: "x", version: "0.1.0" }),
      entry({ id: 1, at: 1, message: "x", buildId: "abc" }),
    ]);
    expect(groups).toHaveLength(6);
  });

  it("merges ids and timestamps that differ per run and flags the variation", () => {
    const groups = groupDiagnostics([
      entry({ id: 2, at: 20, message: "object deadbeef01 missing." }),
      entry({ id: 1, at: 10, message: "object 0123abcdef missing" }),
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].count).toBe(2);
    expect(groups[0].message).toBe("object deadbeef01 missing.");
    expect(groups[0].varied).toBe(true);
  });

  it("orders groups by their most recent occurrence", () => {
    const groups = groupDiagnostics([
      entry({ id: 3, at: 300, message: "newest" }),
      entry({ id: 2, at: 200, message: "old" }),
      entry({ id: 1, at: 100, message: "old" }),
      entry({ id: 0, at: 50, message: "oldest" }),
    ]);
    expect(groups.map((g) => g.message)).toEqual(["newest", "old", "oldest"]);
    // A recurrence of an old failure moves its whole group to the top.
    const moved = groupDiagnostics([
      entry({ id: 4, at: 400, message: "old" }),
      entry({ id: 3, at: 300, message: "newest" }),
      entry({ id: 1, at: 100, message: "old" }),
    ]);
    expect(moved.map((g) => g.message)).toEqual(["old", "newest"]);
  });

  it("loses no occurrence and gives a group the same key when it grows", () => {
    const before = groupDiagnostics([entry({ id: 1, at: 1, message: "x", count: 2 })]);
    const after = groupDiagnostics([
      entry({ id: 3, at: 3, message: "x" }),
      entry({ id: 2, at: 2, message: "y" }),
      entry({ id: 1, at: 1, message: "x", count: 2 }),
    ]);
    expect(after.reduce((n, g) => n + g.count, 0)).toBe(4);
    expect(after.find((g) => g.message === "x")?.key).toBe(before[0].key);
    expect(groupDiagnostics([])).toEqual([]);
  });

  it("reads an entry saved before firstAt existed as first seen at its own time", () => {
    expect(diagnosticEntryFirstAt(entry({ id: 1, at: 70 }))).toBe(70);
    expect(diagnosticEntryFirstAt(entry({ id: 1, at: 70, firstAt: 60 }))).toBe(60);
    expect(diagnosticEntryFirstAt(entry({ id: 1, at: 70, firstAt: 90 }))).toBe(70);
  });
});

describe("formatDiagnosticRange", () => {
  it("shows one time for an instant and a dash-joined span otherwise", () => {
    const now = new Date(2026, 10, 12, 23, 0, 0).getTime();
    const at = (h: number, m: number, s: number) => new Date(2026, 10, 12, h, m, s).getTime();
    expect(formatDiagnosticRange(at(22, 1, 12), at(22, 1, 12), now)).toBe("22:01:12");
    expect(formatDiagnosticRange(at(22, 1, 12), at(22, 8, 37), now)).toBe("22:01:12 – 22:08:37");
  });
});

describe("recording stamps firstAt", () => {
  it("keeps the first time when the head coalesces and the last time moves", () => {
    let clock = 1_000;
    const store = createDiagnostics({ storage: null, now: () => clock });
    let seen: readonly DiagnosticEntry[] = [];
    store.subscribe((value) => (seen = value));
    store.error("repo", "boom");
    clock = 9_000;
    store.error("repo", "boom");
    expect(seen).toHaveLength(1);
    expect(seen[0].firstAt).toBe(1_000);
    expect(seen[0].at).toBe(9_000);
    expect(seen[0].count).toBe(2);
  });
});
