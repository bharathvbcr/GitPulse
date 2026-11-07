/**
 * The Storage panel's user-clicked tuning apply (owner decision, 2026-11-06).
 *
 * The same small state machine as the commit-graph write, kept out of the
 * component so the rules are testable without a DOM: nothing is written until
 * the person has seen each exact command with its scope and its undo, and has
 * confirmed them.
 *
 *   idle -> previewing -> confirming -> running -> done | failed
 *
 * `confirm` is the only call that reaches `cmd_tuning_apply`, and it only does
 * so from `confirming`, echoing back the previewed argv list. The backend
 * refuses anything that is not what a fresh plan would run, and the command
 * gate judges every line. Nothing here runs on a timer, on mount, or from a
 * store subscription.
 */
import { writable, type Readable } from "svelte/store";
import type {
  TuningAdvice,
  TuningChange,
  TuningOutcome,
  TuningPlan,
} from "../storage/types";
import type { Guarded } from "../stores/harnessStore";

/**
 * Whether the Storage panel offers the apply button: at least one change clears
 * its measured threshold and no apply is already running. A report from before
 * the advisor, and one whose inputs could not be read, offer nothing.
 */
export function offersTuning(advice: TuningAdvice | null | undefined): boolean {
  return !!advice && advice.offers.length > 0 && !advice.apply_in_flight;
}

export type TuningApplyStep =
  | { phase: "idle" }
  | { phase: "previewing" }
  | { phase: "confirming"; changes: TuningChange[]; plan: TuningPlan }
  /** `plan` is absent for an apply this flow did not start (a remounted panel). */
  | { phase: "running"; plan?: TuningPlan; cancelling: boolean }
  | { phase: "done"; outcome: TuningOutcome }
  | { phase: "failed"; message: string };

export type TuningInvoke = <T>(command: string, args: Record<string, unknown>) => Promise<T>;

export interface TuningApplyFlow extends Readable<TuningApplyStep> {
  /** Asks the backend what applying `changes` would run, then waits for confirmation. */
  open(changes: TuningChange[]): Promise<void>;
  /** Runs the previewed argv list. Does nothing unless a preview is awaiting it. */
  confirm(): Promise<void>;
  /** Backs out of a preview, or asks the apply running here to stop. */
  cancel(): Promise<void>;
  /** Returns to idle after a finished or failed run. */
  dismiss(): void;
  /** Follows the scan's `apply_in_flight`. Never starts anything. */
  syncInFlight(inFlight: boolean): void;
}

/** The lines the plan will run, in order: what the backend compares the echo to. */
export function runnableArgv(plan: TuningPlan): string[][] {
  return plan.steps.filter((step) => step.skipped === null).map((step) => step.argv);
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createTuningApplyFlow(options: {
  repoPath: () => string;
  invoke: TuningInvoke;
  /** Called after an apply ran, so the panel can scan again. */
  onApplied?: (outcome: TuningOutcome) => void;
}): TuningApplyFlow {
  const { subscribe, set } = writable<TuningApplyStep>({ phase: "idle" });
  let step: TuningApplyStep = { phase: "idle" };
  const phaseNow = (): TuningApplyStep["phase"] => step.phase;
  const move = (next: TuningApplyStep) => {
    step = next;
    set(next);
  };
  /**
   * The repository the preview was made for. `confirm` and `cancel` use it, not
   * the live path: a tab switch between the preview and the click must not turn
   * a confirmation of one repository's commands into a write in another.
   */
  let pinned: string | null = null;
  const target = () => pinned ?? options.repoPath();

  return {
    subscribe,
    async open(changes) {
      if (step.phase !== "idle" && step.phase !== "failed" && step.phase !== "done") return;
      if (changes.length === 0) return;
      move({ phase: "previewing" });
      pinned = options.repoPath();
      try {
        const plan = await options.invoke<TuningPlan>("cmd_tuning_plan", {
          repoPath: pinned,
          changes,
        });
        if (phaseNow() === "previewing") move({ phase: "confirming", changes, plan });
      } catch (error) {
        if (phaseNow() === "previewing") move({ phase: "failed", message: messageOf(error) });
      }
    },
    async confirm() {
      if (step.phase !== "confirming") return;
      const { plan, changes } = step;
      move({ phase: "running", plan, cancelling: false });
      try {
        const result = await options.invoke<Guarded<TuningOutcome>>("cmd_tuning_apply", {
          repoPath: target(),
          changes,
          confirmedArgv: runnableArgv(plan),
        });
        move({ phase: "done", outcome: result.output });
        options.onApplied?.(result.output);
      } catch (error) {
        move({ phase: "failed", message: messageOf(error) });
      }
    },
    async cancel() {
      if (step.phase === "confirming" || step.phase === "previewing") {
        move({ phase: "idle" });
        return;
      }
      if (step.phase === "done" || step.phase === "failed") return;
      if (step.phase === "running") {
        if (step.cancelling) return;
        move({ ...step, cancelling: true });
      }
      try {
        const reached = await options.invoke<boolean>("cmd_tuning_apply_cancel", {
          repoPath: target(),
        });
        // `false`: the apply has not registered yet. The backend remembers the
        // stop for a few seconds, but the person is not left on "Stopping..."
        // and can ask again.
        if (!reached && step.phase === "running") move({ ...step, cancelling: false });
      } catch (error) {
        if (step.phase === "running") move({ phase: "failed", message: messageOf(error) });
      }
    },
    dismiss() {
      if (step.phase === "done" || step.phase === "failed") move({ phase: "idle" });
    },
    syncInFlight(inFlight: boolean) {
      if (inFlight && step.phase === "idle") {
        pinned = options.repoPath();
        move({ phase: "running", cancelling: false });
      } else if (!inFlight && step.phase === "running" && step.plan === undefined) {
        move({ phase: "idle" });
      }
    },
  };
}
