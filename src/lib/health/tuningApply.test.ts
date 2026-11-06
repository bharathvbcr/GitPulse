import { describe, expect, it, vi } from "vitest";
import type {
  TuningAdvice,
  TuningChange,
  TuningOutcome,
  TuningPlan,
  TuningStep,
} from "../storage/types";
import {
  createTuningApplyFlow,
  offersTuning,
  runnableArgv,
  type TuningApplyStep,
} from "./tuningApply";

function advice(over: Partial<TuningAdvice> = {}): TuningAdvice {
  return {
    report: {
      maintenance: { registered: false, strategy: null, auto: null },
      midx: false,
      objects: { packs: 40, loose_objects: 10, loose_exact: true, gc_auto: 6700, loose_over_gc_auto: false },
      index: { state: "present", version: 2, entries: 50_000, entries_exact: true, skip_hash: false, note: null },
      partial_clone: false,
      sparse_checkout: false,
      keys: [],
      config_note: null,
      notes: [],
    },
    offers: [{ change: "many_files", title: "Turn on feature.manyFiles", reason: "50000 tracked files" }],
    warnings: [],
    ...over,
  };
}

const MANY: TuningStep = {
  change: "many_files",
  argv: ["git", "config", "--local", "feature.manyFiles", "true"],
  writes: [{ scope: "repo_config", what: "feature.manyFiles = true" }],
  undo: [["git", "config", "--local", "--unset", "feature.manyFiles"]],
  undo_note: null,
  notes: ["feature.manyFiles sets index.skipHash=true"],
  skipped: null,
};
const SKIPPED: TuningStep = {
  change: "maintenance",
  argv: [],
  writes: [],
  undo: [],
  undo_note: null,
  notes: [],
  skipped: "this repository is already registered for maintenance",
};
const PLAN: TuningPlan = { common_dir: "/repo/.git", steps: [MANY, SKIPPED], timeout_secs: 120 };

const OUTCOME: TuningOutcome = {
  cancelled: false,
  joined: false,
  elapsed_ms: 9,
  results: [{ change: "many_files", argv: MANY.argv, status: "applied", detail: null }],
  advice: advice({ offers: [] }),
};

function harness(overrides: Partial<Record<string, (args: Record<string, unknown>) => unknown>> = {}) {
  const calls: Array<{ command: string; args: Record<string, unknown> }> = [];
  const invoke = vi.fn(async (command: string, args: Record<string, unknown>) => {
    calls.push({ command, args });
    const handler = overrides[command];
    if (handler) return handler(args);
    if (command === "cmd_tuning_plan") return PLAN;
    if (command === "cmd_tuning_apply") return { policy: {}, output: OUTCOME };
    if (command === "cmd_tuning_apply_cancel") return true;
    throw new Error(`unexpected ${command}`);
  });
  const onApplied = vi.fn();
  let path = "/repo";
  const flow = createTuningApplyFlow({ repoPath: () => path, invoke: invoke as never, onApplied });
  let step: TuningApplyStep = { phase: "idle" };
  flow.subscribe((next) => (step = next));
  return { flow, calls, onApplied, step: () => step, setPath: (next: string) => (path = next) };
}

const ONE: TuningChange[] = ["many_files"];

describe("offersTuning", () => {
  it("offers the card only when something clears its threshold", () => {
    expect(offersTuning(advice())).toBe(true);
    expect(offersTuning(advice({ offers: [] }))).toBe(false);
  });

  it("hides the card while an apply runs, and for a payload from before the advisor", () => {
    expect(offersTuning(advice({ apply_in_flight: true }))).toBe(false);
    expect(offersTuning(undefined)).toBe(false);
    expect(offersTuning(null)).toBe(false);
  });
});

describe("the apply flow", () => {
  it("previews first and writes nothing until confirm", async () => {
    const h = harness();
    await h.flow.open(ONE);
    expect(h.step().phase).toBe("confirming");
    expect(h.calls.map((c) => c.command)).toEqual(["cmd_tuning_plan"]);
    expect(h.calls[0].args).toEqual({ repoPath: "/repo", changes: ONE });
  });

  it("confirm echoes exactly the previewed runnable argv and skips what will not run", async () => {
    const h = harness();
    await h.flow.open(["many_files", "maintenance"]);
    await h.flow.confirm();
    const apply = h.calls.find((c) => c.command === "cmd_tuning_apply");
    expect(apply?.args).toEqual({
      repoPath: "/repo",
      changes: ["many_files", "maintenance"],
      confirmedArgv: [MANY.argv],
    });
    expect(runnableArgv(PLAN)).toEqual([MANY.argv]);
    expect(h.step()).toEqual({ phase: "done", outcome: OUTCOME });
    expect(h.onApplied).toHaveBeenCalledWith(OUTCOME);
  });

  it("confirm does nothing from any phase but confirming", async () => {
    const h = harness();
    await h.flow.confirm();
    expect(h.calls).toEqual([]);
    expect(h.step().phase).toBe("idle");
    await h.flow.open(ONE);
    await h.flow.confirm();
    await h.flow.confirm();
    expect(h.calls.filter((c) => c.command === "cmd_tuning_apply")).toHaveLength(1);
  });

  it("open with nothing selected asks the backend nothing", async () => {
    const h = harness();
    await h.flow.open([]);
    expect(h.calls).toEqual([]);
    expect(h.step().phase).toBe("idle");
  });

  it("a refused preview is a failure, with the backend's words, and nothing was applied", async () => {
    const h = harness({
      cmd_tuning_plan: () => {
        throw new Error("feature.manyFiles together with core.splitIndex breaks add-then-commit");
      },
    });
    await h.flow.open(ONE);
    expect(h.step()).toEqual({
      phase: "failed",
      message: "feature.manyFiles together with core.splitIndex breaks add-then-commit",
    });
    expect(h.calls.map((c) => c.command)).toEqual(["cmd_tuning_plan"]);
  });

  it("backing out of a preview applies nothing and returns to idle", async () => {
    const h = harness();
    await h.flow.open(ONE);
    await h.flow.cancel();
    expect(h.step().phase).toBe("idle");
    expect(h.calls.map((c) => c.command)).toEqual(["cmd_tuning_plan"]);
  });

  it("a tab switch between preview and confirm cannot move the write to another repository", async () => {
    const h = harness();
    await h.flow.open(ONE);
    h.setPath("/other");
    await h.flow.confirm();
    const apply = h.calls.find((c) => c.command === "cmd_tuning_apply");
    expect(apply?.args.repoPath).toBe("/repo");
  });

  it("stop asks the backend to cancel the running apply in the pinned repository", async () => {
    let release: (value: unknown) => void = () => {};
    const h = harness({
      cmd_tuning_apply: () => new Promise((resolve) => (release = resolve)),
    });
    await h.flow.open(ONE);
    const running = h.flow.confirm();
    await h.flow.cancel();
    expect(h.calls.at(-1)).toEqual({ command: "cmd_tuning_apply_cancel", args: { repoPath: "/repo" } });
    expect(h.step()).toMatchObject({ phase: "running", cancelling: true });
    release({ policy: {}, output: { ...OUTCOME, cancelled: true } });
    await running;
    expect(h.step().phase).toBe("done");
  });

  it("a stop that found no apply yet leaves the person able to ask again", async () => {
    let release: (value: unknown) => void = () => {};
    const h = harness({
      cmd_tuning_apply: () => new Promise((resolve) => (release = resolve)),
      cmd_tuning_apply_cancel: () => false,
    });
    await h.flow.open(ONE);
    const running = h.flow.confirm();
    await h.flow.cancel();
    expect(h.step()).toMatchObject({ phase: "running", cancelling: false });
    release({ policy: {}, output: OUTCOME });
    await running;
  });

  it("follows the scan's in-flight flag without starting anything", () => {
    const h = harness();
    h.flow.syncInFlight(true);
    expect(h.step()).toMatchObject({ phase: "running", cancelling: false });
    h.flow.syncInFlight(false);
    expect(h.step().phase).toBe("idle");
    expect(h.calls).toEqual([]);
  });

  it("a failed apply reports the message and can be dismissed", async () => {
    const h = harness({
      cmd_tuning_apply: () => {
        throw new Error("The confirmed commands are not the previewed ones; nothing ran");
      },
    });
    await h.flow.open(ONE);
    await h.flow.confirm();
    expect(h.step()).toMatchObject({ phase: "failed" });
    expect(h.onApplied).not.toHaveBeenCalled();
    h.flow.dismiss();
    expect(h.step().phase).toBe("idle");
  });
});
