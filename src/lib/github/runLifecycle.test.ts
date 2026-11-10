import { describe, expect, it } from "vitest";
import { isSettled, isVerdict } from "../delivery/phase";
import { runPhase, runStateClass, runStateLabel, runTimelineRow } from "./runLifecycle";
import { outcomeFor } from "./runOutcomeFixtures";
import { RUN_OUTCOMES, type WorkflowRunInfo } from "./types";

/**
 * A run as Rust would have parsed it: `outcome` is the classifier's answer for
 * the status and conclusion given (`runOutcomeFixtures`), unless a test names
 * one to prove the renderer reads it and nothing else.
 */
function makeRun(overrides: Partial<WorkflowRunInfo> = {}): WorkflowRunInfo {
  const run = baseRun(overrides);
  return { ...run, outcome: overrides.outcome ?? outcomeFor(run.status, run.conclusion) };
}

function baseRun(overrides: Partial<WorkflowRunInfo>): WorkflowRunInfo {
  return {
    outcome: "unknown",
    id: 1,
    name: "CI",
    title: "Fix the thing",
    status: "completed",
    conclusion: "success",
    head_branch: "main",
    url: "https://example.invalid/run/1",
    created_at: "2026-09-17T07:50:00Z",
    started_at: "2026-09-17T08:00:00Z",
    updated_at: "2026-09-17T08:05:00Z",
    head_sha: "0123456789abcdef0123456789abcdef01234567",
    event: "push",
    ...overrides,
  };
}

/**
 * gh 2.101.0's documented `--status` filter vocabulary, which is the union of
 * the two fields. Every word must land somewhere deliberate.
 */
const GH_STATUS_WORDS = [
  "queued",
  "completed",
  "in_progress",
  "requested",
  "waiting",
  "pending",
] as const;

const GH_CONCLUSION_WORDS = [
  "action_required",
  "cancelled",
  "failure",
  "neutral",
  "skipped",
  "stale",
  "startup_failure",
  "success",
  "timed_out",
] as const;

describe("the phase is the Rust outcome and nothing else", () => {
  it("maps every outcome to one deliberate phase", () => {
    const phases = Object.fromEntries(
      RUN_OUTCOMES.map((outcome) => [outcome, runPhase(makeRun({ outcome }))]),
    );
    expect(phases).toEqual({
      in_flight: "in_flight",
      passed: "settled_ok",
      failed: "settled_bad",
      // Not red, but a bad end for the pass rate it shares with Firebase.
      cancelled: "settled_bad",
      // Held for a person: settled, and no verdict.
      needs_person: "unknown",
      unknown: "unknown",
    });
  });

  it("does not look at status or conclusion when judging", () => {
    // If the renderer kept a table of its own, these would come out as a pass
    // and a failure. The only opinion is the one Rust attached.
    expect(
      runPhase(makeRun({ status: "completed", conclusion: "success", outcome: "unknown" })),
    ).toBe("unknown");
    expect(
      runPhase(makeRun({ status: "in_progress", conclusion: "", outcome: "failed" })),
    ).toBe("settled_bad");
  });

  it("gives no verdict to an outcome this build has never heard of", () => {
    const newer = { ...makeRun(), outcome: "quarantined" } as unknown as WorkflowRunInfo;
    expect(runPhase(newer)).toBe("unknown");
  });
});

describe("a run still moving is never given a verdict", () => {
  it("treats every in-flight status as in flight", () => {
    for (const status of GH_STATUS_WORDS) {
      if (status === "completed") continue;
      expect(runPhase(makeRun({ status, conclusion: "" })), status).toBe("in_flight");
    }
  });

  it("lets status override a stale conclusion", () => {
    // A re-run keeps the previous attempt's conclusion visible for a moment.
    // Reading conclusion first would call a running job failed.
    expect(runPhase(makeRun({ status: "in_progress", conclusion: "failure" }))).toBe("in_flight");
    expect(runPhase(makeRun({ status: "queued", conclusion: "success" }))).toBe("in_flight");
  });
});

describe("a completed run is judged only by its conclusion", () => {
  it("calls success a success", () => {
    expect(runPhase(makeRun({ conclusion: "success" }))).toBe("settled_ok");
  });

  it("calls the four real failures failures", () => {
    for (const conclusion of ["failure", "cancelled", "timed_out", "startup_failure"]) {
      expect(runPhase(makeRun({ conclusion })), conclusion).toBe("settled_bad");
    }
  });

  it("refuses to call a non-failure a failure", () => {
    // None of these broke anything. A run awaiting manual approval is not a
    // breakage, and a notice that cries failure over one teaches people to
    // stop reading the notices.
    for (const conclusion of ["neutral", "skipped", "stale", "action_required"]) {
      const phase = runPhase(makeRun({ conclusion }));
      expect(phase, conclusion).toBe("unknown");
      expect(isVerdict(phase), `${conclusion} must not be counted in a rate`).toBe(false);
      expect(isSettled(phase), `${conclusion} must not keep the poll alive`).toBe(true);
    }
  });

  it("NEVER reads a completed run with no conclusion as a success", () => {
    // The single most important case in this file. gh reports an empty
    // conclusion whenever it could not be read, and the naive check —
    // `conclusion !== "failure"` — calls that green. A silent false pass on
    // exactly the runs whose outcome is unknown.
    const phase = runPhase(makeRun({ status: "completed", conclusion: "" }));
    expect(phase).toBe("unknown");
    expect(phase).not.toBe("settled_ok");
    expect(runStateLabel(makeRun({ status: "completed", conclusion: "" }))).toContain(
      "no conclusion",
    );
  });

  it("gives no verdict to a conclusion this build has never heard of", () => {
    for (const conclusion of ["teleported", "SUCCESS_MAYBE", "42", "null"]) {
      expect(runPhase(makeRun({ conclusion })), conclusion).toBe("unknown");
    }
  });
});

describe("every word gh documents lands somewhere deliberate", () => {
  it("classifies each status word without falling through to a guess", () => {
    for (const status of GH_STATUS_WORDS) {
      const phase = runPhase(makeRun({ status, conclusion: "success" }));
      // `completed` defers to the conclusion; everything else is in flight.
      expect(phase, status).toBe(status === "completed" ? "settled_ok" : "in_flight");
    }
  });

  it("labels each conclusion word without falling through to Unknown", () => {
    for (const conclusion of GH_CONCLUSION_WORDS) {
      expect(runStateLabel(makeRun({ conclusion })), conclusion).not.toMatch(/^Unknown/);
    }
  });

  it("labels each in-flight status word without falling through to Unknown", () => {
    for (const status of GH_STATUS_WORDS) {
      if (status === "completed") continue;
      expect(runStateLabel(makeRun({ status, conclusion: "" })), status).not.toMatch(/^Unknown/);
    }
  });
});

describe("malformed input is judged, not trusted", () => {
  it("ignores case and surrounding whitespace", () => {
    expect(runPhase(makeRun({ status: "  COMPLETED  ", conclusion: " Success " }))).toBe(
      "settled_ok",
    );
    expect(runPhase(makeRun({ status: "In_Progress", conclusion: "" }))).toBe("in_flight");
  });

  it("gives no verdict when the status is missing entirely", () => {
    expect(runPhase(makeRun({ status: "", conclusion: "success" }))).toBe("unknown");
    expect(runStateLabel(makeRun({ status: "", conclusion: "" }))).toContain("no status");
  });

  it("does not throw when the fields are absent rather than empty", () => {
    // These arrive from serde and from a CLI; an undefined must degrade to a
    // verdict-free phase, not to a crash inside a panel render.
    const broken = { status: undefined, conclusion: undefined } as unknown as WorkflowRunInfo;
    expect(() => runPhase(broken)).not.toThrow();
    expect(runPhase(broken)).toBe("unknown");
    expect(() => runStateLabel(broken)).not.toThrow();
    expect(() => runStateClass(broken)).not.toThrow();
  });
});

describe("colour", () => {
  it("gives every verdict a status role, themed for both schemes", () => {
    // A bare palette shade is tuned for one theme and sits near 2:1 on the
    // other's card — on exactly the labels a reader opened the panel to check.
    // The status roles are defined per theme, so no `dark:` pairing is needed.
    const expected: Array<[WorkflowRunInfo, string]> = [
      [makeRun({ conclusion: "success" }), "text-ok"],
      [makeRun({ conclusion: "failure" }), "text-danger"],
      [makeRun({ status: "in_progress", conclusion: "" }), "text-warn"],
    ];
    for (const [run, role] of expected) {
      expect(runStateClass(run), `${run.status}/${run.conclusion}`).toBe(role);
    }
  });

  it("gives an unjudgeable run no verdict colour", () => {
    expect(runStateClass(makeRun({ conclusion: "" }))).toBe("text-textMuted");
  });
});

describe("as a timeline row", () => {
  it("measures from started_at, never from created_at", () => {
    // They differ by however long the run waited for a runner. Using the
    // creation time reports queue time as execution time — a run that waited
    // twenty minutes and ran for ten would draw as the slowest in the sample.
    const row = runTimelineRow(makeRun());
    expect(row.startedAt).toBe("2026-09-17T08:00:00Z");
    expect(row.startedAt).not.toBe("2026-09-17T07:50:00Z");
  });

  it("carries the commit, branch and trigger through for the join", () => {
    const row = runTimelineRow(makeRun());
    expect(row.commitSha).toBe("0123456789abcdef0123456789abcdef01234567");
    expect(row.branch).toBe("main");
    expect(row.trigger).toBe("push");
  });

  it("keys on the run id as a string", () => {
    expect(runTimelineRow(makeRun({ id: 987 })).id).toBe("987");
  });

  it("shows the workflow name as a sublabel only when it adds something", () => {
    expect(runTimelineRow(makeRun({ title: "Fix", name: "CI" })).sublabel).toBe("CI");
    expect(runTimelineRow(makeRun({ title: "CI", name: "CI" })).sublabel).toBe("");
  });

  it("always has a label, even with nothing to label it with", () => {
    // An empty row in the list is indistinguishable from a rendering bug.
    expect(runTimelineRow(makeRun({ title: "", name: "" })).label).toBe("Run 1");
    expect(runTimelineRow(makeRun({ title: "", name: "CI" })).label).toBe("CI");
  });

  it("degrades absent optional fields to empty, not to undefined", () => {
    // `undefined` in a Svelte template renders the literal string.
    const sparse = {
      id: 5,
      name: "CI",
      title: "t",
      status: "completed",
      conclusion: "success",
    } as unknown as WorkflowRunInfo;
    const row = runTimelineRow(sparse);
    for (const key of ["startedAt", "endedAt", "commitSha", "branch", "trigger", "url"] as const) {
      expect(row[key], key).toBe("");
    }
  });
});
