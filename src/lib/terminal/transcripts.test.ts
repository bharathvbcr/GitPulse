import { describe, expect, it, vi } from "vitest";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));

const { checkTranscripts, readTranscripts, MAX_TRANSCRIPT_QUERIES } = await import("./transcripts");

const A = "6f0a6d1e-1111-4222-8333-444455556666";
const B = "0a0a0a0a-2222-4333-8444-555566667777";

describe("readTranscripts", () => {
  it("reads found, absent and unknown, with a reason only for unknown", () => {
    const answers = readTranscripts(
      [
        { session_id: A, transcript: "found", reason: null },
        { session_id: B, transcript: "unknown", reason: "permission denied" },
      ],
      [A, B],
    );
    expect(answers.get(A)).toEqual({ state: "found", reason: null });
    expect(answers.get(B)).toEqual({ state: "unknown", reason: "permission denied" });
  });

  it("never reads a missing or unreadable answer as absent", () => {
    for (const raw of [null, {}, [], [{ session_id: A, transcript: "maybe" }], [{ session_id: "someone-else", transcript: "found" }]]) {
      expect(readTranscripts(raw, [A]).get(A)?.state).toBe("unknown");
    }
    expect(readTranscripts([{ session_id: A, transcript: "absent", reason: "ignored" }], [A]).get(A)).toEqual({ state: "absent", reason: null });
  });
});

describe("checkTranscripts", () => {
  it("asks once, for canonical ids only, deduplicated and bounded", async () => {
    const invoke = vi.fn(async () => [{ session_id: A, transcript: "found", reason: null }]);
    const answers = await checkTranscripts([A, A.toUpperCase(), "not-an-id"], invoke as never);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("cmd_terminal_conversations", { sessionIds: [A] });
    expect(answers.get(A)?.state).toBe("found");

    const many = Array.from({ length: MAX_TRANSCRIPT_QUERIES + 5 }, (_, i) => `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`);
    const bounded = vi.fn(async () => []);
    await checkTranscripts(many, bounded as never);
    expect((bounded.mock.calls[0] as unknown[])[1]).toEqual({ sessionIds: many.slice(0, MAX_TRANSCRIPT_QUERIES) });
  });

  it("a call that fails answers unknown, with the failure, for every id", async () => {
    const answers = await checkTranscripts([A, B], (async () => {
      throw new Error("backend gone");
    }) as never);
    expect(answers.get(A)).toEqual({ state: "unknown", reason: "backend gone" });
    expect(answers.get(B)?.state).toBe("unknown");
  });

  it("asks nothing for nothing", async () => {
    const invoke = vi.fn();
    expect((await checkTranscripts([], invoke as never)).size).toBe(0);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("matches the backend's bound", async () => {
    const { readFileSync } = await import("node:fs");
    const source = readFileSync(new URL("../../../src-tauri/src/workbench/conversation.rs", import.meta.url), "utf8");
    const backend = /pub\(crate\) const MAX_TRANSCRIPT_QUERIES: usize = (\d+);/.exec(source);
    expect(backend, "MAX_TRANSCRIPT_QUERIES not found in conversation.rs").not.toBeNull();
    expect(MAX_TRANSCRIPT_QUERIES).toBe(Number(backend?.[1]));
  });
});
