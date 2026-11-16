import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { UNREADABLE_TITLE, updateAttention, WorkbenchError, type Attention, type AttentionRow, type AttentionWrite, type Page, type UnreadAttention } from "./client";
import { createAttentionDismiss, withAttentionUpdate } from "./attentionDismiss";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
beforeEach(() => native.mockReset());

const NOW = 2_000_000;
const item = (n: number, fields: Partial<Attention> = {}): Attention => ({ id: `event-${n}`, revision: 1, updated_at: 1000, source_sequence: n, task_id: "task", task_revision: 3, target_type: "run", target_id: "run", target_revision: 4, target_status: "current", kind: "run_exited", title: "Coding agent exited", created_at: 1000, read_at: null, dismissed_at: null, snoozed_until: null, ...fields });
const unknown: UnreadAttention = { id: "event-9", revision: 1, updated_at: 1000, unreadable: true, source_sequence: 9, kind: "run_quarantined", title: UNREADABLE_TITLE, created_at: 1000, read_at: null, dismissed_at: null, snoozed_until: null };
const pageOf = (items: AttentionRow[], total = items.length): Page<AttentionRow> => ({ items, total, shown: items.length, has_more: total > items.length, next_cursor: total > items.length ? "c" : null });

describe("folding a dismiss receipt into the page", () => {
  const rows = [item(1), item(2), item(3)];
  it("takes a dismissed notice out of Active and Unread, and moves the counts with it", () => {
    for (const filter of ["active", "unread"] as const) {
      const next = withAttentionUpdate(pageOf(rows, 40), item(2, { revision: 2, dismissed_at: NOW }), filter, NOW);
      expect(next.items.map((row) => row.id)).toEqual(["event-1", "event-3"]);
      expect(next).toMatchObject({ shown: 2, total: 39, has_more: true, next_cursor: "c" });
    }
  });
  it("keeps a dismissed notice under All, replaced by its receipt", () => {
    const receipt = item(2, { revision: 2, dismissed_at: NOW });
    const next = withAttentionUpdate(pageOf(rows), receipt, "all", NOW);
    expect(next.items[1]).toBe(receipt);
    expect(next).toMatchObject({ shown: 3, total: 3 });
  });
  it("keeps a restored notice where it was", () => {
    const receipt = item(2, { revision: 3, dismissed_at: null });
    expect(withAttentionUpdate(pageOf(rows), receipt, "active", NOW).items[1]).toBe(receipt);
  });
  it("drops a restored notice that is still snoozed or already read when the filter excludes it", () => {
    expect(withAttentionUpdate(pageOf(rows), item(2, { revision: 3, snoozed_until: NOW + 1 }), "active", NOW).items).toHaveLength(2);
    expect(withAttentionUpdate(pageOf(rows), item(2, { revision: 3, read_at: NOW }), "unread", NOW).items).toHaveLength(2);
    expect(withAttentionUpdate(pageOf(rows), item(2, { revision: 3, read_at: NOW }), "active", NOW).items).toHaveLength(3);
  });
  it("leaves the page alone for a receipt it does not hold", () => {
    const page = pageOf(rows);
    expect(withAttentionUpdate(page, item(7, { dismissed_at: NOW }), "active", NOW)).toBe(page);
  });
  it("never reports a total below what it shows", () => {
    expect(withAttentionUpdate(pageOf(rows, 1), item(1, { dismissed_at: NOW }), "active", NOW)).toMatchObject({ shown: 2, total: 2 });
  });
});

describe("sending a dismissal", () => {
  it("sends one dismiss for the notice's current revision", async () => {
    const write = vi.fn(async (input: AttentionWrite) => item(1, { revision: input.expected_revision + 1, dismissed_at: NOW }));
    await createAttentionDismiss(write).apply(item(1), "dismiss");
    expect(write).toHaveBeenCalledWith(expect.objectContaining({ id: "event-1", expected_revision: 1, action: "dismiss" }));
    expect(write.mock.calls[0]?.[0]).not.toHaveProperty("seconds");
  });
  it("retries an uncertain write with the same request id, and a settled refusal with a new one", async () => {
    const write = vi.fn<(input: AttentionWrite) => Promise<AttentionRow>>();
    const actions = createAttentionDismiss(write);
    write.mockRejectedValueOnce(new WorkbenchError("transport_error", "Reply lost"));
    await expect(actions.apply(item(1), "dismiss")).rejects.toThrow("Reply lost");
    write.mockRejectedValueOnce(new WorkbenchError("conflict", "Changed"));
    await expect(actions.apply(item(1), "dismiss")).rejects.toThrow("Changed");
    write.mockResolvedValueOnce(item(1, { revision: 2, dismissed_at: NOW }));
    await actions.apply(item(1), "dismiss");
    const ids = write.mock.calls.map(([input]) => input.request_id);
    expect(ids[0]).toBe(ids[1]);
    expect(ids[2]).not.toBe(ids[1]);
  });
  it("does not replay a kept write once the row has moved past its revision", async () => {
    const write = vi.fn<(input: AttentionWrite) => Promise<AttentionRow>>();
    const actions = createAttentionDismiss(write);
    write.mockRejectedValueOnce(new WorkbenchError("transport_error", "Reply lost"));
    await expect(actions.apply(item(1), "dismiss")).rejects.toThrow();
    // The lost dismiss landed; a refresh, a restore, and the reader dismisses again at revision 3.
    write.mockResolvedValueOnce(item(1, { revision: 4, dismissed_at: NOW }));
    await actions.apply(item(1, { revision: 3 }), "dismiss");
    const [first, second] = write.mock.calls.map(([input]) => input);
    expect(second).toMatchObject({ expected_revision: 3, action: "dismiss" });
    expect(second?.request_id).not.toBe(first?.request_id);
  });
  it("dismisses a notice of a kind this build cannot read, through the real client", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: { ...unknown, unreadable: undefined, revision: 2, dismissed_at: NOW } }));
    const receipt = await createAttentionDismiss(updateAttention).apply(unknown, "dismiss");
    expect(receipt).toMatchObject({ id: "event-9", unreadable: true, revision: 2, dismissed_at: NOW });
    expect(native).toHaveBeenCalledWith("cmd_workbench_request", expect.objectContaining({ method: "attention.update" }));
    expect(JSON.parse(String((native.mock.calls[0]?.[1] as { input: string }).input))).toMatchObject({ id: "event-9", expected_revision: 1, action: "dismiss" });
  });
});
