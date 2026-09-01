import { describe, it, expect } from "vitest";
import {
  IDLE_OPERATION,
  actionConsequence,
  actionLabel,
  blockedReason,
  blocksOtherMutations,
  operationStatesEqual,
  headline,
  isDestructive,
  kindLabel,
  kindTitle,
  nextStep,
  orderedActions,
  progressLabel,
  tabMarker,
  tabTooltip,
  type OperationAction,
  type OperationKind,
  type RepoOperation,
} from "./operation";

const ALL_KINDS: OperationKind[] = [
  "Merge",
  "Rebase",
  "RebaseApply",
  "ApplyMailbox",
  "CherryPick",
  "Revert",
  "Bisect",
];

const ALL_ACTIONS: OperationAction[] = ["abort", "continue", "skip"];

function op(extra: Partial<RepoOperation> = {}): RepoOperation {
  return {
    kind: "Merge",
    current_step: null,
    total_steps: null,
    head_ref: "main",
    incoming_ref: null,
    conflicted_paths: [],
    conflicted_total: 0,
    available: ["abort"],
    ...extra,
  };
}

describe("labels", () => {
  it("names every kind, with no gaps and no leaked identifiers", () => {
    for (const kind of ALL_KINDS) {
      const label = kindLabel(kind);
      expect(label).toBeTruthy();
      // A raw enum name reaching the UI is the bug this guards.
      expect(label).not.toBe(kind);
      expect(label).toBe(label.toLowerCase());
      expect(kindTitle(kind)[0]).toBe(kindTitle(kind)[0].toUpperCase());
    }
  });

  it("collapses both rebase backends onto one user-facing word", () => {
    // A user does not know or care which backend git chose; showing
    // "RebaseApply in progress" would be an internals leak.
    expect(kindLabel("Rebase")).toBe("rebase");
    expect(kindLabel("RebaseApply")).toBe("rebase");
  });

  it("labels every kind/action pair without repeating git's flag names", () => {
    for (const kind of ALL_KINDS) {
      for (const action of ALL_ACTIONS) {
        const label = actionLabel(kind, action);
        expect(label).toBeTruthy();
        expect(label).not.toContain("--");
        expect(actionConsequence(kind, action)).toBeTruthy();
      }
    }
  });

  it("calls the bisect escape what git calls it", () => {
    // `git bisect --abort` does not exist; a user copying the button text
    // into a terminal must land on a real command.
    expect(actionLabel("Bisect", "abort")).toBe("End bisect");
    expect(actionLabel("Merge", "abort")).toBe("Abort merge");
  });

  it("describes concluding a merge as committing, not continuing", () => {
    expect(actionLabel("Merge", "continue")).toBe("Commit the merge");
    expect(actionLabel("Rebase", "continue")).toBe("Continue rebase");
  });
});

describe("progressLabel", () => {
  it("renders a complete pair", () => {
    expect(progressLabel(op({ current_step: 2, total_steps: 7 }))).toBe("step 2 of 7");
  });

  it("renders nothing for a single-step operation", () => {
    expect(progressLabel(op())).toBeNull();
  });

  it("refuses a partial pair rather than inventing the missing half", () => {
    expect(progressLabel(op({ current_step: 2, total_steps: null }))).toBeNull();
    expect(progressLabel(op({ current_step: null, total_steps: 7 }))).toBeNull();
  });

  it("refuses incoherent counters instead of rendering them", () => {
    // A corrupt control file must not produce "step 9 of 3" in the UI.
    expect(progressLabel(op({ current_step: 9, total_steps: 3 }))).toBeNull();
    expect(progressLabel(op({ current_step: 0, total_steps: 3 }))).toBeNull();
    expect(progressLabel(op({ current_step: -1, total_steps: 3 }))).toBeNull();
    expect(progressLabel(op({ current_step: 1, total_steps: 0 }))).toBeNull();
    expect(
      progressLabel(op({ current_step: Number.NaN, total_steps: 3 })),
    ).toBeNull();
    expect(
      progressLabel(op({ current_step: 1, total_steps: Number.POSITIVE_INFINITY })),
    ).toBeNull();
  });
});

describe("headline", () => {
  it("states the operation alone when there is nothing else to say", () => {
    expect(headline(op({ head_ref: null }))).toBe("Merge in progress");
  });

  it("adds progress and the branch when known", () => {
    expect(
      headline(
        op({ kind: "Rebase", current_step: 2, total_steps: 7, head_ref: "side" }),
      ),
    ).toBe("Rebase in progress — step 2 of 7, rebasing side");
  });

  it("says a merge happens 'on' a branch, not 'rebasing' it", () => {
    expect(headline(op({ head_ref: "main" }))).toBe("Merge in progress — on main");
  });

  it("produces a usable sentence for every kind", () => {
    for (const kind of ALL_KINDS) {
      const line = headline(op({ kind }));
      expect(line).toMatch(/ in progress/);
      expect(line).not.toContain("undefined");
      expect(line).not.toContain("null");
    }
  });

  it("says 'rebasing' only for an actual rebase", () => {
    // Caught by looking at the rendered banner: every non-merge kind used to
    // read "… rebasing main", so a bisect announced that history was being
    // rewritten. Wrong, and alarming in the one place a user is already lost.
    for (const kind of ALL_KINDS) {
      const line = headline(op({ kind, head_ref: "main" }));
      const isRebase = kind === "Rebase" || kind === "RebaseApply";
      expect(
        line.includes("rebasing"),
        `${kind} headline was "${line}"`,
      ).toBe(isRebase);
      expect(line).toContain("main");
    }
    expect(headline(op({ kind: "Bisect", head_ref: "main" }))).toBe(
      "Bisect in progress — on main",
    );
    expect(
      headline(op({ kind: "CherryPick", head_ref: "main", current_step: 2, total_steps: 5 })),
    ).toBe("Cherry-pick in progress — step 2 of 5, on main");
  });
});

describe("nextStep", () => {
  it("counts the blocking files and points at the view that fixes them", () => {
    const text = nextStep(op({ conflicted_total: 3, available: ["abort"] }));
    expect(text).toContain("3 files");
    expect(text).toContain("Resolve view");
  });

  it("uses the singular for one file", () => {
    const text = nextStep(op({ conflicted_total: 1, available: ["abort"] }));
    expect(text).toContain("1 file");
    expect(text).not.toContain("1 files");
  });

  it("switches to the forward instruction once conflicts clear", () => {
    const text = nextStep(op({ conflicted_total: 0, available: ["abort", "continue"] }));
    expect(text).toContain("continue");
    expect(text).not.toContain("Resolve view");
  });

  it("does not invent an instruction when git wants something unmodelled", () => {
    // No conflicts, yet continue is not offered. Claiming "continue to finish"
    // would send the user at a button that is not there.
    const text = nextStep(op({ conflicted_total: 0, available: ["abort"] }));
    expect(text).toContain("terminal");
  });

  it("gives bisect its own instruction, since resolving files is not it", () => {
    expect(nextStep(op({ kind: "Bisect", available: ["abort"] }))).toContain("good or bad");
  });

  it("never names a git internal for any state", () => {
    for (const kind of ALL_KINDS) {
      for (const conflicted of [0, 1, 5]) {
        const text = nextStep(op({ kind, conflicted_total: conflicted }));
        for (const leak of ["MERGE_HEAD", "rebase-merge", "CHERRY_PICK_HEAD", ".git", "--"]) {
          expect(text).not.toContain(leak);
        }
      }
    }
  });
});

describe("destructiveness", () => {
  it("treats abort and skip as destructive and continue as safe", () => {
    expect(isDestructive("abort")).toBe(true);
    expect(isDestructive("skip")).toBe(true);
    expect(isDestructive("continue")).toBe(false);
  });

  it("warns that abort discards working-tree conflict edits", () => {
    // The single most expensive surprise in this feature: a user spends ten
    // minutes resolving, hits Abort expecting "undo the last step", and loses
    // all of it. The consequence text must say so.
    expect(actionConsequence("Merge", "abort")).toContain("discarded");
    expect(actionConsequence("Rebase", "abort")).toContain("discarded");
  });

  it("warns that skip drops the commit entirely", () => {
    expect(actionConsequence("CherryPick", "skip")).toContain("will not appear");
  });
});

describe("orderedActions", () => {
  it("leads with the forward action, never the destructive one", () => {
    const actions = orderedActions(op({ available: ["abort", "skip", "continue"] }));
    expect(actions[0]).toBe("continue");
    expect(actions).toEqual(["continue", "abort", "skip"]);
  });

  it("keeps the order stable however the backend ordered them", () => {
    const shuffled = orderedActions(op({ available: ["skip", "continue", "abort"] }));
    expect(shuffled).toEqual(["continue", "abort", "skip"]);
  });

  it("offers only what the backend allows — never a button git would refuse", () => {
    expect(orderedActions(op({ available: ["abort"] }))).toEqual(["abort"]);
    expect(orderedActions(op({ available: [] }))).toEqual([]);
  });

  it("ignores an unknown action from a newer backend rather than rendering it", () => {
    const rogue = op({ available: ["abort", "quit" as OperationAction] });
    expect(orderedActions(rogue)).toEqual(["abort"]);
  });
});

describe("tab marker and tooltip", () => {
  it("renders nothing for an idle repository", () => {
    expect(tabMarker(IDLE_OPERATION)).toBeNull();
    expect(tabTooltip(IDLE_OPERATION)).toBeNull();
  });

  it("shows the kind, with counters when there are steps", () => {
    expect(tabMarker({ operation: op(), probeFailed: false })).toBe("Merge");
    expect(
      tabMarker({
        operation: op({ kind: "Rebase", current_step: 2, total_steps: 7 }),
        probeFailed: false,
      }),
    ).toBe("Rebase 2/7");
  });

  it("never reports a failed probe as an idle repository", () => {
    // The failure this guards: the probe throws, the UI shows a clean repo,
    // and the user acts on a state the app never actually checked.
    const failed = { operation: null, probeFailed: true };
    expect(tabMarker(failed)).toBe("?");
    expect(tabTooltip(failed)).toContain("could not determine");
  });

  it("carries the full instruction in the tooltip", () => {
    const tip = tabTooltip({
      operation: op({ conflicted_total: 2, available: ["abort"] }),
      probeFailed: false,
    });
    expect(tip).toContain("Merge in progress");
    expect(tip).toContain("2 files");
  });
});

describe("blocking other mutations", () => {
  it("blocks while a merge, rebase, cherry-pick or revert is parked", () => {
    for (const kind of ALL_KINDS.filter((k) => k !== "Bisect")) {
      expect(blocksOtherMutations({ operation: op({ kind }), probeFailed: false })).toBe(true);
    }
  });

  it("does not block during a bisect, where committing is normal", () => {
    expect(
      blocksOtherMutations({ operation: op({ kind: "Bisect" }), probeFailed: false }),
    ).toBe(false);
  });

  it("does not block on an idle repository", () => {
    expect(blocksOtherMutations(IDLE_OPERATION)).toBe(false);
  });

  it("does not block on a failed probe", () => {
    // Deliberate: a probe failure must not freeze the whole app. It surfaces
    // as the "?" marker and its tooltip instead.
    expect(blocksOtherMutations({ operation: null, probeFailed: true })).toBe(false);
  });

  it("explains the block in terms of the operation and the escape", () => {
    const reason = blockedReason({ operation: op({ kind: "CherryPick" }), probeFailed: false }, "merge");
    expect(reason).toContain("cherry-pick");
    expect(reason).toContain("merge");
    expect(reason).toContain("abort");
  });

  it("returns no reason when nothing is blocking", () => {
    expect(blockedReason(IDLE_OPERATION, "commit")).toBeNull();
  });
});

describe("adversarial inputs", () => {
  it("survives a backend payload with every optional field missing", () => {
    // Field-by-field defaults are what keep an older/newer backend from
    // throwing inside a render.
    const sparse = {
      kind: "Merge",
      current_step: null,
      total_steps: null,
      head_ref: null,
      incoming_ref: null,
      conflicted_paths: [],
      conflicted_total: 0,
      available: [],
    } as RepoOperation;
    expect(() => headline(sparse)).not.toThrow();
    expect(() => nextStep(sparse)).not.toThrow();
    expect(orderedActions(sparse)).toEqual([]);
    expect(tabMarker({ operation: sparse, probeFailed: false })).toBe("Merge");
  });

  it("does not break on a hostile branch name", () => {
    const nasty = "</script><img src=x onerror=alert(1)>\n ";
    const line = headline(op({ head_ref: nasty }));
    // Svelte escapes on render; the model's job is only to not lose or mangle
    // the value, so a corrupted name is never shown as a different branch.
    expect(line).toContain(nasty);
  });

  it("handles an enormous conflict count without formatting oddly", () => {
    expect(nextStep(op({ conflicted_total: 250_000 }))).toContain("250000 files");
  });
});

describe("operationStatesEqual", () => {
  const base = { operation: op({ conflicted_paths: ["a", "b"], conflicted_total: 2 }), probeFailed: false };

  it("treats a rebuilt but identical payload as unchanged", () => {
    // This is the whole point: the snapshot builds a fresh object every poll.
    const rebuilt = {
      operation: op({ conflicted_paths: ["a", "b"], conflicted_total: 2 }),
      probeFailed: false,
    };
    expect(operationStatesEqual(base, rebuilt)).toBe(true);
  });

  it("is reflexive and handles both idle sides", () => {
    expect(operationStatesEqual(base, base)).toBe(true);
    expect(operationStatesEqual(IDLE_OPERATION, { operation: null, probeFailed: false })).toBe(true);
  });

  it("never calls idle equal to parked, in either direction", () => {
    expect(operationStatesEqual(base, IDLE_OPERATION)).toBe(false);
    expect(operationStatesEqual(IDLE_OPERATION, base)).toBe(false);
  });

  it("separates a failed probe from an idle repository", () => {
    expect(
      operationStatesEqual(IDLE_OPERATION, { operation: null, probeFailed: true }),
    ).toBe(false);
  });

  it("notices every field that changes what the banner renders", () => {
    const cases: Partial<RepoOperation>[] = [
      { kind: "Rebase" },
      { current_step: 1 },
      { total_steps: 4 },
      { head_ref: "other" },
      { incoming_ref: "abc" },
      { conflicted_total: 3 },
      { conflicted_paths: ["a"] },
      { conflicted_paths: ["a", "c"] },
      { available: ["abort", "continue"] },
      { warnings: ["degraded"] },
    ];
    for (const change of cases) {
      const changed = {
        operation: op({ conflicted_paths: ["a", "b"], conflicted_total: 2, ...change }),
        probeFailed: false,
      };
      expect(
        operationStatesEqual(base, changed),
        `change ${JSON.stringify(change)} went unnoticed`,
      ).toBe(false);
    }
  });

  it("treats a missing warnings array and an empty one as the same", () => {
    // The backend omits the field entirely when empty; that must not read as
    // a change on every single poll.
    const withField = { operation: op({ warnings: [] }), probeFailed: false };
    const withoutField = { operation: op({}), probeFailed: false };
    expect(operationStatesEqual(withField, withoutField)).toBe(true);
  });

  it("is order-sensitive on paths, the safe direction for a publish gate", () => {
    const reordered = {
      operation: op({ conflicted_paths: ["b", "a"], conflicted_total: 2 }),
      probeFailed: false,
    };
    expect(operationStatesEqual(base, reordered)).toBe(false);
  });
});
