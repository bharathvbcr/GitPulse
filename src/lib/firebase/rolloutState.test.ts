import { describe, expect, it } from "vitest";
import {
  commitShaProblem,
  currentRollout,
  isFailed,
  isInFlight,
  isLive,
  rolloutStateClass,
  rolloutStateLabel,
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
