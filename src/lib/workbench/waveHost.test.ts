import { describe, expect, it } from "vitest";
import { readSuggestion } from "./waveHost";
import { taskSuggestions } from "./client";

const answer = (over: Record<string, unknown> = {}) => taskSuggestions({
  ok: true, generated_at: 1, candidates: 2,
  suggestions: [
    { rank: 1, id: "a", title: "A", status: "inbox", priority: 0, score: 9, reason: "", signals: [] },
    { rank: 2, id: "b", title: "B", status: "inbox", priority: 1, score: 8, reason: "", signals: [] },
  ],
  blocked: [],
  excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, unread: 0, filtered: 0 },
  board: { scanned: 535, complete: true, unhydrated: 0 },
  verification: { checked: 535, unchecked: 0, failed: 0 },
  ...over,
});

describe("what a wave reads from a suggest answer", () => {
  it("passes a complete answer through with no caveat", () => {
    expect(readSuggestion(answer())).toEqual({ candidates: [{ id: "a", title: "A" }, { id: "b", title: "B" }], unread: "", unchecked: 0 });
  });

  it("keeps the partial-board facts the wave used to drop", () => {
    const read = readSuggestion(answer({ board: { scanned: 150, complete: false, unhydrated: 385 }, excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, unread: 3, filtered: 0 }, verification: { checked: 10, unchecked: 140, failed: 0 } }));
    expect(read.unread).toMatch(/only partly read \(150 tasks scanned\)/);
    expect(read.unread).toMatch(/385 open tasks' links were not read/);
    expect(read.unread).toMatch(/3 tasks were left out unread/);
    expect(read.unchecked).toBe(140);
  });

  it("never offers a task Manvi lists as blocked", () => {
    const read = readSuggestion(answer({ blocked: [{ id: "a", title: "A", blocked_by: ["z"] }] }));
    expect(read.candidates.map((c) => c.id)).toEqual(["b"]);
  });
});
