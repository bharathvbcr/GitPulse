import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TuningApply from "./TuningApply.svelte";
import type { TuningAdvice } from "../storage/types";

function advice(over: Partial<TuningAdvice> = {}): TuningAdvice {
  return {
    report: {
      maintenance: { registered: false, strategy: null, auto: null },
      midx: false,
      objects: { packs: 40, loose_objects: 10, loose_exact: true, gc_auto: 6700, loose_over_gc_auto: false },
      index: { state: "present", version: 2, entries: 50_000, entries_exact: true, skip_hash: false, split_index: false, note: null },
      partial_clone: false,
      sparse_checkout: false,
      keys: [],
      family: { worktrees: 0, split_index_in: [], unverified_indexes: [], with_worktree_config: [], truncated: false },
      config_note: null,
      notes: [],
    },
    offers: [],
    warnings: [],
    ...over,
  };
}

const props = (a: TuningAdvice | null | undefined) => ({
  advice: a,
  repoPath: "/repo",
  onapplied: () => {},
});

describe("TuningApply", () => {
  it("renders nothing when nothing clears its threshold", () => {
    expect(render(TuningApply, { props: props(advice()) }).body).not.toContain("tuning-apply");
    expect(render(TuningApply, { props: props(undefined) }).body).not.toContain("tuning-apply");
    expect(render(TuningApply, { props: props(null) }).body).not.toContain("tuning-apply");
  });

  it("lists each offer with its reason and only a preview button, never an apply", () => {
    const { body } = render(TuningApply, {
      props: props(
        advice({
          offers: [
            { change: "untracked_cache", title: "Turn on the untracked cache", reason: "50000 tracked files" },
            { change: "maintenance", title: "Start background maintenance", reason: "40 packs" },
          ],
        }),
      ),
    });
    expect(body).toContain('data-testid="tuning-apply"');
    expect(body).toContain('data-phase="idle"');
    expect(body).toContain("Turn on the untracked cache");
    expect(body).toContain("50000 tracked files");
    expect(body).toContain("Start background maintenance");
    expect(body).toContain('data-testid="tuning-apply-open"');
    expect(body).not.toContain('data-testid="tuning-apply-confirm"');
    expect(body.match(/data-testid="tuning-offer"/g)).toHaveLength(2);
  });

  it("calls out the split index even when nothing is offered", () => {
    const { body } = render(TuningApply, {
      props: props(
        advice({
          warnings: [
            "core.splitIndex is on, so GitPulse will not turn the untracked cache on here (it breaks git's own add-then-commit when combined with feature.manyFiles, which includes it).",
          ],
        }),
      ),
    });
    expect(body).toContain('data-testid="tuning-warning"');
    expect(body).toContain("add-then-commit");
    expect(body).not.toContain('data-testid="tuning-apply-open"');
  });

  it("shows a running apply found by the scan as stoppable, with no new preview offered", () => {
    const a = advice({
      offers: [{ change: "untracked_cache", title: "Turn on the untracked cache", reason: "x" }],
      apply_in_flight: true,
    });
    const { body } = render(TuningApply, { props: props(a) });
    // Server render has no effects, so the phase is the idle one; the offer is
    // hidden because an apply is already running.
    expect(body).not.toContain('data-testid="tuning-apply-open"');
  });
});
