import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { latestEnhancement, listEnhancements, WorkbenchError } from "./client";
import { FP, proposal, refine, storedRefine, task, view } from "./proposals.fixtures";
import {
  applyRefine, coverageNotes, dismissFingerprint, dismissRefine, dismissedFingerprints, fileScanProposal, getScan, refineDiff, refineResult,
  requestRefine, scanIsPartial, scanView, startScan,
} from "./proposals";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
beforeEach(() => native.mockReset());

const wire = (n = 0) => JSON.parse(String((native.mock.calls[n]?.[1] as { input: string }).input));
const method = (n = 0) => (native.mock.calls[n]?.[1] as { method: string }).method;

describe("scan wire and decoding", () => {
  it("starts a scan with the repository path and reads it back", async () => {
    native.mockResolvedValueOnce(JSON.stringify(view({ state: "running" })));
    expect((await startScan("/r", { since: " main ", maxProposals: 7 })).state).toBe("running");
    expect(method()).toBe("scans.start");
    expect(wire()).toEqual({ repo_path: "/r", since: "main", max_proposals: 7 });
    native.mockResolvedValueOnce(JSON.stringify(view()));
    expect((await getScan("scan-1")).proposals[0]?.signals).toHaveLength(2);
  });

  it("a sidecar that is down is an error with its code, never an empty scan", async () => {
    native.mockRejectedValueOnce({ code: "not_installed", message: "no manvi binary" });
    await expect(startScan("/r")).rejects.toMatchObject({ code: "not_installed", message: "no manvi binary" });
    native.mockRejectedValueOnce({ code: "E_BAD_ROOT", message: "repo_root is not a git work tree" });
    await expect(startScan("/r")).rejects.toMatchObject({ code: "E_BAD_ROOT" });
  });

  it.each([
    ["unknown state", view({ state: "weird" })], ["short fingerprint", view({ proposals: [{ ...proposal, fingerprint: "abc" }] })],
    ["missing coverage", { ...view(), coverage: undefined }], ["unknown proposal status", view({ proposals: [{ ...proposal, status: "x" }] })],
  ])("refuses a malformed reply whole: %s", (_name, bad) => { expect(() => scanView(bad)).toThrow(WorkbenchError); });
});

describe("a partial scan says so, with both numbers", () => {
  it("states shown and total when capped, and every non-ok category", () => {
    const partial = scanView(view({ complete: false, over_cap: 17, max_proposals: 25, new_count: 25, proposals: [proposal],
      coverage: [{ category: "dead_symbols", status: "truncated", detail: "60 of 140 checked", hidden: 80 }, { category: "doc_drift", status: "ok" }] }));
    expect(scanIsPartial(partial)).toBe(true);
    const notes = coverageNotes(partial).join("\n");
    expect(notes).toContain("Showing 1 of 18 new proposals (cap 25)");
    expect(notes).toContain("60 of 140 checked");
    expect(notes).toContain("80 hidden");
    expect(notes).not.toContain("Stale doc paths");
  });
  it("a complete finished scan is not partial; an unfinished or failed one is", () => {
    expect(scanIsPartial(scanView(view()))).toBe(false);
    expect(coverageNotes(scanView(view()))).toEqual([]);
    expect(scanIsPartial(scanView(view({ state: "deadline", complete: true })))).toBe(true);
    expect(coverageNotes(scanView(view({ state: "failed", error: "boom" }))).join()).toContain("boom");
    expect(coverageNotes(scanView(view({ board_complete: false }))).join()).toContain("duplicate");
  });
});

describe("filing a proposal", () => {
  const draft = { title: "T", description: "D", kind: "maintenance", priority: 2, labels: [] };
  it("sends the scan fingerprint as the origin, so filing twice is one card", async () => {
    native.mockResolvedValue(JSON.stringify({ ok: true, outcome: "filed", item_id: "ft-1", title: "T" }));
    const p = scanView(view()).proposals[0]!;
    expect((await fileScanProposal("/r", p, draft)).outcome).toBe("filed");
    expect(method()).toBe("proposals.file");
    expect(wire().origin).toEqual({ kind: "scan", fingerprint: FP });
  });
  it("decodes the related-task hold, and refuses a receipt that is neither", async () => {
    const p = scanView(view()).proposals[0]!;
    native.mockResolvedValueOnce(JSON.stringify({ ok: false, outcome: "related_tasks_exist", related: [{ item_id: "ft-2", title: "Similar", status: "todo" }] }));
    expect(await fileScanProposal("/r", p, draft)).toEqual({ outcome: "related_tasks_exist", related: [{ item_id: "ft-2", title: "Similar", status: "todo" }] });
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, outcome: "weird" }));
    await expect(fileScanProposal("/r", p, draft)).rejects.toBeInstanceOf(WorkbenchError);
  });
});

describe("dismissal is per viewer and never needs storage", () => {
  it("works without localStorage (node) and with it", () => {
    expect(dismissedFingerprints("/r").size).toBe(0);
    expect(dismissFingerprint("/r", FP).has(FP)).toBe(true);
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) });
    dismissFingerprint("/r", FP);
    expect(dismissedFingerprints("/r").has(FP)).toBe(true);
    expect(dismissedFingerprints("/other").size).toBe(0);
    vi.unstubAllGlobals();
  });
});

describe("refinement", () => {
  it("sends the repository, the task and its revision; a proposal that claims it applied is refused", async () => {
    native.mockResolvedValueOnce(JSON.stringify(refine())).mockResolvedValueOnce(JSON.stringify(storedRefine()));
    const result = await requestRefine("/r", task, null, "q1");
    expect(method()).toBe("enhancements.refine");
    expect(wire()).toEqual({ repo_path: "/r", id: "t1", request_id: "q1", expected_revision: 4 });
    expect(result.criteria).toHaveLength(2);
    expect(() => refineResult(refine({ applied: true }))).toThrow(WorkbenchError);
    expect(() => refineResult(refine({ proposal: { acceptance_criteria: "x" } }))).toThrow(WorkbenchError);
  });
  it("shows the proposal as a diff against the task and flags a task that moved", () => {
    const result = refineResult(refine());
    const diff = refineDiff(task, result);
    expect(diff.stale).toBe(false);
    expect(diff.criteria.map((c) => c.state)).toEqual(["present", "add"]);
    expect(diff.duplicates[0]?.state).toBe("add");
    expect(refineDiff({ ...task, revision: 5 }, result).stale).toBe(true);
    expect(refineDiff({ ...task, links: [{ kind: "duplicate_of", item_id: "t9" }] }, result).duplicates[0]?.state).toBe("present");
  });
  it("reads the stored proposal back for the revision an accept is conditional on", async () => {
    native.mockResolvedValueOnce(JSON.stringify(refine())).mockResolvedValueOnce(JSON.stringify(storedRefine()));
    const result = await requestRefine("/r", task, null, "q1");
    expect(method(1)).toBe("enhancements.get");
    expect(wire(1)).toEqual({ id: "enh-1" });
    expect(result.enhancement_id).toBe("enh-1");
    expect(result.enhancement_revision).toBe(2);
  });
  it.each([
    ["not ready", storedRefine({ state: "dismissed" })],
    ["another task's", storedRefine({ task_id: "t2" })],
    ["drafted against another revision", storedRefine({ source_revision: 3 })],
    ["a text proposal", storedRefine({ kind: "text", fields: ["title"] })],
  ])("a reply naming a stored proposal that is %s is not a proposal", async (_name, stored) => {
    native.mockResolvedValueOnce(JSON.stringify(refine())).mockResolvedValueOnce(JSON.stringify(stored));
    await expect(requestRefine("/r", task, null, "q1")).rejects.toBeInstanceOf(WorkbenchError);
  });
  it("a reply without a stored proposal id is refused", () => {
    const { enhancement_id: _gone, ...bare } = refine();
    expect(() => refineResult(bare)).toThrow(WorkbenchError);
  });
  it("accepts the selected parts of the stored proposal at the reviewed revisions", async () => {
    const result = { ...refineResult(refine()), enhancement_revision: 2 };
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, outcome: "applied", revision: 5, criteria_added: 1, links_added: 0, created_item_ids: ["ft-a"] }));
    const receipt = await applyRefine("/r", result, ["acceptance_criteria", "split_into"]);
    expect(receipt).toEqual({ revision: 5, criteriaAdded: 1, linksAdded: 0, createdTaskIds: ["ft-a"] });
    expect(method()).toBe("proposals.apply");
    expect(wire()).toEqual({ repo_path: "/r", id: "enh-1", expected_revision: 2, expected_task_revision: 4, fields: ["acceptance_criteria", "split_into"] });
    expect(wire()).not.toHaveProperty("split_ids");
  });
  it("a stale task is a visible refusal", async () => {
    const result = { ...refineResult(refine()), enhancement_revision: 2 };
    native.mockRejectedValueOnce({ code: "revision_conflict", message: "the task changed before refinement acceptance" });
    await expect(applyRefine("/r", result, ["acceptance_criteria"])).rejects.toMatchObject({ code: "revision_conflict" });
  });
  it("rejecting a proposal dismisses the stored one at its revision", async () => {
    native.mockResolvedValueOnce(JSON.stringify({ ok: true }));
    await dismissRefine({ ...refineResult(refine()), enhancement_revision: 2 });
    expect(method()).toBe("enhancements.dismiss");
    expect(wire()).toMatchObject({ id: "enh-1", expected_revision: 2 });
  });
});

describe("stored refinements stay out of the text suggestions list", () => {
  const text = { id: "e0", revision: 2, updated_at: 1, state: "ready", task_id: "t1", source_revision: 4, fields: ["title"], provider: "p", model: "m", automatic: false, created_at: 1, expires_at: 2 };
  const rows = (items: unknown[]) => JSON.stringify({ ok: true, items, shown: items.length, total: items.length, has_more: false, next_cursor: null });
  it("a ready refinement newer than a text proposal does not hide it, and is not decoded as one", async () => {
    native.mockResolvedValueOnce(rows([storedRefine().item, text]));
    expect((await listEnhancements("t1")).items.map((row) => row.id)).toEqual(["e0"]);
    native.mockResolvedValueOnce(rows([storedRefine().item, text]));
    expect((await latestEnhancement("t1", ["ready"]))?.id).toBe("e0");
    expect(wire(1)).toMatchObject({ limit: 30, states: ["ready"] });
  });
});
