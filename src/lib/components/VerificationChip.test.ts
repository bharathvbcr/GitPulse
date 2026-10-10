import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import VerificationChip from "./VerificationChip.svelte";
import TaskVerification from "./TaskVerification.svelte";
import { VERDICT_LABELS, VERDICT_STATES, decodeVerification, type Verification, type VerdictState } from "../workbench/verification";

const record = {
  request_id: "r1", head: "abc", worktree: "/w", critic_summary: "",
  criteria: [
    { index: 1, text: "exports csv", status: "unmet", findings: ["no csv writer found"] },
    { index: 2, text: "has tests", status: "met", findings: [] },
  ],
  degraded: ["the critic did not complete"],
};
const html = (verification: Verification) => render(VerificationChip, { props: { verification } }).body;
const at = (state: VerdictState, extra: Partial<Verification> = {}): Verification => ({ state, stale: false, record: null, ...extra });

describe("the verdict chip", () => {
  it.each(VERDICT_STATES)("renders %s with its own label and state", (state) => {
    const out = html(at(state));
    expect(out).toContain(`data-testid="verification-chip" data-state="${state}"`);
    expect(out).toContain(`>${VERDICT_LABELS[state]}</span>`);
  });

  it("keeps could-not-run visibly distinct from failed and from passed", () => {
    const labels = new Set(VERDICT_STATES.map((s) => VERDICT_LABELS[s]));
    expect(labels.size).toBe(VERDICT_STATES.length);
    const out = html(at("could_not_run"));
    expect(out).toContain("not a failure");
    expect(out).not.toMatch(/>(Passed|Failed)</);
  });

  it("never describes degraded as a pass", () => {
    const out = html(at("degraded"));
    expect(out).toContain("not a pass");
    expect(out).not.toContain(">Passed<");
  });

  it("expands to per-criterion results with their findings", () => {
    const out = html(at("failed", { record }));
    expect(out).toContain("<details");
    expect(out).toContain("2 criteria, 1 not met");
    expect(out).toContain("exports csv");
    expect(out).toContain("no csv writer found");
    expect(out).toContain('data-status="met"');
    expect(out).toContain("the critic did not complete");
  });

  it("offers no fold when there is nothing to expand", () => {
    expect(html(at("none"))).not.toContain("<details");
    expect(html(at("running"))).not.toContain("<details");
  });

  it("says when the verdict is for an earlier commit", () => {
    expect(html(at("passed", { stale: true }))).toContain("for an earlier commit");
    expect(html(at("none", { stale: true }))).not.toContain("for an earlier commit");
  });

  it("escapes what a verdict contains", () => {
    const out = html(at("failed", { record: { ...record, criteria: [{ index: 1, text: "<img src=x onerror=alert(1)>", status: "unmet", findings: [] }] } }));
    expect(out).not.toContain("<img src=x");
  });
});

describe("the task's verification section", () => {
  it("renders a Verify action before any verdict has loaded", () => {
    const out = render(TaskVerification, { props: { taskId: "ft-1", revision: 1 } }).body;
    expect(out).toContain('data-testid="task-verify"');
    expect(out).toMatch(/>\s*Verify\s*</);
    expect(out).not.toContain("verification-chip");
  });
});

describe("decoding the host's answer", () => {
  it("accepts every state the host names", () => {
    for (const state of VERDICT_STATES) expect(decodeVerification({ ok: true, state, stale: false, record: null }).state).toBe(state);
  });
  it("refuses an unknown state or a failed envelope instead of drawing it", () => {
    expect(() => decodeVerification({ ok: true, state: "approved" })).toThrow();
    expect(() => decodeVerification({ ok: false, state: "passed" })).toThrow();
    expect(() => decodeVerification(null)).toThrow();
  });
  it("bounds and sanitizes the record", () => {
    const v = decodeVerification({ ok: true, state: "failed", stale: true, record: { criteria: [{ index: "x", text: 5, findings: "no" }, null], degraded: "no" } });
    expect(v.stale).toBe(true);
    expect(v.record?.criteria).toEqual([{ index: 0, text: "", status: "unknown", findings: [] }]);
    expect(v.record?.degraded).toEqual([]);
  });
});
