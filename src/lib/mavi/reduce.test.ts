import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { applyAll, applyLine, emptyThread, openApprovals } from "./reduce";
import { toRows, summarize } from "./rows";
import { collectChanges, hunksFromInput, turnTotals, describeCode } from "./changes";
import type { ThreadState, ToolItem } from "./types";

/** Lines captured from Claude Code 2.1.330 (paths normalised to /repo). */
function fixture(name: string): string[] {
  const url = new URL(`./__fixtures__/${name}`, import.meta.url);
  return readFileSync(fileURLToPath(url), "utf8").split("\n").filter(Boolean);
}

function tools(state: ThreadState): ToolItem[] {
  return state.items.filter((i): i is ToolItem => i.kind === "tool");
}

describe("an allowed edit, then a background command (live stream)", () => {
  const state = applyAll(emptyThread(), fixture("allow-edit.jsonl"));

  it("keeps one item per content block, not one per assistant line", () => {
    const kinds = state.items.map((i) => i.kind);
    expect(kinds[0]).toBe("user");
    // Read, Edit, then the backgrounded Bash.
    expect(tools(state).map((t) => t.name)).toEqual(["Read", "Edit", "Bash"]);
    expect(state.items.filter((i) => i.kind === "user")).toHaveLength(2);
    expect(state.items.filter((i) => i.kind === "turn_end")).toHaveLength(2);
  });

  it("attaches the permission request to its tool and records the answer", () => {
    const edit = tools(state).find((t) => t.name === "Edit")!;
    expect(edit.approval).toMatchObject({ state: "allowed" });
    expect(edit.status).toBe("ok");
    expect(edit.patch?.[0]?.lines).toContain("+world");
    expect(openApprovals(state)).toHaveLength(0);
  });

  it("reads init, cost deltas and the turn summary", () => {
    expect(state.conversationId).toMatch(/^[0-9a-f-]{36}$/);
    expect(state.model).toBe("claude-opus-5-5");
    const ends = state.items.filter((i) => i.kind === "turn_end");
    expect(ends[0]).toMatchObject({ ok: true, subtype: "success" });
    expect(ends[0]!.kind === "turn_end" && ends[0]!.summary?.category).toBe("completed");
    const [first, second] = ends.map((e) => (e.kind === "turn_end" ? e.costUsd ?? 0 : 0));
    // Cumulative totals 0.2286 then 0.2577: the second turn cost the difference.
    expect(first).toBeCloseTo(0.2286, 3);
    expect(second).toBeCloseTo(0.2577 - 0.2286, 3);
  });

  it("does not leave a backgrounded command looking interrupted", () => {
    const bash = tools(state).find((t) => t.name === "Bash")!;
    expect(bash.background).toBe(true);
    expect(bash.status).toBe("ok");
  });

  it("finishes every streamed text block with the complete text", () => {
    const texts = state.items.filter((i) => i.kind === "assistant");
    expect(texts.every((t) => t.kind === "assistant" && !t.streaming && t.final)).toBe(true);
    expect(texts.some((t) => t.kind === "assistant" && t.text.startsWith("Done."))).toBe(true);
  });
});

describe("a denied write, then an interrupted turn", () => {
  const state = applyAll(emptyThread(), fixture("deny-interrupt.jsonl"));

  it("marks the denied call denied, not failed", () => {
    const write = tools(state).find((t) => t.name === "Write")!;
    expect(write.approval?.state).toBe("denied");
    expect(write.status).toBe("denied");
    expect(write.result).toBe("Not now, thanks.");
  });

  it("ends the interrupted turn as interrupted, with a notice and partial text kept", () => {
    const ends = state.items.filter((i) => i.kind === "turn_end");
    expect(ends.at(-1)).toMatchObject({ interrupted: true, ok: false, subtype: "error_during_execution" });
    expect(state.items.some((i) => i.kind === "notice" && i.text === "You stopped this turn.")).toBe(true);
    const poem = state.items.find((i) => i.kind === "assistant" && i.text.startsWith("1."));
    expect(poem).toBeTruthy();
  });

  it("lists the refused change, without counting it", () => {
    const changes = collectChanges(state, "/repo", { kind: "turn", turn: 1 });
    expect(changes).toHaveLength(1);
    expect(changes[0]).toMatchObject({ path: "b.txt", latestState: "denied", added: 0, attributed: true });
  });
});

describe("replay without deltas", () => {
  it("builds the same tools and text from finished lines only", () => {
    const lines = fixture("allow-edit.jsonl").filter((l) => !l.includes('"type": "stream_event"'));
    const replayed = applyAll(emptyThread(), lines);
    const live = applyAll(emptyThread(), fixture("allow-edit.jsonl"));
    expect(tools(replayed).map((t) => [t.name, t.status])).toEqual(tools(live).map((t) => [t.name, t.status]));
    const text = (s: ThreadState) => s.items.filter((i) => i.kind === "assistant").map((i) => (i.kind === "assistant" ? i.text : ""));
    expect(text(replayed)).toEqual(text(live));
  });

  it("skips lines at or below the last sequence number", () => {
    const state = emptyThread();
    applyLine(state, JSON.stringify({ type: "mavi_user", text: "hi", turn: 1 }), 5);
    applyLine(state, JSON.stringify({ type: "mavi_user", text: "hi", turn: 1 }), 5);
    applyLine(state, JSON.stringify({ type: "mavi_user", text: "again", turn: 2 }), 4);
    expect(state.items).toHaveLength(1);
  });
});

describe("approvals and live changes", () => {
  function withOpenEdit(): ThreadState {
    const state = emptyThread();
    applyLine(state, JSON.stringify({ type: "mavi_user", text: "rename", turn: 1 }));
    applyLine(state, JSON.stringify({
      type: "assistant",
      message: { id: "m1", content: [{ type: "tool_use", id: "t1", name: "Edit", input: { file_path: "/repo/src/a.ts", old_string: "const a = 1;\n", new_string: "const b = 1;\n" } }] },
    }));
    applyLine(state, JSON.stringify({
      type: "control_request",
      request_id: "r1",
      request: { subtype: "can_use_tool", tool_name: "Edit", tool_use_id: "t1", input: {}, description: "a.ts" },
    }));
    return state;
  }

  it("shows a proposed edit as its own approval row with a hunk built from the request", () => {
    const state = withOpenEdit();
    expect(openApprovals(state)).toHaveLength(1);
    const rows = toRows(state.items, "/repo");
    expect(rows.map((r) => r.kind)).toEqual(["item", "approval"]);
    const [change] = collectChanges(state, "/repo", { kind: "turn", turn: 1 });
    expect(change).toMatchObject({ path: "src/a.ts", latestState: "proposed", added: 1, removed: 1 });
    expect(change!.edits[0]!.fromInput).toBe(true);
  });

  it("voids open approvals on interrupt and on exit", () => {
    const state = withOpenEdit();
    applyLine(state, JSON.stringify({ type: "mavi_interrupt", request_id: "i1" }));
    expect(openApprovals(state)).toHaveLength(0);
    const exited = withOpenEdit();
    applyLine(exited, JSON.stringify({ type: "mavi_exit", exit: "exited with code 1", ok: false, stderr: ["boom"] }));
    expect(openApprovals(exited)).toHaveLength(0);
    expect(exited.ended).toBe("exited with code 1");
    expect(exited.items.at(-1)).toMatchObject({ kind: "notice", tone: "error" });
  });

  it("lists paths the edit tools never named as other changes", () => {
    const state = withOpenEdit();
    applyLine(state, JSON.stringify({ type: "mavi_answered", request_id: "r1", decision: "allow" }));
    applyLine(state, JSON.stringify({
      type: "user",
      message: { role: "user", content: [{ type: "tool_result", tool_use_id: "t1", content: "ok" }] },
      tool_use_result: { structuredPatch: [{ oldStart: 3, oldLines: 1, newStart: 3, newLines: 1, lines: ["-const a = 1;", "+const b = 1;"] }] },
    }));
    applyLine(state, JSON.stringify({
      type: "mavi_turn_files",
      turn: 1,
      available: true,
      moved: [{ path: "src/a.ts", code: " M" }, { path: "package-lock.json", code: " M" }],
    }));
    const changes = collectChanges(state, "/repo", { kind: "turn", turn: 1 });
    expect(changes.map((c) => [c.path, c.attributed, c.latestState])).toEqual([
      ["src/a.ts", true, "applied"],
      ["package-lock.json", false, "moved"],
    ]);
    expect(changes[0]!.edits[0]!.fromInput).toBe(false);
    expect(changes[0]!.edits[0]!.hunks[0]!.newStart).toBe(3);
    expect(turnTotals(state, "/repo", 1)).toEqual({ files: 2, added: 1, removed: 1 });
  });
});

describe("rows", () => {
  it("summarizes a run of calls by verb and keeps failures and edited files visible", () => {
    const state = emptyThread();
    const call = (id: string, name: string, input: Record<string, unknown>, ok = true) => {
      applyLine(state, JSON.stringify({ type: "assistant", message: { id: `m${id}`, content: [{ type: "tool_use", id, name, input }] } }));
      applyLine(state, JSON.stringify({ type: "user", message: { role: "user", content: [{ type: "tool_result", tool_use_id: id, content: ok ? "ok" : "boom", is_error: !ok }] } }));
    };
    call("a", "Read", { file_path: "/repo/a.ts" });
    call("b", "Read", { file_path: "/repo/b.ts" });
    call("c", "Edit", { file_path: "/repo/a.ts", old_string: "x", new_string: "y" });
    call("d", "Bash", { command: "npm test" }, false);
    const rows = toRows(state.items, "/repo");
    expect(rows).toHaveLength(1);
    const group = rows[0]!;
    expect(group.kind).toBe("tools");
    if (group.kind !== "tools") return;
    expect(group.summary).toBe("Read 2 files · Edited 1 file · Ran 1 command");
    expect(group.failed).toBe(1);
    expect(group.editedFiles).toEqual(["a.ts"]);
  });

  it("names unknown tools without their MCP prefix", () => {
    const tool = { name: "mcp__github__create_issue", input: {} } as unknown as ToolItem;
    expect(summarize([tool])).toBe("Used create_issue");
  });
});

describe("changes helpers", () => {
  it("builds hunks for writes and multi-edits from input", () => {
    expect(hunksFromInput({ name: "Write", input: { content: "a\nb\n" } })[0]!.lines).toEqual(["+a", "+b"]);
    const multi = hunksFromInput({ name: "MultiEdit", input: { edits: [{ old_string: "a", new_string: "b" }, { old_string: "c", new_string: "d" }] } });
    expect(multi).toHaveLength(2);
    expect(hunksFromInput({ name: "Read", input: {} })).toEqual([]);
    const kept = hunksFromInput({ name: "Edit", input: { old_string: "a\nb\nz\n", new_string: "a\nB\nC\nz\n" } });
    expect(kept[0]!.lines).toEqual([" a", "-b", "+B", "+C", " z"]);
  });

  it("describes porcelain codes in words", () => {
    expect(describeCode("??")).toBe("New");
    expect(describeCode("  ")).toBe("Back to HEAD");
    expect(describeCode(" D")).toBe("Deleted");
    expect(describeCode(" M")).toBe("Modified");
  });
});

describe("parsePatch", () => {
  it("reads hunk headers and lines, and notices binary files", async () => {
    const { parsePatch } = await import("./changes");
    const patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,3 @@\n a\n-b\n+c\n+d\n\\ No newline at end of file\n@@ -10 +11 @@\n-x\n+y\n";
    const parsed = parsePatch(patch);
    expect(parsed.hunks).toHaveLength(2);
    expect(parsed.hunks[0]).toMatchObject({ oldStart: 1, oldLines: 2, newStart: 1, newLines: 3, lines: [" a", "-b", "+c", "+d"] });
    expect(parsed.hunks[1]).toMatchObject({ oldStart: 10, oldLines: 1, newStart: 11, newLines: 1 });
    expect(parsePatch("Binary files a and b differ").binary).toBe(true);
  });
});
