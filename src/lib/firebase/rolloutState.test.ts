import { describe, expect, it } from "vitest";
import { isSettled, isVerdict } from "../delivery/phase";
import {
  commitShaProblem,
  currentRollout,
  isFailed,
  isInFlight,
  isLive,
  rolloutPhase,
  rolloutStateClass,
  rolloutStateLabel,
  rolloutTimelineRow,
  shortSha,
} from "./rolloutState";
import type { RolloutInfo, RolloutState } from "./types";

const rollout = (state: RolloutState, id = "r1"): RolloutInfo => ({
  id,
  state,
  create_time: null,
  update_time: null,
  error: null,
  commit: null,
});

describe("rollout state vocabulary", () => {
  it("counts only a succeeded rollout as live", () => {
    expect(isLive({ kind: "succeeded" })).toBe(true);
    for (const kind of [
      "unspecified",
      "queued",
      "pending_build",
      "progressing",
      "paused",
      "failed",
      "cancelled",
      "skipped",
    ] as const) {
      expect(isLive({ kind }), `${kind} must not read as live`).toBe(false);
    }
  });

  it("treats a state this build does not know as unknown, never as a deploy", () => {
    // The first state Firebase invents must not inherit a green badge.
    const unknown: RolloutState = { kind: "unrecognised", raw: "TELEPORTED" };
    expect(isLive(unknown)).toBe(false);
    expect(isFailed(unknown)).toBe(false);
    expect(isInFlight(unknown)).toBe(false);
    // ...and it shows the real string rather than a guess, so a reader can
    // search for what they are actually looking at.
    expect(rolloutStateLabel(unknown)).toContain("TELEPORTED");
    expect(rolloutStateClass(unknown)).toBe("text-textMuted");
  });

  it("does not count an unjudgeable state as a failure", () => {
    // Counting these as failures would inflate a change-failure rate with our
    // own ignorance rather than with Firebase's answer.
    expect(isFailed({ kind: "unrecognised", raw: "X" })).toBe(false);
    expect(isFailed({ kind: "unspecified" })).toBe(false);
    expect(isFailed({ kind: "skipped" })).toBe(false);
    expect(isFailed({ kind: "failed" })).toBe(true);
    expect(isFailed({ kind: "cancelled" })).toBe(true);
  });

  it("puts both light and dark shades on every verdict", () => {
    // A bare `-400` is tuned for the dark theme and sits near 2:1 on the light
    // theme's card — on exactly the labels a reader opened the panel to check.
    for (const kind of ["succeeded", "failed", "cancelled", "queued", "progressing"] as const) {
      const cls = rolloutStateClass({ kind });
      expect(cls, `${kind} must name a light shade`).toMatch(/text-\w+-\d00\b/);
      expect(cls, `${kind} must name a dark shade`).toContain("dark:text-");
    }
  });

  it("labels every known state without falling through to Unknown", () => {
    for (const kind of [
      "unspecified",
      "queued",
      "pending_build",
      "progressing",
      "paused",
      "succeeded",
      "failed",
      "cancelled",
      "skipped",
    ] as const) {
      expect(rolloutStateLabel({ kind }), `${kind} needs a label`).not.toBe("Unknown");
    }
  });

  it("picks the newest live rollout and returns null when none is live", () => {
    expect(
      currentRollout([
        rollout({ kind: "progressing" }, "newest"),
        rollout({ kind: "succeeded" }, "live"),
        rollout({ kind: "succeeded" }, "older"),
      ])?.id,
    ).toBe("live");
    expect(currentRollout([rollout({ kind: "failed" })])).toBeNull();
    expect(currentRollout([])).toBeNull();
  });

  it("abbreviates a SHA for display only", () => {
    expect(shortSha("0123456789abcdef0123456789abcdef01234567")).toBe("0123456");
  });

  it("refuses a deploy target that is not a full SHA, and says why", () => {
    const full = "0123456789abcdef0123456789abcdef01234567";
    expect(commitShaProblem(full)).toBeNull();
    expect(commitShaProblem(`  ${full.toUpperCase()}  `)).toBeNull();

    // Each refusal carries a reason. A disabled control with no explanation is
    // the shape people work around by pasting something else.
    for (const bad of ["", "   ", "0123456", `${full}0`, "z".repeat(40), "HEAD", "main"]) {
      const problem = commitShaProblem(bad);
      expect(problem, `"${bad}" must be refused`).not.toBeNull();
      expect(problem!.length, `"${bad}" must be refused with a reason`).toBeGreaterThan(10);
    }
  });

  it("names the ambiguity that makes an abbreviation the wrong deploy target", () => {
    // The value decides what production serves, so the refusal has to explain
    // itself rather than read as arbitrary strictness.
    const problem = commitShaProblem("0123456")!;
    expect(problem).toContain("40");
    expect(problem.toLowerCase()).toContain("ambiguous");
  });
});

const ALL_STATE_KINDS = [
  "unspecified",
  "queued",
  "pending_build",
  "progressing",
  "paused",
  "succeeded",
  "failed",
  "cancelled",
  "skipped",
] as const;

describe("rollout phase in the shared delivery vocabulary", () => {
  it("agrees with the predicates it is derived from, for every state", () => {
    // The point of deriving rather than re-listing: a parallel mapping would
    // be free to drift from isLive, and the drift would show up as a deploy
    // the timeline calls green and the badge beside it calls unknown.
    for (const kind of ALL_STATE_KINDS) {
      const state: RolloutState = { kind };
      const phase = rolloutPhase(state);
      expect(phase === "settled_ok", kind).toBe(isLive(state));
      expect(phase === "settled_bad", kind).toBe(isFailed(state));
      expect(phase === "in_flight", kind).toBe(isInFlight(state));
    }
  });

  it("gives an unrecognised state no verdict and keeps it out of the poll", () => {
    const unknown: RolloutState = { kind: "unrecognised", raw: "TELEPORTED" };
    expect(rolloutPhase(unknown)).toBe("unknown");
    expect(isSettled(rolloutPhase(unknown)), "must not poll forever").toBe(true);
    expect(isVerdict(rolloutPhase(unknown)), "must not enter a rate").toBe(false);
  });

  it("maps every known state to a phase without falling through by accident", () => {
    expect(rolloutPhase({ kind: "succeeded" })).toBe("settled_ok");
    expect(rolloutPhase({ kind: "failed" })).toBe("settled_bad");
    expect(rolloutPhase({ kind: "cancelled" })).toBe("settled_bad");
    expect(rolloutPhase({ kind: "progressing" })).toBe("in_flight");
    expect(rolloutPhase({ kind: "pending_build" })).toBe("in_flight");
    expect(rolloutPhase({ kind: "queued" })).toBe("in_flight");
    expect(rolloutPhase({ kind: "paused" })).toBe("in_flight");
    expect(rolloutPhase({ kind: "skipped" })).toBe("unknown");
    expect(rolloutPhase({ kind: "unspecified" })).toBe("unknown");
  });
});

describe("rollout as a timeline row", () => {
  const full = "0123456789abcdef0123456789abcdef01234567";
  const withCommit = (overrides: Partial<RolloutInfo> = {}): RolloutInfo => ({
    id: "r1",
    state: { kind: "succeeded" },
    create_time: "2026-09-17T08:00:00Z",
    update_time: "2026-09-17T08:04:00Z",
    error: null,
    commit: {
      hash: full,
      branch: "main",
      message: "Ship it\nlong body",
      author: "a",
      commit_time: null,
      present_locally: true,
    },
    ...overrides,
  });

  it("spans create to update time", () => {
    const row = rolloutTimelineRow(withCommit());
    expect(row.startedAt).toBe("2026-09-17T08:00:00Z");
    expect(row.endedAt).toBe("2026-09-17T08:04:00Z");
  });

  it("takes only the commit subject, never a multi-line body", () => {
    // A newline inside a single-line row pushes every later row down.
    expect(rolloutTimelineRow(withCommit()).sublabel).toBe("Ship it");
  });

  it("degrades every absent field to empty rather than undefined", () => {
    const bare = rolloutTimelineRow({
      id: "r2",
      state: { kind: "queued" },
      create_time: null,
      update_time: null,
      error: null,
      commit: null,
    });
    for (const key of ["sublabel", "startedAt", "endedAt", "commitSha", "branch", "trigger"] as const) {
      expect(bare[key], key).toBe("");
    }
    expect(bare.label).toBe("r2");
  });

  it("reports no trigger rather than guessing one", () => {
    // App Hosting does not publish what started a rollout; an invented
    // "push" chip would be a fact this app made up.
    expect(rolloutTimelineRow(withCommit()).trigger).toBe("");
  });
});
